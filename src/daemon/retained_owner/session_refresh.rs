//! Daemon-owned admission boundary for retained session refreshes.
//!
//! The port returns the public application projection alongside the lower
//! durable outcome.  MCP may adapt its envelope service to this port, but the
//! retained application route never consumes MCP view types or `ToolResult`.

use std::future::Future;
use std::pin::Pin;

use sha2::{Digest, Sha256};
use tracedecay_application::retained_surfaces::{
    SessionRefreshActionV1, SessionRefreshGrainV1, SessionRefreshRequestV1, SessionRefreshResultV1,
    SessionRefreshTemporalModeV1,
};
use tracedecay_application::{
    CancellationSignal, RequestContext, retained_surface_application_operation,
};
use tracedecay_domain::{
    ManifestDigest, ProjectId, RepositoryId, RetrievalGrainV1, SessionId, TemporalModeV1,
    UserProfileId, UtcMicros, WorktreeId,
};
use tracedecay_store::SessionRefreshFrontierV1;
use tracedecay_usecases::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use tracedecay_usecases::session::{
    SessionRefreshCancelDispositionKind, SessionRefreshHandle, SessionRefreshOutcome,
    SessionRefreshTarget, SessionRequestBinding,
};

const REQUEST_MAX_RESULTS: u64 = 64;
const REQUEST_MAX_BYTES: u64 = 64 * 1024 * 1024;
const REQUEST_MAX_WORK_UNITS: u64 = 10_000;

/// Lower action selected by the retained application request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetainedSessionRefreshActionV1 {
    Begin,
    Status,
    Cancel,
}

/// Public projection action. Begin retains the established start/join
/// vocabulary at the envelope boundary without making it receipt authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetainedSessionRefreshProjectionActionV1 {
    Start,
    Status,
    Cancel,
}

/// Fully admitted lower command. All scope, identity, digest, and cancellation
/// fields are constructed from the retained request context before the port is
/// invoked.
#[derive(Clone, Debug)]
pub(crate) struct RetainedSessionRefreshCommandV1 {
    pub(crate) action: RetainedSessionRefreshActionV1,
    pub(crate) projection_action: RetainedSessionRefreshProjectionActionV1,
    pub(crate) context: RequestContext,
    pub(crate) binding: SessionRequestBinding,
    pub(crate) target: SessionRefreshTarget,
    pub(crate) handle: Option<String>,
}

/// Exact durable outcome retained with the application projection.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RetainedSessionRefreshExecutionV1 {
    pub(crate) result: SessionRefreshResultV1,
    pub(crate) exact: SessionRefreshOutcome,
    pub(crate) exact_handle: Option<SessionRefreshHandle>,
    pub(crate) cancel_disposition: Option<SessionRefreshCancelDispositionKind>,
}

pub(crate) type RetainedSessionRefreshFutureV1<'a> =
    Pin<Box<dyn Future<Output = RetainedSessionRefreshExecutionV1> + Send + 'a>>;

/// Mounted lower refresh authority for retained application operations.
///
/// Implementations may encode a client handle, but must return the exact
/// lower outcome and handle separately so receipts cannot trust that encoding.
pub(crate) trait RetainedSessionRefreshPortV1: Send + Sync {
    fn execute_admitted(
        &self,
        command: RetainedSessionRefreshCommandV1,
    ) -> RetainedSessionRefreshFutureV1<'_>;
}

pub(crate) fn admitted_session_refresh_command(
    request: &SessionRefreshRequestV1,
    context: &RequestContext,
    cancellation_signal: &CancellationSignal,
    mounted_profile_id: &UserProfileId,
    mounted_session_store_id: &SessionStoreId,
    mounted_session_root_id: &SessionRootId,
    mounted_configuration_digest: &ManifestDigest,
) -> Result<RetainedSessionRefreshCommandV1, RetainedSurfaceExecutionErrorV1> {
    if cancellation_signal.context().token_id != context.cancellation().token_id {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    if request.request.project.profile_id != mounted_profile_id.as_str() {
        return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
    }
    if request.request.session.store_id != mounted_session_store_id.as_str()
        || request.request.session.root_id != mounted_session_root_id.as_str()
    {
        return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
    }
    if !request_matches_mounted_project_scope(request, context) {
        return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
    }

    let (action, projection_action) = match request.action {
        SessionRefreshActionV1::Begin => (
            RetainedSessionRefreshActionV1::Begin,
            RetainedSessionRefreshProjectionActionV1::Start,
        ),
        SessionRefreshActionV1::Status => (
            RetainedSessionRefreshActionV1::Status,
            RetainedSessionRefreshProjectionActionV1::Status,
        ),
        SessionRefreshActionV1::Cancel => (
            RetainedSessionRefreshActionV1::Cancel,
            RetainedSessionRefreshProjectionActionV1::Cancel,
        ),
    };
    let selectors = &request.request;
    match (action, selectors.handle.as_deref()) {
        (RetainedSessionRefreshActionV1::Begin, Some(_)) => {
            return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
        }
        (RetainedSessionRefreshActionV1::Status | RetainedSessionRefreshActionV1::Cancel, None) => {
            return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
        }
        _ => {}
    }
    if selectors
        .handle
        .as_deref()
        .is_some_and(|handle| handle.trim().is_empty())
    {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }

    let identity = admitted_identity(selectors)?;
    let resolved_scope = identity
        .session_request_scope()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    if resolved_scope != *context.scope() {
        return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
    }
    let target = admitted_target(selectors)?;
    let operation = retained_surface_application_operation(request.operation())
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unsupported)?;
    let capability_digest = CapabilityDigest::new(admitted_digest(
        b"tracedecay.retained.session-refresh.capability.v1\0",
        operation.capability_id().as_str().as_bytes(),
    ));
    let policy_digest = PolicyDigest::new(admitted_digest(
        b"tracedecay.retained.session-refresh.policy.v1\0",
        context.grant().digest.as_str().as_bytes(),
    ));
    let configuration_digest = ConfigurationDigest::new(admitted_digest(
        b"tracedecay.retained.session-refresh.configuration.v1\0",
        mounted_configuration_digest.as_str().as_bytes(),
    ));
    let cancellation = CancellationToken::for_admitted_application_request(
        context.cancellation().token_id.as_str(),
    )
    .ok_or(RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    if context.cancellation().is_cancelled() {
        cancellation.cancel();
    }
    let budgets = RequestBudgets::new(
        REQUEST_MAX_RESULTS,
        REQUEST_MAX_BYTES,
        REQUEST_MAX_WORK_UNITS,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    let binding = SessionRequestBinding::for_admitted_context(
        identity,
        capability_digest,
        policy_digest,
        configuration_digest,
        cancellation,
        budgets,
        context.grant().digest.clone(),
    );
    Ok(RetainedSessionRefreshCommandV1 {
        action,
        projection_action,
        context: context.clone(),
        binding,
        target,
        handle: selectors.handle.clone(),
    })
}

fn request_matches_mounted_project_scope(
    request: &SessionRefreshRequestV1,
    context: &RequestContext,
) -> bool {
    let project = &request.request.project;
    let scope = context.scope();
    let branch_matches = scope
        .reference
        .as_ref()
        .and_then(|reference| reference.as_str().strip_prefix("refs/heads/"))
        .is_some_and(|branch| branch == project.branch_id);
    project.id == scope.project_id.as_str()
        && project.repository_id == scope.repository_id.as_str()
        && project.worktree_id == scope.worktree_id.as_str()
        && branch_matches
}

fn admitted_identity(
    request: &tracedecay_application::SessionRefreshActionRequestV1,
) -> Result<ResolvedSessionIdentity, RetainedSurfaceExecutionErrorV1> {
    let profile_id = ProfileId::new(request.project.profile_id.clone())
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    let store_id = SessionStoreId::new(request.session.store_id.clone())
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    let root_id = SessionRootId::new(request.session.root_id.clone())
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    Ok(ResolvedSessionIdentity::for_project(
        profile_id,
        ProjectId::new(request.project.id.clone())
            .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
        store_id,
        root_id,
        ResolvedGitRoute::new(
            RepositoryId::new(request.project.repository_id.clone())
                .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
            WorktreeId::new(request.project.worktree_id.clone())
                .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
            BranchId::new(request.project.branch_id.clone())
                .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
        ),
    ))
}

fn admitted_target(
    request: &tracedecay_application::SessionRefreshActionRequestV1,
) -> Result<SessionRefreshTarget, RetainedSurfaceExecutionErrorV1> {
    let temporal_mode = match request.target.temporal_mode {
        SessionRefreshTemporalModeV1::Current => TemporalModeV1::Current,
        SessionRefreshTemporalModeV1::AsOf { cutoff } => TemporalModeV1::AsOf {
            cutoff: UtcMicros(
                i64::try_from(cutoff)
                    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
            ),
        },
        SessionRefreshTemporalModeV1::Evolution => TemporalModeV1::Evolution,
        SessionRefreshTemporalModeV1::Forensic => TemporalModeV1::Forensic,
    };
    let grain = match request.target.grain {
        SessionRefreshGrainV1::Occurrence => RetrievalGrainV1::Occurrence,
        SessionRefreshGrainV1::LogicalMessage => RetrievalGrainV1::LogicalMessage,
        SessionRefreshGrainV1::Turn => RetrievalGrainV1::Turn,
        SessionRefreshGrainV1::Session => RetrievalGrainV1::Session,
        SessionRefreshGrainV1::Thread => RetrievalGrainV1::Thread,
        SessionRefreshGrainV1::Agent => RetrievalGrainV1::Agent,
        SessionRefreshGrainV1::Summary => RetrievalGrainV1::Summary,
    };
    let frontier = SessionRefreshFrontierV1::new(
        request.target.frontier.observed_through,
        request.target.frontier.committed_through,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    SessionRefreshTarget::new(
        SessionId::new(request.session.id.clone())
            .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
        Some(request.source.scope.clone()),
        temporal_mode,
        grain,
        frontier,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)
}

fn admitted_digest(domain: &[u8], material: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(material);
    digest.finalize().into()
}

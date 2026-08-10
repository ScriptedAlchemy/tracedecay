//! Daemon-owned admission boundary for retained session refresh status.
//!
//! The mounted service is shared with MCP, while this adapter independently
//! binds the retained request to the canonical session application context.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use tracedecay_application::retained_surfaces::{
    RetainedSurfaceExecutionErrorV1, SessionRefreshActionRequestV1, SessionRefreshActionV1,
    SessionRefreshGrainV1, SessionRefreshRequestV1, SessionRefreshTemporalModeV1,
};
use tracedecay_application::{
    CancellationContext, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, RequestContext, retained_surface_application_operation,
};
use tracedecay_domain::{
    ManifestDigest, ProjectId, RepositoryId, RetrievalGrainV1, SessionId, TemporalModeV1,
    UserProfileId, UtcMicros, WorktreeId,
};
use tracedecay_store::SessionRefreshFrontierV1;
use tracedecay_usecases::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    session_application_grant_digest,
};
use tracedecay_usecases::session::{SessionRefreshTarget, SessionRequestBinding};

pub(crate) use crate::mcp::tools::SessionRefreshServicePort as RetainedSessionRefreshPortV1;
use crate::mcp::tools::{SessionRefreshAction, SessionRefreshCommand};

const REQUEST_MAX_RESULTS: u64 = 64;
const REQUEST_MAX_BYTES: u64 = 64 * 1024 * 1024;
const REQUEST_MAX_WORK_UNITS: u64 = 10_000;

pub(crate) fn admitted_session_refresh_command(
    request: &SessionRefreshRequestV1,
    context: &RequestContext,
    cancellation_signal: &CancellationSignal,
    mounted_profile_id: &UserProfileId,
    mounted_session_store_id: &SessionStoreId,
    mounted_session_root_id: &SessionRootId,
    mounted_configuration_digest: &ManifestDigest,
) -> Result<SessionRefreshCommand, RetainedSurfaceExecutionErrorV1> {
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

    if request.action != SessionRefreshActionV1::Status {
        return Err(RetainedSurfaceExecutionErrorV1::Unsupported);
    }
    let action = SessionRefreshAction::Status;
    let selectors = &request.request;
    if selectors.handle.is_none() {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
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
    let cancellation = CancellationToken::for_application_request(context.request_id().as_str());
    if context.cancellation().is_cancelled() {
        cancellation.cancel();
    }
    let budgets = RequestBudgets::new(
        REQUEST_MAX_RESULTS,
        REQUEST_MAX_BYTES,
        REQUEST_MAX_WORK_UNITS,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    let grant_digest = session_application_grant_digest(
        capability_digest,
        policy_digest,
        configuration_digest,
        &cancellation,
        budgets,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    let expires_at = std::cmp::min(context.grant().expires_at, context.deadline().expires_at);
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.retained.session-refresh")
            .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
        1,
        grant_digest,
        context.actor().clone(),
        context.grant().issued_at,
        expires_at,
        resolved_scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    let lower_context = RequestContext::new(
        context.actor().clone(),
        resolved_scope,
        grant,
        context.request_id().clone(),
        Deadline::new(expires_at).map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
        CancellationContext::active(
            cancellation
                .application_token_id()
                .ok_or(RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
        )
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    let binding = SessionRequestBinding::new(
        identity,
        capability_digest,
        policy_digest,
        configuration_digest,
        cancellation,
        budgets,
    );
    Ok(SessionRefreshCommand {
        action,
        context: lower_context,
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
    request: &SessionRefreshActionRequestV1,
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
    request: &SessionRefreshActionRequestV1,
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

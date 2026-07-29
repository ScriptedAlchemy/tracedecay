use std::fmt;

use sha2::{Digest, Sha256};
use tracedecay_application::RequestContext;
use tracedecay_domain::{
    RetrievalGrainV1, SessionId, SessionRefreshKeyV1, SessionRefreshOperationIdV1,
    SessionRefreshSourceTargetV1, SessionSourceCoverageReceiptV1, SessionSourceCoverageV1,
    SessionSourceFrontierV1, SessionSourceIdV1, SessionTemporalCoverageRequestV1,
    TemporalCoverageCountsV1, TemporalModeV1, UtcMicros,
};
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCancellationRequestV1,
    SessionRefreshDispositionV1, SessionRefreshFrontierV1, SessionRefreshProgressRequestV1,
    SessionRefreshProgressV1, SessionRefreshReceiptRequestV1, SessionRefreshReceiptV1,
    SessionRefreshStore, SessionRefreshTerminalStateV1, SessionStoreError,
};

use crate::application::context::{
    RequestInterruption, ResolvedSessionIdentity, SessionOwner, application_request_interruption,
    run_application_request_interruptible,
};
use crate::application::session::types::{
    SessionAccess, SessionAuthorizationError, SessionAuthorizationGrant, SessionRequestBinding,
    SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
};

const MAX_VERSION_LEN: usize = 128;
const MAX_SOURCE_SCOPE_LEN: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshConfiguration {
    projector_version: String,
    config_version: String,
}

impl SessionRefreshConfiguration {
    pub fn new(
        projector_version: impl Into<String>,
        config_version: impl Into<String>,
    ) -> Result<Self, SessionRefreshRequestError> {
        let projector_version = projector_version.into();
        let config_version = config_version.into();
        validate_component(
            &projector_version,
            MAX_VERSION_LEN,
            SessionRefreshRequestError::InvalidProjectorVersion,
        )?;
        validate_component(
            &config_version,
            MAX_VERSION_LEN,
            SessionRefreshRequestError::InvalidConfigVersion,
        )?;
        Ok(Self {
            projector_version,
            config_version,
        })
    }

    pub fn projector_version(&self) -> &str {
        &self.projector_version
    }

    pub fn config_version(&self) -> &str {
        &self.config_version
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshTarget {
    session_id: SessionId,
    source_scope: Option<String>,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    frozen_frontier: SessionRefreshFrontierV1,
}

impl SessionRefreshTarget {
    pub fn new(
        session_id: SessionId,
        source_scope: Option<String>,
        temporal_mode: TemporalModeV1,
        grain: RetrievalGrainV1,
        frozen_frontier: SessionRefreshFrontierV1,
    ) -> Result<Self, SessionRefreshRequestError> {
        if let Some(source_scope) = source_scope.as_deref() {
            validate_component(
                source_scope,
                MAX_SOURCE_SCOPE_LEN,
                SessionRefreshRequestError::InvalidSourceScope,
            )?;
        }
        Ok(Self {
            session_id,
            source_scope,
            temporal_mode,
            grain,
            frozen_frontier,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn source_scope(&self) -> Option<&str> {
        self.source_scope.as_deref()
    }

    pub const fn temporal_mode(&self) -> TemporalModeV1 {
        self.temporal_mode
    }

    pub const fn grain(&self) -> RetrievalGrainV1 {
        self.grain
    }

    pub const fn frozen_frontier(&self) -> SessionRefreshFrontierV1 {
        self.frozen_frontier
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionRefreshDigest([u8; 32]);

impl SessionRefreshDigest {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshHandle {
    operation_id: SessionRefreshOperationIdV1,
    target: SessionRefreshTarget,
    accepted_at: UtcMicros,
    caller_idempotency_digest: SessionRefreshDigest,
    join_digest: SessionRefreshDigest,
}

impl SessionRefreshHandle {
    pub fn operation_id(&self) -> &SessionRefreshOperationIdV1 {
        &self.operation_id
    }

    pub fn target(&self) -> &SessionRefreshTarget {
        &self.target
    }

    pub const fn accepted_at(&self) -> UtcMicros {
        self.accepted_at
    }

    pub const fn caller_idempotency_digest(&self) -> SessionRefreshDigest {
        self.caller_idempotency_digest
    }

    pub const fn join_digest(&self) -> SessionRefreshDigest {
        self.join_digest
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionRefreshOutcome {
    Started(SessionRefreshHandle),
    Joined(SessionRefreshHandle),
    Busy,
    Running(Option<SessionRefreshProgressV1>),
    Complete(SessionRefreshReceiptV1),
    Failed(SessionRefreshReceiptV1),
    Cancelled(SessionRefreshReceiptV1),
    Denied,
    WrongScope,
    Stale,
    NotFound,
    Aborted,
    DeadlineExceeded,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionRefreshSchedulerError;

impl fmt::Display for SessionRefreshSchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("session refresh scheduler unavailable")
    }
}

impl std::error::Error for SessionRefreshSchedulerError {}

pub trait SessionRefreshSchedulerPort {
    fn wake(&self) -> Result<(), SessionRefreshSchedulerError>;
}

impl<T> SessionRefreshSchedulerPort for &T
where
    T: SessionRefreshSchedulerPort + ?Sized,
{
    fn wake(&self) -> Result<(), SessionRefreshSchedulerError> {
        (*self).wake()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRefreshRequestError {
    InvalidProjectorVersion,
    InvalidConfigVersion,
    InvalidSourceScope,
}

impl fmt::Display for SessionRefreshRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidProjectorVersion => "invalid session refresh projector version",
            Self::InvalidConfigVersion => "invalid session refresh config version",
            Self::InvalidSourceScope => "invalid session refresh source scope",
        })
    }
}

impl std::error::Error for SessionRefreshRequestError {}

pub struct SessionRefreshService<A, S, W> {
    authorizer: A,
    store: S,
    scheduler: W,
    configuration: SessionRefreshConfiguration,
}

impl<A, S, W> SessionRefreshService<A, S, W> {
    pub fn new(
        authorizer: A,
        store: S,
        scheduler: W,
        configuration: SessionRefreshConfiguration,
    ) -> Self {
        Self {
            authorizer,
            store,
            scheduler,
            configuration,
        }
    }
}

impl<A, S, W> SessionRefreshService<A, S, W>
where
    A: SessionScopeAuthorizer,
    S: SessionRefreshStore,
    W: SessionRefreshSchedulerPort,
{
    pub async fn begin_or_join(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        target: SessionRefreshTarget,
    ) -> SessionRefreshOutcome {
        if let Some(outcome) = request_interruption(context, binding) {
            return outcome;
        }
        let grant = match authorize(&self.authorizer, context, binding, &target) {
            Ok(grant) => grant,
            Err(outcome) => return outcome,
        };
        let digests = refresh_digests(context, binding, &target, &grant, &self.configuration);
        let Ok(source_id) = SessionSourceIdV1::new(format!(
            "{}:{}",
            target.session_id().as_str(),
            target.source_scope().unwrap_or("all")
        )) else {
            return SessionRefreshOutcome::Unavailable;
        };
        let Ok(refresh_key) = SessionRefreshKeyV1::new(
            grant.scope().identity().root_id().as_str(),
            target.session_id().clone(),
            vec![match SessionRefreshSourceTargetV1::new(
                source_id,
                SessionSourceFrontierV1::new(target.frozen_frontier().observed_through()),
                SessionSourceFrontierV1::new(target.frozen_frontier().observed_through()),
            ) {
                Ok(source) => source,
                Err(_) => return SessionRefreshOutcome::Unavailable,
            }],
            self.configuration.projector_version(),
            format!("sha256:{}", hex::encode(digests.projection.as_bytes())),
        ) else {
            return SessionRefreshOutcome::Unavailable;
        };
        let request = SessionRefreshBeginOrJoinRequestV1::new(
            target.session_id().clone(),
            target.frozen_frontier(),
        )
        .with_refresh_key(refresh_key)
        .with_coverage_request(SessionTemporalCoverageRequestV1::new(
            target.temporal_mode(),
        ));
        let receipt = match await_with_request_controls(
            context,
            binding,
            self.store.begin_or_join_session_refresh(request),
        )
        .await
        {
            Ok(Ok(receipt)) => receipt,
            Ok(Err(SessionStoreError::IdempotencyConflict { .. })) => {
                return SessionRefreshOutcome::Busy;
            }
            Ok(Err(_)) => return SessionRefreshOutcome::Unavailable,
            Err(outcome) => return outcome,
        };
        if receipt.session_id() != target.session_id()
            || receipt.target_frontier() != target.frozen_frontier()
        {
            return SessionRefreshOutcome::Unavailable;
        }
        let handle = SessionRefreshHandle {
            operation_id: receipt.operation_id().clone(),
            target,
            accepted_at: receipt.accepted_at(),
            caller_idempotency_digest: digests.caller,
            join_digest: digests.join,
        };

        // The durable operation is authoritative once the store call returns.
        // A failed wake is recoverable by the daemon's restart scan or a later
        // equivalent caller, so it must not erase or misreport acceptance.
        let _ = self.scheduler.wake();
        match receipt.disposition() {
            SessionRefreshDispositionV1::Started => SessionRefreshOutcome::Started(handle),
            SessionRefreshDispositionV1::Joined => SessionRefreshOutcome::Joined(handle),
        }
    }

    pub async fn status(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        handle: &SessionRefreshHandle,
    ) -> SessionRefreshOutcome {
        if let Some(outcome) = request_interruption(context, binding) {
            return outcome;
        }
        if let Err(outcome) = self.authorize_handle(context, binding, handle) {
            return outcome;
        }
        let progress = match await_with_request_controls(
            context,
            binding,
            self.store
                .session_refresh_progress(SessionRefreshProgressRequestV1::new(
                    handle.operation_id.clone(),
                    handle.target.session_id.clone(),
                )),
        )
        .await
        {
            Ok(Ok(progress)) => progress,
            Ok(Err(_)) => return SessionRefreshOutcome::Unavailable,
            Err(outcome) => return outcome,
        }
        .map(|progress| {
            let source_coverage = progress.source_coverage().and_then(|coverage| {
                source_coverage_for_target(Some(coverage), &handle.target, progress.frontier())
            });
            match source_coverage {
                Some(coverage) => progress.with_source_coverage(coverage),
                None => progress,
            }
        });
        match self.read_receipt(context, binding, handle).await {
            Ok(Some(receipt)) => return terminal_outcome(receipt),
            Ok(None) => {}
            Err(outcome) => return outcome,
        }
        SessionRefreshOutcome::Running(progress)
    }

    pub async fn cancel(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        handle: &SessionRefreshHandle,
    ) -> SessionRefreshOutcome {
        if let Some(outcome) = request_interruption(context, binding) {
            return outcome;
        }
        if let Err(outcome) = self.authorize_handle(context, binding, handle) {
            return outcome;
        }
        let progress = match await_with_request_controls(
            context,
            binding,
            self.store
                .session_refresh_progress(SessionRefreshProgressRequestV1::new(
                    handle.operation_id.clone(),
                    handle.target.session_id.clone(),
                )),
        )
        .await
        {
            Ok(Ok(progress)) => progress,
            Ok(Err(_)) => return SessionRefreshOutcome::Unavailable,
            Err(outcome) => return outcome,
        };
        match self.read_receipt(context, binding, handle).await {
            Ok(Some(receipt)) => return terminal_outcome(receipt),
            Ok(None) => {}
            Err(outcome) => return outcome,
        }
        let (frontier, coverage) = progress.as_ref().map_or_else(
            || (handle.target.frozen_frontier(), empty_coverage()),
            |progress| (progress.frontier(), *progress.coverage()),
        );
        let request = SessionRefreshCancellationRequestV1::new(
            handle.operation_id.clone(),
            handle.target.session_id.clone(),
            frontier,
            coverage,
        );
        let request = match progress
            .as_ref()
            .and_then(SessionRefreshProgressV1::source_coverage)
            .cloned()
            .or_else(|| source_coverage_for_target(None, &handle.target, frontier))
        {
            Some(source_coverage) => request.with_source_coverage(source_coverage),
            None => request,
        };
        match await_with_request_controls(
            context,
            binding,
            self.store.cancel_session_refresh(request),
        )
        .await
        {
            Ok(Ok(receipt)) => {
                let _ = self.scheduler.wake();
                terminal_outcome(receipt)
            }
            Ok(Err(
                SessionStoreError::InvalidRefreshState { .. }
                | SessionStoreError::InvalidStateTransition { .. }
                | SessionStoreError::ReceiptIdentityMismatch { .. },
            )) => self.status(context, binding, handle).await,
            Ok(Err(_)) => SessionRefreshOutcome::Unavailable,
            Err(outcome) => outcome,
        }
    }

    // The outcome enum carries receipts/handles by value; boxing its variants
    // for this private Result would churn the public refresh API.
    #[allow(clippy::result_large_err)]
    fn authorize_handle(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        handle: &SessionRefreshHandle,
    ) -> Result<(), SessionRefreshOutcome> {
        let grant = authorize(&self.authorizer, context, binding, &handle.target)?;
        let digests = refresh_digests(
            context,
            binding,
            &handle.target,
            &grant,
            &self.configuration,
        );
        if digests.join != handle.join_digest || digests.caller != handle.caller_idempotency_digest
        {
            return Err(SessionRefreshOutcome::WrongScope);
        }
        Ok(())
    }

    async fn read_receipt(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        handle: &SessionRefreshHandle,
    ) -> Result<Option<SessionRefreshReceiptV1>, SessionRefreshOutcome> {
        match await_with_request_controls(
            context,
            binding,
            self.store
                .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
                    handle.operation_id.clone(),
                    handle.target.session_id.clone(),
                )),
        )
        .await
        {
            Ok(Ok(receipt)) => Ok(receipt.map(|receipt| {
                let source_coverage = receipt.source_coverage().and_then(|coverage| {
                    source_coverage_for_target(Some(coverage), &handle.target, receipt.frontier())
                });
                match source_coverage {
                    Some(coverage) => receipt.with_source_coverage(coverage),
                    None => receipt,
                }
            })),
            Ok(Err(_)) => Err(SessionRefreshOutcome::Unavailable),
            Err(outcome) => Err(outcome),
        }
    }
}

#[derive(Clone, Copy)]
struct RefreshDigests {
    caller: SessionRefreshDigest,
    join: SessionRefreshDigest,
    projection: SessionRefreshDigest,
}

// The outcome enum carries receipts/handles by value; boxing its variants
// for this private Result would churn the public refresh API.
#[allow(clippy::result_large_err)]
fn authorize<A>(
    authorizer: &A,
    context: &RequestContext,
    binding: &SessionRequestBinding,
    target: &SessionRefreshTarget,
) -> Result<SessionAuthorizationGrant, SessionRefreshOutcome>
where
    A: SessionScopeAuthorizer,
{
    let request = SessionScopeAuthorizationRequest::new(
        context.actor().clone(),
        binding.identity().clone(),
        target.session_id.clone(),
        target.source_scope.clone(),
        target.temporal_mode,
        target.grain,
        SessionAccess::Hydrate,
    )
    .map_err(|_| SessionRefreshOutcome::Unavailable)?;
    let grant = authorizer
        .authorize(context, binding, &request)
        .map_err(map_authorization_error)?;
    grant
        .validate(context, binding, &request)
        .map_err(map_authorization_error)?;
    Ok(grant)
}

fn map_authorization_error(error: SessionAuthorizationError) -> SessionRefreshOutcome {
    match error {
        SessionAuthorizationError::Denied | SessionAuthorizationError::WrongContext => {
            SessionRefreshOutcome::Denied
        }
        SessionAuthorizationError::WrongScope
        | SessionAuthorizationError::WrongTarget
        | SessionAuthorizationError::WrongAccess
        | SessionAuthorizationError::UnresolvedGitRoute
        | SessionAuthorizationError::UnresolvedApplicationScope => {
            SessionRefreshOutcome::WrongScope
        }
        SessionAuthorizationError::Unavailable
        | SessionAuthorizationError::InvalidGrantId
        | SessionAuthorizationError::InvalidProviderScope
        | SessionAuthorizationError::ZeroRevision => SessionRefreshOutcome::Unavailable,
    }
}

fn terminal_outcome(receipt: SessionRefreshReceiptV1) -> SessionRefreshOutcome {
    match receipt.state() {
        SessionRefreshTerminalStateV1::Complete => SessionRefreshOutcome::Complete(receipt),
        SessionRefreshTerminalStateV1::Failed => SessionRefreshOutcome::Failed(receipt),
        SessionRefreshTerminalStateV1::Cancelled => SessionRefreshOutcome::Cancelled(receipt),
    }
}

fn request_interruption(
    context: &RequestContext,
    binding: &SessionRequestBinding,
) -> Option<SessionRefreshOutcome> {
    match application_request_interruption(context, binding.cancellation()) {
        Some(RequestInterruption::Cancelled) => Some(SessionRefreshOutcome::Aborted),
        Some(RequestInterruption::DeadlineExceeded) => {
            Some(SessionRefreshOutcome::DeadlineExceeded)
        }
        None => None,
    }
}

async fn await_with_request_controls<T>(
    context: &RequestContext,
    binding: &SessionRequestBinding,
    future: impl std::future::Future<Output = T>,
) -> Result<T, SessionRefreshOutcome> {
    if let Some(outcome) = request_interruption(context, binding) {
        return Err(outcome);
    }
    run_application_request_interruptible(context, binding.cancellation(), future, || {})
        .await
        .map_err(|interruption| match interruption {
            RequestInterruption::Cancelled => SessionRefreshOutcome::Aborted,
            RequestInterruption::DeadlineExceeded => SessionRefreshOutcome::DeadlineExceeded,
        })
}

const fn empty_coverage() -> TemporalCoverageCountsV1 {
    TemporalCoverageCountsV1 {
        visible: 0,
        hidden: 0,
        unknown: 0,
        redacted: 0,
    }
}

fn source_coverage_for_target(
    existing: Option<&SessionSourceCoverageReceiptV1>,
    target: &SessionRefreshTarget,
    frontier: SessionRefreshFrontierV1,
) -> Option<SessionSourceCoverageReceiptV1> {
    let request = SessionTemporalCoverageRequestV1::new(target.temporal_mode());
    if let Some(existing) = existing {
        let sources = existing
            .sources()
            .iter()
            .map(|source| {
                SessionSourceCoverageV1::new(
                    source.source_id().clone(),
                    source.observed_frontier(),
                    source.committed_frontier(),
                    source.target_watermark(),
                    request.clone(),
                    source.covered_intervals().to_vec(),
                    source.missing_intervals().to_vec(),
                    source.state(),
                    source.reason().clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        return SessionSourceCoverageReceiptV1::new(request, sources).ok();
    }
    let source = SessionSourceCoverageV1::from_frontiers(
        SessionSourceIdV1::new(format!(
            "{}:{}",
            target.session_id().as_str(),
            target.source_scope().unwrap_or("all")
        ))
        .ok()?,
        SessionSourceFrontierV1::new(frontier.observed_through()),
        SessionSourceFrontierV1::new(frontier.committed_through()),
        SessionSourceFrontierV1::new(target.frozen_frontier().observed_through()),
        request.clone(),
    )
    .ok()?;
    SessionSourceCoverageReceiptV1::new(request, vec![source]).ok()
}

fn refresh_digests(
    context: &RequestContext,
    binding: &SessionRequestBinding,
    target: &SessionRefreshTarget,
    grant: &SessionAuthorizationGrant,
    configuration: &SessionRefreshConfiguration,
) -> RefreshDigests {
    let mut projection = CanonicalDigest::new("session-refresh-projection.v1");
    digest_identity(&mut projection, binding.identity());
    projection.string("session", target.session_id.as_str());
    projection.optional_string("source", target.source_scope.as_deref());
    projection.u64(
        "source_frontier",
        target.frozen_frontier.committed_through(),
    );
    projection.u64("target_frontier", target.frozen_frontier.observed_through());
    projection.string("projector_version", configuration.projector_version());
    projection.string("config_version", configuration.config_version());
    projection.bytes("capability_digest", grant.capability_digest().as_bytes());
    projection.bytes("policy_digest", grant.policy_digest().as_bytes());
    projection.bytes(
        "configuration_digest",
        grant.configuration_digest().as_bytes(),
    );
    let projection = projection.finish();

    let mut join = CanonicalDigest::new("session-refresh-join.v1");
    digest_identity(&mut join, binding.identity());
    join.string("session", target.session_id.as_str());
    join.optional_string("source", target.source_scope.as_deref());
    join.string("temporal_mode", target.temporal_mode.as_str());
    if let TemporalModeV1::AsOf { cutoff } = target.temporal_mode {
        join.i64("temporal_cutoff", cutoff.0);
    }
    join.string("grain", target.grain.as_str());
    join.string("access", "hydrate");
    join.u64(
        "source_frontier",
        target.frozen_frontier.committed_through(),
    );
    join.u64("target_frontier", target.frozen_frontier.observed_through());
    join.string("projector_version", configuration.projector_version());
    join.string("config_version", configuration.config_version());
    join.bytes("capability_digest", grant.capability_digest().as_bytes());
    join.bytes("policy_digest", grant.policy_digest().as_bytes());
    join.bytes(
        "configuration_digest",
        grant.configuration_digest().as_bytes(),
    );
    let join = join.finish();

    let mut caller = CanonicalDigest::new("session-refresh-caller-idempotency.v1");
    caller.string("actor", context.actor().as_str());
    caller.bytes("join_digest", join.as_bytes());
    RefreshDigests {
        caller: caller.finish(),
        join,
        projection,
    }
}

fn digest_identity(digest: &mut CanonicalDigest, identity: &ResolvedSessionIdentity) {
    match identity.owner() {
        SessionOwner::Profile { profile_id } => {
            digest.string("owner", "profile");
            digest.string("profile_id", profile_id.as_str());
            digest.optional_string("project_id", None);
        }
        SessionOwner::Project {
            profile_id,
            project_id,
        } => {
            digest.string("owner", "project");
            digest.string("profile_id", profile_id.as_str());
            digest.optional_string("project_id", Some(project_id.as_str()));
        }
    }
    digest.string("store_id", identity.store_id().as_str());
    digest.string("root_id", identity.root_id().as_str());
    if let Some(route) = identity.git_route() {
        digest.string("git_route", "present");
        digest.string("repository_id", route.repository_id().as_str());
        digest.string("worktree_id", route.worktree_id().as_str());
        digest.string("branch_id", route.branch_id().as_str());
    } else {
        digest.string("git_route", "absent");
    }
}

struct CanonicalDigest(Sha256);

impl CanonicalDigest {
    fn new(domain: &str) -> Self {
        let mut digest = Self(Sha256::new());
        digest.string("domain", domain);
        digest
    }

    fn string(&mut self, label: &str, value: &str) {
        self.bytes(label, value.as_bytes());
    }

    fn optional_string(&mut self, label: &str, value: Option<&str>) {
        match value {
            Some(value) => {
                self.bytes(label, &[1]);
                self.bytes(label, value.as_bytes());
            }
            None => self.bytes(label, &[0]),
        }
    }

    fn u64(&mut self, label: &str, value: u64) {
        self.bytes(label, &value.to_be_bytes());
    }

    fn i64(&mut self, label: &str, value: i64) {
        self.bytes(label, &value.to_be_bytes());
    }

    fn bytes(&mut self, label: &str, value: &[u8]) {
        self.0.update((label.len() as u64).to_be_bytes());
        self.0.update(label.as_bytes());
        self.0.update((value.len() as u64).to_be_bytes());
        self.0.update(value);
    }

    fn finish(self) -> SessionRefreshDigest {
        SessionRefreshDigest(self.0.finalize().into())
    }
}

fn validate_component(
    value: &str,
    max_len: usize,
    error: SessionRefreshRequestError,
) -> Result<(), SessionRefreshRequestError> {
    if value.is_empty()
        || value.len() > max_len
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(error);
    }
    Ok(())
}

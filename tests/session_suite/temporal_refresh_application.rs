use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::TempDir;
use tracedecay::application::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use tracedecay::application::session::{
    AuthorizationGrantId, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionRefreshConfiguration, SessionRefreshHandle, SessionRefreshOutcome,
    SessionRefreshSchedulerError, SessionRefreshSchedulerPort, SessionRefreshService,
    SessionRefreshTarget, SessionRequestBinding, SessionScopeAuthorizationRequest,
    SessionScopeAuthorizer,
};
use tracedecay::store::GlobalDbSessionTemporalStore;
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, RetrievalGrainV1, SessionId, TemporalCoverageCountsV1,
    TemporalModeV1, UtcMicros, WorktreeId,
};
use tracedecay_store::{
    SessionRefreshCompletionRequestV1, SessionRefreshFailureRequestV1, SessionRefreshFrontierV1,
    SessionRefreshProgressV1, SessionRefreshStore, SessionTemporalProjectionBatchV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::common::{LcmTestRuntime, open_lcm_db};

const DIGEST: [u8; 32] = [0x6b; 32];
const PROJECTOR_VERSION: &str = "session-temporal-projector.v1";
const CONFIG_VERSION: &str = "session-refresh-config.v1";

fn session_temporal_store(db: &LcmTestRuntime) -> GlobalDbSessionTemporalStore<'_> {
    db.session_temporal_store()
        .expect("registered profile session-temporal store")
}

#[derive(Clone, Copy)]
struct AllowAuthorizer;

impl SessionScopeAuthorizer for AllowAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.refresh.application").unwrap(),
            1,
            context,
            binding,
            request,
        )
    }
}

#[derive(Clone, Copy)]
struct DenyAuthorizer;

impl SessionScopeAuthorizer for DenyAuthorizer {
    fn authorize(
        &self,
        _context: &RequestContext,
        _binding: &SessionRequestBinding,
        _request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        Err(SessionAuthorizationError::Denied)
    }
}

#[derive(Clone, Default)]
struct RecordingWake {
    calls: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
}

impl RecordingWake {
    fn failing() -> Self {
        let wake = Self::default();
        wake.fail.store(true, Ordering::Release);
        wake
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

impl SessionRefreshSchedulerPort for RecordingWake {
    fn wake(&self) -> Result<(), SessionRefreshSchedulerError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.fail.load(Ordering::Acquire) {
            Err(SessionRefreshSchedulerError)
        } else {
            Ok(())
        }
    }
}

fn configuration() -> SessionRefreshConfiguration {
    SessionRefreshConfiguration::new(PROJECTOR_VERSION, CONFIG_VERSION).unwrap()
}

#[derive(Clone)]
struct TestRequestContext {
    request: RequestContext,
    binding: SessionRequestBinding,
}

impl TestRequestContext {
    fn binding(&self) -> &SessionRequestBinding {
        &self.binding
    }

    fn actor_id(&self) -> &ActorId {
        self.request.actor()
    }

    fn identity(&self) -> &ResolvedSessionIdentity {
        self.binding.identity()
    }

    fn capability_digest(&self) -> CapabilityDigest {
        self.binding.capability_digest()
    }

    fn policy_digest(&self) -> PolicyDigest {
        self.binding.policy_digest()
    }

    fn configuration_digest(&self) -> ConfigurationDigest {
        self.binding.configuration_digest()
    }

    fn budgets(&self) -> RequestBudgets {
        self.binding.budgets()
    }
}

impl std::ops::Deref for TestRequestContext {
    type Target = RequestContext;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

fn project_context(
    actor: &str,
    request: &str,
    profile: &str,
    project: &str,
    root: &str,
) -> TestRequestContext {
    request_context(
        actor,
        request,
        ResolvedSessionIdentity::for_project(
            ProfileId::new(profile).unwrap(),
            ProjectId::new(project).unwrap(),
            SessionStoreId::new(format!("store.{project}")).unwrap(),
            SessionRootId::new(root).unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new(format!("repository.{project}")).unwrap(),
                WorktreeId::new(format!("worktree.{project}")).unwrap(),
                BranchId::new("branch.refresh-application").unwrap(),
            ),
        ),
        CancellationToken::for_application_request(RequestId::new(request).unwrap().as_str()),
        UtcMicros(i64::MAX - 1),
    )
}

fn profile_context(actor: &str, request: &str, profile: &str, root: &str) -> TestRequestContext {
    request_context(
        actor,
        request,
        ResolvedSessionIdentity::for_profile(
            ProfileId::new(profile).unwrap(),
            SessionStoreId::new(format!("store.{profile}")).unwrap(),
            SessionRootId::new(root).unwrap(),
        ),
        CancellationToken::for_application_request(RequestId::new(request).unwrap().as_str()),
        UtcMicros(i64::MAX - 1),
    )
}

fn context_with_controls(
    template: &TestRequestContext,
    request: &str,
    expires_at: UtcMicros,
    cancellation: CancellationToken,
) -> TestRequestContext {
    request_context_with_digests(
        template.actor_id().as_str(),
        request,
        template.identity().clone(),
        template.capability_digest(),
        template.policy_digest(),
        template.configuration_digest(),
        cancellation,
        template.budgets(),
        expires_at,
    )
}

fn request_context(
    actor: &str,
    request: &str,
    identity: ResolvedSessionIdentity,
    cancellation: CancellationToken,
    expires_at: UtcMicros,
) -> TestRequestContext {
    request_context_with_digests(
        actor,
        request,
        identity,
        CapabilityDigest::new(DIGEST),
        PolicyDigest::new(DIGEST),
        ConfigurationDigest::new(DIGEST),
        cancellation,
        RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap(),
        expires_at,
    )
}

#[allow(clippy::too_many_arguments)]
fn request_context_with_digests(
    actor: &str,
    request: &str,
    identity: ResolvedSessionIdentity,
    capability: CapabilityDigest,
    policy: PolicyDigest,
    configuration: ConfigurationDigest,
    cancellation: CancellationToken,
    budgets: RequestBudgets,
    expires_at: UtcMicros,
) -> TestRequestContext {
    let actor = ActorId::new(actor).unwrap();
    let request_id = RequestId::new(request).unwrap();
    let scope = identity.application_scope().unwrap();
    let observed_at = application_observed_at();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.refresh.application.context").unwrap(),
        1,
        session_application_grant_digest(capability, policy, configuration, &cancellation, budgets)
            .unwrap(),
        actor.clone(),
        observed_at,
        UtcMicros(i64::MAX - 1),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.session.refresh").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.session.refresh").unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    let request = RequestContext::new(
        actor,
        scope,
        grant,
        request_id,
        Deadline::new(expires_at).unwrap(),
        CancellationContext::active(cancellation.application_token_id().unwrap()).unwrap(),
    )
    .unwrap();
    let binding = SessionRequestBinding::new(
        identity,
        capability,
        policy,
        configuration,
        cancellation,
        budgets,
    );
    TestRequestContext { request, binding }
}

fn frontier(observed: u64, committed: u64) -> SessionRefreshFrontierV1 {
    SessionRefreshFrontierV1::new(observed, committed).unwrap()
}

fn target(session: &str, observed: u64) -> SessionRefreshTarget {
    target_with_mode(session, observed, TemporalModeV1::Current)
}

fn target_with_mode(
    session: &str,
    observed: u64,
    temporal_mode: TemporalModeV1,
) -> SessionRefreshTarget {
    target_with_mode_and_grain(
        session,
        observed,
        temporal_mode,
        RetrievalGrainV1::LogicalMessage,
    )
}

fn target_with_mode_and_grain(
    session: &str,
    observed: u64,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
) -> SessionRefreshTarget {
    SessionRefreshTarget::new(
        SessionId::new(session).unwrap(),
        Some("cursor".to_owned()),
        temporal_mode,
        grain,
        frontier(observed, 0),
    )
    .unwrap()
}

fn started(outcome: SessionRefreshOutcome) -> SessionRefreshHandle {
    match outcome {
        SessionRefreshOutcome::Started(handle) => handle,
        other => panic!("expected started refresh, got {other:?}"),
    }
}

fn handle(outcome: SessionRefreshOutcome) -> SessionRefreshHandle {
    match outcome {
        SessionRefreshOutcome::Started(handle) | SessionRefreshOutcome::Joined(handle) => handle,
        other => panic!("expected accepted refresh, got {other:?}"),
    }
}

fn now() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros(),
        )
        .unwrap(),
    )
}

fn zero_coverage() -> TemporalCoverageCountsV1 {
    TemporalCoverageCountsV1 {
        visible: 0,
        hidden: 0,
        unknown: 0,
        redacted: 0,
    }
}

async fn persist_initial_progress(
    store: &GlobalDbSessionTemporalStore<'_>,
    session_id: &SessionId,
) -> SessionRefreshProgressV1 {
    let recovery = store
        .session_refresh_recovery(session_id)
        .await
        .unwrap()
        .unwrap();
    let progress = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        recovery.target_frontier(),
        zero_coverage(),
        1,
        0,
        now(),
    );
    let batch = SessionTemporalProjectionBatchV1::new(
        recovery.session_id().clone(),
        recovery.candidate_generation(),
        recovery.frozen_watermarks().clone(),
        vec![],
        vec![],
        vec![],
    )
    .unwrap()
    .with_checkpoint(
        0,
        recovery.target_frontier().committed_through(),
        recovery.target_frontier().committed_through(),
    )
    .unwrap();
    store
        .persist_session_refresh_projection_batch(progress.clone(), batch)
        .await
        .unwrap();
    progress
}

#[tokio::test]
async fn equivalent_requests_join_with_stable_digests_excluding_request_id() {
    let temp = TempDir::new().unwrap();
    let db = open_lcm_db(&temp).await;
    let wake = RecordingWake::default();
    let service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&db),
        wake.clone(),
        configuration(),
    );
    let first_context = project_context(
        "actor.cursor",
        "request.refresh.first",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let second_context = project_context(
        "actor.cursor",
        "request.refresh.second",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );

    let first = started(
        service
            .begin_or_join(
                &first_context,
                first_context.binding(),
                target("session.refresh.equivalent", 4),
            )
            .await,
    );
    let joined = handle(
        service
            .begin_or_join(
                &second_context,
                second_context.binding(),
                target("session.refresh.equivalent", 4),
            )
            .await,
    );

    assert_eq!(first.operation_id(), joined.operation_id());
    assert_eq!(first.join_digest(), joined.join_digest());
    assert_eq!(
        first.caller_idempotency_digest(),
        joined.caller_idempotency_digest()
    );
    assert_eq!(wake.calls(), 2);
}

#[tokio::test]
async fn query_only_mode_and_grain_share_one_projection_refresh() {
    let temp = TempDir::new().unwrap();
    let db = open_lcm_db(&temp).await;
    let wake = RecordingWake::default();
    let service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&db),
        wake.clone(),
        configuration(),
    );
    let context = project_context(
        "actor.cursor",
        "request.refresh.query-only",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let session = "session.refresh.query-only";
    let first = started(
        service
            .begin_or_join(
                &context,
                context.binding(),
                target_with_mode_and_grain(
                    session,
                    4,
                    TemporalModeV1::Current,
                    RetrievalGrainV1::LogicalMessage,
                ),
            )
            .await,
    );
    let joined = handle(
        service
            .begin_or_join(
                &context,
                context.binding(),
                target_with_mode_and_grain(
                    session,
                    4,
                    TemporalModeV1::Forensic,
                    RetrievalGrainV1::Occurrence,
                ),
            )
            .await,
    );

    assert_eq!(joined.operation_id(), first.operation_id());
    assert_ne!(joined.join_digest(), first.join_digest());
    assert_eq!(wake.calls(), 2);

    assert!(matches!(
        service.cancel(&context, context.binding(), &joined).await,
        SessionRefreshOutcome::Cancelled(receipt)
            if receipt
                .source_coverage()
                .expect("forensic source coverage")
                .request()
                .mode()
                == TemporalModeV1::Forensic
    ));
    assert!(matches!(
        service.status(&context, context.binding(), &first).await,
        SessionRefreshOutcome::Cancelled(receipt)
            if receipt
                .source_coverage()
                .expect("current source coverage")
                .request()
                .mode()
                == TemporalModeV1::Current
    ));
    assert!(matches!(
        service.status(&context, context.binding(), &joined).await,
        SessionRefreshOutcome::Cancelled(receipt)
            if receipt
                .source_coverage()
                .expect("forensic source coverage")
                .request()
                .mode()
                == TemporalModeV1::Forensic
    ));
}

#[tokio::test]
async fn conflicting_target_is_busy_and_does_not_wake_scheduler() {
    let temp = TempDir::new().unwrap();
    let db = open_lcm_db(&temp).await;
    let wake = RecordingWake::default();
    let service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&db),
        wake.clone(),
        configuration(),
    );
    let context = project_context(
        "actor.cursor",
        "request.refresh.target",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );

    assert!(matches!(
        service
            .begin_or_join(
                &context,
                context.binding(),
                target("session.refresh.target", 4),
            )
            .await,
        SessionRefreshOutcome::Started(_)
    ));
    assert_eq!(
        service
            .begin_or_join(
                &context,
                context.binding(),
                target("session.refresh.target", 5),
            )
            .await,
        SessionRefreshOutcome::Busy
    );
    assert_eq!(wake.calls(), 1);
}

#[tokio::test]
async fn wake_failure_leaves_recoverable_operation_that_joins_after_restart() {
    let temp = TempDir::new().unwrap();
    let db = open_lcm_db(&temp).await;
    let failing_wake = RecordingWake::failing();
    let context = project_context(
        "actor.cursor",
        "request.refresh.wake-failure",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let target = target("session.refresh.wake-failure", 4);
    let first = {
        let service = SessionRefreshService::new(
            AllowAuthorizer,
            session_temporal_store(&db),
            failing_wake.clone(),
            configuration(),
        );
        started(
            service
                .begin_or_join(&context, context.binding(), target.clone())
                .await,
        )
    };

    let recovery = session_temporal_store(&db)
        .session_refresh_recovery(target.session_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovery.operation_id(), first.operation_id());
    assert_eq!(failing_wake.calls(), 1);

    let healthy_wake = RecordingWake::default();
    let restarted = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&db),
        healthy_wake.clone(),
        configuration(),
    );
    let joined = handle(
        restarted
            .begin_or_join(&context, context.binding(), target)
            .await,
    );
    assert_eq!(joined.operation_id(), first.operation_id());
    assert_eq!(healthy_wake.calls(), 1);
    assert!(matches!(
        restarted.status(&context, context.binding(), &joined).await,
        SessionRefreshOutcome::Running(None)
    ));
}

#[tokio::test]
async fn status_and_cancel_reauthorize_and_preserve_terminal_coverage() {
    let temp = TempDir::new().unwrap();
    let db = open_lcm_db(&temp).await;
    let wake = RecordingWake::default();
    let context = project_context(
        "actor.cursor",
        "request.refresh.cancel",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let denied = SessionRefreshService::new(
        DenyAuthorizer,
        session_temporal_store(&db),
        wake.clone(),
        configuration(),
    );
    let allowed = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&db),
        wake.clone(),
        configuration(),
    );
    let target = target("session.refresh.cancel", 0);
    let accepted = started(
        allowed
            .begin_or_join(&context, context.binding(), target.clone())
            .await,
    );
    let progress =
        persist_initial_progress(&session_temporal_store(&db), target.session_id()).await;

    assert!(matches!(
        allowed
            .status(&context, context.binding(), &accepted)
            .await,
        SessionRefreshOutcome::Running(Some(found)) if found == progress
    ));
    assert_eq!(
        denied.status(&context, context.binding(), &accepted).await,
        SessionRefreshOutcome::Denied
    );
    assert_eq!(
        denied.cancel(&context, context.binding(), &accepted).await,
        SessionRefreshOutcome::Denied
    );

    let cancelled = allowed.cancel(&context, context.binding(), &accepted).await;
    let receipt = match cancelled {
        SessionRefreshOutcome::Cancelled(receipt) => receipt,
        other => panic!("expected durable cancellation, got {other:?}"),
    };
    assert_eq!(receipt.coverage(), progress.coverage());
    assert_eq!(wake.calls(), 2);
    assert!(matches!(
        allowed
            .status(&context, context.binding(), &accepted)
            .await,
        SessionRefreshOutcome::Cancelled(found) if found == receipt
    ));
}

#[tokio::test]
async fn project_and_profile_scopes_are_isolated_without_root_fallback() {
    let project_temp = TempDir::new().unwrap();
    let profile_temp = TempDir::new().unwrap();
    let project_db = open_lcm_db(&project_temp).await;
    let profile_db = open_lcm_db(&profile_temp).await;
    let project_context = project_context(
        "actor.cursor",
        "request.refresh.project",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let profile_context = profile_context(
        "actor.cursor",
        "request.refresh.profile",
        "profile.primary",
        "root.profile",
    );
    let project_service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&project_db),
        RecordingWake::default(),
        configuration(),
    );
    let profile_service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&profile_db),
        RecordingWake::default(),
        configuration(),
    );
    let target = target("session.refresh.scope", 0);

    let project = started(
        project_service
            .begin_or_join(&project_context, project_context.binding(), target.clone())
            .await,
    );
    let profile = started(
        profile_service
            .begin_or_join(&profile_context, profile_context.binding(), target)
            .await,
    );

    assert_ne!(project.join_digest(), profile.join_digest());
    assert_eq!(
        project_service
            .status(&profile_context, profile_context.binding(), &project)
            .await,
        SessionRefreshOutcome::WrongScope
    );
    assert_eq!(
        profile_service
            .status(&project_context, project_context.binding(), &profile)
            .await,
        SessionRefreshOutcome::WrongScope
    );
}

#[tokio::test]
async fn status_maps_complete_and_failed_receipts_without_error_details() {
    let complete_temp = TempDir::new().unwrap();
    let failed_temp = TempDir::new().unwrap();
    let complete_db = open_lcm_db(&complete_temp).await;
    let failed_db = open_lcm_db(&failed_temp).await;
    let context = project_context(
        "actor.cursor",
        "request.refresh.terminals",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );

    let complete_store = session_temporal_store(&complete_db);
    let complete_service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&complete_db),
        RecordingWake::default(),
        configuration(),
    );
    let complete_target = target("session.refresh.complete", 0);
    let complete_handle = started(
        complete_service
            .begin_or_join(&context, context.binding(), complete_target.clone())
            .await,
    );
    let complete_progress =
        persist_initial_progress(&complete_store, complete_target.session_id()).await;
    complete_store
        .complete_session_refresh(
            SessionRefreshCompletionRequestV1::new(
                complete_handle.operation_id().clone(),
                complete_target.session_id().clone(),
                complete_progress.frontier(),
                *complete_progress.coverage(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        complete_service
            .status(&context, context.binding(), &complete_handle)
            .await,
        SessionRefreshOutcome::Complete(receipt)
            if receipt.coverage() == complete_progress.coverage()
    ));

    let failed_store = session_temporal_store(&failed_db);
    let failed_service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&failed_db),
        RecordingWake::default(),
        configuration(),
    );
    let failed_target = target("session.refresh.failed", 0);
    let failed_handle = started(
        failed_service
            .begin_or_join(&context, context.binding(), failed_target.clone())
            .await,
    );
    let failed_progress = persist_initial_progress(&failed_store, failed_target.session_id()).await;
    failed_store
        .fail_session_refresh(
            SessionRefreshFailureRequestV1::new(
                failed_handle.operation_id().clone(),
                failed_target.session_id().clone(),
                failed_progress.frontier(),
                *failed_progress.coverage(),
                "source_unavailable",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        failed_service
            .status(&context, context.binding(), &failed_handle)
            .await,
        SessionRefreshOutcome::Failed(receipt)
            if receipt.coverage() == failed_progress.coverage()
                && receipt.failure_code().unwrap().as_str() == "source_unavailable"
    ));
}

#[tokio::test]
async fn concurrent_callers_share_one_operation_and_keep_caller_idempotency() {
    let temp = TempDir::new().unwrap();
    let db = open_lcm_db(&temp).await;
    let wake = RecordingWake::default();
    let service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&db),
        wake,
        configuration(),
    );
    let first_context = project_context(
        "actor.cursor.one",
        "request.refresh.concurrent.one",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let second_context = project_context(
        "actor.cursor.two",
        "request.refresh.concurrent.two",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let first = service.begin_or_join(
        &first_context,
        first_context.binding(),
        target("session.refresh.concurrent", 0),
    );
    let second = service.begin_or_join(
        &second_context,
        second_context.binding(),
        target("session.refresh.concurrent", 0),
    );

    let (first, second) = tokio::join!(first, second);
    assert!(
        matches!(
            (&first, &second),
            (
                SessionRefreshOutcome::Started(_),
                SessionRefreshOutcome::Joined(_)
            ) | (
                SessionRefreshOutcome::Joined(_),
                SessionRefreshOutcome::Started(_)
            )
        ),
        "exactly one caller must own operation creation: {first:?}, {second:?}"
    );
    let first = handle(first);
    let second = handle(second);
    assert_eq!(first.operation_id(), second.operation_id());
    assert_eq!(first.join_digest(), second.join_digest());
    assert_ne!(
        first.caller_idempotency_digest(),
        second.caller_idempotency_digest()
    );
}

#[tokio::test]
async fn cancel_before_first_progress_returns_durable_zero_coverage_receipt() {
    let temp = TempDir::new().unwrap();
    let db = open_lcm_db(&temp).await;
    let wake = RecordingWake::default();
    let service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&db),
        wake.clone(),
        configuration(),
    );
    let context = project_context(
        "actor.cursor",
        "request.refresh.cancel-empty",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let target = target("session.refresh.cancel-empty", 4);
    let accepted = started(
        service
            .begin_or_join(&context, context.binding(), target.clone())
            .await,
    );

    let receipt = match service.cancel(&context, context.binding(), &accepted).await {
        SessionRefreshOutcome::Cancelled(receipt) => receipt,
        other => panic!("expected durable zero-progress cancellation, got {other:?}"),
    };

    assert_eq!(receipt.frontier(), target.frozen_frontier());
    assert_eq!(receipt.coverage(), &zero_coverage());
    assert_eq!(wake.calls(), 2);
    assert!(
        session_temporal_store(&db)
            .session_refresh_recovery(target.session_id())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn application_preserves_each_temporal_mode_in_terminal_source_coverage() {
    for (suffix, mode) in [
        ("current", TemporalModeV1::Current),
        (
            "as-of",
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(17),
            },
        ),
        ("evolution", TemporalModeV1::Evolution),
        ("forensic", TemporalModeV1::Forensic),
    ] {
        let temp = TempDir::new().unwrap();
        let db = open_lcm_db(&temp).await;
        let service = SessionRefreshService::new(
            AllowAuthorizer,
            session_temporal_store(&db),
            RecordingWake::default(),
            configuration(),
        );
        let context = project_context(
            "actor.cursor",
            &format!("request.refresh.mode.{suffix}"),
            "profile.primary",
            "project.tracedecay",
            "root.project",
        );
        let accepted = started(
            service
                .begin_or_join(
                    &context,
                    context.binding(),
                    target_with_mode(&format!("session.refresh.mode.{suffix}"), 4, mode),
                )
                .await,
        );
        let receipt = match service.cancel(&context, context.binding(), &accepted).await {
            SessionRefreshOutcome::Cancelled(receipt) => receipt,
            other => panic!("expected durable cancellation, got {other:?}"),
        };
        assert_eq!(
            receipt
                .source_coverage()
                .expect("source-aware terminal coverage")
                .request()
                .mode(),
            mode
        );
    }
}

#[tokio::test]
async fn expired_or_cancelled_requests_do_not_create_refresh_operations() {
    let temp = TempDir::new().unwrap();
    let db = open_lcm_db(&temp).await;
    let wake = RecordingWake::default();
    let service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&db),
        wake.clone(),
        configuration(),
    );
    let template = project_context(
        "actor.cursor",
        "request.refresh.controls",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let expired = context_with_controls(
        &template,
        "request.refresh.expired",
        UtcMicros(0),
        CancellationToken::for_application_request(
            RequestId::new("request.refresh.expired").unwrap().as_str(),
        ),
    );
    let cancellation = CancellationToken::for_application_request(
        RequestId::new("request.refresh.cancelled")
            .unwrap()
            .as_str(),
    );
    cancellation.cancel();
    let cancelled = context_with_controls(
        &template,
        "request.refresh.cancelled",
        UtcMicros(i64::MAX - 1),
        cancellation,
    );

    assert_eq!(
        service
            .begin_or_join(
                &expired,
                expired.binding(),
                target("session.refresh.expired", 0),
            )
            .await,
        SessionRefreshOutcome::DeadlineExceeded
    );
    assert_eq!(
        service
            .begin_or_join(
                &cancelled,
                cancelled.binding(),
                target("session.refresh.cancelled", 0),
            )
            .await,
        SessionRefreshOutcome::Aborted
    );
    assert_eq!(wake.calls(), 0);
    let store = session_temporal_store(&db);
    assert!(
        store
            .session_refresh_recovery(&SessionId::new("session.refresh.expired").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .session_refresh_recovery(&SessionId::new("session.refresh.cancelled").unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn request_abort_and_deadline_do_not_claim_durable_operation_cancellation() {
    let temp = TempDir::new().unwrap();
    let db = open_lcm_db(&temp).await;
    let wake = RecordingWake::default();
    let service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&db),
        wake,
        configuration(),
    );
    let context = project_context(
        "actor.cursor",
        "request.refresh.interruption",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let handle = started(
        service
            .begin_or_join(
                &context,
                context.binding(),
                target("session.refresh.interruption", 4),
            )
            .await,
    );
    let expired = context_with_controls(
        &context,
        "request.refresh.status.expired",
        UtcMicros(0),
        CancellationToken::for_application_request(
            RequestId::new("request.refresh.status.expired")
                .unwrap()
                .as_str(),
        ),
    );
    let cancellation = CancellationToken::for_application_request(
        RequestId::new("request.refresh.cancel.aborted")
            .unwrap()
            .as_str(),
    );
    cancellation.cancel();
    let aborted = context_with_controls(
        &context,
        "request.refresh.cancel.aborted",
        UtcMicros(i64::MAX - 1),
        cancellation,
    );

    assert_eq!(
        service.status(&expired, expired.binding(), &handle).await,
        SessionRefreshOutcome::DeadlineExceeded
    );
    assert_eq!(
        service.cancel(&aborted, aborted.binding(), &handle).await,
        SessionRefreshOutcome::Aborted
    );
    assert!(matches!(
        service.status(&context, context.binding(), &handle).await,
        SessionRefreshOutcome::Running(_)
    ));
}

#[tokio::test]
async fn status_is_read_only_and_does_not_wake_the_daemon() {
    let temp = TempDir::new().unwrap();
    let db = open_lcm_db(&temp).await;
    let wake = RecordingWake::default();
    let service = SessionRefreshService::new(
        AllowAuthorizer,
        session_temporal_store(&db),
        wake.clone(),
        configuration(),
    );
    let context = project_context(
        "actor.cursor",
        "request.refresh.read-only-status",
        "profile.primary",
        "project.tracedecay",
        "root.project",
    );
    let accepted = started(
        service
            .begin_or_join(
                &context,
                context.binding(),
                target("session.refresh.read-only-status", 0),
            )
            .await,
    );

    assert_eq!(
        service.status(&context, context.binding(), &accepted).await,
        SessionRefreshOutcome::Running(None)
    );
    assert_eq!(
        service.status(&context, context.binding(), &accepted).await,
        SessionRefreshOutcome::Running(None)
    );
    assert_eq!(wake.calls(), 1);
}

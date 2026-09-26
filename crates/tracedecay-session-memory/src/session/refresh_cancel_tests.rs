//! Cancel must follow a refresh that finishes, or moves, between the receipt
//! read and the cancel write. The store reports that gap as an idempotency
//! conflict or a stale progress transition; neither is an unavailable service.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_contracts::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, RetrievalGrainV1, SessionId, SessionRefreshOperationIdV1, TemporalCoverageCountsV1,
    TemporalModeV1, UtcMicros,
};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_store::{
    SessionRefreshBeginOrJoinPermit, SessionRefreshBeginOrJoinReceiptV1,
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCancelPermit,
    SessionRefreshCancellationRequestV1, SessionRefreshCompletePermit,
    SessionRefreshCompletionRequestV1, SessionRefreshDispositionV1, SessionRefreshFailPermit,
    SessionRefreshFailureRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressPersistPermit,
    SessionRefreshProgressReadPermit, SessionRefreshProgressRequestV1, SessionRefreshProgressV1,
    SessionRefreshReceiptReadPermit, SessionRefreshReceiptRequestV1, SessionRefreshReceiptV1,
    SessionRefreshStore, SessionRefreshTerminalStateV1, SessionStoreError, SessionStoreResult,
    SessionTemporalCapabilitiesV1, SessionTemporalCapabilityProvider, SessionTemporalCapabilityV1,
};
use tracedecay_temporal_query::execution::ExecutionControl;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{
    SessionRefreshConfiguration, SessionRefreshOutcome, SessionRefreshService, SessionRefreshTarget,
};
use crate::context::{
    CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId, RequestBudgets,
    ResolvedSessionIdentity, SessionRootId, SessionStoreId, application_observed_at,
    session_application_grant_digest,
};
use crate::session::types::{
    AuthorizationGrantId, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionRequestBinding, SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
};

const DIGEST: [u8; 32] = [0x6b; 32];

struct AllowAuthorizer;

impl SessionScopeAuthorizer for AllowAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.session.refresh.cancel").unwrap(),
            1,
            context,
            binding,
            request,
        )
    }
}

struct TestRequest {
    request: RequestContext,
    binding: SessionRequestBinding,
}

fn test_request() -> TestRequest {
    let request_name = "request.refresh.cancel.terminal-race";
    let identity = ResolvedSessionIdentity::for_profile(
        ProfileId::new("profile.refresh").unwrap(),
        SessionStoreId::new("store.profile.refresh").unwrap(),
        SessionRootId::new("root.profile.refresh").unwrap(),
    );
    let cancellation =
        CancellationToken::for_application_request(RequestId::new(request_name).unwrap().as_str());
    let budgets = RequestBudgets::new(64, 64 * 1024 * 1024, 10_000).unwrap();
    let capability = CapabilityDigest::new(DIGEST);
    let policy = PolicyDigest::new(DIGEST);
    let configuration = ConfigurationDigest::new(DIGEST);
    let actor = ActorId::new("actor.refresh").unwrap();
    let scope = identity.session_request_scope().unwrap();
    let observed_at = application_observed_at();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.refresh.cancel.context").unwrap(),
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
        RequestId::new(request_name).unwrap(),
        Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
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
    TestRequest { request, binding }
}

fn refresh_target() -> SessionRefreshTarget {
    SessionRefreshTarget::new(
        SessionId::new("session.refresh.cancel.terminal-race").unwrap(),
        Some("codex".to_owned()),
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
        SessionRefreshFrontierV1::new(0, 0).unwrap(),
    )
    .unwrap()
}

fn configuration() -> SessionRefreshConfiguration {
    SessionRefreshConfiguration::new("session-temporal-projector.v1", "session-refresh-config.v1")
        .unwrap()
}

fn zero_coverage() -> TemporalCoverageCountsV1 {
    TemporalCoverageCountsV1 {
        visible: 0,
        hidden: 0,
        unknown: 0,
        redacted: 0,
    }
}

/// Receipt reads miss once, which is the window where the worker commits, and
/// the cancel write then sees the terminal receipt as an idempotency conflict.
struct TerminalWinsStore {
    capabilities: SessionTemporalCapabilitiesV1,
    operation_id: SessionRefreshOperationIdV1,
    receipt_reads: AtomicUsize,
}

impl TerminalWinsStore {
    fn new() -> Self {
        Self {
            capabilities: SessionTemporalCapabilitiesV1::new([
                SessionTemporalCapabilityV1::RefreshJoin,
                SessionTemporalCapabilityV1::RefreshProgressPersistence,
                SessionTemporalCapabilityV1::RefreshCancellation,
            ]),
            operation_id: SessionRefreshOperationIdV1::new("refresh.terminal-race").unwrap(),
            receipt_reads: AtomicUsize::new(0),
        }
    }

    fn completed_receipt(&self, session_id: &SessionId) -> SessionRefreshReceiptV1 {
        let frontier = SessionRefreshFrontierV1::new(0, 0).unwrap();
        SessionRefreshReceiptV1::completed(
            SessionRefreshCompletionRequestV1::new(
                self.operation_id.clone(),
                session_id.clone(),
                frontier,
                zero_coverage(),
            )
            .unwrap(),
            UtcMicros(1),
        )
    }
}

impl SessionTemporalCapabilityProvider for TerminalWinsStore {
    fn session_temporal_capabilities(&self) -> &SessionTemporalCapabilitiesV1 {
        &self.capabilities
    }
}

impl SessionRefreshStore for TerminalWinsStore {
    fn begin_or_join_session_refresh_supported(
        &self,
        _permit: SessionRefreshBeginOrJoinPermit,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshBeginOrJoinReceiptV1>> + Send {
        let receipt = SessionRefreshBeginOrJoinReceiptV1::new(
            self.operation_id.clone(),
            request.session_id().clone(),
            request.target_frontier(),
            SessionRefreshDispositionV1::Started,
            UtcMicros(1),
        );
        async move { Ok(receipt) }
    }

    async fn persist_session_refresh_progress_supported(
        &self,
        _permit: SessionRefreshProgressPersistPermit,
        _progress: SessionRefreshProgressV1,
    ) -> SessionStoreResult<SessionRefreshProgressV1> {
        refused()
    }

    async fn session_refresh_progress_supported(
        &self,
        _permit: SessionRefreshProgressReadPermit,
        _request: SessionRefreshProgressRequestV1,
    ) -> SessionStoreResult<Option<SessionRefreshProgressV1>> {
        Ok(None)
    }

    async fn complete_session_refresh_supported(
        &self,
        _permit: SessionRefreshCompletePermit,
        _request: SessionRefreshCompletionRequestV1,
        _execution_control: ExecutionControl,
    ) -> SessionStoreResult<SessionRefreshReceiptV1> {
        refused()
    }

    async fn fail_session_refresh_supported(
        &self,
        _permit: SessionRefreshFailPermit,
        _request: SessionRefreshFailureRequestV1,
    ) -> SessionStoreResult<SessionRefreshReceiptV1> {
        refused()
    }

    async fn cancel_session_refresh_supported(
        &self,
        _permit: SessionRefreshCancelPermit,
        _request: SessionRefreshCancellationRequestV1,
    ) -> SessionStoreResult<SessionRefreshReceiptV1> {
        Err(SessionStoreError::IdempotencyConflict {
            context: "refresh terminal retry",
        })
    }

    fn session_refresh_receipt_supported(
        &self,
        _permit: SessionRefreshReceiptReadPermit,
        request: SessionRefreshReceiptRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionRefreshReceiptV1>>> + Send {
        let reads = self.receipt_reads.fetch_add(1, Ordering::Relaxed);
        let receipt = (reads > 0).then(|| self.completed_receipt(request.session_id()));
        async move { Ok(receipt) }
    }
}

fn refused<T>() -> SessionStoreResult<T> {
    Err(SessionStoreError::IdempotencyConflict {
        context: "unused refresh test port",
    })
}

#[tokio::test]
async fn cancel_returns_the_terminal_receipt_when_the_worker_finishes_first() {
    let context = test_request();
    let service = SessionRefreshService::new(
        AllowAuthorizer,
        TerminalWinsStore::new(),
        || Ok(()),
        configuration(),
    );
    let started = service
        .begin_or_join(&context.request, &context.binding, refresh_target())
        .await;
    let SessionRefreshOutcome::Started(handle) = started else {
        panic!("begin must start the refresh, got {started:?}");
    };

    let cancelled = service
        .cancel(&context.request, &context.binding, &handle)
        .await;

    let SessionRefreshOutcome::Complete(receipt) = cancelled else {
        panic!("a cancel that loses to completion must return that receipt, got {cancelled:?}");
    };
    assert_eq!(receipt.operation_id(), handle.operation_id());
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
}

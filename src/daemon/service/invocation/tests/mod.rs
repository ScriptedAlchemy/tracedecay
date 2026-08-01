//! Shared test fixtures for the invocation-handler test suite; every
//! themed submodule below reaches these (and all production items) via
//! `use super::*;`.

use super::*;

use tracedecay_lsp::{
    CanonicalContextProjectionAuthority, CanonicalDiagnosticRefreshRequest,
    CanonicalDiagnosticSnapshotAuthority, ContextProjectionOutcome, ContextProjectionRegistration,
    ContextProjectionRequest, GenerationDiagnostics, LspAnalyzerCancellationAuthority,
    LspRequestId, UnavailableSemanticProvider,
};

#[derive(Default)]
struct RecordingFeedbackCycleObservations(std::sync::Mutex<Vec<Plan26FeedbackSourceEventV1>>);

impl Plan26FeedbackObservationEmitterV1 for RecordingFeedbackCycleObservations {
    fn observe_source_event(
        &self,
        _input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        source_event: Plan26FeedbackSourceEventV1,
    ) {
        self.0.lock().expect("observations").push(source_event);
    }

    fn observe_source_event_for_subject(
        &self,
        _subject_digest: ManifestDigest,
        _observed_at: UtcMicros,
        source_event: Plan26FeedbackSourceEventV1,
    ) {
        self.0.lock().expect("observations").push(source_event);
    }
}

struct UnavailableDiagnosticAuthority;

impl CanonicalDiagnosticSnapshotAuthority for UnavailableDiagnosticAuthority {
    fn refresh(
        &self,
        _request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>> {
        Box::pin(async { Err(LspRuntimeFailure::new("test-diagnostics-unavailable")) })
    }
}

struct UnavailableCancellationAuthority;

impl LspAnalyzerCancellationAuthority for UnavailableCancellationAuthority {
    fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
        false
    }
}

struct UnavailableContextAuthority;

impl CanonicalContextProjectionAuthority for UnavailableContextAuthority {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        Vec::new()
    }

    fn snapshot(
        &self,
        _root: AdmittedRoot,
        _request_id: LspRequestId,
        _request: ContextProjectionRequest,
    ) -> LspRuntimeFuture<ContextProjectionOutcome> {
        Box::pin(async { ContextProjectionOutcome::Unsupported })
    }
}

fn unavailable_lsp_session_factory() -> Arc<DaemonLspSessionFactory> {
    Arc::new(DaemonLspSessionFactory::new(
        tokio::runtime::Handle::current(),
        Arc::new(unavailable_feedback_cycle(Arc::new(
            RecordingFeedbackCycleObservations::default(),
        ))),
        Arc::new(UnavailableSemanticProvider),
        Arc::new(UnavailableDiagnosticAuthority),
        Arc::new(UnavailableCancellationAuthority),
        Arc::new(UnavailableContextAuthority),
        GatewayCapabilities::default(),
        UpstreamCapabilities::default(),
    ))
}

struct CountingFeedbackCycle(Arc<std::sync::atomic::AtomicUsize>);

impl FeedbackCycleRuntimePort for CountingFeedbackCycle {
    fn execute(
        &self,
        _request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let calls = Arc::clone(&self.0);
        Box::pin(async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }
}

fn unavailable_feedback_cycle(
    observations: Arc<RecordingFeedbackCycleObservations>,
) -> UnavailableFeedbackCycleRuntimeV1 {
    UnavailableFeedbackCycleRuntimeV1::new(
        ProjectId::new("project.feedback-cycle-unavailable").expect("project"),
        observations,
    )
}

fn hook_envelope(event: HookEventV2) -> HookEventEnvelopeV2 {
    HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [1; 16],
        producer: tracedecay_hooks::HookHostV1::Codex,
        protected_session_id: [2; 32],
        project_id: [3; 16],
        repository_id: [4; 16],
        worktree_id: [5; 16],
        worktree_epoch: 1,
        binding_token: [6; 32],
        ordering: tracedecay_hooks::HookOrderingV1::Unknown,
        observed_at: UtcMicros(1),
        event,
    }
}

fn hook_binding() -> HookScopeBindingV1 {
    HookScopeBindingV1 {
        host: tracedecay_hooks::HookHostV1::Codex,
        project_id: [3; 16],
        repository_id: [4; 16],
        worktree_id: [5; 16],
        worktree_epoch: 1,
        binding_token: [6; 32],
        capabilities: [
            tracedecay_hooks::HookEventFamily::SessionBoundary,
            tracedecay_hooks::HookEventFamily::PromptBoundary,
            tracedecay_hooks::HookEventFamily::ToolLifecycle,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            tracedecay_hooks::HookEventFamily::TestLifecycle,
        ]
        .into_iter()
        .map(|family| tracedecay_hooks::HookCapabilityV1 {
            family,
            support: tracedecay_hooks::stock_event_support(
                tracedecay_hooks::HookHostV1::Codex,
                family,
            ),
        })
        .collect(),
    }
}

fn hook_lifecycle() -> ContextScoutLifecycleAddressV1 {
    ContextScoutLifecycleAddressV1 {
        profile_id: tracedecay_domain::UserProfileId::new("profile.pr13-hook").unwrap(),
        provider_id: tracedecay_domain::ProviderId::new("codex").unwrap(),
        project_id: ProjectId::new("project.pr13-hook").unwrap(),
        worktree_id: tracedecay_domain::WorktreeId::new("worktree.pr13-hook").unwrap(),
        session_id: tracedecay_domain::SessionId::new("session.pr13-hook").unwrap(),
        thread_id: tracedecay_domain::ThreadId::new("thread.pr13-hook").unwrap(),
        turn_id: tracedecay_domain::TurnId::new("turn.pr13-hook").unwrap(),
        agent_id: tracedecay_domain::AgentInstanceId::new("agent.pr13-hook").unwrap(),
        logical_message_id: tracedecay_domain::MessageId::new("message.pr13-hook").unwrap(),
    }
}

mod configuration_tests;
mod dispatch_tests;
mod feedback_tests;
mod git_tests;
mod lsp_tests;
mod plan26_tests;
mod primitive_tests;
mod registrars_tests;
mod types_tests;
mod work_tests;

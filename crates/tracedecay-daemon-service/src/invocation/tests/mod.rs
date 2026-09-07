//! Shared test fixtures for the invocation-handler unit suite.

use super::*;

use tracedecay_lsp::{
    CanonicalContextProjectionAuthority, CanonicalDiagnosticRefreshRequest,
    CanonicalDiagnosticSnapshotAuthority, ContextProjectionOutcome, ContextProjectionRegistration,
    ContextProjectionRequest, GenerationDiagnostics, LspAnalyzerCancellationAuthority,
    LspRequestId, UnavailableSemanticProvider,
};

#[derive(Default)]
struct RecordingFeedbackCycleObservations(std::sync::Mutex<Vec<FeedbackSourceEventV1>>);

impl FeedbackObservationEmitterV1 for RecordingFeedbackCycleObservations {
    fn observe_source_event(
        &self,
        _input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        source_event: FeedbackSourceEventV1,
    ) {
        self.0.lock().expect("observations").push(source_event);
    }

    fn observe_source_event_for_subject(
        &self,
        _subject_digest: ManifestDigest,
        _observed_at: UtcMicros,
        source_event: FeedbackSourceEventV1,
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

pub(crate) fn unavailable_lsp_session_factory() -> Arc<DaemonLspSessionFactory> {
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

fn unavailable_feedback_cycle(
    observations: Arc<RecordingFeedbackCycleObservations>,
) -> UnavailableFeedbackCycleRuntimeV1 {
    UnavailableFeedbackCycleRuntimeV1::new(
        ProjectId::new("project.feedback-cycle-unavailable").expect("project"),
        observations,
    )
}

mod configuration_tests;
mod dispatch_tests;
mod feedback_tests;
mod git_tests;
mod handoff_tests;
mod invocation_observability_tests;
mod project_admission_tests;

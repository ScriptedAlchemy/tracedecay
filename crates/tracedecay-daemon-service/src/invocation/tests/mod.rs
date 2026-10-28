//! Shared test fixtures for the invocation-handler unit suite.

use std::path::Path;

use super::*;

use tracedecay_lsp::{
    CanonicalContextProjectionAuthority, CanonicalDiagnosticRefreshRequest,
    CanonicalDiagnosticSnapshotAuthority, ContextProjectionOutcome, ContextProjectionRegistration,
    ContextProjectionRequest, GenerationDiagnostics, LspAnalyzerCancellationAuthority,
    LspRequestId, UnavailableSemanticProvider,
};
use url::Url;

fn fixture_directory_uri(path: &Path) -> String {
    Url::from_directory_path(path)
        .expect("fixture directory path must convert to a file URL")
        .to_string()
}

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

/// An absolute project root and the `file:` URI that resolves back to it on
/// the running host.
///
/// The daemon maps an admitted root URI to the owning project by converting it
/// with `Url::to_file_path`, and that conversion rejects a drive-less path on
/// Windows. A hardcoded `file:///name` fixture is therefore a Unix-only shape:
/// it resolves to no owner on Windows and every LSP open is refused.
/// `name` is a `/`-separated relative path such as `projects/recovery-a`.
pub(super) fn admitted_root_fixture(name: &str) -> (std::path::PathBuf, String) {
    if cfg!(windows) {
        (
            std::path::PathBuf::from(format!("C:\\{}", name.replace('/', "\\"))),
            format!("file:///C:/{name}"),
        )
    } else {
        (
            std::path::PathBuf::from(format!("/{name}")),
            format!("file:///{name}"),
        )
    }
}

mod configuration_registrars_tests;
mod dispatch_tests;
mod feedback_tests;
mod git_tests;
mod handoff_tests;
mod hook_orchestration_tests;
mod invocation_observability_tests;
mod lsp_lease_tests;
mod lsp_tests;
mod project_admission_tests;
mod project_lifecycle_tests;
mod source_edit_tests;
mod work_routing_fixtures;
mod work_tests;

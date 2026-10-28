//! Root integration tests for the daemon invocation service.
//!
//! These suites use composition-root fixtures (`TraceDecay`, host-admission,
//! session retrieval). Pure unit tests live in `tracedecay-daemon-service`.

use std::sync::Arc;

use tracedecay_application::feedback::observations::FeedbackObservationEmitterV1;
use tracedecay_application::lsp_runtime::DaemonLspSessionFactory;
use tracedecay_application::work::{
    WorkTaskSessionAdmittedRetrievalPortV1, WorkTaskSessionEvidenceRetrievalV1,
};
use tracedecay_contracts::feedback::observations::FeedbackSourceEventV1;
use tracedecay_contracts::{CapabilityGrantSnapshot, ResolvedScope};
use tracedecay_daemon_service::{
    DaemonInvocationService, DaemonWorkProposalRoutingAuthorityV1,
    UnavailableFeedbackCycleRuntimeV1,
};
use tracedecay_domain::{ManifestDigest, ProjectId, UtcMicros};
use tracedecay_lsp::{
    AdmittedRoot, CanonicalContextProjectionAuthority, CanonicalDiagnosticRefreshRequest,
    CanonicalDiagnosticSnapshotAuthority, ContextProjectionOutcome, ContextProjectionRegistration,
    ContextProjectionRequest, GatewayCapabilities, GenerationDiagnostics,
    LspAnalyzerCancellationAuthority, LspRequestId, LspRuntimeFailure, LspRuntimeFuture,
    UnavailableSemanticProvider, UpstreamCapabilities,
};

struct DeniedWorkEvidenceRetrieval;

impl WorkTaskSessionAdmittedRetrievalPortV1 for DeniedWorkEvidenceRetrieval {}

pub(super) fn denied_work_evidence_retrieval() -> WorkTaskSessionEvidenceRetrievalV1 {
    WorkTaskSessionEvidenceRetrievalV1::new(Arc::new(DeniedWorkEvidenceRetrieval))
}

pub(super) fn empty_work_proposal_routing(
    scope: ResolvedScope,
    grant: &CapabilityGrantSnapshot,
) -> (DaemonWorkProposalRoutingAuthorityV1, ManifestDigest) {
    let revision = tracedecay_domain::configuration::ConfigurationRevisionId::new(
        "configuration.revision.work-empty-routing",
    )
    .expect("configuration revision");
    let key = tracedecay_domain::configuration::SettingKey::new(
        tracedecay_domain::configuration::WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
    )
    .expect("work executable bindings key");
    // The runtime pin rejects a snapshot that omitted registry defaults
    // such as `index.include.v1`. Resolve through the same core registry
    // the daemon uses at project open, then overlay empty Work bindings.
    let snapshot = tracedecay_project::config::resolver::resolve_configuration(
        &tracedecay_project::config::registry::ConfigurationRegistry::core()
            .expect("configuration registry defaults"),
        &[tracedecay_project::config::resolver::ConfigurationLayerV1 {
            layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                project_id: scope.project_id.clone(),
            },
            revision_id: revision.clone(),
            entries: std::collections::BTreeMap::from([(
                key,
                tracedecay_domain::configuration::ConfigurationValueV1::WorkExecutableBindings(
                    Vec::new(),
                ),
            )]),
        }],
    )
    .expect("empty Work routing resolved through production defaults")
    .snapshot;
    let digest = snapshot.effective_behavior_digest.clone();
    let configuration = tracedecay_configuration::config::PinnedRuntimeConfiguration::new(
        tracedecay_configuration::config::RuntimeConfigurationTarget {
            profile_root: std::path::PathBuf::from("/profile"),
            project_id: scope.project_id.clone(),
            project_root: std::env::current_dir().expect("test project root"),
        },
        revision,
        snapshot,
    )
    .expect("empty pinned Work routing configuration");
    let routing =
        DaemonWorkProposalRoutingAuthorityV1::mount(scope, &configuration, &digest, grant)
            .expect("empty Work proposal routing");
    (routing, digest)
}

pub(super) async fn mount_test_work_observability(
    service: &DaemonInvocationService,
    project_root: &std::path::Path,
    database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    scope: &ResolvedScope,
    configuration_digest: &ManifestDigest,
) -> ManifestDigest {
    let configuration_provenance_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.test.work-observability-provenance.v1",
        configuration_digest,
    ))
    .expect("configuration provenance digest");
    let policy_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.daemon.configuration-policy.v1",
        &scope.scope_digest,
        configuration_digest,
        &configuration_provenance_digest,
    ))
    .expect("policy digest");
    service
        .mount_observability_producer(
            project_root.to_path_buf(),
            database,
            scope.project_id.clone(),
            configuration_digest.clone(),
            policy_digest.clone(),
        )
        .await
        .expect("mounted Work observability producer");
    policy_digest
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

fn unavailable_lsp_session_factory() -> Arc<DaemonLspSessionFactory> {
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
mod lsp_lease_tests;
mod types_tests;
mod work_evidence_journey_tests;
mod work_tests;

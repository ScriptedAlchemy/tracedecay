//! Root integration tests for the daemon invocation service.
//!
//! These suites use composition-root fixtures (`TraceDecay`, host-admission,
//! session retrieval). Pure unit tests live in `tracedecay-daemon-service`.

use std::sync::Arc;

use tracedecay_agent_hosts::agents::context_scout_ports::ContextScoutLifecycleAddressV1;
use tracedecay_application::ResolvedScope;
use tracedecay_application::feedback::observations::FeedbackSourceEventV1;
use tracedecay_daemon_service::{
    DaemonInvocationService, DaemonWorkProposalRoutingAuthorityV1,
    UnavailableFeedbackCycleRuntimeV1,
};
use tracedecay_domain::{ManifestDigest, ProjectId, UtcMicros};
use tracedecay_hooks::{HookEventEnvelopeV2, HookEventV2, HookScopeBindingV1};
use tracedecay_lsp::{
    AdmittedRoot, CanonicalContextProjectionAuthority, CanonicalDiagnosticRefreshRequest,
    CanonicalDiagnosticSnapshotAuthority, ContextProjectionOutcome, ContextProjectionRegistration,
    ContextProjectionRequest, GatewayCapabilities, GenerationDiagnostics,
    LspAnalyzerCancellationAuthority, LspRequestId, LspRuntimeFailure, LspRuntimeFuture,
    UnavailableSemanticProvider, UpstreamCapabilities,
};
use tracedecay_usecases::feedback::observations::FeedbackObservationEmitterV1;
use tracedecay_usecases::lsp_runtime::DaemonLspSessionFactory;

struct DeniedWorkEvidenceRetrieval;

impl tracedecay_session_runtime::session_retrieval::SessionApplicationRetrievalPortV1
    for DeniedWorkEvidenceRetrieval
{
    fn retrieve_admitted<'a>(
        &'a self,
        _context: &'a tracedecay_application::RequestContext,
        _query: tracedecay_session_memory::session::SessionTemporalQuery,
    ) -> tracedecay_session_runtime::session_retrieval::SessionApplicationRetrievalFutureV1<'a>
    {
        Box::pin(async {
            tracedecay_session_runtime::session_retrieval::SessionRetrievalServiceOutcome::Denied
        })
    }
}

pub(super) fn denied_work_evidence_retrieval()
-> crate::daemon::work_evidence_retrieval::DaemonWorkEvidenceRetrievalV1 {
    crate::daemon::work_evidence_retrieval::DaemonWorkEvidenceRetrievalV1::new(Arc::new(
        DeniedWorkEvidenceRetrieval,
    ))
}

pub(super) fn empty_work_proposal_routing(
    scope: ResolvedScope,
) -> (DaemonWorkProposalRoutingAuthorityV1, ManifestDigest) {
    let revision = tracedecay_domain::configuration::ConfigurationRevisionId::new(
        "configuration.revision.work-empty-routing",
    )
    .expect("configuration revision");
    let key = tracedecay_domain::configuration::SettingKey::new(
        tracedecay_domain::configuration::WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
    )
    .expect("work executable bindings key");
    let snapshot = tracedecay_domain::configuration::ConfigurationSnapshotV1::new(
        std::collections::BTreeMap::from([(
            key.clone(),
            tracedecay_domain::configuration::ConfigurationValueV1::WorkExecutableBindings(
                Vec::new(),
            ),
        )]),
        std::collections::BTreeMap::from([(
            key,
            vec![tracedecay_domain::configuration::ConfigurationCandidateV1 {
                layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                    project_id: scope.project_id.clone(),
                },
                revision_id: revision.clone(),
                disposition: tracedecay_domain::configuration::CandidateDispositionV1::Winning,
                safe_reason: None,
            }],
        )]),
    )
    .expect("empty Work routing snapshot");
    let digest = snapshot.effective_behavior_digest.clone();
    let routing = DaemonWorkProposalRoutingAuthorityV1::mount(scope, revision, &snapshot, &digest)
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
            configuration_provenance_digest,
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
        profile_id: tracedecay_domain::UserProfileId::new("profile.advisory-hook").unwrap(),
        provider_id: tracedecay_domain::ProviderId::new("codex").unwrap(),
        project_id: ProjectId::new("project.advisory-hook").unwrap(),
        worktree_id: tracedecay_domain::WorktreeId::new("worktree.advisory-hook").unwrap(),
        session_id: tracedecay_domain::SessionId::new("session.advisory-hook").unwrap(),
        thread_id: tracedecay_domain::ThreadId::new("thread.advisory-hook").unwrap(),
        turn_id: tracedecay_domain::TurnId::new("turn.advisory-hook").unwrap(),
        agent_id: tracedecay_domain::AgentInstanceId::new("agent.advisory-hook").unwrap(),
        logical_message_id: tracedecay_domain::MessageId::new("message.advisory-hook").unwrap(),
    }
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

/// The `file:` URI half of [`admitted_root_fixture`].
pub(super) fn admitted_root_uri_fixture(name: &str) -> String {
    admitted_root_fixture(name).1
}

mod configuration_registrars_tests;
mod lsp_lease_tests;
mod lsp_tests;
mod observability_tests;
mod primitive_tests;
mod project_lifecycle_tests;
mod types_tests;
mod work_evidence_journey_tests;
mod work_tests;

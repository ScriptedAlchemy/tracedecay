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

struct DeniedWorkEvidenceRetrieval;

impl crate::daemon::session_retrieval::SessionApplicationRetrievalPortV1
    for DeniedWorkEvidenceRetrieval
{
    fn retrieve_admitted<'a>(
        &'a self,
        _context: &'a RequestContext,
        _query: tracedecay_usecases::session::SessionTemporalQuery,
    ) -> crate::daemon::session_retrieval::SessionApplicationRetrievalFutureV1<'a> {
        Box::pin(async { crate::daemon::session_retrieval::SessionRetrievalServiceOutcome::Denied })
    }
}

pub(in crate::daemon) fn denied_work_evidence_retrieval()
-> crate::daemon::work_evidence_retrieval::DaemonWorkEvidenceRetrievalV1 {
    crate::daemon::work_evidence_retrieval::DaemonWorkEvidenceRetrievalV1::new(Arc::new(
        DeniedWorkEvidenceRetrieval,
    ))
}

pub(in crate::daemon) fn empty_work_proposal_routing(
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

pub(in crate::daemon) async fn mount_test_work_observability(
    service: &DaemonInvocationService,
    project_root: &std::path::Path,
    database: crate::global_db::RegisteredGlobalDbLeaseV1,
    scope: &ResolvedScope,
    configuration_digest: &ManifestDigest,
) -> ManifestDigest {
    let policy_digest =
        ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).expect("policy digest");
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

mod configuration_registrars_tests;
mod configuration_tests;
mod dispatch_tests;
mod feedback_tests;
mod git_tests;
mod handoff_tests;
mod invocation_observability_tests;
mod lsp_lease_tests;
mod lsp_tests;
mod primitive_tests;
mod project_admission_tests;
mod project_lifecycle_tests;
mod types_tests;
mod work_evidence_journey_tests;
mod work_tests;

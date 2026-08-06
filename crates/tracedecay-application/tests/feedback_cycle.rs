mod common;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::task::{Context, Poll, Waker};

use tracedecay_application::feedback::{
    FeedbackBudgetUsage, FeedbackCompletedPublicationV1, FeedbackCycleControl,
    FeedbackCycleDedupePort, FeedbackCycleDedupePublicationState, FeedbackCycleDedupeState,
    FeedbackCycleExecutionRequest, FeedbackCycleExecutionResult, FeedbackCycleService,
    FeedbackDiagnosticsPort, FeedbackDiagnosticsRequest, FeedbackImpactPort,
    FeedbackImpactPortOutcome, FeedbackImpactRequest, FeedbackObservationPort,
    FeedbackRuntimeStatePort, FeedbackRuntimeStateV1, GenerationBoundFeedbackDiagnosticsAdapter,
};
use tracedecay_application::{
    AnalyzerAdmittedDiagnosticProviderV1, AuthorizationService, CancellationContext,
    CurrentDiagnosticsRequest, Deadline, DiagnosticProviderDescriptor, DiagnosticProviderIdentity,
    DiagnosticProviderIdentityParts, DiagnosticProviderPort, DiagnosticProviderResult,
    DiagnosticProviderState, FreshnessState, GenerationDiagnosticHistoryPort,
    GenerationDiagnosticHistoryRequest, ProviderCoverage, ProviderDocumentIdentity,
    ProviderFreshness, ProviderOrigin, ProviderProvenance, ProviderSourceIdentity, RequestContext,
    RevisionDigest,
};
use tracedecay_domain::configuration::{
    AnalyzerExecutableId, AnalyzerExecutableReferenceV1, AnalyzerLanguageId,
    AnalyzerLanguageSelectionV1, AnalyzerPrivacyClassV1, AnalyzerResourceLimitsV1,
    AnalyzerRestartPolicyV1, AnalyzerSettingsV1,
};
use tracedecay_domain::feedback::{
    FeedbackActorContextV1, FeedbackAuthoritativeRuntimeStateV1, FeedbackBaselineHorizonV1,
    FeedbackBaselineStateV1, FeedbackBudgetV1, FeedbackContentIdentityV1, FeedbackCycleId,
    FeedbackCycleObservationV1, FeedbackCycleRequestV1, FeedbackCycleRuntimeSnapshotV1,
    FeedbackCycleTerminationV1, FeedbackDiagnosticBaselineIdentityV1, FeedbackDiagnosticBaselineV1,
    FeedbackDiagnosticClassificationV1, FeedbackDiagnosticV1, FeedbackDurabilityV1,
    FeedbackEvaluationInputV1, FeedbackEvaluationStageV1, FeedbackImpactStateV1, FeedbackImpactV1,
    FeedbackObservationKindV1, FeedbackScopeV1, FeedbackSessionDiagnosticV1, FeedbackTargetV1,
    FeedbackTriggerV1, ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, ComponentVersion, ContentDigest, DiagnosticEvidenceClassV1,
    DiagnosticProducerKindV1, DiagnosticProvenanceV1, DiagnosticRecordStateV1,
    DiagnosticSeverityV1, FileOccurrenceId, GenerationDiagnosticV1, HostInstanceId,
    LanguageDescriptorRevision, LanguageId, ProviderId, RefId, RepositoryId, RetrievalAnchorId,
    SessionId, SourceSpan, SymbolOccurrenceId, UtcMicros, WorktreeId,
};
use tracedecay_policy::analyzer::{
    AnalyzerAdmissionEvaluatorV1, AnalyzerAdmissionInputV1, AnalyzerAvailabilityV1,
    AnalyzerCandidateV1, AnalyzerExecutionLocationV1,
};
use tracedecay_policy::authorization::SourceAuthorizationEvaluatorV1;
use tracedecay_tool_catalog::CapabilityId;

const GENERATION: &str = "generation.v1.fixture.00000001";
const FILE: &str = "file.feedback.fixture";
const SYMBOL: &str = "symbol.feedback.fixture";

#[allow(dead_code)]
fn application_feedback_ports_are_object_safe(
    runtime: &dyn FeedbackRuntimeStatePort,
    diagnostics: &dyn FeedbackDiagnosticsPort,
    impact: &dyn FeedbackImpactPort,
    dedupe: &dyn FeedbackCycleDedupePort,
    observations: &dyn FeedbackObservationPort,
) {
    let _ = (runtime, diagnostics, impact, dedupe, observations);
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = Box::pin(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("feedback fixture futures must complete immediately"),
    }
}

#[derive(Clone)]
struct DiagnosticsFixture {
    calls: Rc<Cell<usize>>,
    results: Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>,
}

impl FeedbackDiagnosticsPort for DiagnosticsFixture {
    fn diagnostics<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a FeedbackDiagnosticsRequest,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<
        'a,
        Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>,
    > {
        self.calls.set(self.calls.get() + 1);
        let results = self.results.clone();
        Box::pin(async move { results })
    }

    fn diagnostic_history<'a>(
        &'a self,
        _context: &'a RequestContext,
        request: &'a FeedbackDiagnosticsRequest,
        _runtime: &'a FeedbackRuntimeStateV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, Vec<FeedbackDiagnosticBaselineV1>>
    {
        let baselines = request
            .providers
            .iter()
            .map(|provider| matching_baseline(&request.input, provider, Vec::new()))
            .collect();
        Box::pin(async move { baselines })
    }
}

#[derive(Clone)]
struct GenerationDiagnosticsSourceFixture {
    current_calls: Arc<AtomicUsize>,
    history_calls: Arc<AtomicUsize>,
    current: DiagnosticProviderResult<Vec<GenerationDiagnosticV1>>,
    history: DiagnosticProviderResult<Vec<GenerationDiagnosticV1>>,
}

impl DiagnosticProviderPort for GenerationDiagnosticsSourceFixture {
    fn current_diagnostics<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a CurrentDiagnosticsRequest,
    ) -> tracedecay_application::DiagnosticProviderFuture<'a, Vec<GenerationDiagnosticV1>> {
        Box::pin(async move {
            self.current_calls.fetch_add(1, Ordering::Relaxed);
            self.current.clone()
        })
    }
}

impl GenerationDiagnosticHistoryPort for GenerationDiagnosticsSourceFixture {
    fn diagnostics_for_generation<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GenerationDiagnosticHistoryRequest,
    ) -> tracedecay_application::DiagnosticProviderFuture<'a, Vec<GenerationDiagnosticV1>> {
        Box::pin(async move {
            self.history_calls.fetch_add(1, Ordering::Relaxed);
            self.history.clone()
        })
    }
}

#[derive(Clone)]
struct HistoryDiagnosticsFixture {
    calls: Rc<Cell<usize>>,
    history_calls: Rc<Cell<usize>>,
    results: Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>,
    baselines: Vec<FeedbackDiagnosticBaselineV1>,
}

impl FeedbackDiagnosticsPort for HistoryDiagnosticsFixture {
    fn diagnostics<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a FeedbackDiagnosticsRequest,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<
        'a,
        Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>,
    > {
        self.calls.set(self.calls.get() + 1);
        let results = self.results.clone();
        Box::pin(async move { results })
    }

    fn diagnostic_history<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a FeedbackDiagnosticsRequest,
        _runtime: &'a FeedbackRuntimeStateV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, Vec<FeedbackDiagnosticBaselineV1>>
    {
        self.history_calls.set(self.history_calls.get() + 1);
        let baselines = self.baselines.clone();
        Box::pin(async move { baselines })
    }
}

#[derive(Clone)]
struct ImpactFixture {
    calls: Rc<Cell<usize>>,
    outcome: FeedbackImpactPortOutcome,
}

impl FeedbackImpactPort for ImpactFixture {
    fn impact<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a FeedbackImpactRequest,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, FeedbackImpactPortOutcome> {
        self.calls.set(self.calls.get() + 1);
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }
}

struct DedupeFixture(FeedbackCycleDedupeState);

impl FeedbackCycleDedupePort for DedupeFixture {
    fn lookup_completed<'a>(
        &'a self,
        _context: &'a RequestContext,
        _key: &'a tracedecay_domain::feedback::FeedbackDedupeKeyV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, FeedbackCycleDedupeState> {
        let state = self.0;
        Box::pin(async move { state })
    }

    fn record_completed<'a>(
        &'a self,
        _context: &'a RequestContext,
        _publication: &'a FeedbackCompletedPublicationV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, FeedbackCycleDedupePublicationState>
    {
        Box::pin(async { FeedbackCycleDedupePublicationState::Recorded })
    }
}

#[derive(Clone)]
struct RecordingDedupeFixture {
    state: FeedbackCycleDedupeState,
    keys: Rc<RefCell<Vec<tracedecay_domain::feedback::FeedbackDedupeKeyV1>>>,
}

impl FeedbackCycleDedupePort for RecordingDedupeFixture {
    fn lookup_completed<'a>(
        &'a self,
        _context: &'a RequestContext,
        key: &'a tracedecay_domain::feedback::FeedbackDedupeKeyV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, FeedbackCycleDedupeState> {
        self.keys.borrow_mut().push(key.clone());
        let state = self.state;
        Box::pin(async move { state })
    }

    fn record_completed<'a>(
        &'a self,
        _context: &'a RequestContext,
        _publication: &'a FeedbackCompletedPublicationV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, FeedbackCycleDedupePublicationState>
    {
        Box::pin(async { FeedbackCycleDedupePublicationState::Recorded })
    }
}

#[derive(Clone)]
struct SerializedRaceDedupeFixture {
    barrier: Arc<Barrier>,
    completed: Arc<Mutex<BTreeSet<String>>>,
    record_calls: Arc<AtomicUsize>,
}

impl FeedbackCycleDedupePort for SerializedRaceDedupeFixture {
    fn lookup_completed<'a>(
        &'a self,
        _context: &'a RequestContext,
        _key: &'a tracedecay_domain::feedback::FeedbackDedupeKeyV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, FeedbackCycleDedupeState> {
        Box::pin(async { FeedbackCycleDedupeState::Unique })
    }

    fn record_completed<'a>(
        &'a self,
        _context: &'a RequestContext,
        publication: &'a FeedbackCompletedPublicationV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, FeedbackCycleDedupePublicationState>
    {
        let key = publication.dedupe_key.as_str().to_owned();
        let barrier = self.barrier.clone();
        let completed = self.completed.clone();
        let record_calls = self.record_calls.clone();
        Box::pin(async move {
            barrier.wait();
            record_calls.fetch_add(1, Ordering::Relaxed);
            if completed
                .lock()
                .expect("serialized dedupe fixture lock is not poisoned")
                .insert(key)
            {
                FeedbackCycleDedupePublicationState::Recorded
            } else {
                FeedbackCycleDedupePublicationState::Duplicate
            }
        })
    }
}

#[derive(Clone)]
struct ConcurrentRuntimeFixture(FeedbackRuntimeStateV1);

impl FeedbackRuntimeStatePort for ConcurrentRuntimeFixture {
    fn resolve<'a>(
        &'a self,
        _context: &'a RequestContext,
        _input: &'a FeedbackEvaluationInputV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, Option<FeedbackRuntimeStateV1>>
    {
        let runtime = self.0.clone();
        Box::pin(async move { Some(runtime) })
    }
}

#[derive(Clone)]
struct ConcurrentDiagnosticsFixture {
    results: Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>,
}

impl FeedbackDiagnosticsPort for ConcurrentDiagnosticsFixture {
    fn diagnostics<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a FeedbackDiagnosticsRequest,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<
        'a,
        Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>,
    > {
        let results = self.results.clone();
        Box::pin(async move { results })
    }

    fn diagnostic_history<'a>(
        &'a self,
        _context: &'a RequestContext,
        request: &'a FeedbackDiagnosticsRequest,
        _runtime: &'a FeedbackRuntimeStateV1,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, Vec<FeedbackDiagnosticBaselineV1>>
    {
        let baselines = request
            .providers
            .iter()
            .map(|provider| matching_baseline(&request.input, provider, Vec::new()))
            .collect();
        Box::pin(async move { baselines })
    }
}

#[derive(Clone)]
struct ConcurrentImpactFixture(FeedbackImpactPortOutcome);

impl FeedbackImpactPort for ConcurrentImpactFixture {
    fn impact<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a FeedbackImpactRequest,
    ) -> tracedecay_application::feedback::FeedbackPortFuture<'a, FeedbackImpactPortOutcome> {
        let outcome = self.0.clone();
        Box::pin(async move { outcome })
    }
}

#[derive(Clone, Default)]
struct NoopObservationFixture;

impl FeedbackObservationPort for NoopObservationFixture {
    fn observe(
        &self,
        _input: &FeedbackEvaluationInputV1,
        _observation: FeedbackCycleObservationV1,
    ) {
    }
}

#[derive(Clone, Default)]
struct ObservationFixture(Rc<RefCell<Vec<FeedbackCycleObservationV1>>>);

impl FeedbackObservationPort for ObservationFixture {
    fn observe(&self, _input: &FeedbackEvaluationInputV1, observation: FeedbackCycleObservationV1) {
        self.0.borrow_mut().push(observation);
    }
}

fn scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: common::scope().project_id,
        repository_id: common::id::<RepositoryId>("repository.fixture"),
        worktree_id: common::id::<WorktreeId>("worktree.fixture"),
        branch_ref: "refs/heads/main".to_owned(),
        head_commit_id: common::id::<CommitId>("commit.fixture"),
    }
}

fn baseline_horizon() -> FeedbackBaselineHorizonV1 {
    FeedbackBaselineHorizonV1 {
        comparison_generation_id: common::id::<CodeGenerationId>("generation.v1.feedback.previous"),
        comparison_generation_digest: common::digest(common::SHA256_B),
        comparison_head_commit_id: common::id::<CommitId>("commit.previous.fixture"),
        comparison_content_digest: common::digest(common::SHA256_B),
        watermark: common::digest(common::SHA256_B),
    }
}

fn saved_input() -> FeedbackEvaluationInputV1 {
    let request = FeedbackCycleRequestV1::new(
        common::id::<FeedbackCycleId>("cycle.feedback.fixture"),
        scope(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: common::digest(common::SHA256_A),
            file_digest: common::digest(common::SHA256_A),
        },
        FeedbackTriggerV1::PostEditHook,
        common::digest(common::SHA256_B),
        common::digest(common::SHA256_A),
        FeedbackBudgetV1::bounded(100, 100, 1_000, 1_000),
    )
    .unwrap();
    FeedbackEvaluationInputV1 {
        request,
        target: FeedbackTargetV1 {
            file: common::id::<FileOccurrenceId>(FILE),
            span: Some(SourceSpan {
                start_byte: 10,
                end_byte: 42,
            }),
            symbol: Some(common::id::<SymbolOccurrenceId>(SYMBOL)),
            generation_id: Some(common::id::<CodeGenerationId>(GENERATION)),
        },
        actor: FeedbackActorContextV1::default(),
        observed_at: UtcMicros(2),
    }
}

fn overlay_input() -> FeedbackEvaluationInputV1 {
    let session_id = common::id::<SessionId>("session.feedback.fixture");
    let owner_client_id = common::id::<HostInstanceId>("client.feedback.fixture");
    let request = FeedbackCycleRequestV1::new(
        common::id::<FeedbackCycleId>("cycle.feedback.overlay"),
        scope(),
        FeedbackContentIdentityV1::EphemeralOverlay {
            session_id: session_id.clone(),
            owner_client_id: owner_client_id.clone(),
            agent_id: None,
            document_version: 7,
            overlay_digest: common::digest(common::SHA256_A),
        },
        FeedbackTriggerV1::DocumentSave,
        common::digest(common::SHA256_B),
        common::digest(common::SHA256_A),
        FeedbackBudgetV1::bounded(100, 100, 1_000, 1_000),
    )
    .unwrap();
    FeedbackEvaluationInputV1 {
        request,
        target: FeedbackTargetV1 {
            file: common::id::<FileOccurrenceId>(FILE),
            span: Some(SourceSpan {
                start_byte: 10,
                end_byte: 42,
            }),
            symbol: Some(common::id::<SymbolOccurrenceId>(SYMBOL)),
            generation_id: None,
        },
        actor: FeedbackActorContextV1 {
            session_id: Some(session_id),
            client_id: Some(owner_client_id),
            agent_id: None,
            turn_id: None,
        },
        observed_at: UtcMicros(2),
    }
}

fn provider_identity(input: &FeedbackEvaluationInputV1) -> DiagnosticProviderIdentity {
    let source = match &input.request.content {
        FeedbackContentIdentityV1::SavedContent { .. } => ProviderSourceIdentity::CleanGeneration {
            generation: input.target.generation_id.clone().unwrap(),
        },
        FeedbackContentIdentityV1::EphemeralOverlay {
            session_id,
            owner_client_id,
            document_version,
            overlay_digest,
            ..
        } => ProviderSourceIdentity::SessionOverlay {
            session_id: session_id.clone(),
            client_id: owner_client_id.clone(),
            document_version: *document_version,
            overlay_digest: overlay_digest.clone(),
        },
    };
    DiagnosticProviderIdentity::new(DiagnosticProviderIdentityParts {
        scope: common::scope(),
        source,
        document: ProviderDocumentIdentity {
            file: input.target.file.clone(),
            content_digest: common::id::<ContentDigest>(common::SHA256_A),
            document_version: match &input.request.content {
                FeedbackContentIdentityV1::SavedContent { .. } => None,
                FeedbackContentIdentityV1::EphemeralOverlay {
                    document_version, ..
                } => Some(*document_version),
            },
        },
        producer: DiagnosticProviderDescriptor {
            provider: common::id::<ProviderId>("provider.feedback.fixture"),
            analyzer_revision: common::id::<ComponentVersion>("analyzer.feedback.v1"),
            language: common::id::<LanguageId>("rust"),
            language_descriptor_revision: common::id::<LanguageDescriptorRevision>(
                "language.rust.feedback.v1",
            ),
        },
        requested_capability: CapabilityId::new("capability.diagnostics.current").unwrap(),
        freshness: ProviderFreshness::current(UtcMicros(2)),
        coverage: ProviderCoverage::complete(1, 1),
        provenance: ProviderProvenance {
            origin: ProviderOrigin::ConfiguredAnalyzer,
            anchor: Some(common::id::<RetrievalAnchorId>(
                "anchor.provider.feedback.fixture",
            )),
        },
        configuration: RevisionDigest {
            revision: common::id::<ComponentVersion>("configuration.feedback.v1"),
            digest: input.request.configuration_digest.clone(),
        },
        policy: common::authority(&common::context(&common::operation()))
            .policy
            .clone(),
    })
    .unwrap()
}

fn admitted_provider(
    provider: &DiagnosticProviderIdentity,
    availability: AnalyzerAvailabilityV1,
    scope_authorized: bool,
) -> AnalyzerAdmittedDiagnosticProviderV1 {
    let analyzer_language = common::id::<AnalyzerLanguageId>("rust");
    let executable = common::id::<AnalyzerExecutableId>("analyzer.feedback.fixture");
    let input = AnalyzerAdmissionInputV1 {
        settings: AnalyzerSettingsV1 {
            schema_version: AnalyzerSettingsV1::SCHEMA_VERSION,
            selections: vec![AnalyzerLanguageSelectionV1 {
                language_id: analyzer_language.clone(),
                enabled: true,
                executable: AnalyzerExecutableReferenceV1::BuiltIn {
                    executable_id: executable.clone(),
                },
                arguments: Vec::new(),
                initialization_options: BTreeMap::new(),
                settings: BTreeMap::new(),
                environment_allowlist: BTreeSet::new(),
                privacy_class: AnalyzerPrivacyClassV1::NonSensitive,
                resource_limits: AnalyzerResourceLimitsV1 {
                    maximum_memory_mib: 256,
                    startup_timeout_millis: 1_000,
                    request_timeout_millis: 1_000,
                },
                restart_policy: AnalyzerRestartPolicyV1::RestartOnConfigurationChange,
            }],
        },
        language_id: analyzer_language,
        requested_capability: common::id::<tracedecay_domain::CapabilityId>(
            provider.requested_capability.as_str(),
        ),
        candidates: vec![AnalyzerCandidateV1 {
            executable_id: executable,
            approved_external_digest: None,
            language_id: common::id::<AnalyzerLanguageId>("rust"),
            capability_id: common::id::<tracedecay_domain::CapabilityId>(
                provider.requested_capability.as_str(),
            ),
            availability,
            execution_location: AnalyzerExecutionLocationV1::Local,
            scope_authorized,
            available_memory_mib: 512,
            catalog_digest: common::digest(common::SHA256_A),
        }],
        privacy_constraints: BTreeSet::new(),
        configuration_digest: provider.configuration.digest.clone(),
        policy_revision: provider.policy.revision,
        policy_digest: provider.policy.digest.clone(),
        evaluated_at: UtcMicros(2),
    };
    let snapshot = AnalyzerAdmissionEvaluatorV1::default().snapshot(&input);
    AnalyzerAdmittedDiagnosticProviderV1::from_plan20_plan35_snapshot(
        provider.clone(),
        input,
        snapshot,
    )
    .unwrap()
}

#[test]
fn analyzer_admission_rebinds_only_request_evidence_for_current_document() {
    let input = saved_input();
    let template = provider_identity(&input);
    let admission = admitted_provider(&template, AnalyzerAvailabilityV1::Available, true);
    let mut current = template.clone();
    current.source = ProviderSourceIdentity::CleanGeneration {
        generation: common::id::<CodeGenerationId>("generation.feedback.next"),
    };
    current.document.file = common::id::<FileOccurrenceId>("file.feedback.next");
    current.document.content_digest = common::id::<ContentDigest>(common::SHA256_B);
    current.freshness.observed_at = UtcMicros(3);

    assert!(admission.admits_identity(&current));

    let mut overlay = current.clone();
    overlay.source = ProviderSourceIdentity::SessionOverlay {
        session_id: common::id::<SessionId>("session.feedback.rebind"),
        client_id: common::id::<HostInstanceId>("client.feedback.rebind"),
        document_version: 7,
        overlay_digest: common::digest(common::SHA256_B),
    };
    overlay.document.document_version = Some(7);
    assert!(!admission.admits_identity(&overlay));

    current.configuration.digest = common::digest(common::SHA256_B);
    assert!(!admission.admits_identity(&current));
}

#[test]
fn generation_bound_diagnostics_reuses_exact_current_and_previous_generations() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let runtime = runtime_state(&input);
    let current_calls = Arc::new(AtomicUsize::new(0));
    let history_calls = Arc::new(AtomicUsize::new(0));
    let current = diagnostic(&input, "anchor.feedback.current");
    let mut previous = diagnostic(&input, "anchor.feedback.previous");
    previous.generation_id = runtime
        .authoritative
        .baseline_horizon
        .as_ref()
        .unwrap()
        .comparison_generation_id
        .clone();
    let source = GenerationDiagnosticsSourceFixture {
        current_calls: current_calls.clone(),
        history_calls: history_calls.clone(),
        current: DiagnosticProviderResult::new(
            provider.clone(),
            DiagnosticProviderState::SupportedComplete,
            Some(vec![current]),
        )
        .unwrap(),
        history: DiagnosticProviderResult::new(
            provider.clone(),
            DiagnosticProviderState::SupportedComplete,
            Some(vec![previous]),
        )
        .unwrap(),
    };
    let adapter = GenerationBoundFeedbackDiagnosticsAdapter::new(
        source,
        vec![admitted_provider(
            &provider,
            AnalyzerAvailabilityV1::Available,
            true,
        )],
    )
    .unwrap();
    let request = FeedbackDiagnosticsRequest {
        input: input.clone(),
        providers: vec![provider.clone()],
    };

    let context = common::context(&common::operation());
    let current = block_on(adapter.diagnostics(&context, &request));
    let baselines = block_on(adapter.diagnostic_history(&context, &request, &runtime));

    assert_eq!(current_calls.load(Ordering::Relaxed), 1);
    assert_eq!(history_calls.load(Ordering::Relaxed), 1);
    assert_eq!(current[0].state, DiagnosticProviderState::SupportedComplete);
    assert_eq!(baselines.len(), 1);
    assert_eq!(baselines[0].state, FeedbackBaselineStateV1::Complete);
    assert_eq!(
        baselines[0].diagnostic_anchors,
        vec![common::id::<RetrievalAnchorId>("anchor.feedback.previous")]
    );
}

#[test]
fn denied_analyzer_admission_suppresses_diagnostic_store_reads() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let current_calls = Arc::new(AtomicUsize::new(0));
    let history_calls = Arc::new(AtomicUsize::new(0));
    let source = GenerationDiagnosticsSourceFixture {
        current_calls: current_calls.clone(),
        history_calls: history_calls.clone(),
        current: DiagnosticProviderResult::new(
            provider.clone(),
            DiagnosticProviderState::SupportedComplete,
            Some(Vec::new()),
        )
        .unwrap(),
        history: DiagnosticProviderResult::new(
            provider.clone(),
            DiagnosticProviderState::SupportedComplete,
            Some(Vec::new()),
        )
        .unwrap(),
    };
    let adapter = GenerationBoundFeedbackDiagnosticsAdapter::new(
        source,
        vec![admitted_provider(
            &provider,
            AnalyzerAvailabilityV1::Available,
            false,
        )],
    )
    .unwrap();
    let request = FeedbackDiagnosticsRequest {
        input,
        providers: vec![provider],
    };

    let diagnostics =
        block_on(adapter.diagnostics(&common::context(&common::operation()), &request));

    assert_eq!(current_calls.load(Ordering::Relaxed), 0);
    assert_eq!(history_calls.load(Ordering::Relaxed), 0);
    assert_eq!(diagnostics[0].state, DiagnosticProviderState::Unsupported);
}

#[test]
fn stale_analyzer_admission_preserves_staleness_without_store_reads() {
    let input = saved_input();
    let mut provider = provider_identity(&input);
    provider.freshness.state = FreshnessState::Stale;
    let current_calls = Arc::new(AtomicUsize::new(0));
    let history_calls = Arc::new(AtomicUsize::new(0));
    let source = GenerationDiagnosticsSourceFixture {
        current_calls: current_calls.clone(),
        history_calls: history_calls.clone(),
        current: DiagnosticProviderResult::new(
            provider.clone(),
            DiagnosticProviderState::Failed,
            None,
        )
        .unwrap(),
        history: DiagnosticProviderResult::new(
            provider.clone(),
            DiagnosticProviderState::Failed,
            None,
        )
        .unwrap(),
    };
    let adapter = GenerationBoundFeedbackDiagnosticsAdapter::new(
        source,
        vec![admitted_provider(
            &provider,
            AnalyzerAvailabilityV1::Stale,
            true,
        )],
    )
    .unwrap();
    let request = FeedbackDiagnosticsRequest {
        input,
        providers: vec![provider],
    };

    let diagnostics =
        block_on(adapter.diagnostics(&common::context(&common::operation()), &request));

    assert_eq!(current_calls.load(Ordering::Relaxed), 0);
    assert_eq!(history_calls.load(Ordering::Relaxed), 0);
    assert_eq!(diagnostics[0].state, DiagnosticProviderState::Stale);
}

#[test]
fn partial_provider_coverage_is_not_promoted_by_analyzer_admission() {
    let input = saved_input();
    let mut provider = provider_identity(&input);
    provider.coverage.completeness = tracedecay_application::CoverageCompleteness::Partial;
    provider.coverage.returned = 0;
    let current_calls = Arc::new(AtomicUsize::new(0));
    let history_calls = Arc::new(AtomicUsize::new(0));
    let source = GenerationDiagnosticsSourceFixture {
        current_calls: current_calls.clone(),
        history_calls: history_calls.clone(),
        current: DiagnosticProviderResult::new(
            provider.clone(),
            DiagnosticProviderState::Partial,
            Some(Vec::new()),
        )
        .unwrap(),
        history: DiagnosticProviderResult::new(
            provider.clone(),
            DiagnosticProviderState::Partial,
            Some(Vec::new()),
        )
        .unwrap(),
    };
    let adapter = GenerationBoundFeedbackDiagnosticsAdapter::new(
        source,
        vec![admitted_provider(
            &provider,
            AnalyzerAvailabilityV1::Available,
            true,
        )],
    )
    .unwrap();
    let request = FeedbackDiagnosticsRequest {
        input,
        providers: vec![provider],
    };

    let diagnostics =
        block_on(adapter.diagnostics(&common::context(&common::operation()), &request));

    assert_eq!(current_calls.load(Ordering::Relaxed), 0);
    assert_eq!(history_calls.load(Ordering::Relaxed), 0);
    assert_eq!(diagnostics[0].state, DiagnosticProviderState::Partial);
}

#[test]
fn diagnostics_adapter_short_circuits_cancellation_and_deadline_before_source_reads() {
    let operation = common::operation();
    let cancelled = common::context(&operation).with_cancellation(
        CancellationContext::cancelled("cancel.feedback.adapter", UtcMicros(1)).unwrap(),
    );
    let timed_out = common::context(&operation).with_deadline(Deadline::new(UtcMicros(2)).unwrap());

    for (context, expected_diagnostic_state) in [
        (cancelled, DiagnosticProviderState::Cancelled),
        (timed_out, DiagnosticProviderState::TimedOut),
    ] {
        let input = saved_input();
        let runtime = runtime_state(&input);
        let provider = provider_identity(&input);
        let current_calls = Arc::new(AtomicUsize::new(0));
        let history_calls = Arc::new(AtomicUsize::new(0));
        let source = GenerationDiagnosticsSourceFixture {
            current_calls: current_calls.clone(),
            history_calls: history_calls.clone(),
            current: DiagnosticProviderResult::new(
                provider.clone(),
                DiagnosticProviderState::SupportedComplete,
                Some(Vec::new()),
            )
            .unwrap(),
            history: DiagnosticProviderResult::new(
                provider.clone(),
                DiagnosticProviderState::SupportedComplete,
                Some(Vec::new()),
            )
            .unwrap(),
        };
        let diagnostics = GenerationBoundFeedbackDiagnosticsAdapter::new(
            source,
            vec![admitted_provider(
                &provider,
                AnalyzerAvailabilityV1::Available,
                true,
            )],
        )
        .unwrap();
        let diagnostics_request = FeedbackDiagnosticsRequest {
            input: input.clone(),
            providers: vec![provider],
        };

        let result = block_on(diagnostics.diagnostics(&context, &diagnostics_request));
        let history =
            block_on(diagnostics.diagnostic_history(&context, &diagnostics_request, &runtime));
        assert_eq!(result[0].state, expected_diagnostic_state);
        assert!(result[0].payload.is_none());
        assert!(history.is_empty());
        assert_eq!(current_calls.load(Ordering::Relaxed), 0);
        assert_eq!(history_calls.load(Ordering::Relaxed), 0);
    }
}

fn matching_baseline(
    input: &FeedbackEvaluationInputV1,
    provider: &DiagnosticProviderIdentity,
    diagnostic_anchors: Vec<RetrievalAnchorId>,
) -> FeedbackDiagnosticBaselineV1 {
    let FeedbackContentIdentityV1::SavedContent {
        generation_digest,
        file_digest,
    } = &input.request.content
    else {
        panic!("overlay cycles must not request diagnostics history")
    };
    FeedbackDiagnosticBaselineV1 {
        identity: FeedbackDiagnosticBaselineIdentityV1 {
            current_generation_id: input.target.generation_id.clone().unwrap(),
            current_generation_digest: generation_digest.clone(),
            current_head_commit_id: input.request.scope.head_commit_id.clone(),
            current_content_digest: file_digest.clone(),
            provider_identity_digest: provider.compute_digest().unwrap(),
            horizon: baseline_horizon(),
        },
        diagnostic_anchors,
        state: FeedbackBaselineStateV1::Complete,
    }
}

fn diagnostic(input: &FeedbackEvaluationInputV1, anchor: &str) -> GenerationDiagnosticV1 {
    let mut diagnostic = GenerationDiagnosticV1 {
        diagnostic_anchor: common::id::<RetrievalAnchorId>(anchor),
        generation_id: input.target.generation_id.clone().unwrap(),
        repository: input.request.scope.repository_id.clone(),
        worktree: Some(input.request.scope.worktree_id.clone()),
        reference: Some(common::id::<RefId>(&input.request.scope.branch_ref)),
        source_revision: Some(input.request.scope.head_commit_id.clone()),
        file_occurrence_id: input.target.file.clone(),
        content_digest: common::id::<ContentDigest>(common::SHA256_A),
        span: input.target.span.unwrap(),
        symbol_occurrence_id: input.target.symbol.clone(),
        code: "E0308".to_owned(),
        severity: DiagnosticSeverityV1::Error,
        message: "mismatched types".to_owned(),
        message_digest: common::digest(common::SHA256_A),
        provenance: DiagnosticProvenanceV1 {
            producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
            producer: common::id::<ProviderId>("provider.feedback.fixture"),
            analyzer_revision: common::id::<ComponentVersion>("analyzer.feedback.v1"),
            configuration_revision: common::id::<ComponentVersion>("configuration.feedback.v1"),
            sanitization_receipt: None,
        },
        evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
        collected_at: UtcMicros(2),
        state: DiagnosticRecordStateV1::Current,
    };
    diagnostic.message_digest = diagnostic.compute_message_digest().unwrap();
    diagnostic
}

fn complete_result(
    identity: DiagnosticProviderIdentity,
    diagnostics: Vec<GenerationDiagnosticV1>,
) -> DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>> {
    DiagnosticProviderResult::new(
        identity,
        DiagnosticProviderState::SupportedComplete,
        Some(
            diagnostics
                .into_iter()
                .map(|diagnostic| FeedbackDiagnosticV1::Saved(Box::new(diagnostic)))
                .collect(),
        ),
    )
    .unwrap()
}

fn complete_overlay_result(
    identity: DiagnosticProviderIdentity,
    diagnostics: Vec<FeedbackSessionDiagnosticV1>,
) -> DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>> {
    DiagnosticProviderResult::new(
        identity,
        DiagnosticProviderState::SupportedComplete,
        Some(
            diagnostics
                .into_iter()
                .map(FeedbackDiagnosticV1::SessionOverlay)
                .collect(),
        ),
    )
    .unwrap()
}

fn complete_impact(input: &FeedbackEvaluationInputV1) -> FeedbackImpactPortOutcome {
    FeedbackImpactPortOutcome::Complete(FeedbackImpactV1 {
        target: input.target.clone(),
        affected_files: vec![common::id::<FileOccurrenceId>("file.affected.fixture")],
        affected_callers: vec![common::id::<SymbolOccurrenceId>(
            "symbol.caller.feedback.fixture",
        )],
        affected_tests: vec![common::id::<SymbolOccurrenceId>(
            "symbol.test.feedback.fixture",
        )],
        evidence_anchors: (input.request.durability() == FeedbackDurabilityV1::Durable)
            .then(|| common::id::<RetrievalAnchorId>("anchor.impact.feedback.fixture"))
            .into_iter()
            .collect(),
        state: FeedbackImpactStateV1::Complete,
        affected_tests_state: FeedbackImpactStateV1::Complete,
    })
}

fn runtime_state(input: &FeedbackEvaluationInputV1) -> FeedbackRuntimeStateV1 {
    FeedbackRuntimeStateV1::new(
        FeedbackAuthoritativeRuntimeStateV1 {
            snapshot: FeedbackCycleRuntimeSnapshotV1::from_request(&input.request),
            baseline_horizon: matches!(
                &input.request.content,
                FeedbackContentIdentityV1::SavedContent { .. }
            )
            .then(baseline_horizon),
            runtime_watermark: common::digest(common::SHA256_B),
        },
        input.target.generation_id.clone(),
    )
    .unwrap()
}

fn runtime_port(
    input: &FeedbackEvaluationInputV1,
) -> impl Fn(&RequestContext, &FeedbackEvaluationInputV1) -> Option<FeedbackRuntimeStateV1> + use<>
{
    let state = runtime_state(input);
    move |_context, _input| Some(state.clone())
}

fn sequenced_runtime(
    states: Vec<Option<FeedbackRuntimeStateV1>>,
    calls: Rc<Cell<usize>>,
) -> impl Fn(&RequestContext, &FeedbackEvaluationInputV1) -> Option<FeedbackRuntimeStateV1> {
    let states = Rc::new(RefCell::new(states.into_iter().collect::<VecDeque<_>>()));
    move |_context, _input| {
        calls.set(calls.get() + 1);
        states
            .borrow_mut()
            .pop_front()
            .expect("runtime-state sequence is not exhausted")
    }
}

fn execution_request(
    input: FeedbackEvaluationInputV1,
    provider: DiagnosticProviderIdentity,
) -> FeedbackCycleExecutionRequest {
    FeedbackCycleExecutionRequest {
        input,
        providers: vec![provider],
        maximum_returned_findings: 10,
        usage: FeedbackBudgetUsage {
            completed_at: UtcMicros(3),
            tokens_consumed: 1,
            cost_microunits: 1,
        },
        control: FeedbackCycleControl::Continue,
    }
}

fn execute_concurrent_cycle(
    input: FeedbackEvaluationInputV1,
    provider: DiagnosticProviderIdentity,
    dedupe: SerializedRaceDedupeFixture,
) -> FeedbackCycleExecutionResult {
    let runtime = ConcurrentRuntimeFixture(runtime_state(&input));
    let diagnostics = ConcurrentDiagnosticsFixture {
        results: vec![complete_result(provider.clone(), Vec::new())],
    };
    let impact = ConcurrentImpactFixture(complete_impact(&input));
    let operation = common::operation();
    let context = common::context(&operation);
    let service = FeedbackCycleService::new(
        runtime,
        diagnostics,
        impact,
        dedupe,
        NoopObservationFixture,
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operation,
    );
    block_on(service.execute(&context, execution_request(input, provider))).unwrap()
}

fn execute_before_provider_work(
    context: &RequestContext,
    dedupe_state: FeedbackCycleDedupeState,
    configure: impl FnOnce(&mut FeedbackCycleExecutionRequest),
) -> FeedbackCycleExecutionResult {
    let input = saved_input();
    let provider = provider_identity(&input);
    let diagnostics_calls = Rc::new(Cell::new(0));
    let impact_calls = Rc::new(Cell::new(0));
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: diagnostics_calls.clone(),
            results: Vec::new(),
        },
        ImpactFixture {
            calls: impact_calls.clone(),
            outcome: FeedbackImpactPortOutcome::Unavailable,
        },
        DedupeFixture(dedupe_state),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );
    let mut request = execution_request(input, provider);
    configure(&mut request);
    let result = block_on(service.execute(context, request)).unwrap();
    assert_eq!(diagnostics_calls.get(), 0);
    assert_eq!(impact_calls.get(), 0);
    result
}

#[test]
fn cycle_runs_diagnostics_impact_and_tests_once_with_anchored_new_findings() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let diagnostics_calls = Rc::new(Cell::new(0));
    let impact_calls = Rc::new(Cell::new(0));
    let observations = ObservationFixture::default();
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: diagnostics_calls.clone(),
            results: vec![complete_result(
                provider.clone(),
                vec![diagnostic(&input, "anchor.diagnostic.feedback.fixture")],
            )],
        },
        ImpactFixture {
            calls: impact_calls.clone(),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        observations.clone(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );

    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert_eq!(diagnostics_calls.get(), 1);
    assert_eq!(impact_calls.get(), 1);
    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::Blocked
    );
    assert_eq!(
        result.cycle.provider_states,
        vec![ProviderEvaluationStateV1::SupportedCompletedComplete]
    );
    assert_eq!(result.cycle.findings.len(), 1);
    assert_eq!(
        result.cycle.findings[0].classification,
        FeedbackDiagnosticClassificationV1::New
    );
    assert_eq!(
        result.cycle.findings[0]
            .retrieval_anchor_id
            .as_ref()
            .unwrap()
            .as_str(),
        "anchor.diagnostic.feedback.fixture"
    );
    assert_eq!(
        result.cycle.impact.as_ref().unwrap().affected_tests[0].as_str(),
        "symbol.test.feedback.fixture"
    );
    assert_eq!(
        result
            .publication
            .as_ref()
            .map(|publication| &publication.result.result_id),
        Some(&result.cycle.result_id)
    );
    let observations = observations.0.borrow();
    assert_eq!(
        observations
            .iter()
            .filter(|event| event.kind == FeedbackObservationKindV1::Trigger)
            .count(),
        1
    );
    assert_eq!(
        observations
            .iter()
            .filter(|event| event.kind == FeedbackObservationKindV1::Terminal)
            .count(),
        1
    );
    assert_eq!(
        observations
            .iter()
            .filter(|event| event.kind == FeedbackObservationKindV1::Latency)
            .count(),
        1
    );
    for stage in [
        FeedbackEvaluationStageV1::Admission,
        FeedbackEvaluationStageV1::Diagnostics,
        FeedbackEvaluationStageV1::BaselineClassification,
        FeedbackEvaluationStageV1::Impact,
        FeedbackEvaluationStageV1::AffectedTests,
        FeedbackEvaluationStageV1::ResultAssembly,
    ] {
        assert_eq!(
            observations
                .iter()
                .filter(|event| {
                    event.kind == FeedbackObservationKindV1::EvaluationStage
                        && event.stage == Some(stage)
                })
                .count(),
            1,
            "{stage:?} must be observed exactly once"
        );
    }
}

#[test]
fn authoritative_history_identity_drives_pre_existing_and_stale_classification() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let anchor = "anchor.diagnostic.authoritative-history";
    let current = diagnostic(&input, anchor);
    let history_calls = Rc::new(Cell::new(0));
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        HistoryDiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            history_calls: history_calls.clone(),
            results: vec![complete_result(provider.clone(), vec![current.clone()])],
            baselines: vec![matching_baseline(
                &input,
                &provider,
                vec![common::id::<RetrievalAnchorId>(anchor)],
            )],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );
    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input.clone(), provider.clone()),
    ))
    .unwrap();
    assert_eq!(history_calls.get(), 1);
    assert_eq!(
        result.cycle.findings[0].classification,
        FeedbackDiagnosticClassificationV1::PreExisting
    );
    assert_eq!(
        result.cycle.baseline_states,
        vec![FeedbackBaselineStateV1::Complete]
    );

    let mut wrong_identity = matching_baseline(&input, &provider, Vec::new());
    wrong_identity.identity.current_head_commit_id = common::id::<CommitId>("commit.history.stale");
    let stale_service = FeedbackCycleService::new(
        runtime_port(&input),
        HistoryDiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            history_calls: Rc::new(Cell::new(0)),
            results: vec![complete_result(provider.clone(), vec![current])],
            baselines: vec![wrong_identity],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );
    let stale = block_on(stale_service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();
    assert_eq!(
        stale.cycle.termination,
        FeedbackCycleTerminationV1::StaleReplanRequired
    );
    assert_eq!(
        stale.cycle.baseline_states,
        vec![FeedbackBaselineStateV1::Stale]
    );
    assert!(stale.cycle.findings.is_empty());
}

#[test]
fn dedupe_key_changes_when_authoritative_evidence_changes() {
    let keys = Rc::new(RefCell::new(Vec::new()));
    for anchor in ["anchor.dedupe.first", "anchor.dedupe.second"] {
        let input = saved_input();
        let provider = provider_identity(&input);
        let service = FeedbackCycleService::new(
            runtime_port(&input),
            DiagnosticsFixture {
                calls: Rc::new(Cell::new(0)),
                results: vec![complete_result(
                    provider.clone(),
                    vec![diagnostic(&input, anchor)],
                )],
            },
            ImpactFixture {
                calls: Rc::new(Cell::new(0)),
                outcome: complete_impact(&input),
            },
            RecordingDedupeFixture {
                state: FeedbackCycleDedupeState::Unique,
                keys: keys.clone(),
            },
            ObservationFixture::default(),
            AuthorizationService::new(
                common::StaticAuthorizationPort::authorized(),
                SourceAuthorizationEvaluatorV1::default(),
            ),
            common::operation(),
        );
        block_on(service.execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        ))
        .unwrap();
    }

    let keys = keys.borrow();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
}

#[test]
fn unavailable_authoritative_baseline_cannot_produce_clean() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        HistoryDiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            history_calls: Rc::new(Cell::new(0)),
            results: vec![complete_result(provider.clone(), Vec::new())],
            baselines: Vec::new(),
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );
    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::IncompleteCoverage
    );
    assert_eq!(
        result.cycle.baseline_states,
        vec![FeedbackBaselineStateV1::Unavailable]
    );
    assert_eq!(
        result.cycle.impact_state,
        Some(FeedbackImpactStateV1::Complete)
    );
}

#[test]
fn authoritative_no_prior_baseline_is_explicit_and_never_invented() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let history_calls = Rc::new(Cell::new(0));
    let mut no_prior_runtime = runtime_state(&input);
    no_prior_runtime.authoritative.baseline_horizon = None;
    let service = FeedbackCycleService::new(
        move |_context: &RequestContext, _input: &FeedbackEvaluationInputV1| {
            Some(no_prior_runtime.clone())
        },
        HistoryDiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            history_calls: history_calls.clone(),
            results: vec![complete_result(provider.clone(), Vec::new())],
            baselines: Vec::new(),
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );

    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert_eq!(history_calls.get(), 0);
    assert_eq!(
        result.cycle.baseline_states,
        vec![FeedbackBaselineStateV1::NoPriorBaseline]
    );
    assert_eq!(result.cycle.termination, FeedbackCycleTerminationV1::Clean);
}

#[test]
fn complete_zero_diagnostics_and_impact_are_clean() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            results: vec![complete_result(provider.clone(), Vec::new())],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );
    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert_eq!(result.cycle.termination, FeedbackCycleTerminationV1::Clean);
    assert_eq!(result.cycle.total_findings, 0);
    assert_eq!(
        result.cycle.baseline_states,
        vec![FeedbackBaselineStateV1::Complete]
    );
    assert_eq!(
        result.cycle.impact_state,
        Some(FeedbackImpactStateV1::Complete)
    );
    assert_eq!(
        result.cycle.affected_tests_state,
        Some(FeedbackImpactStateV1::Complete)
    );
}

#[test]
fn duplicate_noop_is_decided_after_authoritative_evidence_is_read() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let diagnostics_calls = Rc::new(Cell::new(0));
    let impact_calls = Rc::new(Cell::new(0));
    let observations = ObservationFixture::default();
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: diagnostics_calls.clone(),
            results: vec![complete_result(provider.clone(), Vec::new())],
        },
        ImpactFixture {
            calls: impact_calls.clone(),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Duplicate),
        observations.clone(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );

    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert_eq!(diagnostics_calls.get(), 1);
    assert_eq!(impact_calls.get(), 1);
    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::DuplicateNoop
    );
    assert!(result.cycle.provider_states.is_empty());
    assert!(result.cycle.findings.is_empty());
    assert!(result.publication.is_none());
    assert!(
        observations
            .0
            .borrow()
            .iter()
            .any(|event| event.kind == FeedbackObservationKindV1::DedupeSuppressed)
    );
}

#[test]
fn serialized_completed_publication_converges_concurrent_record_races() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let dedupe = SerializedRaceDedupeFixture {
        barrier: Arc::new(Barrier::new(2)),
        completed: Arc::new(Mutex::new(BTreeSet::new())),
        record_calls: Arc::new(AtomicUsize::new(0)),
    };
    let completed = dedupe.completed.clone();
    let record_calls = dedupe.record_calls.clone();

    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn({
            let input = input.clone();
            let provider = provider.clone();
            let dedupe = dedupe.clone();
            move || execute_concurrent_cycle(input, provider, dedupe)
        });
        let second = scope.spawn(move || execute_concurrent_cycle(input, provider, dedupe));
        (
            first.join().expect("first feedback cycle completes"),
            second.join().expect("second feedback cycle completes"),
        )
    });

    assert_eq!(record_calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        completed
            .lock()
            .expect("serialized dedupe fixture lock is not poisoned")
            .len(),
        1
    );
    assert_eq!(first.dedupe_key, second.dedupe_key);
    assert_eq!(
        usize::from(first.publication.is_some()) + usize::from(second.publication.is_some()),
        1
    );
    assert!(
        matches!(first.cycle.termination, FeedbackCycleTerminationV1::Clean)
            && matches!(
                second.cycle.termination,
                FeedbackCycleTerminationV1::DuplicateNoop
            )
            || matches!(second.cycle.termination, FeedbackCycleTerminationV1::Clean)
                && matches!(
                    first.cycle.termination,
                    FeedbackCycleTerminationV1::DuplicateNoop
                )
    );
}

#[test]
fn duplicate_provider_diagnostics_collapse_to_one_finding() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let repeated = diagnostic(&input, "anchor.diagnostic.duplicate");
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            results: vec![complete_result(
                provider.clone(),
                vec![repeated.clone(), repeated],
            )],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );

    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert_eq!(result.cycle.total_findings, 1);
    assert_eq!(result.cycle.findings.len(), 1);
}

#[test]
fn mismatched_diagnostic_address_is_failed_not_current_truth() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let mut mismatched = diagnostic(&input, "anchor.diagnostic.mismatched");
    mismatched.content_digest = common::id::<ContentDigest>(common::SHA256_B);
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            results: vec![complete_result(provider.clone(), vec![mismatched])],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );

    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert_eq!(
        result.cycle.provider_states,
        vec![ProviderEvaluationStateV1::Failed]
    );
    assert!(result.cycle.findings.is_empty());
    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::IncompleteCoverage
    );
}

#[test]
fn bounded_preview_respects_its_byte_limit_for_unicode() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let mut diagnostic = diagnostic(&input, "anchor.diagnostic.unicode");
    diagnostic.message = "é".repeat(300);
    diagnostic.message_digest = diagnostic.compute_message_digest().unwrap();
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            results: vec![complete_result(provider.clone(), vec![diagnostic])],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );

    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert!(
        result.cycle.findings[0]
            .safe_bounded_preview
            .as_ref()
            .unwrap()
            .len()
            <= 512
    );
}

#[test]
fn overlay_cycle_returns_session_only_truth_without_observations() {
    let input = overlay_input();
    let provider = provider_identity(&input);
    let observations = ObservationFixture::default();
    let dedupe_keys = Rc::new(RefCell::new(Vec::new()));
    let overlay_diagnostic = FeedbackSessionDiagnosticV1 {
        span: input.target.span.unwrap(),
        symbol: input.target.symbol.clone(),
        code: "overlay.type-error".to_owned(),
        severity: DiagnosticSeverityV1::Error,
        safe_bounded_message: "unsaved overlay mismatch".to_owned(),
    };
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            results: vec![complete_overlay_result(
                provider.clone(),
                vec![overlay_diagnostic],
            )],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        RecordingDedupeFixture {
            state: FeedbackCycleDedupeState::Duplicate,
            keys: dedupe_keys.clone(),
        },
        observations.clone(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );

    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert_eq!(result.cycle.durability, FeedbackDurabilityV1::SessionOnly);
    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::Blocked
    );
    assert_eq!(result.cycle.findings.len(), 1);
    assert_eq!(
        result.cycle.findings[0].classification,
        FeedbackDiagnosticClassificationV1::Unknown
    );
    assert!(result.cycle.findings[0].retrieval_anchor_id.is_none());
    assert!(result.dedupe_key.is_none());
    assert!(result.authority.is_none());
    assert!(result.cycle.baseline_states.is_empty());
    assert!(
        result
            .cycle
            .impact
            .as_ref()
            .unwrap()
            .evidence_anchors
            .is_empty()
    );
    assert!(dedupe_keys.borrow().is_empty());
    assert!(observations.0.borrow().is_empty());
}

#[test]
fn overlay_provider_client_must_match_the_authenticated_owner_binding() {
    let input = overlay_input();
    let mut provider = provider_identity(&input);
    let ProviderSourceIdentity::SessionOverlay { client_id, .. } = &mut provider.source else {
        unreachable!()
    };
    *client_id = common::id::<HostInstanceId>("client.feedback.not-owner");
    let diagnostics_calls = Rc::new(Cell::new(0));
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: diagnostics_calls.clone(),
            results: Vec::new(),
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: FeedbackImpactPortOutcome::Unavailable,
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );

    assert!(
        block_on(service.execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        ))
        .is_err()
    );
    assert_eq!(diagnostics_calls.get(), 0);
}

#[test]
fn every_post_port_runtime_drift_suppresses_evidence_and_later_reads() {
    for drift_snapshot in [false, true] {
        for drift_on_runtime_call in 2..=5 {
            let input = saved_input();
            let provider = provider_identity(&input);
            let history_calls = Rc::new(Cell::new(0));
            let diagnostics_calls = Rc::new(Cell::new(0));
            let impact_calls = Rc::new(Cell::new(0));
            let dedupe_keys = Rc::new(RefCell::new(Vec::new()));
            let observations = ObservationFixture::default();
            let first = runtime_state(&input);
            let mut second = first.clone();
            if drift_snapshot {
                second.authoritative.snapshot.scope.head_commit_id =
                    common::id::<CommitId>("commit.feedback.runtime-drift");
            } else {
                second.authoritative.runtime_watermark = common::digest(common::SHA256_A);
            }
            let runtime_calls = Rc::new(Cell::new(0));
            let mut runtime_states = vec![Some(first.clone()); drift_on_runtime_call - 1];
            runtime_states.push(Some(second.clone()));
            runtime_states.push(Some(second));
            let service = FeedbackCycleService::new(
                sequenced_runtime(runtime_states, runtime_calls.clone()),
                HistoryDiagnosticsFixture {
                    calls: diagnostics_calls.clone(),
                    history_calls: history_calls.clone(),
                    results: vec![complete_result(provider.clone(), Vec::new())],
                    baselines: vec![matching_baseline(&input, &provider, Vec::new())],
                },
                ImpactFixture {
                    calls: impact_calls.clone(),
                    outcome: complete_impact(&input),
                },
                RecordingDedupeFixture {
                    state: FeedbackCycleDedupeState::Unique,
                    keys: dedupe_keys.clone(),
                },
                observations.clone(),
                AuthorizationService::new(
                    common::StaticAuthorizationPort::authorized(),
                    SourceAuthorizationEvaluatorV1::default(),
                ),
                common::operation(),
            );

            let result = block_on(service.execute(
                &common::context(&common::operation()),
                execution_request(input, provider),
            ))
            .unwrap();

            assert_eq!(runtime_calls.get(), drift_on_runtime_call + 1);
            assert_eq!(history_calls.get(), 1);
            assert_eq!(
                diagnostics_calls.get(),
                usize::from(drift_on_runtime_call >= 3)
            );
            assert_eq!(impact_calls.get(), usize::from(drift_on_runtime_call >= 4));
            assert_eq!(
                dedupe_keys.borrow().len(),
                usize::from(drift_on_runtime_call >= 5)
            );
            assert_eq!(
                result.cycle.termination,
                FeedbackCycleTerminationV1::StaleReplanRequired
            );
            assert!(result.cycle.findings.is_empty());
            assert!(result.cycle.impact.is_none());
            assert!(result.dedupe_key.is_none());
            let observations = observations.0.borrow();
            assert_eq!(
                observations
                    .iter()
                    .filter(|observation| {
                        observation.kind == FeedbackObservationKindV1::Trigger
                    })
                    .count(),
                1
            );
            assert_eq!(
                observations
                    .iter()
                    .filter(|observation| {
                        observation.kind == FeedbackObservationKindV1::Terminal
                            && observation.termination
                                == Some(FeedbackCycleTerminationV1::StaleReplanRequired)
                    })
                    .count(),
                1
            );
            assert_eq!(
                observations
                    .iter()
                    .filter(|observation| {
                        observation.kind == FeedbackObservationKindV1::Latency
                    })
                    .count(),
                1
            );
            assert!(observations.iter().all(|observation| {
                observation.kind != FeedbackObservationKindV1::DedupeSuppressed
            }));
        }
    }
}

#[test]
fn partial_and_unavailable_impact_truth_never_becomes_clean() {
    for (outcome, expected_state, has_impact) in [
        (
            FeedbackImpactPortOutcome::Partial(FeedbackImpactV1 {
                target: saved_input().target,
                affected_files: Vec::new(),
                affected_callers: Vec::new(),
                affected_tests: Vec::new(),
                evidence_anchors: vec![common::id::<RetrievalAnchorId>(
                    "anchor.impact.partial.fixture",
                )],
                state: FeedbackImpactStateV1::Partial,
                affected_tests_state: FeedbackImpactStateV1::Partial,
            }),
            FeedbackImpactStateV1::Partial,
            true,
        ),
        (
            FeedbackImpactPortOutcome::Unavailable,
            FeedbackImpactStateV1::Unavailable,
            false,
        ),
    ] {
        let input = saved_input();
        let provider = provider_identity(&input);
        let service = FeedbackCycleService::new(
            runtime_port(&input),
            DiagnosticsFixture {
                calls: Rc::new(Cell::new(0)),
                results: vec![complete_result(provider.clone(), Vec::new())],
            },
            ImpactFixture {
                calls: Rc::new(Cell::new(0)),
                outcome,
            },
            DedupeFixture(FeedbackCycleDedupeState::Unique),
            ObservationFixture::default(),
            AuthorizationService::new(
                common::StaticAuthorizationPort::authorized(),
                SourceAuthorizationEvaluatorV1::default(),
            ),
            common::operation(),
        );

        let result = block_on(service.execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        ))
        .unwrap();

        assert_eq!(
            result.cycle.termination,
            FeedbackCycleTerminationV1::IncompleteCoverage
        );
        assert_eq!(result.cycle.impact_state, Some(expected_state));
        assert_eq!(result.cycle.affected_tests_state, Some(expected_state));
        assert_eq!(result.cycle.impact.is_some(), has_impact);
    }
}

#[test]
fn partial_affected_test_coverage_never_becomes_clean() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let FeedbackImpactPortOutcome::Complete(mut impact) = complete_impact(&input) else {
        unreachable!()
    };
    impact.affected_tests_state = FeedbackImpactStateV1::Partial;
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            results: vec![complete_result(provider.clone(), Vec::new())],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: FeedbackImpactPortOutcome::Complete(impact),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );
    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::IncompleteCoverage
    );
    assert_eq!(
        result.cycle.impact_state,
        Some(FeedbackImpactStateV1::Complete)
    );
    assert_eq!(
        result.cycle.affected_tests_state,
        Some(FeedbackImpactStateV1::Partial)
    );
}

#[test]
fn every_terminal_reason_is_exact_and_one_shot() {
    let operation = common::operation();
    let context = common::context(&operation);

    let user_stop =
        execute_before_provider_work(&context, FeedbackCycleDedupeState::Unique, |request| {
            request.control = FeedbackCycleControl::UserStop;
        });
    assert_eq!(
        user_stop.cycle.termination,
        FeedbackCycleTerminationV1::UserStop
    );

    let budget =
        execute_before_provider_work(&context, FeedbackCycleDedupeState::Unique, |request| {
            request.usage.tokens_consumed = request.input.request.budget.maximum_tokens + 1;
        });
    assert_eq!(
        budget.cycle.termination,
        FeedbackCycleTerminationV1::BudgetExceeded
    );

    let stale =
        execute_before_provider_work(&context, FeedbackCycleDedupeState::Unique, |request| {
            request.input.request.scope.head_commit_id =
                common::id::<CommitId>("commit.feedback.changed");
        });
    assert_eq!(
        stale.cycle.termination,
        FeedbackCycleTerminationV1::StaleReplanRequired
    );

    let unavailable_input = saved_input();
    let unavailable_provider = provider_identity(&unavailable_input);
    let unavailable_service = FeedbackCycleService::new(
        runtime_port(&unavailable_input),
        DiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            results: vec![complete_result(unavailable_provider.clone(), Vec::new())],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&unavailable_input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unavailable),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );
    let unavailable = block_on(unavailable_service.execute(
        &context,
        execution_request(unavailable_input, unavailable_provider),
    ))
    .unwrap();
    assert_eq!(
        unavailable.cycle.termination,
        FeedbackCycleTerminationV1::DaemonUnavailable
    );

    let cancelled_context = common::context(&operation).with_cancellation(
        CancellationContext::cancelled("cancel.feedback.fixture", UtcMicros(1)).unwrap(),
    );
    let cancelled =
        execute_before_provider_work(&cancelled_context, FeedbackCycleDedupeState::Unique, |_| {});
    assert_eq!(
        cancelled.cycle.termination,
        FeedbackCycleTerminationV1::Cancelled
    );

    let elapsed_context =
        common::context(&operation).with_deadline(Deadline::new(UtcMicros(1)).unwrap());
    let timed_out =
        execute_before_provider_work(&elapsed_context, FeedbackCycleDedupeState::Unique, |_| {});
    assert_eq!(
        timed_out.cycle.termination,
        FeedbackCycleTerminationV1::BudgetExceeded
    );
}

#[test]
fn post_read_authorization_is_rechecked_before_findings_publish() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            results: vec![complete_result(
                provider.clone(),
                vec![diagnostic(&input, "anchor.diagnostic.recheck")],
            )],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::SequencedAuthorizationPort::snapshots([
                common::source_snapshot(common::authorized_source_input()),
                common::source_snapshot(common::source_authorization_input(
                    "temporarily_unavailable_is_not_deletion",
                )),
            ]),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );

    let result = block_on(service.execute(
        &common::context(&common::operation()),
        execution_request(input, provider),
    ))
    .unwrap();

    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::DaemonUnavailable
    );
    assert!(result.cycle.findings.is_empty());
    assert!(result.cycle.impact.is_none());
    assert!(result.dedupe_key.is_none());
    assert!(result.authority.is_none());
}

#[test]
fn authorization_revocation_overrides_early_and_duplicate_terminal_outcomes() {
    let operation = common::operation();

    let early_input = saved_input();
    let early_provider = provider_identity(&early_input);
    let early_runtime_calls = Rc::new(Cell::new(0));
    let early_diagnostics_calls = Rc::new(Cell::new(0));
    let early_observations = ObservationFixture::default();
    let early_service = FeedbackCycleService::new(
        sequenced_runtime(
            vec![Some(runtime_state(&early_input))],
            early_runtime_calls.clone(),
        ),
        DiagnosticsFixture {
            calls: early_diagnostics_calls.clone(),
            results: Vec::new(),
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: FeedbackImpactPortOutcome::Unavailable,
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        early_observations.clone(),
        AuthorizationService::new(
            common::SequencedAuthorizationPort::snapshots([
                common::source_snapshot(common::authorized_source_input()),
                common::source_snapshot(common::source_authorization_input(
                    "temporarily_unavailable_is_not_deletion",
                )),
            ]),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operation.clone(),
    );
    let mut early_request = execution_request(early_input, early_provider);
    early_request.usage.tokens_consumed = early_request.input.request.budget.maximum_tokens + 1;
    let early =
        block_on(early_service.execute(&common::context(&operation), early_request)).unwrap();
    assert_eq!(
        early.cycle.termination,
        FeedbackCycleTerminationV1::DaemonUnavailable
    );
    assert!(early.authority.is_none());
    assert_eq!(early_runtime_calls.get(), 1);
    assert_eq!(early_diagnostics_calls.get(), 0);
    assert!(early_observations.0.borrow().is_empty());

    let duplicate_input = saved_input();
    let duplicate_provider = provider_identity(&duplicate_input);
    let duplicate_runtime_calls = Rc::new(Cell::new(0));
    let duplicate_observations = ObservationFixture::default();
    let duplicate_service = FeedbackCycleService::new(
        sequenced_runtime(
            vec![
                Some(runtime_state(&duplicate_input)),
                Some(runtime_state(&duplicate_input)),
                Some(runtime_state(&duplicate_input)),
                Some(runtime_state(&duplicate_input)),
                Some(runtime_state(&duplicate_input)),
            ],
            duplicate_runtime_calls.clone(),
        ),
        DiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            results: vec![complete_result(duplicate_provider.clone(), Vec::new())],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&duplicate_input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Duplicate),
        duplicate_observations.clone(),
        AuthorizationService::new(
            common::SequencedAuthorizationPort::snapshots([
                common::source_snapshot(common::authorized_source_input()),
                common::source_snapshot(common::source_authorization_input(
                    "temporarily_unavailable_is_not_deletion",
                )),
            ]),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operation,
    );
    let duplicate = block_on(duplicate_service.execute(
        &common::context(&common::operation()),
        execution_request(duplicate_input, duplicate_provider),
    ))
    .unwrap();
    assert_eq!(
        duplicate.cycle.termination,
        FeedbackCycleTerminationV1::DaemonUnavailable
    );
    assert!(duplicate.dedupe_key.is_none());
    assert!(duplicate.authority.is_none());
    assert_eq!(duplicate_runtime_calls.get(), 5);
    assert!(duplicate_observations.0.borrow().is_empty());
}

#[test]
fn cancellation_suppresses_findings_from_other_completed_providers() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let mut cancelled_provider = provider.clone();
    cancelled_provider.producer.provider = common::id::<ProviderId>("provider.feedback.cancelled");
    let service = FeedbackCycleService::new(
        runtime_port(&input),
        DiagnosticsFixture {
            calls: Rc::new(Cell::new(0)),
            results: vec![
                complete_result(
                    provider.clone(),
                    vec![diagnostic(&input, "anchor.diagnostic.late")],
                ),
                DiagnosticProviderResult::new(
                    cancelled_provider.clone(),
                    DiagnosticProviderState::Cancelled,
                    None,
                )
                .unwrap(),
            ],
        },
        ImpactFixture {
            calls: Rc::new(Cell::new(0)),
            outcome: complete_impact(&input),
        },
        DedupeFixture(FeedbackCycleDedupeState::Unique),
        ObservationFixture::default(),
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        common::operation(),
    );
    let mut request = execution_request(input, provider);
    request.providers.push(cancelled_provider);

    let result =
        block_on(service.execute(&common::context(&common::operation()), request)).unwrap();

    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::Cancelled
    );
    assert!(result.cycle.findings.is_empty());
    assert_eq!(result.cycle.total_findings, 0);
    assert!(result.cycle.impact.is_none());
}

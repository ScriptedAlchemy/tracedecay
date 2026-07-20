mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tracedecay_application::feedback::{
    FeedbackBudgetUsage, FeedbackCycleControl, FeedbackCycleDedupePort, FeedbackCycleDedupeState,
    FeedbackCycleExecutionRequest, FeedbackCycleExecutionResult, FeedbackCycleService,
    FeedbackDiagnosticsPort, FeedbackDiagnosticsRequest, FeedbackImpactPort,
    FeedbackImpactPortOutcome, FeedbackImpactRequest, FeedbackObservationPort,
};
use tracedecay_application::{
    AuthorizationService, CancellationContext, Deadline, DiagnosticProviderDescriptor,
    DiagnosticProviderIdentity, DiagnosticProviderIdentityParts, DiagnosticProviderResult,
    DiagnosticProviderState, ProviderCoverage, ProviderDocumentIdentity, ProviderFreshness,
    ProviderOrigin, ProviderProvenance, ProviderSourceIdentity, RequestContext, RevisionDigest,
};
use tracedecay_domain::feedback::{
    FeedbackActorContextV1, FeedbackBaselineStateV1, FeedbackBudgetV1, FeedbackContentIdentityV1,
    FeedbackCycleId, FeedbackCycleObservationV1, FeedbackCycleRequestV1,
    FeedbackCycleRuntimeSnapshotV1, FeedbackCycleTerminationV1,
    FeedbackDiagnosticBaselineIdentityV1, FeedbackDiagnosticBaselineV1,
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
use tracedecay_policy::authorization::SourceAuthorizationEvaluatorV1;
use tracedecay_tool_catalog::CapabilityId;

const GENERATION: &str = "generation.v1.fixture.00000001";
const FILE: &str = "file.feedback.fixture";
const SYMBOL: &str = "symbol.feedback.fixture";

#[derive(Clone)]
struct DiagnosticsFixture {
    calls: Rc<Cell<usize>>,
    results: Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>,
}

impl FeedbackDiagnosticsPort for DiagnosticsFixture {
    fn diagnostics(
        &self,
        _request: &FeedbackDiagnosticsRequest,
    ) -> Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>> {
        self.calls.set(self.calls.get() + 1);
        self.results.clone()
    }

    fn diagnostic_history(
        &self,
        request: &FeedbackDiagnosticsRequest,
    ) -> Vec<FeedbackDiagnosticBaselineV1> {
        request
            .providers
            .iter()
            .map(|provider| matching_baseline(&request.input, provider, Vec::new()))
            .collect()
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
    fn diagnostics(
        &self,
        _request: &FeedbackDiagnosticsRequest,
    ) -> Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>> {
        self.calls.set(self.calls.get() + 1);
        self.results.clone()
    }

    fn diagnostic_history(
        &self,
        _request: &FeedbackDiagnosticsRequest,
    ) -> Vec<FeedbackDiagnosticBaselineV1> {
        self.history_calls.set(self.history_calls.get() + 1);
        self.baselines.clone()
    }
}

#[derive(Clone)]
struct ImpactFixture {
    calls: Rc<Cell<usize>>,
    outcome: FeedbackImpactPortOutcome,
}

impl FeedbackImpactPort for ImpactFixture {
    fn impact(&self, _request: &FeedbackImpactRequest) -> FeedbackImpactPortOutcome {
        self.calls.set(self.calls.get() + 1);
        self.outcome.clone()
    }
}

struct DedupeFixture(FeedbackCycleDedupeState);

impl FeedbackCycleDedupePort for DedupeFixture {
    fn check(
        &self,
        _key: &tracedecay_domain::feedback::FeedbackDedupeKeyV1,
    ) -> FeedbackCycleDedupeState {
        self.0
    }
}

#[derive(Clone)]
struct RecordingDedupeFixture {
    state: FeedbackCycleDedupeState,
    keys: Rc<RefCell<Vec<tracedecay_domain::feedback::FeedbackDedupeKeyV1>>>,
}

impl FeedbackCycleDedupePort for RecordingDedupeFixture {
    fn check(
        &self,
        key: &tracedecay_domain::feedback::FeedbackDedupeKeyV1,
    ) -> FeedbackCycleDedupeState {
        self.keys.borrow_mut().push(key.clone());
        self.state
    }
}

#[derive(Clone, Default)]
struct ObservationFixture(Rc<RefCell<Vec<FeedbackCycleObservationV1>>>);

impl FeedbackObservationPort for ObservationFixture {
    fn observe(&self, observation: FeedbackCycleObservationV1) {
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
    let request = FeedbackCycleRequestV1::new(
        common::id::<FeedbackCycleId>("cycle.feedback.overlay"),
        scope(),
        FeedbackContentIdentityV1::EphemeralOverlay {
            session_id: session_id.clone(),
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
            document_version,
            overlay_digest,
            ..
        } => ProviderSourceIdentity::SessionOverlay {
            session_id: session_id.clone(),
            client_id: common::id::<HostInstanceId>("client.feedback.fixture"),
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
            generation_id: input.target.generation_id.clone().unwrap(),
            generation_digest: generation_digest.clone(),
            head_commit_id: input.request.scope.head_commit_id.clone(),
            content_digest: file_digest.clone(),
            provider_identity_digest: provider.compute_digest().unwrap(),
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

fn execution_request(
    input: FeedbackEvaluationInputV1,
    provider: DiagnosticProviderIdentity,
) -> FeedbackCycleExecutionRequest {
    FeedbackCycleExecutionRequest {
        runtime: FeedbackCycleRuntimeSnapshotV1::from_request(&input.request),
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
    let result = service.execute(context, request).unwrap();
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

    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
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
    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input.clone(), provider.clone()),
        )
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
    wrong_identity.identity.head_commit_id = common::id::<CommitId>("commit.history.stale");
    let stale_service = FeedbackCycleService::new(
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
    let stale = stale_service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
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
        service
            .execute(
                &common::context(&common::operation()),
                execution_request(input, provider),
            )
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
    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
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
fn complete_zero_diagnostics_and_impact_are_clean() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let service = FeedbackCycleService::new(
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
    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
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

    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
        .unwrap();

    assert_eq!(diagnostics_calls.get(), 1);
    assert_eq!(impact_calls.get(), 1);
    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::DuplicateNoop
    );
    assert!(result.cycle.provider_states.is_empty());
    assert!(result.cycle.findings.is_empty());
    assert!(
        observations
            .0
            .borrow()
            .iter()
            .any(|event| event.kind == FeedbackObservationKindV1::DedupeSuppressed)
    );
}

#[test]
fn duplicate_provider_diagnostics_collapse_to_one_finding() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let repeated = diagnostic(&input, "anchor.diagnostic.duplicate");
    let service = FeedbackCycleService::new(
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

    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
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

    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
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

    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
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

    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
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

        let result = service
            .execute(
                &common::context(&common::operation()),
                execution_request(input, provider),
            )
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
    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
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
            request.runtime.scope.head_commit_id =
                common::id::<CommitId>("commit.feedback.changed");
        });
    assert_eq!(
        stale.cycle.termination,
        FeedbackCycleTerminationV1::StaleReplanRequired
    );

    let unavailable_input = saved_input();
    let unavailable_provider = provider_identity(&unavailable_input);
    let unavailable_service = FeedbackCycleService::new(
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
    let unavailable = unavailable_service
        .execute(
            &context,
            execution_request(unavailable_input, unavailable_provider),
        )
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

    let result = service
        .execute(
            &common::context(&common::operation()),
            execution_request(input, provider),
        )
        .unwrap();

    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::DaemonUnavailable
    );
    assert!(result.cycle.findings.is_empty());
    assert!(result.cycle.impact.is_none());
    assert!(result.authority.is_none());
}

#[test]
fn cancellation_suppresses_findings_from_other_completed_providers() {
    let input = saved_input();
    let provider = provider_identity(&input);
    let mut cancelled_provider = provider.clone();
    cancelled_provider.producer.provider = common::id::<ProviderId>("provider.feedback.cancelled");
    let service = FeedbackCycleService::new(
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

    let result = service
        .execute(&common::context(&common::operation()), request)
        .unwrap();

    assert_eq!(
        result.cycle.termination,
        FeedbackCycleTerminationV1::Cancelled
    );
    assert!(result.cycle.findings.is_empty());
    assert_eq!(result.cycle.total_findings, 0);
    assert!(result.cycle.impact.is_none());
}

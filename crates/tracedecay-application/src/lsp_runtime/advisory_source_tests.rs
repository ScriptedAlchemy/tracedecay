use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::diagnostics_store::DiagnosticsStore;
use tracedecay_contracts::diagnostics::{
    DiagnosticProviderDescriptor, DiagnosticProviderIdentity, DiagnosticProviderIdentityParts,
    DiagnosticProviderResult, DiagnosticProviderState, ProviderCoverage, ProviderDocumentIdentity,
    ProviderFreshness, ProviderOrigin, ProviderProvenance, ProviderSourceIdentity, RevisionDigest,
};
use tracedecay_contracts::feedback::{
    FeedbackBudgetUsage, FeedbackCycleAdvisoryV1, FeedbackCycleDedupePort,
    FeedbackCycleDedupeState, FeedbackCycleExecutionRequest, FeedbackCycleService,
    FeedbackDiagnosticsPort, FeedbackDiagnosticsRequest, FeedbackImpactPort,
    FeedbackImpactPortOutcome, FeedbackImpactRequest, FeedbackObservationPort, FeedbackPortFuture,
    FeedbackPublicationRecordState, FeedbackPublicationV1, FeedbackRuntimeStatePort,
    FeedbackRuntimeStateV1, feedback_surface_operation,
};
use tracedecay_contracts::{
    ApplicationOperation, ApplicationOutcome, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, PolicyDecisionRef, RequestContext,
    RequestId, ResolvedScope, now_micros,
};
use tracedecay_domain::configuration::{
    AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceBindingId, SourceKindV1,
};
use tracedecay_domain::feedback::{
    FeedbackActorContextV1, FeedbackAdvisoryProviderStateV1, FeedbackAuthoritativeRuntimeStateV1,
    FeedbackBaselineHorizonV1, FeedbackBaselineStateV1, FeedbackBudgetV1,
    FeedbackContentIdentityV1, FeedbackCycleId, FeedbackCycleRequestV1,
    FeedbackCycleRuntimeSnapshotV1, FeedbackDiagnosticBaselineIdentityV1,
    FeedbackDiagnosticBaselineV1, FeedbackDiagnosticClassificationV1, FeedbackDiagnosticProducerV1,
    FeedbackDiagnosticProjectionV1, FeedbackDiagnosticV1, FeedbackFindingId,
    FeedbackFindingLifecycleV1, FeedbackFindingV1, FeedbackImpactStateV1, FeedbackImpactV1,
    FeedbackScopeV1, FeedbackTargetV1, FeedbackTriggerV1, ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, CommitId, ComponentVersion, ContentDigest, DiagnosticSeverityV1,
    FileOccurrenceId, LanguageDescriptorRevision, LanguageId, LocatorDigest, ManifestDigest,
    ProjectId, ProviderId, RefId, RepositoryId, RetrievalAnchorId, SourceSpan, UtcMicros,
    WorktreeId,
};
use tracedecay_lsp::{
    AdmittedRoot, CanonicalContextProjectionAuthority, ContextCoverage, ContextExpansionOutcome,
    ContextExpansionRequest, ContextProjectionKind, ContextProjectionOutcome,
    ContextProjectionRequest, DiagnosticTrigger, FeedbackCycleRequest, FeedbackCycleRuntimePort,
    LspRequestId, LspRuntimeFailure, LspRuntimeFuture,
};
use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay_tool_catalog::CapabilityId;

use super::{
    ConcreteFeedbackLspSource, LspFeedbackDiagnosticProjectionPort, LspFeedbackProjectionScope,
    LspFeedbackProjectionScopePort, LspTestRunProjectionPort,
};
use crate::diagnostics_publication::{
    CleanGenerationDiagnosticScopeV1, CleanGenerationDiagnosticSnapshotBuilderV1,
    DiagnosticContributionV1, DiagnosticPillarV1,
};
use crate::feedback::concrete::open_feedback_runtime;
use crate::feedback::cycle_runtime::compose_canonical_result;
use crate::feedback::owner::{
    FeedbackCanonicalProjectionKindV1, FeedbackReadInvocationResultV1, FeedbackReadOperationV1,
};
use crate::source_authorization::ProjectSourceAccessSnapshot;

const SOURCE: &str = "fn reviewed() {}\n";

fn digest(fill: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).expect("digest")
}

fn scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.lsp-advisory-source").expect("project"),
        RepositoryId::new("repository.lsp-advisory-source").expect("repository"),
        WorktreeId::new("worktree.lsp-advisory-source").expect("worktree"),
        Some(RefId::new("refs/heads/lsp-advisory-source").expect("reference")),
    )
    .expect("scope")
}

fn operation() -> ApplicationOperation {
    feedback_surface_operation("feedback_diagnostics")
        .expect("feedback operation catalog")
        .expect("feedback diagnostics operation")
}

fn policy() -> PolicyDecisionRef {
    PolicyDecisionRef::new(
        "policy.lsp-advisory-source",
        1,
        digest('d'),
        ComponentVersion::new("policy.lsp-advisory-source.v1").expect("policy revision"),
    )
    .expect("policy")
}

fn context(
    scope: &ResolvedScope,
    operation: &ApplicationOperation,
    now: UtcMicros,
) -> RequestContext {
    let expires_at = UtcMicros(now.0.saturating_add(60_000_000));
    let requester = ActorId::new("actor.lsp-advisory-source").expect("requester");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.lsp-advisory-source").expect("grant"),
        1,
        digest('e'),
        ActorId::new("actor.lsp-advisory-source.issuer").expect("issuer"),
        now,
        expires_at,
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .expect("grant snapshot");
    RequestContext::new(
        requester,
        scope.clone(),
        grant,
        RequestId::new("request.lsp-advisory-source").expect("request"),
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.lsp-advisory-source").expect("cancellation"),
    )
    .expect("context")
}

fn source_access(
    scope: &ResolvedScope,
    operation: &ApplicationOperation,
    now: UtcMicros,
) -> ProjectSourceAccessSnapshot {
    let expand = feedback_surface_operation("feedback_expand")
        .expect("feedback operation catalog")
        .expect("feedback expand operation");
    ProjectSourceAccessSnapshot {
        scope: scope.clone(),
        requester: ActorId::new("actor.lsp-advisory-source").expect("requester"),
        binding: ScopeSourceBinding::new(
            SourceBindingId::new("binding.lsp-advisory-source").expect("binding"),
            SourceKindV1::Cursor,
            LocatorDigest::new(digest('f').as_str().to_owned()).expect("locator"),
            AuthorityRef::Project(scope.project_id.clone()),
        )
        .expect("binding"),
        configuration_revision: ConfigurationRevisionId::new("configuration.lsp-advisory-source")
            .expect("configuration revision"),
        configuration_digest: digest('a'),
        configuration_provenance_digest: digest('b'),
        effective_capabilities: BTreeSet::from([
            operation.capability_id().clone(),
            expand.capability_id().clone(),
        ]),
        grant_expires_at: UtcMicros(now.0.saturating_add(60_000_000)),
    }
}

fn projection_scope() -> LspFeedbackProjectionScope {
    LspFeedbackProjectionScope {
        head_commit_id: CommitId::new("0123456789abcdef0123456789abcdef01234567").expect("head"),
        code_generation_id: CodeGenerationId::new("generation.v1.aaaaaaaa.00000001")
            .expect("generation"),
        snapshot_digest: digest('a'),
        invalidation_digest: digest('b'),
        snapshot_content_digest: ContentDigest::new(digest('c').as_str().to_owned())
            .expect("snapshot content"),
        document_file_occurrence_id: Some(
            FileOccurrenceId::new("src/lib.rs").expect("document file"),
        ),
        document_content_digest: Some(
            ContentDigest::new(digest('c').as_str().to_owned()).expect("document content"),
        ),
        document_relative_path: Some("src/lib.rs".to_owned()),
        generation: 1,
    }
}

fn cycle_request(
    configuration_digest: ManifestDigest,
    observed_at: UtcMicros,
) -> FeedbackCycleExecutionRequest {
    let projection = projection_scope();
    let request = FeedbackCycleRequestV1::new(
        FeedbackCycleId::new(format!(
            "cycle.lsp-advisory-source.{}",
            configuration_digest
                .as_str()
                .rsplit(':')
                .next()
                .expect("digest body")
        ))
        .expect("cycle"),
        FeedbackScopeV1 {
            project_id: ProjectId::new("project.lsp-advisory-source").expect("project"),
            repository_id: RepositoryId::new("repository.lsp-advisory-source").expect("repository"),
            worktree_id: WorktreeId::new("worktree.lsp-advisory-source").expect("worktree"),
            branch_ref: "refs/heads/lsp-advisory-source".to_owned(),
            head_commit_id: projection.head_commit_id.clone(),
        },
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: projection.snapshot_digest.clone(),
            file_digest: ManifestDigest::new(
                projection
                    .document_content_digest
                    .as_ref()
                    .expect("document content")
                    .as_str()
                    .to_owned(),
            )
            .expect("file digest"),
        },
        FeedbackTriggerV1::DocumentSave,
        digest('d'),
        configuration_digest,
        FeedbackBudgetV1::bounded(1_000, 1_000, 10_000, 10_000),
    )
    .expect("cycle request");
    let input = tracedecay_domain::feedback::FeedbackEvaluationInputV1 {
        request,
        target: FeedbackTargetV1 {
            file: FileOccurrenceId::new("src/lib.rs").expect("file"),
            span: Some(SourceSpan {
                start_byte: 0,
                end_byte: 2,
            }),
            symbol: None,
            generation_id: Some(projection.code_generation_id),
        },
        actor: FeedbackActorContextV1::default(),
        observed_at,
    };
    FeedbackCycleExecutionRequest {
        providers: vec![diagnostic_provider(&input)],
        input,
        maximum_returned_findings: 16,
        usage: FeedbackBudgetUsage {
            completed_at: UtcMicros(observed_at.0.saturating_add(1)),
            tokens_consumed: 1,
            cost_microunits: 1,
        },
        control: tracedecay_contracts::feedback::FeedbackCycleControl::Continue,
    }
}

fn saved_content_digests(
    input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
) -> (&ManifestDigest, &ManifestDigest) {
    let FeedbackContentIdentityV1::SavedContent {
        generation_digest,
        file_digest,
    } = &input.request.content
    else {
        panic!("saved edit fixture must use durable content");
    };
    (generation_digest, file_digest)
}

fn diagnostic_provider(
    input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
) -> DiagnosticProviderIdentity {
    DiagnosticProviderIdentity::new(DiagnosticProviderIdentityParts {
        scope: scope(),
        source: ProviderSourceIdentity::CleanGeneration {
            generation: input
                .target
                .generation_id
                .clone()
                .expect("saved generation"),
        },
        document: ProviderDocumentIdentity {
            file: input.target.file.clone(),
            content_digest: ContentDigest::new(saved_content_digests(input).1.as_str().to_owned())
                .expect("provider content"),
            document_version: None,
        },
        producer: DiagnosticProviderDescriptor {
            provider: ProviderId::new("provider.lsp-advisory-source").expect("provider"),
            analyzer_revision: ComponentVersion::new("analyzer.lsp-advisory-source.v1")
                .expect("analyzer"),
            language: LanguageId::new("rust").expect("language"),
            language_descriptor_revision: LanguageDescriptorRevision::new(
                "language.rust.lsp-advisory-source.v1",
            )
            .expect("language descriptor"),
        },
        requested_capability: CapabilityId::new("capability.diagnostics.current")
            .expect("diagnostics capability"),
        freshness: ProviderFreshness::current(input.observed_at),
        coverage: ProviderCoverage::complete(1, 1),
        provenance: ProviderProvenance {
            origin: ProviderOrigin::ConfiguredAnalyzer,
            anchor: Some(
                RetrievalAnchorId::new("anchor.lsp-advisory-source.provider")
                    .expect("provider anchor"),
            ),
        },
        configuration: RevisionDigest {
            revision: ComponentVersion::new("configuration.lsp-advisory-source.v1")
                .expect("configuration"),
            digest: input.request.configuration_digest.clone(),
        },
        policy: policy(),
    })
    .expect("saved diagnostic provider")
}

fn baseline_horizon() -> FeedbackBaselineHorizonV1 {
    FeedbackBaselineHorizonV1 {
        comparison_generation_id: CodeGenerationId::new("generation.v1.previous.00000001")
            .expect("comparison generation"),
        comparison_generation_digest: digest('e'),
        comparison_head_commit_id: CommitId::new("fedcba9876543210fedcba9876543210fedcba98")
            .expect("comparison head"),
        comparison_content_digest: digest('e'),
        watermark: digest('f'),
    }
}

#[derive(Clone)]
struct RuntimeState(FeedbackBaselineHorizonV1);

impl FeedbackRuntimeStatePort for RuntimeState {
    fn resolve<'a>(
        &'a self,
        _context: &'a RequestContext,
        input: &'a tracedecay_domain::feedback::FeedbackEvaluationInputV1,
    ) -> FeedbackPortFuture<'a, Option<FeedbackRuntimeStateV1>> {
        let state = FeedbackRuntimeStateV1::new(
            FeedbackAuthoritativeRuntimeStateV1 {
                snapshot: FeedbackCycleRuntimeSnapshotV1::from_request(&input.request),
                baseline_horizon: Some(self.0.clone()),
                runtime_watermark: digest('f'),
            },
            input.target.generation_id.clone(),
        )
        .expect("runtime state");
        Box::pin(async move { Some(state) })
    }
}

#[derive(Clone)]
struct SavedDiagnostics {
    results: Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>,
    baselines: Vec<FeedbackDiagnosticBaselineV1>,
}

impl FeedbackDiagnosticsPort for SavedDiagnostics {
    fn diagnostics<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a FeedbackDiagnosticsRequest,
    ) -> FeedbackPortFuture<'a, Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>> {
        let results = self.results.clone();
        Box::pin(async move { results })
    }

    fn diagnostic_history<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a FeedbackDiagnosticsRequest,
        _runtime: &'a FeedbackRuntimeStateV1,
    ) -> FeedbackPortFuture<'a, Vec<FeedbackDiagnosticBaselineV1>> {
        let baselines = self.baselines.clone();
        Box::pin(async move { baselines })
    }
}

#[derive(Clone)]
struct FixedImpact(FeedbackImpactV1);

impl FeedbackImpactPort for FixedImpact {
    fn impact<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a FeedbackImpactRequest,
    ) -> FeedbackPortFuture<'a, FeedbackImpactPortOutcome> {
        let impact = self.0.clone();
        Box::pin(async move { FeedbackImpactPortOutcome::Complete(impact) })
    }
}

#[derive(Clone)]
struct Observations(Arc<dyn FeedbackObservationPort + Send + Sync>);

impl FeedbackObservationPort for Observations {
    fn observe(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        observation: tracedecay_domain::feedback::FeedbackCycleObservationV1,
    ) {
        self.0.observe(input, observation);
    }
}

fn feedback_service(
    runtime: Arc<crate::feedback::concrete::FeedbackRuntime>,
    request: &FeedbackCycleExecutionRequest,
) -> FeedbackCycleService<
    RuntimeState,
    SavedDiagnostics,
    FixedImpact,
    crate::feedback::concrete::ProjectFeedbackStore,
    Observations,
    crate::feedback::concrete::ProjectFeedbackRouteAuthorization,
> {
    let provider = request.providers.first().expect("saved provider").clone();
    let horizon = baseline_horizon();
    let (generation_digest, file_digest) = saved_content_digests(&request.input);
    let baseline = FeedbackDiagnosticBaselineV1 {
        identity: FeedbackDiagnosticBaselineIdentityV1 {
            current_generation_id: request
                .input
                .target
                .generation_id
                .clone()
                .expect("saved generation"),
            current_generation_digest: generation_digest.clone(),
            current_head_commit_id: request.input.request.scope.head_commit_id.clone(),
            current_content_digest: file_digest.clone(),
            provider_identity_digest: provider.compute_digest().expect("provider digest"),
            horizon: horizon.clone(),
        },
        diagnostic_anchors: Vec::new(),
        state: FeedbackBaselineStateV1::Complete,
    };
    let impact = FeedbackImpactV1 {
        target: request.input.target.clone(),
        affected_files: vec![request.input.target.file.clone()],
        affected_callers: Vec::new(),
        affected_tests: Vec::new(),
        evidence_anchors: Vec::new(),
        state: FeedbackImpactStateV1::Complete,
        affected_tests_state: FeedbackImpactStateV1::Complete,
    };
    FeedbackCycleService::new(
        RuntimeState(horizon),
        SavedDiagnostics {
            results: vec![
                DiagnosticProviderResult::new(
                    provider,
                    DiagnosticProviderState::SupportedComplete,
                    Some(Vec::new()),
                )
                .expect("complete saved diagnostic provider"),
            ],
            baselines: vec![baseline],
        },
        FixedImpact(impact),
        runtime.publication_store(),
        Observations(runtime.observation_port()),
        runtime.route_authorization(),
        operation(),
    )
}

type CycleStep = Box<dyn Fn() -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> + Send + Sync>;

struct SequencedCycle {
    steps: Mutex<VecDeque<CycleStep>>,
}

impl FeedbackCycleRuntimePort for SequencedCycle {
    fn execute(
        &self,
        _request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let step = self
            .steps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front();
        match step {
            Some(step) => step(),
            None => Box::pin(async { Err(LspRuntimeFailure::new("feedback-cycle-exhausted")) }),
        }
    }
}

#[derive(Clone)]
struct FixedScope(LspFeedbackProjectionScope);

impl LspFeedbackProjectionScopePort for FixedScope {
    fn resolve(
        &self,
        _root: AdmittedRoot,
        _document_uri: Option<String>,
    ) -> LspRuntimeFuture<Result<LspFeedbackProjectionScope, LspRuntimeFailure>> {
        let scope = self.0.clone();
        Box::pin(async move { Ok(scope) })
    }
}

struct UnusedDiagnosticProjection;

impl LspFeedbackDiagnosticProjectionPort for UnusedDiagnosticProjection {
    fn project(
        &self,
        _root: AdmittedRoot,
        _document_uri: String,
        _scope: LspFeedbackProjectionScope,
        _cycle: tracedecay_domain::feedback::FeedbackCycleResultV1,
        _expansion_handles: std::collections::BTreeMap<String, String>,
    ) -> LspRuntimeFuture<Result<Vec<tracedecay_lsp::GatewayDiagnostic>, LspRuntimeFailure>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

struct UnusedTestRuns;

impl LspTestRunProjectionPort for UnusedTestRuns {
    fn snapshot(
        &self,
        _root: AdmittedRoot,
        _document_uri: Option<String>,
        _document_content_digest: Option<ContentDigest>,
    ) -> LspRuntimeFuture<ContextProjectionOutcome> {
        Box::pin(async { ContextProjectionOutcome::Unsupported })
    }
}

fn github_finding() -> FeedbackFindingV1 {
    FeedbackFindingV1 {
        finding_id: FeedbackFindingId::new("finding.lsp-advisory-source.github").expect("finding"),
        classification: FeedbackDiagnosticClassificationV1::New,
        lifecycle: FeedbackFindingLifecycleV1::Active,
        retrieval_anchor_id: Some(
            tracedecay_domain::RetrievalAnchorId::new("anchor.lsp-advisory-source.github")
                .expect("anchor"),
        ),
        provider_state: ProviderEvaluationStateV1::SupportedCompletedComplete,
        safe_bounded_preview: Some("GitHub review finding".to_owned()),
        diagnostic_projection: Some(FeedbackDiagnosticProjectionV1 {
            file: FileOccurrenceId::new("src/lib.rs").expect("file"),
            span: SourceSpan {
                start_byte: 0,
                end_byte: 2,
            },
            symbol: None,
            code: "github-review".to_owned(),
            severity: DiagnosticSeverityV1::Warning,
            safe_bounded_message: "GitHub review finding".to_owned(),
            producer: FeedbackDiagnosticProducerV1::GitHubReview,
            code_description_uri: None,
        }),
    }
}

fn complete_advisory_providers() -> Vec<FeedbackAdvisoryProviderStateV1> {
    [
        FeedbackDiagnosticProducerV1::GitHubReview,
        FeedbackDiagnosticProducerV1::CiLocalization,
        FeedbackDiagnosticProducerV1::Proximity,
    ]
    .map(|producer| FeedbackAdvisoryProviderStateV1 {
        producer,
        state: ProviderEvaluationStateV1::SupportedCompletedComplete,
    })
    .to_vec()
}

async fn database(root: &std::path::Path) -> Database {
    let path = root.join("feedback.db");
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(&path, "LSP advisory source journey")
        .expect("database authority");
    let database =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("database")
            .0;
    DiagnosticsStore::new(database.clone())
        .ensure_schema()
        .await
        .expect("diagnostics schema");
    database
}

async fn seed_github_diagnostic(database: &Database, observed_at: UtcMicros) {
    let projection = projection_scope();
    let mut snapshot =
        CleanGenerationDiagnosticSnapshotBuilderV1::new(CleanGenerationDiagnosticScopeV1 {
            generation_id: projection.code_generation_id,
            repository: RepositoryId::new("repository.lsp-advisory-source").expect("repository"),
            worktree: Some(WorktreeId::new("worktree.lsp-advisory-source").expect("worktree")),
            reference: Some(RefId::new("refs/heads/lsp-advisory-source").expect("reference")),
            source_revision: Some(projection.head_commit_id),
            analyzer_revision: ComponentVersion::new("analyzer.lsp-advisory-source.v1")
                .expect("analyzer"),
            configuration_revision: ComponentVersion::new("configuration.lsp-advisory-source.v1")
                .expect("configuration"),
            collected_at: observed_at,
        });
    snapshot
        .contribute(
            DiagnosticPillarV1::GitHubReview,
            DiagnosticContributionV1 {
                anchor: tracedecay_domain::RetrievalAnchorId::new(
                    "anchor.lsp-advisory-source.github",
                )
                .expect("anchor"),
                file_occurrence_id: FileOccurrenceId::new("src/lib.rs").expect("file"),
                content_digest: projection
                    .document_content_digest
                    .expect("document content"),
                span: SourceSpan {
                    start_byte: 0,
                    end_byte: 2,
                },
                symbol_occurrence_id: None,
                code: "github-review".to_owned(),
                severity: DiagnosticSeverityV1::Warning,
                message: "GitHub review finding".to_owned(),
            },
        )
        .expect("contribution");
    let store = DiagnosticsStore::new(database.clone());
    snapshot
        .publish(&store)
        .await
        .expect("published diagnostic");
}

#[tokio::test]
async fn concrete_feedback_source_projects_expands_and_clears_a_saved_github_finding() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    std::fs::write(root.path().join("src/lib.rs"), SOURCE).expect("source");
    let database = database(root.path()).await;
    let observed_at = now_micros();
    seed_github_diagnostic(&database, observed_at).await;

    let scope = scope();
    let operation = operation();
    let context = context(&scope, &operation, observed_at);
    let runtime = Arc::new(
        open_feedback_runtime(
            database,
            root.path(),
            scope.clone(),
            source_access(&scope, &operation, observed_at),
        )
        .await
        .expect("feedback runtime"),
    );
    let active_request = cycle_request(digest('a'), observed_at);
    let clean_request = cycle_request(digest('b'), UtcMicros(observed_at.0.saturating_add(10)));
    let active_service = Arc::new(feedback_service(runtime.clone(), &active_request));
    let clean_service = Arc::new(feedback_service(runtime.clone(), &clean_request));
    let active_context = context.clone();
    let clean_context = context;
    let active_advisory = FeedbackCycleAdvisoryV1 {
        providers: complete_advisory_providers(),
        findings: vec![github_finding()],
    };
    let clean_advisory = FeedbackCycleAdvisoryV1 {
        providers: complete_advisory_providers(),
        findings: Vec::new(),
    };
    let cycle = Arc::new(SequencedCycle {
        steps: Mutex::new(VecDeque::from([
            // The return type is named so the async block coerces to the
            // trait-object future `CycleStep` holds; the `as CycleStep` cast
            // on the outer box cannot drive that coercion by itself.
            Box::new(move || -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
                let service = active_service.clone();
                let context = active_context.clone();
                let request = active_request.clone();
                let advisory = active_advisory.clone();
                Box::pin(async move {
                    service
                        .execute_with_advisory(&context, request, advisory)
                        .await
                        .map(|_| ())
                        .map_err(|_| LspRuntimeFailure::new("feedback-cycle-failed"))
                })
            }) as CycleStep,
            Box::new(move || -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
                let service = clean_service.clone();
                let context = clean_context.clone();
                let request = clean_request.clone();
                let advisory = clean_advisory.clone();
                Box::pin(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    service
                        .execute_with_advisory(&context, request, advisory)
                        .await
                        .map(|_| ())
                        .map_err(|_| LspRuntimeFailure::new("feedback-cycle-failed"))
                })
            }) as CycleStep,
        ])),
    });
    let source = ConcreteFeedbackLspSource::new(
        runtime,
        |_| cycle,
        Arc::new(FixedScope(projection_scope())),
        Arc::new(UnusedDiagnosticProjection),
        Arc::new(UnusedTestRuns),
    );
    let root_uri = AdmittedRoot::new("file:///project");
    let document_uri = "file:///project/src/lib.rs".to_owned();

    source
        .execute(FeedbackCycleRequest {
            root_uri: root_uri.uri().to_owned(),
            document_uri: document_uri.clone(),
            trigger: DiagnosticTrigger::DocumentSave,
        })
        .await
        .expect("saved edit publication");
    let ContextProjectionOutcome::Ready(active) = source
        .snapshot(
            root_uri.clone(),
            LspRequestId::Number(1),
            ContextProjectionRequest::new(
                ContextProjectionKind::github_review(),
                Some(document_uri.clone()),
            ),
        )
        .await
    else {
        panic!("saved GitHub finding must project");
    };
    assert_eq!(active.coverage, ContextCoverage::Complete);
    assert_eq!(active.items.len(), 1);
    let handle = active.items[0]
        .retrieval_handle
        .clone()
        .expect("authorized expansion handle");
    let ContextExpansionOutcome::Ready(expanded) = source
        .expand(
            root_uri.clone(),
            LspRequestId::Number(2),
            ContextExpansionRequest {
                retrieval_handle: handle,
            },
        )
        .await
    else {
        panic!("canonical GitHub finding expansion must be authorized");
    };
    assert_eq!(expanded.coverage, ContextCoverage::Complete);
    assert!(expanded.evidence.is_some());

    source
        .execute(FeedbackCycleRequest {
            root_uri: root_uri.uri().to_owned(),
            document_uri: document_uri.clone(),
            trigger: DiagnosticTrigger::DocumentSave,
        })
        .await
        .expect("clean saved edit publication");
    let ContextProjectionOutcome::Ready(cleared) = source
        .snapshot(
            root_uri,
            LspRequestId::Number(3),
            ContextProjectionRequest::new(
                ContextProjectionKind::github_review(),
                Some(document_uri),
            ),
        )
        .await
    else {
        panic!("clean successor must project a clear");
    };
    assert!(cleared.items.is_empty());
    assert_eq!(cleared.coverage, ContextCoverage::Complete);
}

#[tokio::test]
async fn incomplete_publication_remains_readable_without_consuming_completed_dedupe() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    std::fs::write(root.path().join("src/lib.rs"), SOURCE).expect("source");
    let database = database(root.path()).await;
    let observed_at = now_micros();
    seed_github_diagnostic(&database, observed_at).await;

    let resolved = scope();
    let operation = operation();
    let context = context(&resolved, &operation, observed_at);
    let mut access = source_access(&resolved, &operation, observed_at);
    for surface in ["feedback_get", "feedback_list", "feedback_impact"] {
        access.effective_capabilities.insert(
            feedback_surface_operation(surface)
                .expect("feedback operation catalog")
                .expect("feedback read operation")
                .capability_id()
                .clone(),
        );
    }
    let runtime = Arc::new(
        open_feedback_runtime(database, root.path(), resolved, access)
            .await
            .expect("feedback runtime"),
    );
    let request = cycle_request(digest('a'), observed_at);
    let service = feedback_service(runtime.clone(), &request);
    let mut providers = complete_advisory_providers();
    providers[1].state = ProviderEvaluationStateV1::Unavailable;
    let execution = service
        .execute_with_advisory(
            &context,
            request,
            FeedbackCycleAdvisoryV1 {
                providers,
                findings: vec![github_finding()],
            },
        )
        .await
        .expect("incomplete cycle");
    assert_eq!(
        execution.cycle.termination,
        tracedecay_domain::feedback::FeedbackCycleTerminationV1::IncompleteCoverage
    );
    let publication = execution
        .publication
        .clone()
        .expect("current incomplete result must publish");
    let canonical = compose_canonical_result(
        &runtime,
        execution,
        tracedecay_domain::feedback::FeedbackDurabilityV1::Durable,
    )
    .expect("published result handles");
    let read_handles = canonical.read_handles.as_ref().expect("cycle read handles");
    let finding_handles = canonical
        .finding_handles
        .first()
        .expect("finding read handles");
    // Public readers run in later CLI processes. The durable handle must
    // remain usable beyond one application-request deadline.
    tokio::time::sleep(std::time::Duration::from_secs(16)).await;
    let read_at = now_micros();

    let diagnostics_result = runtime
        .owner()
        .invoke(
            FeedbackReadOperationV1::Diagnostics,
            &read_handles.diagnostics_handle,
            read_at,
        )
        .await
        .expect("diagnostics read");
    let FeedbackReadInvocationResultV1::Diagnostics(diagnostics_result) = diagnostics_result else {
        panic!("diagnostics result variant");
    };
    let diagnostics = diagnostics_result.expect("diagnostics result");
    let ApplicationOutcome::Evidence(diagnostics) = diagnostics.outcome else {
        panic!("diagnostics evidence");
    };
    assert_eq!(
        diagnostics.payload.expect("diagnostics payload").cycle,
        publication.result.clone()
    );
    let impact = runtime
        .owner()
        .invoke_projection_with_controls(
            FeedbackCanonicalProjectionKindV1::Impact,
            &read_handles.diagnostics_handle,
            read_at,
            Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000)))
                .expect("impact deadline"),
            CancellationContext::active("cancel.lsp-advisory-source.impact")
                .expect("impact cancellation"),
        )
        .await
        .expect("impact read");
    assert!(matches!(
        impact,
        FeedbackReadInvocationResultV1::Impact(Ok(_))
    ));

    for (operation, handle) in [
        (FeedbackReadOperationV1::Get, &finding_handles.get_handle),
        (
            FeedbackReadOperationV1::Expand,
            finding_handles
                .expansion_handle
                .as_ref()
                .expect("expansion handle"),
        ),
        (FeedbackReadOperationV1::List, &read_handles.list_handle),
    ] {
        let result = runtime
            .owner()
            .invoke(operation, handle, read_at)
            .await
            .expect("published partial read");
        assert!(matches!(
            result,
            FeedbackReadInvocationResultV1::Get(Ok(_))
                | FeedbackReadInvocationResultV1::Expand(Ok(_))
                | FeedbackReadInvocationResultV1::List(Ok(_))
        ));
    }
    assert!(
        runtime
            .owner()
            .invoke(
                FeedbackReadOperationV1::Get,
                &read_handles.list_handle,
                read_at,
            )
            .await
            .is_err(),
        "cross-operation handle use must stay denied"
    );

    let store = runtime.publication_store();
    assert_eq!(
        store
            .lookup_completed(&context, &publication.dedupe_key)
            .await,
        FeedbackCycleDedupeState::Unique,
        "incomplete publication must not consume completed dedupe"
    );
    let mut repeated_input = publication.input.clone();
    repeated_input.request.cycle_id =
        FeedbackCycleId::new("cycle.lsp-advisory-source.repeated").expect("cycle");
    let mut repeated_runtime = publication.runtime.clone();
    repeated_runtime.authoritative.snapshot =
        FeedbackCycleRuntimeSnapshotV1::from_request(&repeated_input.request);
    let repeated_result =
        tracedecay_domain::feedback::FeedbackCycleResultV1::new_with_advisory_provider_states(
            &repeated_input.request,
            tracedecay_domain::feedback::FeedbackCycleTerminationV1::IncompleteCoverage,
            publication.result.provider_states.clone(),
            publication.result.advisory_provider_states.clone(),
            publication.result.baseline_states.clone(),
            publication.result.impact.clone(),
            publication.result.impact_state,
            publication.result.affected_tests_state,
            publication.result.findings.clone(),
            publication.result.total_findings,
            publication.result.returned_findings,
            publication.result.omitted_findings,
        )
        .expect("repeated incomplete result");
    let repeated = FeedbackPublicationV1::new(
        repeated_input.clone(),
        publication.dedupe_key.clone(),
        repeated_result,
        repeated_runtime.clone(),
        publication.authorized_scope.clone(),
        publication.authority.clone(),
    )
    .expect("repeated incomplete publication");
    assert_eq!(
        store.record_publication(&context, &repeated).await,
        FeedbackPublicationRecordState::Recorded
    );
    assert_eq!(
        store
            .lookup_completed(&context, &publication.dedupe_key)
            .await,
        FeedbackCycleDedupeState::Unique,
        "repeated incomplete publications must not consume completed dedupe"
    );
    let complete_result =
        tracedecay_domain::feedback::FeedbackCycleResultV1::new_with_advisory_provider_states(
            &repeated_input.request,
            tracedecay_domain::feedback::FeedbackCycleTerminationV1::Blocked,
            publication.result.provider_states.clone(),
            complete_advisory_providers(),
            publication.result.baseline_states.clone(),
            publication.result.impact.clone(),
            publication.result.impact_state,
            publication.result.affected_tests_state,
            publication.result.findings.clone(),
            publication.result.total_findings,
            publication.result.returned_findings,
            publication.result.omitted_findings,
        )
        .expect("complete retry result");
    let complete = FeedbackPublicationV1::new(
        repeated_input,
        publication.dedupe_key.clone(),
        complete_result,
        repeated_runtime,
        publication.authorized_scope.clone(),
        publication.authority.clone(),
    )
    .expect("complete retry publication");
    assert_eq!(
        store.record_publication(&context, &complete).await,
        FeedbackPublicationRecordState::Recorded,
        "same-key complete retry must publish"
    );
    assert_eq!(
        store
            .lookup_completed(&context, &publication.dedupe_key)
            .await,
        FeedbackCycleDedupeState::Duplicate
    );

    let mut stale = complete;
    stale.result.scope.head_commit_id =
        CommitId::new("1111111111111111111111111111111111111111").expect("stale head");
    assert_eq!(
        store.record_publication(&context, &stale).await,
        FeedbackPublicationRecordState::Unavailable,
        "stale publication must fail closed"
    );
    let cancelled = context.clone().with_cancellation(
        CancellationContext::cancelled("cancel.lsp-advisory-source", observed_at)
            .expect("cancelled context"),
    );
    assert_eq!(
        store.record_publication(&cancelled, &publication).await,
        FeedbackPublicationRecordState::Cancelled,
        "cancelled publication must not persist"
    );
}

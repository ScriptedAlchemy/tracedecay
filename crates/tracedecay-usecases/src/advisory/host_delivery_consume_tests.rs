//! Falsifiable coverage for the production consume handoff: one atomically
//! recorded advisory publication yields exactly one Hook V2 lookup notice,
//! hook-less hosts stay typed unavailable, and unpublished cycles deliver
//! nothing. The publication below is produced by the real
//! `FeedbackCycleService` over the concrete project feedback runtime, never
//! hand-assembled.

use std::collections::BTreeSet;
use std::sync::Arc;

use tokio::runtime::Handle;
use tracedecay_application::feedback::{
    FeedbackBudgetUsage, FeedbackCycleAdvisoryV1, FeedbackCycleControl,
    FeedbackCycleExecutionRequest, FeedbackCycleService, FeedbackDiagnosticsPort,
    FeedbackDiagnosticsRequest, FeedbackImpactPort, FeedbackImpactPortOutcome,
    FeedbackImpactRequest, FeedbackPortFuture, FeedbackRuntimeStatePort, FeedbackRuntimeStateV1,
    feedback_surface_operation,
};
use tracedecay_application::{
    ApplicationOperation, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DiagnosticProviderDescriptor, DiagnosticProviderIdentity,
    DiagnosticProviderIdentityParts, DiagnosticProviderResult, DiagnosticProviderState,
    DisclosureClass, PolicyDecisionRef, ProviderCoverage, ProviderDocumentIdentity,
    ProviderFreshness, ProviderOrigin, ProviderProvenance, ProviderSourceIdentity, RequestContext,
    RequestId, ResolvedScope, RevisionDigest, now_micros,
};
use tracedecay_domain::configuration::{
    AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceBindingId, SourceKindV1,
};
use tracedecay_domain::feedback::{
    FeedbackActorContextV1, FeedbackAdvisoryProviderStateV1, FeedbackAuthoritativeRuntimeStateV1,
    FeedbackBaselineHorizonV1, FeedbackBaselineStateV1, FeedbackBudgetV1,
    FeedbackContentIdentityV1, FeedbackCycleId, FeedbackCycleRequestV1,
    FeedbackCycleRuntimeSnapshotV1, FeedbackDiagnosticBaselineIdentityV1,
    FeedbackDiagnosticBaselineV1, FeedbackDiagnosticProducerV1, FeedbackDiagnosticV1,
    FeedbackImpactStateV1, FeedbackImpactV1, FeedbackScopeV1, FeedbackTargetV1, FeedbackTriggerV1,
    ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, CommitId, ComponentVersion, ContentDigest, FileOccurrenceId,
    LanguageDescriptorRevision, LanguageId, LocatorDigest, ManifestDigest, ProjectId, ProviderId,
    RefId, RepositoryId, RetrievalAnchorId, SourceSpan, UtcMicros, WorktreeId,
};
use tracedecay_hooks::{
    HookFeedbackDeliveryOutcomeV1, HookFeedbackDeliveryRouteV1, HookFeedbackRollbackSwitchV1,
};
use tracedecay_lsp::{
    AdmittedRoot, CanonicalContextProjectionAuthority, CanonicalDiagnosticRefreshRequest,
    CanonicalDiagnosticSnapshotAuthority, ContextProjectionOutcome, ContextProjectionRegistration,
    ContextProjectionRequest, FeedbackCycleRequest, FeedbackCycleRuntimePort, GatewayCapabilities,
    LspAnalyzerCancellationAuthority, LspRequestId, LspRuntimeFailure, LspRuntimeFuture,
    UnavailableSemanticProvider, UpstreamCapabilities,
};
use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay_tool_catalog::CapabilityId;

use tracedecay_host_integration::HostKindV1;

use super::{
    AdvisoryHookDeliveryV1, AdvisoryHookLookupNoticeV1, AdvisoryHookNoticeQueueV1,
    AdvisoryHookNoticeSinkV1, AdvisoryHostDeliveryErrorV1, AdvisoryHostDeliveryRegistrationV1,
    new_advisory_hook_delivery_port,
};
use crate::advisory::{AdvisoryContributionsV1, AdvisoryCycleOutcome};
use crate::feedback::CanonicalFeedbackResultV1;
use crate::feedback::concrete::{FeedbackRuntime, open_feedback_runtime};
use crate::lsp_runtime::DaemonLspSessionFactory;
use crate::source_authorization::ProjectSourceAccessSnapshot;

fn digest(fill: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).expect("digest")
}

fn resolved_scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.advisory-host-delivery").expect("project"),
        RepositoryId::new("repository.advisory-host-delivery").expect("repository"),
        WorktreeId::new("worktree.advisory-host-delivery").expect("worktree"),
        Some(RefId::new("refs/heads/advisory-host-delivery").expect("reference")),
    )
    .expect("scope")
}

fn feedback_scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.advisory-host-delivery").expect("project"),
        repository_id: RepositoryId::new("repository.advisory-host-delivery").expect("repository"),
        worktree_id: WorktreeId::new("worktree.advisory-host-delivery").expect("worktree"),
        branch_ref: "refs/heads/advisory-host-delivery".to_owned(),
        head_commit_id: CommitId::new("0123456789abcdef0123456789abcdef01234567").expect("head"),
    }
}

fn operation() -> ApplicationOperation {
    feedback_surface_operation("feedback_diagnostics")
        .expect("feedback operation catalog")
        .expect("feedback diagnostics operation")
}

fn context(
    scope: &ResolvedScope,
    operation: &ApplicationOperation,
    now: UtcMicros,
) -> RequestContext {
    let expires_at = UtcMicros(now.0.saturating_add(60_000_000));
    let requester = ActorId::new("actor.advisory-host-delivery").expect("requester");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.advisory-host-delivery").expect("grant"),
        1,
        digest('e'),
        ActorId::new("actor.advisory-host-delivery.issuer").expect("issuer"),
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
        RequestId::new("request.advisory-host-delivery").expect("request"),
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.advisory-host-delivery").expect("cancellation"),
    )
    .expect("context")
}

fn source_access(
    scope: &ResolvedScope,
    operation: &ApplicationOperation,
    now: UtcMicros,
) -> ProjectSourceAccessSnapshot {
    ProjectSourceAccessSnapshot {
        scope: scope.clone(),
        requester: ActorId::new("actor.advisory-host-delivery").expect("requester"),
        binding: ScopeSourceBinding::new(
            SourceBindingId::new("binding.advisory-host-delivery").expect("binding"),
            SourceKindV1::Cursor,
            LocatorDigest::new(digest('f').as_str().to_owned()).expect("locator"),
            AuthorityRef::Project(scope.project_id.clone()),
        )
        .expect("binding"),
        configuration_revision: ConfigurationRevisionId::new(
            "configuration.advisory-host-delivery",
        )
        .expect("configuration revision"),
        configuration_digest: digest('a'),
        configuration_provenance_digest: digest('b'),
        effective_capabilities: BTreeSet::from([operation.capability_id().clone()]),
        grant_expires_at: UtcMicros(now.0.saturating_add(60_000_000)),
    }
}

fn cycle_request(observed_at: UtcMicros) -> FeedbackCycleExecutionRequest {
    let request = FeedbackCycleRequestV1::new(
        FeedbackCycleId::new("cycle.advisory-host-delivery").expect("cycle"),
        feedback_scope(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('1'),
            file_digest: digest('2'),
        },
        FeedbackTriggerV1::DocumentSave,
        digest('d'),
        digest('a'),
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
            generation_id: Some(
                CodeGenerationId::new("generation.v1.aaaaaaaa.00000001").expect("generation"),
            ),
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
        control: FeedbackCycleControl::Continue,
    }
}

fn diagnostic_provider(
    input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
) -> DiagnosticProviderIdentity {
    let FeedbackContentIdentityV1::SavedContent { file_digest, .. } = &input.request.content else {
        panic!("saved edit fixture must use durable content");
    };
    DiagnosticProviderIdentity::new(DiagnosticProviderIdentityParts {
        scope: resolved_scope(),
        source: ProviderSourceIdentity::CleanGeneration {
            generation: input
                .target
                .generation_id
                .clone()
                .expect("saved generation"),
        },
        document: ProviderDocumentIdentity {
            file: input.target.file.clone(),
            content_digest: ContentDigest::new(file_digest.as_str().to_owned())
                .expect("provider content"),
            document_version: None,
        },
        producer: DiagnosticProviderDescriptor {
            provider: ProviderId::new("provider.advisory-host-delivery").expect("provider"),
            analyzer_revision: ComponentVersion::new("analyzer.advisory-host-delivery.v1")
                .expect("analyzer"),
            language: LanguageId::new("rust").expect("language"),
            language_descriptor_revision: LanguageDescriptorRevision::new(
                "language.rust.advisory-host-delivery.v1",
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
                RetrievalAnchorId::new("anchor.advisory-host-delivery.provider")
                    .expect("provider anchor"),
            ),
        },
        configuration: RevisionDigest {
            revision: ComponentVersion::new("configuration.advisory-host-delivery.v1")
                .expect("configuration"),
            digest: input.request.configuration_digest.clone(),
        },
        policy: PolicyDecisionRef::new(
            "policy.advisory-host-delivery",
            1,
            digest('d'),
            ComponentVersion::new("policy.advisory-host-delivery.v1").expect("policy revision"),
        )
        .expect("policy"),
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
struct Observations(
    Arc<dyn tracedecay_application::feedback::FeedbackObservationPort + Send + Sync>,
);

impl tracedecay_application::feedback::FeedbackObservationPort for Observations {
    fn observe(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        observation: tracedecay_domain::feedback::FeedbackCycleObservationV1,
    ) {
        self.0.observe(input, observation);
    }
}

struct NoopFeedbackCycle;

impl FeedbackCycleRuntimePort for NoopFeedbackCycle {
    fn execute(
        &self,
        _request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        Box::pin(async { Ok(()) })
    }
}

struct NoopCancellation;

impl LspAnalyzerCancellationAuthority for NoopCancellation {
    fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
        false
    }
}

struct UnsupportedContext;

impl CanonicalContextProjectionAuthority for UnsupportedContext {
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

struct UnavailableDiagnostics;

impl CanonicalDiagnosticSnapshotAuthority for UnavailableDiagnostics {
    fn refresh(
        &self,
        _request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<tracedecay_lsp::GenerationDiagnostics, LspRuntimeFailure>> {
        Box::pin(async { Err(LspRuntimeFailure::new("diagnostics-unavailable")) })
    }

    fn supports_workspace_diagnostics(&self) -> bool {
        false
    }
}

fn lsp_session_factory() -> Arc<DaemonLspSessionFactory> {
    Arc::new(DaemonLspSessionFactory::new(
        Handle::current(),
        Arc::new(NoopFeedbackCycle),
        Arc::new(UnavailableSemanticProvider),
        Arc::new(UnavailableDiagnostics),
        Arc::new(NoopCancellation),
        Arc::new(UnsupportedContext),
        GatewayCapabilities::default(),
        UpstreamCapabilities::default(),
    ))
}

fn unavailable_hook_notice(_notice: &AdvisoryHookLookupNoticeV1) -> HookFeedbackDeliveryOutcomeV1 {
    HookFeedbackDeliveryOutcomeV1::Unavailable
}

fn unavailable_hook_sink() -> Arc<AdvisoryHookNoticeSinkV1> {
    Arc::new(unavailable_hook_notice)
}

fn rollback() -> HookFeedbackRollbackSwitchV1 {
    HookFeedbackRollbackSwitchV1 {
        configuration_revision: 1,
        route: HookFeedbackDeliveryRouteV1::HookV2,
    }
}

async fn database(root: &std::path::Path) -> Database {
    let path = root.join("feedback.db");
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(&path, "advisory host delivery consume")
        .expect("database authority");
    Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
        .await
        .expect("database")
        .0
}

struct ConsumeFixture {
    registration: AdvisoryHostDeliveryRegistrationV1,
    queue: Arc<AdvisoryHookNoticeQueueV1>,
    completed: AdvisoryCycleOutcome,
    _root: tempfile::TempDir,
}

/// Runs the real feedback cycle service to one atomically recorded
/// publication, then mounts the host-delivery registration over the same
/// runtime's owner/store and the daemon hook notice queue.
async fn consume_fixture() -> ConsumeFixture {
    let root = tempfile::tempdir().expect("root");
    let database = database(root.path()).await;
    let observed_at = now_micros();
    let scope = resolved_scope();
    let operation = operation();
    let context = context(&scope, &operation, observed_at);
    let runtime: Arc<FeedbackRuntime> = Arc::new(
        open_feedback_runtime(
            database,
            root.path(),
            scope.clone(),
            source_access(&scope, &operation, observed_at),
        )
        .await
        .expect("feedback runtime"),
    );

    let request = cycle_request(observed_at);
    let provider = request.providers.first().expect("saved provider").clone();
    let horizon = baseline_horizon();
    let FeedbackContentIdentityV1::SavedContent {
        generation_digest,
        file_digest,
    } = &request.input.request.content
    else {
        panic!("saved edit fixture must use durable content");
    };
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
    let service = FeedbackCycleService::new(
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
        operation,
    );
    let advisory = FeedbackCycleAdvisoryV1 {
        providers: [
            FeedbackDiagnosticProducerV1::GitHubReview,
            FeedbackDiagnosticProducerV1::CiLocalization,
            FeedbackDiagnosticProducerV1::Proximity,
        ]
        .map(|producer| FeedbackAdvisoryProviderStateV1 {
            producer,
            state: ProviderEvaluationStateV1::SupportedCompletedComplete,
        })
        .to_vec(),
        findings: Vec::new(),
    };
    let observation_input = request.input.clone();
    let execution = service
        .execute_with_advisory(&context, request, advisory)
        .await
        .expect("one canonical feedback cycle");
    assert!(
        execution.publication.is_some(),
        "a durable saved-content cycle must record its shared-store publication"
    );
    let completed = AdvisoryCycleOutcome::Completed {
        cycle: CanonicalFeedbackResultV1 {
            execution,
            finding_handles: Vec::new(),
        },
        contributions: AdvisoryContributionsV1::absent(),
        observation_input,
    };

    let queue = AdvisoryHookNoticeQueueV1::new(feedback_scope());
    let registration = AdvisoryHostDeliveryRegistrationV1 {
        scope,
        feedback_owner: runtime.owner(),
        publication_store: runtime.publication_store(),
        lsp_session_factory: lsp_session_factory(),
        hook_delivery_port: new_advisory_hook_delivery_port(
            feedback_scope(),
            queue.sink(),
            unavailable_hook_sink(),
        ),
        source_observations: runtime.source_observation_port(),
    };
    ConsumeFixture {
        registration,
        queue,
        completed,
        _root: root,
    }
}

#[tokio::test]
async fn completed_publication_is_consumed_into_exactly_one_hook_notice() {
    let fixture = consume_fixture().await;
    let publication = fixture
        .completed
        .publication()
        .expect("recorded publication")
        .clone();

    let first = fixture
        .registration
        .consume_completed_publication(HostKindV1::ClaudeCode, &fixture.completed, rollback())
        .expect("first consumption");
    assert!(
        matches!(
            first.hook,
            AdvisoryHookDeliveryV1::Delivered {
                outcome: HookFeedbackDeliveryOutcomeV1::Delivered,
                ..
            }
        ),
        "the recorded publication must reach the hook notice queue"
    );
    let notice = fixture.queue.peek().expect("one pending hook notice");
    assert_eq!(notice.result_id, publication.result.result_id);
    assert_eq!(notice.cycle_id, publication.result.cycle_id);
    assert_eq!(
        notice.returned_findings,
        publication.result.returned_findings
    );

    // Consuming the same recorded publication again is idempotent: the queue
    // reports the typed duplicate and holds exactly one pending notice.
    let second = fixture
        .registration
        .consume_completed_publication(HostKindV1::ClaudeCode, &fixture.completed, rollback())
        .expect("duplicate consumption");
    assert!(matches!(
        second.hook,
        AdvisoryHookDeliveryV1::Delivered {
            outcome: HookFeedbackDeliveryOutcomeV1::Duplicate,
            ..
        }
    ));
    assert!(fixture.queue.acknowledge(&notice));
    assert_eq!(
        fixture.queue.peek(),
        None,
        "one publication must enqueue exactly one hook notice"
    );
}

#[tokio::test]
async fn unpublished_cycles_and_hookless_hosts_deliver_nothing() {
    let fixture = consume_fixture().await;

    // A tick without a completed publication is a typed error, not a delivery.
    let cancelled = AdvisoryCycleOutcome::Cancelled {
        contributions: AdvisoryContributionsV1::absent(),
    };
    assert!(matches!(
        fixture.registration.consume_completed_publication(
            HostKindV1::ClaudeCode,
            &cancelled,
            rollback()
        ),
        Err(AdvisoryHostDeliveryErrorV1::AdvisoryNotCompleted)
    ));
    assert_eq!(fixture.queue.peek(), None);

    // Copilot's checked-in matrix has no live Hook V2 route, so even a
    // recorded publication stays typed unavailable instead of enqueued.
    let delivery = fixture
        .registration
        .consume_completed_publication(HostKindV1::Copilot, &fixture.completed, rollback())
        .expect("typed unavailable delivery");
    assert!(matches!(
        delivery.hook,
        AdvisoryHookDeliveryV1::Unavailable(_)
    ));
    assert_eq!(fixture.queue.peek(), None);
}

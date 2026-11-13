//! Direct behavior of the shared advisory owner for editor/hook feedback
//! versus explicit advisory demand. Every cycle runs the production
//! `AdvisoryRuntime` over a real feedback cycle runtime, the durable
//! publication store, the production CI archive and exact-evidence mapping,
//! the retained CI and GitHub stores, and a real GitHub review owner. Remote
//! reads are counted at their only exits: the explicit CI live read (held at a
//! barrier) and TCP connections to the GitHub provider address.

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use tracedecay_contracts::diagnostics::FeedbackDiagnosticProviderAdmissionV1;
use tracedecay_contracts::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    CiFailureLocalizationRequestV1, FeedbackBudgetUsage, FeedbackCycleControl,
    FeedbackCycleExecutionRequest, FeedbackPortFuture, FeedbackRuntimeStatePort,
    FeedbackRuntimeStateV1, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
    GITHUB_REVIEW_INGEST_USE_CASE_ID_V1, GitHubReviewReadRequestV1, GitHubReviewReadResponseV1,
    ProximityEvaluationRequestV1, feedback_surface_operation,
};
use tracedecay_contracts::{
    AdvisoryFindingValidityWindowV1, ApplicationOperation, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DiagnosticProviderDescriptor, DiagnosticProviderIdentity,
    DiagnosticProviderIdentityParts, DisclosureClass, PolicyDecisionRef, ProviderCoverage,
    ProviderDocumentIdentity, ProviderFreshness, ProviderOrigin, ProviderProvenance,
    ProviderSourceIdentity, RequestContext, RequestId, ResolvedScope, RevisionDigest, now_micros,
};
use tracedecay_domain::configuration::{
    AuthorityRef, ConfigurationRevisionId, GitHubStackedPullRequestPolicyV1, ScopeSourceBinding,
    SourceBindingId, SourceKindV1,
};
use tracedecay_domain::feedback::{
    CiFailureCoverageV1, CiFailureGenerationEvidenceV1, CiFailureLocalizationStateV1,
    CiFailureParserIdentityV1, CiFailureSymbolEvidenceV1, FeedbackActorContextV1,
    FeedbackAuthoritativeRuntimeStateV1, FeedbackBudgetV1, FeedbackContentIdentityV1,
    FeedbackCycleId, FeedbackCycleRequestV1, FeedbackCycleRuntimeSnapshotV1,
    FeedbackCycleTerminationV1, FeedbackEvaluationInputV1, FeedbackFindingLifecycleV1,
    FeedbackScopeV1, FeedbackTargetV1, FeedbackTriggerV1, GitHubPullRequestIdV1,
    GitHubReviewCoverageV1, GitHubReviewCurrentBranchRemapV1, GitHubReviewImmutableAnchorV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1,
    GitHubReviewReadCheckpointV1, GitHubReviewReadOperationV1, ProviderEvaluationStateV1,
};
use tracedecay_domain::test_fixtures::digest;
use tracedecay_domain::{
    ActorId, CodeGenerationId, CommitId, ComponentVersion, ContentDigest, FileOccurrenceId,
    LanguageDescriptorRevision, LanguageId, LocatorDigest, ProjectId, ProviderId, RefId,
    RepositoryId, RetrievalAnchorId, SourceSpan, SymbolOccurrenceId, UtcMicros, WorktreeId,
};
use tracedecay_global_db::RegisteredGlobalDbOwnerV1;
use tracedecay_global_db::configuration::store::OwnedGlobalDbConfigurationControlStore;
use tracedecay_global_db::tests::harness::open_registered_test_database_fixture;
use tracedecay_graph_query::{
    CodeGraphProjectionReadPort, CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
};
use tracedecay_lsp::LspRuntimeFailure;
use tracedecay_runtime_core::cancellation::MonotonicDeadline;
use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::*;
use crate::advisory::ci_runtime::{
    CiCodeAnchorStoreV1, CiExactCodeEvidenceV1, CiProviderReadResultV1,
    CiRetainedProviderObservationAuthorityV1, CiRetainedProviderRecordV1, CiRetainedReadV1,
    CiSourceAccessAuthorityV1, CiSourceAccessOutcomeV1, ProductionCiArchiveHandleV1,
    ProductionCiExactEvidenceHandleV1, ProductionCiProviderConfigV1,
    ProjectCiRetainedObservationStoreV1, open_production_ci_provider_authorities_v1,
};
use crate::advisory::fixtures::{
    AdvisorySourceBackedCompositeFixtureV1, load_advisory_source_backed_composite_fixture_v1,
};
use crate::advisory::github_runtime::{
    GitHubCanonicalReviewAnchorsV1, GitHubHttpReadConfigV1, GitHubProviderLifecycleV1,
    GitHubReadOnlyCredentialV1, GitHubRepositoryTargetV1, GitHubReviewAnchorSeedV1,
    GitHubReviewAtomicRefreshStoreV1, GitHubReviewProviderIdentityV1, GitHubReviewRefreshStateV1,
    GitHubReviewRefreshStoreCommitOutcomeV1, ProjectGitHubReviewStoreV1,
};
use crate::feedback::concrete::{FeedbackRuntime, open_feedback_runtime};
use crate::feedback::cycle_runtime::{FeedbackCycleLspInput, open_feedback_cycle_runtime};
use crate::operation_stream::{OperationEventAuthority, OperationKind};
use crate::source_authorization::ProjectSourceAccessSnapshot;
use crate::stack_coordinator::DaemonGitHubStackCoordinatorV1;

const PULL_REQUEST_NUMBER: u64 = 4817;

/// Graph boundary for the explicit-refresh-free proof: no impact read is
/// needed, and any attempt is a typed unavailable graph, not a fixture.
struct UnavailableGraph;

impl CodeGraphProjectionReadPort for UnavailableGraph {
    fn open<'a>(&'a self, _request: CodeGraphReadRequest<'a>) -> CodeGraphReadFuture<'a> {
        Box::pin(async {
            Err(CodeGraphReadError::Unavailable {
                detail: "local-cycle proof has no code graph".to_owned(),
            })
        })
    }
}

struct NoAffectedTests;

impl tracedecay_contracts::AffectedTestsRetrievalPort for NoAffectedTests {
    fn affected_tests(
        &self,
        _context: &tracedecay_contracts::RetrievalPortContext<'_>,
        _requests: &[tracedecay_contracts::AffectedTestsRequest],
    ) -> Vec<
        tracedecay_contracts::RetrievalPortOutcome<
            tracedecay_contracts::retrieval::AffectedTestsResult,
        >,
    > {
        Vec::new()
    }
}

struct CurrentRuntimeState;

impl FeedbackRuntimeStatePort for CurrentRuntimeState {
    fn resolve<'a>(
        &'a self,
        _context: &'a RequestContext,
        input: &'a FeedbackEvaluationInputV1,
    ) -> FeedbackPortFuture<'a, Option<FeedbackRuntimeStateV1>> {
        let state = FeedbackRuntimeStateV1::new(
            FeedbackAuthoritativeRuntimeStateV1 {
                snapshot: FeedbackCycleRuntimeSnapshotV1::from_request(&input.request),
                baseline_horizon: None,
                runtime_watermark: digest('9'),
            },
            input.target.generation_id.clone(),
        )
        .ok();
        Box::pin(async move { state })
    }
}

/// Review anchors are never resolved here: retained GitHub state is read back
/// already decoded, and the seeded review collection is empty.
#[derive(Clone)]
struct NoReviewAnchors;

impl GitHubCanonicalReviewAnchorAuthorityV1 for NoReviewAnchors {
    fn resolve<'a>(
        &'a self,
        _request: &'a GitHubReviewReadRequestV1,
        _seed: &'a GitHubReviewAnchorSeedV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubCanonicalReviewAnchorsV1>> {
        Box::pin(async { None })
    }
}

impl GitHubCurrentBranchRemapper for NoReviewAnchors {
    fn remap<'a>(
        &'a self,
        _context: &'a RequestContext,
        _scope: &'a FeedbackScopeV1,
        _original: &'a GitHubReviewImmutableAnchorV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubReviewCurrentBranchRemapV1>> {
        Box::pin(async { None })
    }
}

struct NoProximityEvidence;

impl CanonicalProximityEvidenceAuthorityV1 for NoProximityEvidence {
    fn current_evidence<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a ProximityEvaluationRequestV1,
    ) -> FeedbackPortFuture<
        'a,
        Option<crate::advisory::proximity_runtime::CanonicalProximityEvidenceBatchV1>,
    > {
        Box::pin(async { None })
    }
}

/// Source policy shared by both remote providers; tests revoke it mid-run.
#[derive(Default)]
struct SourcePolicy {
    revoked: AtomicBool,
}

impl CiSourceAccessAuthorityV1 for SourcePolicy {
    fn authorize_ci<'a>(
        &'a self,
        _context: &'a RequestContext,
        _scope: &'a FeedbackScopeV1,
    ) -> FeedbackPortFuture<'a, CiSourceAccessOutcomeV1> {
        let outcome = if self.revoked.load(Ordering::SeqCst) {
            CiSourceAccessOutcomeV1::Denied
        } else {
            CiSourceAccessOutcomeV1::Ready
        };
        Box::pin(async move { outcome })
    }
}

impl GitHubSourceAccessAuthorityV1 for SourcePolicy {
    fn authorize<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubProviderLifecycleV1> {
        let outcome = if self.revoked.load(Ordering::SeqCst) {
            GitHubProviderLifecycleV1::Denied
        } else {
            GitHubProviderLifecycleV1::Ready
        };
        Box::pin(async move { outcome })
    }
}

/// Canonical graph boundary for CI localization. The production
/// exact-evidence mapping above it still enforces scope, run, policy and
/// state monotonicity.
struct ResolvedCiSymbol;

impl CiCodeAnchorStoreV1 for ResolvedCiSymbol {
    fn resolve<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a CiFailureLocalizationRequestV1,
        _record: &'a CiRetainedProviderRecordV1,
    ) -> FeedbackPortFuture<'a, Option<CiExactCodeEvidenceV1>> {
        Box::pin(async {
            Some(CiExactCodeEvidenceV1 {
                state: CiFailureLocalizationStateV1::Complete,
                coverage: CiFailureCoverageV1::Complete,
                generation: Some(CiFailureGenerationEvidenceV1 {
                    generation_id: generation(),
                    retrieval_anchor_id: RetrievalAnchorId::new("anchor.local-cycle.generation")
                        .unwrap(),
                }),
                symbol: Some(CiFailureSymbolEvidenceV1 {
                    retrieval_anchor_id: RetrievalAnchorId::new("anchor.local-cycle.symbol")
                        .unwrap(),
                    file: target_file(),
                    span: SourceSpan {
                        start_byte: 0,
                        end_byte: 8,
                    },
                    symbol: SymbolOccurrenceId::new("symbol.local-cycle.failed").unwrap(),
                }),
                callers: Vec::new(),
                tests: Vec::new(),
            })
        })
    }
}

/// The explicit path's only CI provider read. It is counted and held at a
/// barrier; retained reads delegate unchanged to the production archive.
struct HeldLiveCiArchive {
    production: ProductionCiArchiveHandleV1,
    live_reads: AtomicUsize,
    arrived: Mutex<Option<oneshot::Sender<()>>>,
    release: Mutex<Option<oneshot::Receiver<()>>>,
}

impl CiReadOnlyProviderArchiveV1 for HeldLiveCiArchive {
    type Record = CiRetainedProviderRecordV1;

    fn read_record<'a>(
        &'a self,
        _context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiProviderReadResultV1<Self::Record>> {
        self.live_reads.fetch_add(1, Ordering::SeqCst);
        let arrived = self.arrived.lock().unwrap().take();
        let release = self.release.lock().unwrap().take();
        Box::pin(async move {
            if let Some(arrived) = arrived {
                let _ = arrived.send(());
            }
            if let Some(release) = release {
                let _ = release.await;
            }
            CiProviderReadResultV1 {
                provider: ProviderId::new("provider.github-actions").unwrap(),
                run: request.run.clone(),
                state: CiFailureLocalizationStateV1::Unavailable,
                coverage: CiFailureCoverageV1::Unavailable,
                source_degradation: None,
                failures: 0,
                checks: 0,
                annotations: 0,
                record: None,
            }
        })
    }

    fn read_retained<'a>(
        &'a self,
        context: &'a RequestContext,
        scope: &'a FeedbackScopeV1,
    ) -> FeedbackPortFuture<'a, CiRetainedReadV1<Self::Record>> {
        self.production.read_retained(context, scope)
    }
}

type LocalCycleRuntime = AdvisoryRuntime<
    NoReviewAnchors,
    NoReviewAnchors,
    Arc<HeldLiveCiArchive>,
    ProductionCiExactEvidenceHandleV1,
    NoProximityEvidence,
    OwnedGlobalDbConfigurationControlStore,
>;

struct Harness {
    _root: tempfile::TempDir,
    _global_owner: RegisteredGlobalDbOwnerV1,
    runtime: LocalCycleRuntime,
    feedback: Arc<FeedbackRuntime>,
    events: OperationEventAuthority,
    database: Database,
    resolved: ResolvedScope,
    scope: FeedbackScopeV1,
    fixture: AdvisorySourceBackedCompositeFixtureV1,
    ci_archive: Arc<HeldLiveCiArchive>,
    policy: Arc<SourcePolicy>,
    github_connections: Arc<AtomicUsize>,
    ordinal: AtomicUsize,
}

fn generation() -> CodeGenerationId {
    CodeGenerationId::new("generation.v1.localcycle.00000001").unwrap()
}

fn target_file() -> FileOccurrenceId {
    FileOccurrenceId::new("src/lib.rs").unwrap()
}

fn diagnostics_operation() -> ApplicationOperation {
    feedback_surface_operation("feedback_diagnostics")
        .unwrap()
        .unwrap()
}

fn surface_operation(name: &str) -> ApplicationOperation {
    feedback_surface_operation(name).unwrap().unwrap()
}

impl Harness {
    async fn open() -> Self {
        let fixture = load_advisory_source_backed_composite_fixture_v1().unwrap();
        let branch_ref = format!("refs/heads/{}", fixture.branch);
        let resolved = ResolvedScope::new(
            ProjectId::new("project.local-cycle").unwrap(),
            RepositoryId::new("repository.local-cycle").unwrap(),
            WorktreeId::new("worktree.local-cycle").unwrap(),
            Some(RefId::new(branch_ref.clone()).unwrap()),
        )
        .unwrap();
        let scope = FeedbackScopeV1 {
            project_id: resolved.project_id.clone(),
            repository_id: resolved.repository_id.clone(),
            worktree_id: resolved.worktree_id.clone(),
            branch_ref,
            head_commit_id: fixture.head_commit_id.clone(),
        };
        let root = tempfile::tempdir().unwrap();
        crate::register_test_schema_installer();
        let path = root.path().join("local-cycle.db");
        let authority = DatabaseAuthority::acquire_test(&path, "advisory local cycle").unwrap();
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        let (global, global_owner) = open_registered_test_database_fixture(
            &root.path().join("project-runtime.db"),
            TestDatabaseRuntimeScope::ProjectSessions {
                project_id: scope.project_id.clone(),
            },
        )
        .await
        .unwrap();
        let now = now_micros();
        let operation = diagnostics_operation();
        let list = surface_operation("feedback_list");
        let feedback = Arc::new(
            open_feedback_runtime(
                database.clone(),
                root.path(),
                root.path().join("response-handles"),
                resolved.clone(),
                ProjectSourceAccessSnapshot {
                    scope: resolved.clone(),
                    requester: ActorId::new("actor.local-cycle").unwrap(),
                    binding: ScopeSourceBinding::new(
                        SourceBindingId::new("binding.local-cycle").unwrap(),
                        SourceKindV1::Cursor,
                        LocatorDigest::new(digest('f').as_str().to_owned()).unwrap(),
                        AuthorityRef::Project(resolved.project_id.clone()),
                    )
                    .unwrap(),
                    configuration_revision: ConfigurationRevisionId::new(
                        "configuration.local-cycle",
                    )
                    .unwrap(),
                    configuration_digest: digest('a'),
                    configuration_provenance_digest: digest('b'),
                    effective_capabilities: BTreeSet::from([
                        operation.capability_id().clone(),
                        list.capability_id().clone(),
                    ]),
                    grant_expires_at: UtcMicros(now.0.saturating_add(600_000_000)),
                },
            )
            .await
            .unwrap(),
        );
        let lsp_input: FeedbackCycleLspInput = Arc::new(|_request| {
            Box::pin(async { Err(LspRuntimeFailure::new("local-cycle-proof-has-no-lsp")) })
        });
        let feedback_cycle = open_feedback_cycle_runtime(
            database.clone(),
            Arc::clone(&feedback),
            Arc::new(CurrentRuntimeState),
            vec![
                FeedbackDiagnosticProviderAdmissionV1::from_stored_publication(provider_identity(
                    &resolved,
                    &cycle_input(&scope, "admission", now),
                ))
                .unwrap(),
            ],
            root.path().to_path_buf(),
            Arc::new(UnavailableGraph),
            Arc::new(NoAffectedTests),
            feedback.observation_port(),
            operation,
            surface_operation("feedback_impact"),
            surface_operation("affected_tests"),
            lsp_input,
            None,
        )
        .unwrap();

        let policy = Arc::new(SourcePolicy::default());
        let retained_ci: Arc<dyn crate::advisory::CiRetainedProviderObservationAuthorityV1> =
            Arc::new(
                ProjectCiRetainedObservationStoreV1::new(database.clone(), scope.clone()).unwrap(),
            );
        let (production_ci, ci_exact_evidence) = open_production_ci_provider_authorities_v1(
            ProductionCiProviderConfigV1 {
                provider: ProviderId::new("provider.github-actions").unwrap(),
                parser: CiFailureParserIdentityV1 {
                    parser_id: "parser.github-actions.v1".to_owned(),
                    parser_version: "1".to_owned(),
                },
                target: crate::advisory::GitHubCiRepositoryTargetV1 {
                    owner: "ScriptedAlchemy".to_owned(),
                    repository: "tracedecay".to_owned(),
                },
                credential: GitHubReadOnlyCredentialV1::anonymous(),
                http: GitHubHttpReadConfigV1::default(),
                source_access: Arc::clone(&policy) as Arc<dyn CiSourceAccessAuthorityV1>,
            },
            retained_ci,
            Arc::new(ResolvedCiSymbol),
        )
        .unwrap()
        .into_registrar_parts();
        let ci_archive = Arc::new(HeldLiveCiArchive {
            production: production_ci,
            live_reads: AtomicUsize::new(0),
            arrived: Mutex::new(None),
            release: Mutex::new(None),
        });

        // The GitHub provider address only counts connections: any attempt
        // to reach it is remote work, and no TLS session is ever offered.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let github_connections = Arc::new(AtomicUsize::new(0));
        {
            let github_connections = Arc::clone(&github_connections);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    github_connections.fetch_add(1, Ordering::SeqCst);
                    drop(stream);
                }
            });
        }
        let stack_coordinator = Arc::new(DaemonGitHubStackCoordinatorV1::default());
        stack_coordinator
            .register_scope(&resolved, GitHubStackedPullRequestPolicyV1::Disabled)
            .unwrap();
        let pull_request_id = pull_request_id();
        let runtime = AdvisoryRuntime::open(
            AdvisoryRuntimeOpenV1 {
                database: database.clone(),
                project_root: root.path().to_path_buf(),
                resolved_scope: resolved.clone(),
                feedback_scope: scope.clone(),
                github: Some(GitHubReviewRuntimeOwnerConfigV1 {
                    database: database.clone(),
                    resolved_scope: resolved.clone(),
                    feedback_scope: scope.clone(),
                    target: GitHubRepositoryTargetV1 {
                        owner: "ScriptedAlchemy".to_owned(),
                        repository: "tracedecay".to_owned(),
                        pull_request_number: PULL_REQUEST_NUMBER,
                        pull_request_id: pull_request_id.clone(),
                    },
                    credential: GitHubReadOnlyCredentialV1::anonymous(),
                    http: GitHubHttpReadConfigV1 {
                        rest_base_uri: format!("https://{address}"),
                        graphql_uri: format!("https://{address}/graphql"),
                        ..GitHubHttpReadConfigV1::default()
                    },
                    identity: GitHubReviewProviderIdentityV1 {
                        provider: ProviderId::new("provider.github").unwrap(),
                        repository_owner: "ScriptedAlchemy".to_owned(),
                        repository_name: "tracedecay".to_owned(),
                        pull_request_number: PULL_REQUEST_NUMBER,
                        base_commit_id: fixture.base_commit_id.clone(),
                        head_commit_id: scope.head_commit_id.clone(),
                        merge_base_commit_id: fixture.merge_base_commit_id.clone(),
                    },
                    stack_coordinator,
                    stack_anchor_db: global.clone(),
                    stack_observability: None,
                }),
                feedback_cycle,
            },
            AdvisoryProviderAuthoritiesV1 {
                github_remapper: NoReviewAnchors,
                github_anchors: NoReviewAnchors,
                ci_source: Arc::clone(&ci_archive),
                ci_exact_evidence,
                proximity_evidence: NoProximityEvidence,
                github_source_access: Some(
                    Arc::clone(&policy) as Arc<dyn GitHubSourceAccessAuthorityV1>
                ),
                configuration:
                    OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
                        global,
                    ),
            },
        )
        .unwrap_or_else(|error| panic!("production advisory owner must open: {error:?}"));
        Self {
            _root: root,
            _global_owner: global_owner,
            runtime,
            feedback,
            events: OperationEventAuthority::default(),
            database,
            resolved,
            scope,
            fixture,
            ci_archive,
            policy,
            github_connections,
            ordinal: AtomicUsize::new(0),
        }
    }

    fn github_request(&self) -> GitHubReviewReadRequestV1 {
        GitHubReviewReadRequestV1 {
            // Anonymous reads use REST, as the production composition does.
            operation: GitHubReviewReadOperationV1::RestListPullRequestReviewComments,
            scope: self.scope.clone(),
            pull_request_id: pull_request_id(),
        }
    }

    fn ci_request(&self) -> CiFailureLocalizationRequestV1 {
        CiFailureLocalizationRequestV1 {
            scope: self.scope.clone(),
            run: self.fixture.ci_provider_record.run_identity(),
        }
    }

    fn context(&self, remote_grants: bool) -> RequestContext {
        let ordinal = self.ordinal.fetch_add(1, Ordering::SeqCst);
        let now = now_micros();
        let operation = diagnostics_operation();
        let mut capabilities = BTreeSet::from([operation.capability_id().clone()]);
        let mut use_cases = BTreeSet::from([operation.use_case_id().clone()]);
        if remote_grants {
            capabilities.extend([
                CapabilityId::new(CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1).unwrap(),
                CapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1).unwrap(),
            ]);
            use_cases.extend([
                UseCaseId::new(CI_FAILURE_LOCALIZE_USE_CASE_ID_V1).unwrap(),
                UseCaseId::new(GITHUB_REVIEW_INGEST_USE_CASE_ID_V1).unwrap(),
            ]);
        }
        let expires_at = UtcMicros(now.0.saturating_add(600_000_000));
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.local-cycle").unwrap(),
            1,
            digest('e'),
            ActorId::new("actor.local-cycle.issuer").unwrap(),
            UtcMicros(now.0.saturating_sub(1_000_000)),
            expires_at,
            self.resolved.clone(),
            capabilities,
            use_cases,
            DisclosureClass::Evidence,
        )
        .unwrap();
        RequestContext::new(
            ActorId::new("actor.local-cycle").unwrap(),
            self.resolved.clone(),
            grant,
            RequestId::new(format!("request.local-cycle.{ordinal}")).unwrap(),
            Deadline::new(expires_at).unwrap(),
            CancellationContext::active(format!("cancel.local-cycle.{ordinal}")).unwrap(),
        )
        .unwrap()
    }

    async fn control(&self, context: &RequestContext) -> AdvisoryCycleControl {
        AdvisoryCycleControl {
            operation: self
                .events
                .begin(context, OperationKind::FeedbackDiagnostics, now_micros())
                .await
                .unwrap(),
            deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(30)),
        }
    }

    fn request(&self, label: &str, remote: AdvisoryRemoteEvidenceV1) -> AdvisoryCycleRequest {
        let observed_at = now_micros();
        let input = cycle_input(&self.scope, label, observed_at);
        AdvisoryCycleRequest {
            feedback: FeedbackCycleExecutionRequest {
                providers: vec![provider_identity(&self.resolved, &input)],
                input,
                maximum_returned_findings: 16,
                usage: FeedbackBudgetUsage {
                    completed_at: UtcMicros(observed_at.0.saturating_add(1)),
                    tokens_consumed: 1,
                    cost_microunits: 1,
                },
                control: FeedbackCycleControl::Continue,
            },
            remote,
            proximity: None,
            validity: AdvisoryFindingValidityWindowV1 {
                valid_at: observed_at,
                expires_at: UtcMicros(observed_at.0.saturating_add(300_000_000)),
            },
        }
    }

    fn local(&self, label: &str) -> AdvisoryCycleRequest {
        self.request(
            label,
            AdvisoryRemoteEvidenceV1::Retained {
                github: Some(self.github_request()),
                ci_configured: true,
            },
        )
    }

    fn explicit(&self, label: &str) -> AdvisoryCycleRequest {
        self.request(
            label,
            AdvisoryRemoteEvidenceV1::Refresh {
                github: Some(self.github_request()),
                ci: crate::advisory::ProductionCiFailureDiscoveryOutcomeV1::Found(Box::new(
                    self.ci_request(),
                )),
            },
        )
    }

    async fn retain_ci(&self) {
        let context = self.context(true);
        ProjectCiRetainedObservationStoreV1::new(self.database.clone(), self.scope.clone())
            .unwrap()
            .retain(
                &context,
                &self.ci_request(),
                &self.fixture.ci_provider_record,
                CiFailureLocalizationStateV1::Complete,
                CiFailureCoverageV1::Complete,
            )
            .await
            .expect("an explicit refresh retained this CI failure");
    }

    async fn retain_github(&self) {
        let context = self.context(true);
        let request = self.github_request();
        let response = GitHubReviewReadResponseV1 {
            completed_collection_scan: false,
            ingress: GitHubReviewIngressResultV1 {
                provider: ProviderId::new("provider.github").unwrap(),
                scope: self.scope.clone(),
                pull_request_id: request.pull_request_id.clone(),
                provider_base_commit_id: self.fixture.base_commit_id.clone(),
                provider_head_commit_id: self.scope.head_commit_id.clone(),
                merge_base_commit_id: self.fixture.merge_base_commit_id.clone(),
                operation: request.operation,
                outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
                coverage: GitHubReviewCoverageV1::Complete,
                items: Vec::new(),
                quarantined: Vec::new(),
                pull_request: None,
                conversation: Vec::new(),
                reviews: Vec::new(),
                fetched_at: now_micros(),
            },
            checkpoint: GitHubReviewReadCheckpointV1 {
                etag: None,
                next_cursor: None,
                rate_limit: None,
            },
        };
        let state = GitHubReviewRefreshStateV1::transition(&request, None, response)
            .expect("a complete review read is a valid retained state");
        assert_eq!(
            ProjectGitHubReviewStoreV1::new(self.database.clone(), self.scope.clone())
                .unwrap()
                .compare_and_record(&context, &request, None, &state)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Recorded
        );
    }

    fn remote_reads(&self) -> (usize, usize) {
        (
            self.ci_archive.live_reads.load(Ordering::SeqCst),
            self.github_connections.load(Ordering::SeqCst),
        )
    }
}

fn pull_request_id() -> GitHubPullRequestIdV1 {
    GitHubPullRequestIdV1::new("pull-request.local-cycle").unwrap()
}

fn cycle_input(
    scope: &FeedbackScopeV1,
    label: &str,
    observed_at: UtcMicros,
) -> FeedbackEvaluationInputV1 {
    FeedbackEvaluationInputV1 {
        request: FeedbackCycleRequestV1::new(
            FeedbackCycleId::new(format!("cycle.local-cycle.{label}")).unwrap(),
            scope.clone(),
            FeedbackContentIdentityV1::SavedContent {
                generation_digest: digest('1'),
                file_digest: digest('2'),
            },
            FeedbackTriggerV1::DocumentSave,
            digest('d'),
            digest('a'),
            FeedbackBudgetV1::bounded(10_000, 10_000, 100_000, 100_000),
        )
        .unwrap(),
        target: FeedbackTargetV1 {
            file: target_file(),
            span: Some(SourceSpan {
                start_byte: 0,
                end_byte: 8,
            }),
            symbol: None,
            generation_id: Some(generation()),
        },
        actor: FeedbackActorContextV1::default(),
        observed_at,
    }
}

fn provider_identity(
    resolved: &ResolvedScope,
    input: &FeedbackEvaluationInputV1,
) -> DiagnosticProviderIdentity {
    let FeedbackContentIdentityV1::SavedContent { file_digest, .. } = &input.request.content else {
        panic!("saved content");
    };
    DiagnosticProviderIdentity::new(DiagnosticProviderIdentityParts {
        scope: resolved.clone(),
        source: ProviderSourceIdentity::CleanGeneration {
            generation: generation(),
        },
        document: ProviderDocumentIdentity {
            file: input.target.file.clone(),
            content_digest: ContentDigest::new(file_digest.as_str().to_owned()).unwrap(),
            document_version: None,
        },
        producer: DiagnosticProviderDescriptor {
            provider: ProviderId::new("provider.local-cycle").unwrap(),
            analyzer_revision: ComponentVersion::new("analyzer.local-cycle.v1").unwrap(),
            language: LanguageId::new("rust").unwrap(),
            language_descriptor_revision: LanguageDescriptorRevision::new(
                "language.rust.local-cycle.v1",
            )
            .unwrap(),
        },
        requested_capability: CapabilityId::new("capability.diagnostics.current").unwrap(),
        freshness: ProviderFreshness::current(input.observed_at),
        coverage: ProviderCoverage::complete(1, 1),
        provenance: ProviderProvenance {
            origin: ProviderOrigin::ConfiguredAnalyzer,
            anchor: Some(RetrievalAnchorId::new("anchor.local-cycle.provider").unwrap()),
        },
        configuration: RevisionDigest {
            revision: ComponentVersion::new("configuration.local-cycle.v1").unwrap(),
            digest: input.request.configuration_digest.clone(),
        },
        policy: PolicyDecisionRef::new(
            "policy.local-cycle",
            1,
            digest('d'),
            ComponentVersion::new("policy.local-cycle.v1").unwrap(),
        )
        .unwrap(),
    })
    .unwrap()
}

fn provider_state(
    contributions: &AdvisoryContributionsV1,
    provider: AdvisoryProviderV1,
) -> ProviderEvaluationStateV1 {
    contributions
        .providers
        .iter()
        .find(|state| state.provider == provider)
        .unwrap()
        .state
}

fn completed(
    outcome: AdvisoryCycleOutcome,
) -> (CanonicalFeedbackResultV1, AdvisoryContributionsV1) {
    match outcome {
        AdvisoryCycleOutcome::Completed {
            cycle,
            contributions,
            ..
        } => (cycle, contributions),
        other => panic!("local feedback must complete, got {other:?}"),
    }
}

/// A held explicit refresh cannot hold an editor/hook cycle: the local cycle
/// publishes with retained GitHub and CI evidence (never fresh) while the
/// explicit cycle is still inside its CI provider read, and the local cycle
/// performs no provider read of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_cycle_publishes_retained_evidence_while_an_explicit_refresh_is_held() {
    let harness = Arc::new(Harness::open().await);
    harness.retain_ci().await;
    harness.retain_github().await;
    let (arrived_tx, arrived) = oneshot::channel();
    let (release, release_rx) = oneshot::channel();
    *harness.ci_archive.arrived.lock().unwrap() = Some(arrived_tx);
    *harness.ci_archive.release.lock().unwrap() = Some(release_rx);

    let explicit = {
        let harness = Arc::clone(&harness);
        tokio::spawn(async move {
            let context = harness.context(true);
            let control = harness.control(&context).await;
            harness
                .runtime
                .run_once(&context, control, harness.explicit("explicit-held"))
                .await
        })
    };
    arrived
        .await
        .expect("explicit refresh reached its CI provider read");
    let before_local = harness.remote_reads();
    assert_eq!(
        before_local.0, 1,
        "explicit refresh is inside its live CI read"
    );
    assert!(
        before_local.1 >= 1,
        "explicit refresh attempted its GitHub provider read first"
    );

    let context = harness.context(true);
    let control = harness.control(&context).await;
    let (cycle, contributions) = completed(
        harness
            .runtime
            .run_once(&context, control, harness.local("local-during-held"))
            .await
            .unwrap(),
    );
    assert!(
        !explicit.is_finished(),
        "the local cycle finished while the explicit refresh was still held"
    );
    assert_eq!(
        harness.remote_reads(),
        before_local,
        "local feedback performed no CI or GitHub provider read"
    );

    // Retained evidence attaches with partial coverage: local completion is
    // not evidence of remote success, so this cycle can never be clean, yet a
    // retained provider never forces a stale replan that would withhold the
    // local findings.
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::Ci),
        ProviderEvaluationStateV1::Partial
    );
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::GitHub),
        ProviderEvaluationStateV1::Partial
    );
    let ci_finding = cycle
        .execution
        .cycle
        .findings
        .iter()
        .find(|finding| {
            finding
                .finding_id
                .as_str()
                .starts_with("finding.ci-localization.")
        })
        .expect("the retained CI failure is published with this local cycle");
    assert_eq!(ci_finding.lifecycle, FeedbackFindingLifecycleV1::Active);
    assert_eq!(
        ci_finding.provider_state,
        ProviderEvaluationStateV1::Partial
    );
    assert!(
        ci_finding.diagnostic_projection.is_some(),
        "the retained CI failure still projects onto the edited symbol"
    );
    assert!(
        !matches!(
            cycle.execution.cycle.termination,
            FeedbackCycleTerminationV1::Clean | FeedbackCycleTerminationV1::StaleReplanRequired
        ),
        "{:?}",
        cycle.execution.cycle.termination
    );
    let publication = cycle
        .execution
        .publication
        .as_ref()
        .expect("local feedback is durably published");
    assert_eq!(
        harness
            .feedback
            .publication_store()
            .doctor_latest_publication(&context)
            .await
            .unwrap()
            .map(|stored| stored.result.result_id),
        Some(publication.result.result_id.clone()),
        "the local publication is the latest durable record"
    );

    release.send(()).unwrap();
    let (_, explicit_contributions) = completed(explicit.await.unwrap().unwrap());
    assert_eq!(
        provider_state(&explicit_contributions, AdvisoryProviderV1::Ci),
        ProviderEvaluationStateV1::Unavailable,
        "the explicit cycle reports its own unavailable CI read"
    );
    assert_eq!(
        harness.remote_reads().0,
        1,
        "only the explicit cycle read the CI provider"
    );
}

/// Retained evidence is admitted only for the exact scope, grant and current
/// source policy; otherwise the remote provider is typed unavailable and the
/// independently authorized local cycle still publishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_cycle_admits_retained_evidence_only_for_exact_scope_grant_and_policy() {
    let harness = Harness::open().await;
    let run = |label: &'static str, remote_grants: bool| {
        let harness = &harness;
        async move {
            let context = harness.context(remote_grants);
            let control = harness.control(&context).await;
            completed(
                harness
                    .runtime
                    .run_once(&context, control, harness.local(label))
                    .await
                    .unwrap(),
            )
        }
    };

    // Nothing retained yet: absent evidence is unavailable, never clean.
    let (cycle, contributions) = run("nothing-retained", true).await;
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::Ci),
        ProviderEvaluationStateV1::Unavailable
    );
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::GitHub),
        ProviderEvaluationStateV1::Unavailable
    );
    assert!(cycle.execution.publication.is_some());

    // Evidence retained for another head is not evidence for this one.
    let mut other_head = harness.scope.clone();
    other_head.head_commit_id = CommitId::new("e".repeat(40)).unwrap();
    let context = harness.context(true);
    let other_store =
        ProjectCiRetainedObservationStoreV1::new(harness.database.clone(), other_head.clone())
            .unwrap();
    let mut other_record = harness.fixture.ci_provider_record.clone();
    let other_sha = other_head.head_commit_id.as_str().to_owned();
    other_record.workflow_run.head_sha = other_sha.clone();
    other_record.workflow_job.head_sha = other_sha.clone();
    other_record.check_run.head_sha = other_sha;
    other_store
        .retain(
            &context,
            &CiFailureLocalizationRequestV1 {
                scope: other_head,
                run: other_record.run_identity(),
            },
            &other_record,
            CiFailureLocalizationStateV1::Complete,
            CiFailureCoverageV1::Complete,
        )
        .await
        .expect("evidence retained for a different head");
    let (_, contributions) = run("other-head-retained", true).await;
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::Ci),
        ProviderEvaluationStateV1::Unavailable
    );

    harness.retain_ci().await;
    harness.retain_github().await;
    let (_, contributions) = run("exact-scope-retained", true).await;
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::Ci),
        ProviderEvaluationStateV1::Partial
    );
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::GitHub),
        ProviderEvaluationStateV1::Partial
    );

    // A request without remote-source grants gets no retained evidence.
    let (cycle, contributions) = run("no-remote-grant", false).await;
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::Ci),
        ProviderEvaluationStateV1::Unavailable
    );
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::GitHub),
        ProviderEvaluationStateV1::Unavailable
    );
    assert!(cycle.execution.publication.is_some());

    // Revoking the source policy withholds retained evidence immediately.
    harness.policy.revoked.store(true, Ordering::SeqCst);
    let (cycle, contributions) = run("policy-revoked", true).await;
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::Ci),
        ProviderEvaluationStateV1::Unavailable
    );
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::GitHub),
        ProviderEvaluationStateV1::Unavailable
    );
    assert!(
        cycle.execution.cycle.findings.iter().all(|finding| !finding
            .finding_id
            .as_str()
            .starts_with("finding.ci-localization.")),
        "revoked evidence is not published"
    );
    assert!(cycle.execution.publication.is_some());
    assert_eq!(
        harness.remote_reads(),
        (0, 0),
        "no local cycle read a provider"
    );
}

/// Cancelling a held explicit refresh stops that refresh only, while local
/// cancellation still stops local work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_and_local_cancellation_are_isolated() {
    let harness = Arc::new(Harness::open().await);
    harness.retain_ci().await;
    let (arrived_tx, arrived) = oneshot::channel();
    let (_release, release_rx) = oneshot::channel::<()>();
    *harness.ci_archive.arrived.lock().unwrap() = Some(arrived_tx);
    *harness.ci_archive.release.lock().unwrap() = Some(release_rx);

    let explicit_context = harness.context(true);
    let explicit = {
        let harness = Arc::clone(&harness);
        let context = explicit_context.clone();
        tokio::spawn(async move {
            let control = harness.control(&context).await;
            harness
                .runtime
                .run_once(&context, control, harness.explicit("explicit-cancelled"))
                .await
        })
    };
    arrived
        .await
        .expect("explicit refresh reached its CI provider read");
    harness
        .events
        .cancel(
            &crate::operation_stream::OperationId::from_request(
                explicit_context.request_id().clone(),
            ),
            &explicit_context,
            now_micros(),
        )
        .await
        .unwrap();
    assert!(matches!(
        explicit.await.unwrap().unwrap(),
        AdvisoryCycleOutcome::Cancelled { .. }
    ));

    let context = harness.context(true);
    let control = harness.control(&context).await;
    let (_, contributions) = completed(
        harness
            .runtime
            .run_once(&context, control, harness.local("after-remote-cancel"))
            .await
            .unwrap(),
    );
    assert_eq!(
        provider_state(&contributions, AdvisoryProviderV1::Ci),
        ProviderEvaluationStateV1::Partial,
        "the cancelled refresh did not cancel or disturb local feedback"
    );

    let context = harness.context(true);
    let control = harness.control(&context).await;
    harness
        .events
        .cancel(
            &crate::operation_stream::OperationId::from_request(context.request_id().clone()),
            &context,
            now_micros(),
        )
        .await
        .unwrap();
    assert!(matches!(
        harness
            .runtime
            .run_once(&context, control, harness.local("local-cancelled"))
            .await
            .unwrap(),
        AdvisoryCycleOutcome::Cancelled { .. }
    ));
}

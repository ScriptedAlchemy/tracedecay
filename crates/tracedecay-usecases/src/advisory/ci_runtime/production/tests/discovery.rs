use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestId, ResolvedScope,
};
use tracedecay_domain::feedback::{CiFailureParserIdentityV1, FeedbackScopeV1};
use tracedecay_domain::{
    ActorId, CanonicalObservationIdV1, ManifestDigest, ProjectId, RefId, RepositoryId,
    RetrievalAnchorId, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::discovery::*;
use super::provider::*;
use super::*;

struct SequencedSourceAccess {
    calls: AtomicUsize,
    deny_at: usize,
}

impl SequencedSourceAccess {
    fn ready() -> Arc<dyn CiSourceAccessAuthorityV1> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            deny_at: usize::MAX,
        })
    }

    fn revoke_at(deny_at: usize) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            deny_at,
        })
    }
}

impl CiSourceAccessAuthorityV1 for SequencedSourceAccess {
    fn authorize_ci<'a>(
        &'a self,
        _context: &'a RequestContext,
        _scope: &'a FeedbackScopeV1,
    ) -> FeedbackPortFuture<'a, CiSourceAccessOutcomeV1> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let outcome = if call >= self.deny_at {
            CiSourceAccessOutcomeV1::Denied
        } else {
            CiSourceAccessOutcomeV1::Ready
        };
        Box::pin(async move { outcome })
    }
}

struct StaleSourceAccess;

impl CiSourceAccessAuthorityV1 for StaleSourceAccess {
    fn authorize_ci<'a>(
        &'a self,
        _context: &'a RequestContext,
        _scope: &'a FeedbackScopeV1,
    ) -> FeedbackPortFuture<'a, CiSourceAccessOutcomeV1> {
        Box::pin(async { CiSourceAccessOutcomeV1::Stale })
    }
}

fn scope(
    fixture: &crate::advisory::fixtures::Pr13SourceBackedCompositeFixtureV1,
) -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.ci-discovery").unwrap(),
        repository_id: RepositoryId::new("repository.ci-discovery").unwrap(),
        worktree_id: WorktreeId::new("worktree.ci-discovery").unwrap(),
        branch_ref: format!("refs/heads/{}", fixture.branch),
        head_commit_id: fixture.head_commit_id.clone(),
    }
}

fn target(
    _fixture: &crate::advisory::fixtures::Pr13SourceBackedCompositeFixtureV1,
) -> GitHubCiRepositoryTargetV1 {
    GitHubCiRepositoryTargetV1 {
        owner: "ScriptedAlchemy".to_owned(),
        repository: "tracedecay".to_owned(),
    }
}

fn config(
    fixture: &crate::advisory::fixtures::Pr13SourceBackedCompositeFixtureV1,
) -> ProductionCiProviderConfigV1 {
    config_with_source(fixture, SequencedSourceAccess::ready())
}

fn config_with_source(
    fixture: &crate::advisory::fixtures::Pr13SourceBackedCompositeFixtureV1,
    source_access: Arc<dyn CiSourceAccessAuthorityV1>,
) -> ProductionCiProviderConfigV1 {
    ProductionCiProviderConfigV1 {
        provider: ProviderId::new(GITHUB_ACTIONS_PROVIDER_ID_V1).unwrap(),
        parser: CiFailureParserIdentityV1 {
            parser_id: "parser.github-actions.v1".to_owned(),
            parser_version: "1".to_owned(),
        },
        target: target(fixture),
        credential: GitHubReadOnlyCredentialV1::anonymous(),
        http: GitHubHttpReadConfigV1::default(),
        source_access,
    }
}

fn context(scope: &FeedbackScopeV1, expires_at: UtcMicros) -> RequestContext {
    let resolved = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).unwrap()),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.ci-discovery").unwrap(),
        1,
        ManifestDigest::new(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .unwrap(),
        ActorId::new("actor.ci-discovery").unwrap(),
        UtcMicros(1),
        expires_at,
        resolved.clone(),
        BTreeSet::from([CapabilityId::new(
            tracedecay_application::feedback::CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        )
        .unwrap()]),
        BTreeSet::from([UseCaseId::new(
            tracedecay_application::feedback::CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
        )
        .unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        ActorId::new("actor.ci-discovery").unwrap(),
        resolved,
        grant,
        RequestId::new("request.ci-discovery").unwrap(),
        Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
        CancellationContext::active("cancel.ci-discovery").unwrap(),
    )
    .unwrap()
}

struct CountingDiscoveryClient {
    calls: Arc<AtomicUsize>,
}

impl ProductionCiDiscoveryReadPortV1 for CountingDiscoveryClient {
    fn read_workflow_runs_for_head<'a>(
        &'a self,
        _context: &'a RequestContext,
        _head_sha: &'a str,
        _page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
    }

    fn read_workflow_jobs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _run_id: u64,
        _page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
    }

    fn read_check_runs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _check_suite_id: u64,
        _page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
    }
}

struct PagedDiscoveryClient {
    workflow_run_pages: Vec<Vec<u8>>,
    requested_pages: Mutex<Vec<u32>>,
}

impl ProductionCiDiscoveryReadPortV1 for PagedDiscoveryClient {
    fn read_workflow_runs_for_head<'a>(
        &'a self,
        _context: &'a RequestContext,
        _head_sha: &'a str,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.requested_pages.lock().unwrap().push(page);
        let outcome = usize::try_from(page.saturating_sub(1))
            .ok()
            .and_then(|index| self.workflow_run_pages.get(index))
            .cloned()
            .map_or(
                GitHubCiTransportOutcomeV1::Unavailable,
                GitHubCiTransportOutcomeV1::Response,
            );
        Box::pin(async move { outcome })
    }

    fn read_workflow_jobs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _run_id: u64,
        _page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
    }

    fn read_check_runs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _check_suite_id: u64,
        _page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
    }
}

struct PagedWorkflowJobDiscoveryClient {
    workflow_job_pages: Vec<Vec<u8>>,
    requested_pages: Mutex<Vec<u32>>,
}

impl ProductionCiDiscoveryReadPortV1 for PagedWorkflowJobDiscoveryClient {
    fn read_workflow_runs_for_head<'a>(
        &'a self,
        _context: &'a RequestContext,
        _head_sha: &'a str,
        _page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
    }

    fn read_workflow_jobs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.requested_pages.lock().unwrap().push(page);
        let outcome = usize::try_from(page.saturating_sub(1))
            .ok()
            .and_then(|index| self.workflow_job_pages.get(index))
            .cloned()
            .map_or(
                GitHubCiTransportOutcomeV1::Unavailable,
                GitHubCiTransportOutcomeV1::Response,
            );
        Box::pin(async move { outcome })
    }

    fn read_check_runs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _check_suite_id: u64,
        _page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
    }
}

struct ConfiguredDiscoveryClient {
    record: GitHubCiProviderRecordV1,
    requests: Mutex<Vec<&'static str>>,
}

impl ProductionCiDiscoveryReadPortV1 for ConfiguredDiscoveryClient {
    fn read_workflow_runs_for_head<'a>(
        &'a self,
        _context: &'a RequestContext,
        _head_sha: &'a str,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.requests.lock().unwrap().push("workflow-runs");
        let outcome = (page == 1)
            .then(|| {
                serde_json::to_vec(&serde_json::json!({
                    "total_count": 1,
                    "workflow_runs": [self.record.workflow_run.clone()],
                }))
                .ok()
                .map_or(
                    GitHubCiTransportOutcomeV1::Unavailable,
                    GitHubCiTransportOutcomeV1::Response,
                )
            })
            .unwrap_or(GitHubCiTransportOutcomeV1::Unavailable);
        Box::pin(async move { outcome })
    }

    fn read_workflow_jobs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.requests.lock().unwrap().push("workflow-jobs");
        let outcome = (page == 1)
            .then(|| {
                serde_json::to_vec(&serde_json::json!({
                    "total_count": 1,
                    "jobs": [self.record.workflow_job.clone()],
                }))
                .ok()
                .map_or(
                    GitHubCiTransportOutcomeV1::Unavailable,
                    GitHubCiTransportOutcomeV1::Response,
                )
            })
            .unwrap_or(GitHubCiTransportOutcomeV1::Unavailable);
        Box::pin(async move { outcome })
    }

    fn read_check_runs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _check_suite_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.requests.lock().unwrap().push("check-runs");
        let outcome = (page == 1)
            .then(|| {
                serde_json::to_vec(&serde_json::json!({
                    "total_count": 1,
                    "check_runs": [self.record.check_run.clone()],
                }))
                .ok()
                .map_or(
                    GitHubCiTransportOutcomeV1::Unavailable,
                    GitHubCiTransportOutcomeV1::Response,
                )
            })
            .unwrap_or(GitHubCiTransportOutcomeV1::Unavailable);
        Box::pin(async move { outcome })
    }
}

#[tokio::test]
async fn denied_context_performs_zero_ci_discovery_reads() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let calls = Arc::new(AtomicUsize::new(0));
    let client = CountingDiscoveryClient {
        calls: Arc::clone(&calls),
    };

    assert_eq!(
        discover_production_ci_failure_request_with_v1(
            &context(&scope, UtcMicros(2)),
            &config(&fixture),
            &scope,
            &client,
        )
        .await,
        ProductionCiFailureDiscoveryOutcomeV1::Denied
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn stale_ci_access_remains_stale_without_a_network_read() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let calls = Arc::new(AtomicUsize::new(0));
    let client = CountingDiscoveryClient {
        calls: Arc::clone(&calls),
    };

    let outcome = discover_production_ci_failure_request_with_v1(
        &context(&scope, UtcMicros(i64::MAX)),
        &config_with_source(&fixture, Arc::new(StaleSourceAccess)),
        &scope,
        &client,
    )
    .await;

    assert_eq!(outcome, ProductionCiFailureDiscoveryOutcomeV1::Stale);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn configured_ci_discovery_reads_workflow_jobs_in_both_consensus_scans() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let client = ConfiguredDiscoveryClient {
        record: fixture.ci_provider_record.clone(),
        requests: Mutex::new(Vec::new()),
    };

    let outcome = discover_production_ci_failure_request_with_v1(
        &context(&scope, UtcMicros(i64::MAX)),
        &config(&fixture),
        &scope,
        &client,
    )
    .await;

    assert_eq!(
        outcome.request().map(|request| &request.run),
        Some(&fixture.ci.run)
    );
    assert_eq!(
        *client.requests.lock().unwrap(),
        vec![
            "workflow-runs",
            "workflow-jobs",
            "check-runs",
            "workflow-runs",
            "workflow-jobs",
            "check-runs",
        ]
    );
}

#[test]
fn configured_github_actions_builds_exact_failure_request_from_provider_records() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let record = &fixture.ci_provider_record;
    let outcome = select_production_ci_failure_request_v1(
        &ProviderId::new("provider.github-actions").unwrap(),
        &target(&fixture),
        &scope,
        std::slice::from_ref(&record.workflow_run),
        std::slice::from_ref(&record.workflow_job),
        std::slice::from_ref(&record.check_run),
    );

    let ProductionCiFailureDiscoveryOutcomeV1::Found(request) = outcome else {
        panic!("expected exact GitHub Actions failure request");
    };
    assert_eq!(request.scope, scope);
    assert_eq!(request.run, fixture.ci.run);
}

#[test]
fn ci_discovery_does_not_require_pull_request_resolution() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let mut record = fixture.ci_provider_record.clone();
    record.workflow_run.pull_requests.clear();
    record.check_run.pull_requests.clear();

    let outcome = select_production_ci_failure_request_v1(
        &ProviderId::new("provider.github-actions").unwrap(),
        &target(&fixture),
        &scope,
        std::slice::from_ref(&record.workflow_run),
        std::slice::from_ref(&record.workflow_job),
        std::slice::from_ref(&record.check_run),
    );

    assert!(matches!(
        outcome,
        ProductionCiFailureDiscoveryOutcomeV1::Found(_)
    ));
}

#[test]
fn discovery_preserves_rate_limit_and_decode_failure_kinds() {
    let checkpoint = tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1 {
        limit: 5_000,
        remaining: 0,
        reset_at: UtcMicros(42),
    };
    assert_eq!(
        discovery_response_body(GitHubCiTransportOutcomeV1::RateLimited(checkpoint)),
        Err(ProductionCiFailureDiscoveryOutcomeV1::RateLimited(
            CiFailureRateLimitCheckpointV1 {
                limit: 5_000,
                remaining: 0,
                reset_at: UtcMicros(42),
            },
        ))
    );
    let parse = serde_json::from_slice::<GitHubActionsWorkflowRunsPageV1>(b"{")
        .err()
        .unwrap();
    assert_eq!(
        discovery_decode_failure(parse),
        ProductionCiFailureDiscoveryOutcomeV1::Failed(CiFailureSourceFailureV1::Parse)
    );
    let schema = serde_json::from_slice::<GitHubActionsWorkflowRunsPageV1>(b"{}")
        .err()
        .unwrap();
    assert_eq!(
        discovery_decode_failure(schema),
        ProductionCiFailureDiscoveryOutcomeV1::Failed(CiFailureSourceFailureV1::Schema)
    );
}

struct RetainedFixture(CiRetainedProviderRecordV1);

impl CiRetainedProviderObservationAuthorityV1 for RetainedFixture {
    fn load<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, Option<CiRetainedProviderRecordV1>> {
        let record = self.0.clone();
        Box::pin(async move { Some(record) })
    }

    fn retain<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a CiFailureLocalizationRequestV1,
        _record: &'a GitHubCiProviderRecordV1,
        _state: CiFailureLocalizationStateV1,
        _coverage: CiFailureCoverageV1,
    ) -> FeedbackPortFuture<'a, Option<CiRetainedProviderObservationV1>> {
        Box::pin(async { None })
    }
}

#[tokio::test]
async fn retained_stale_fallback_exposes_rate_limit_cause_and_coverage() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let request = CiFailureLocalizationRequestV1 {
        scope: scope.clone(),
        run: fixture.ci.run.clone(),
    };
    let target = target(&fixture);
    let archive = ProductionGitHubCiArchiveV1 {
        provider: ProviderId::new(GITHUB_ACTIONS_PROVIDER_ID_V1).unwrap(),
        client: GitHubReadOnlyClientV1::new_for_ci(
            target.clone(),
            GitHubReadOnlyCredentialV1::anonymous(),
            GitHubHttpReadConfigV1::default(),
        )
        .unwrap(),
        retained: Arc::new(RetainedFixture(CiRetainedProviderRecordV1 {
            provider_record: fixture.ci_provider_record.clone(),
            observation: CiRetainedProviderObservationV1 {
                observation_id: CanonicalObservationIdV1::new(
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .unwrap(),
                failure_anchor: RetrievalAnchorId::new("anchor.ci-retained").unwrap(),
                provider_head_commit_id: scope.head_commit_id.clone(),
                failure_kind: CiFailureKindV1::LintFailure,
                observed_at: UtcMicros(7),
            },
        })),
        target,
        source_access: SequencedSourceAccess::ready(),
    };
    let degradation = CiFailureSourceDegradationV1::RateLimited(CiFailureRateLimitCheckpointV1 {
        limit: 5_000,
        remaining: 0,
        reset_at: UtcMicros(42),
    });

    let read = archive
        .retained_result(
            &context(&scope, UtcMicros(i64::MAX)),
            &request,
            degradation.clone(),
        )
        .await;

    assert_eq!(read.state, CiFailureLocalizationStateV1::Stale);
    assert_eq!(read.coverage, CiFailureCoverageV1::Stale);
    assert_eq!(read.source_degradation, Some(degradation));
    assert!(read.record.is_some());
    assert!(read.validate_for(&request));
}

struct TerminalArchive(CiFailureSourceDegradationV1);

impl CiReadOnlyProviderArchiveV1 for TerminalArchive {
    type Record = ();

    fn read_record<'a>(
        &'a self,
        _context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiProviderReadResultV1<Self::Record>> {
        let degradation = self.0.clone();
        Box::pin(async move {
            CiProviderReadResultV1 {
                provider: ProviderId::new(GITHUB_ACTIONS_PROVIDER_ID_V1).unwrap(),
                run: request.run.clone(),
                state: CiFailureLocalizationStateV1::Failed,
                coverage: CiFailureCoverageV1::Unavailable,
                source_degradation: Some(degradation),
                failures: 0,
                checks: 0,
                annotations: 0,
                record: None,
            }
        })
    }
}

struct NeverExact;

impl CiExactEvidenceAuthorityV1<()> for NeverExact {
    fn map_exact_evidence<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a CiFailureLocalizationRequestV1,
        _read: &'a CiProviderReadResultV1<()>,
        _record: &'a (),
    ) -> FeedbackPortFuture<'a, Option<CiFailureLocalizationResultV1>> {
        Box::pin(async { None })
    }
}

#[tokio::test]
async fn localization_reader_preserves_rate_limit_and_failed_outcomes() {
    use crate::advisory::{CiReadOnlyEvidenceSource, DaemonCiReadOnlyEvidenceSourceV1};
    use tracedecay_application::feedback::CiFailureLocalizationPortOutcomeV1;

    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let request = CiFailureLocalizationRequestV1 {
        scope: scope.clone(),
        run: fixture.ci.run.clone(),
    };
    let context = context(&scope, UtcMicros(i64::MAX));
    let checkpoint = CiFailureRateLimitCheckpointV1 {
        limit: 5_000,
        remaining: 0,
        reset_at: UtcMicros(42),
    };
    let rate_limited = DaemonCiReadOnlyEvidenceSourceV1::new(
        TerminalArchive(CiFailureSourceDegradationV1::RateLimited(
            checkpoint.clone(),
        )),
        NeverExact,
    )
    .read_localization(&context, &request)
    .await;
    assert_eq!(
        rate_limited,
        CiFailureLocalizationPortOutcomeV1::RateLimited(checkpoint)
    );

    let failed = DaemonCiReadOnlyEvidenceSourceV1::new(
        TerminalArchive(CiFailureSourceDegradationV1::Failed(
            CiFailureSourceFailureV1::Schema,
        )),
        NeverExact,
    )
    .read_localization(&context, &request)
    .await;
    assert_eq!(
        failed,
        CiFailureLocalizationPortOutcomeV1::Failed(CiFailureSourceFailureV1::Schema)
    );
}

#[test]
fn ci_discovery_requires_exact_two_scan_consensus() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let request = CiFailureLocalizationRequestV1 {
        scope: scope(&fixture),
        run: fixture.ci.run.clone(),
    };
    assert_eq!(
        consensus_ci_discovery_outcome(
            ProductionCiFailureDiscoveryOutcomeV1::found(request.clone()),
            ProductionCiFailureDiscoveryOutcomeV1::found(request.clone()),
        ),
        ProductionCiFailureDiscoveryOutcomeV1::found(request.clone())
    );

    let mut drifted = request.clone();
    drifted.run.attempt_id = (drifted.run.attempt_id.parse::<u64>().unwrap() + 1).to_string();
    assert_eq!(
        consensus_ci_discovery_outcome(
            ProductionCiFailureDiscoveryOutcomeV1::found(request.clone()),
            ProductionCiFailureDiscoveryOutcomeV1::found(drifted),
        ),
        ProductionCiFailureDiscoveryOutcomeV1::Ambiguous
    );
    assert_eq!(
        consensus_ci_discovery_outcome(
            ProductionCiFailureDiscoveryOutcomeV1::found(request),
            ProductionCiFailureDiscoveryOutcomeV1::Denied,
        ),
        ProductionCiFailureDiscoveryOutcomeV1::Denied
    );
}

#[test]
fn non_github_and_ambiguous_provider_records_fail_closed() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let record = &fixture.ci_provider_record;
    assert_eq!(
        select_production_ci_failure_request_v1(
            &ProviderId::new("provider.other-ci").unwrap(),
            &target(&fixture),
            &scope,
            std::slice::from_ref(&record.workflow_run),
            std::slice::from_ref(&record.workflow_job),
            std::slice::from_ref(&record.check_run),
        ),
        ProductionCiFailureDiscoveryOutcomeV1::Unavailable
    );
    assert_eq!(
        select_production_ci_failure_request_v1(
            &ProviderId::new("provider.github-actions").unwrap(),
            &target(&fixture),
            &scope,
            &[],
            &[],
            &[],
        ),
        ProductionCiFailureDiscoveryOutcomeV1::NotFound
    );
    assert_eq!(
        select_production_ci_failure_request_v1(
            &ProviderId::new("provider.github-actions").unwrap(),
            &target(&fixture),
            &scope,
            std::slice::from_ref(&record.workflow_run),
            &[],
            &[],
        ),
        ProductionCiFailureDiscoveryOutcomeV1::Unavailable
    );
    assert_eq!(
        select_production_ci_failure_request_v1(
            &ProviderId::new("provider.github-actions").unwrap(),
            &target(&fixture),
            &scope,
            &[record.workflow_run.clone(), record.workflow_run.clone()],
            &[],
            &[],
        ),
        ProductionCiFailureDiscoveryOutcomeV1::Ambiguous
    );
}

#[test]
fn workflow_job_check_run_url_is_the_exact_check_identity() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let record = &fixture.ci_provider_record;
    let mut workflow_job = record.workflow_job.clone();
    workflow_job.check_run_url =
        "https://api.github.com/repos/ScriptedAlchemy/other/check-runs/88773147767".to_owned();

    assert_eq!(
        select_production_ci_failure_request_v1(
            &ProviderId::new("provider.github-actions").unwrap(),
            &target(&fixture),
            &scope,
            std::slice::from_ref(&record.workflow_run),
            std::slice::from_ref(&workflow_job),
            std::slice::from_ref(&record.check_run),
        ),
        ProductionCiFailureDiscoveryOutcomeV1::Unavailable
    );

    workflow_job.check_run_url =
        "https://attacker.example/repos/ScriptedAlchemy/tracedecay/check-runs/88773147767"
            .to_owned();
    assert_eq!(
        select_production_ci_failure_request_v1(
            &ProviderId::new("provider.github-actions").unwrap(),
            &target(&fixture),
            &scope,
            std::slice::from_ref(&record.workflow_run),
            std::slice::from_ref(&workflow_job),
            std::slice::from_ref(&record.check_run),
        ),
        ProductionCiFailureDiscoveryOutcomeV1::Unavailable
    );

    let request = CiFailureLocalizationRequestV1 {
        scope,
        run: fixture.ci.run.clone(),
    };
    let mut stale_branch = record.clone();
    stale_branch.workflow_job.head_branch = "stale-branch".to_owned();
    assert!(
        !validate_provider_record(&target(&fixture), &request, &stale_branch),
        "provider records from a different branch must not become current"
    );
}

#[tokio::test]
async fn discovery_collects_every_bounded_page_in_order() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let first = fixture.ci_provider_record.workflow_run.clone();
    let mut second = first.clone();
    second.id += 1;
    let client = PagedDiscoveryClient {
        workflow_run_pages: vec![
            serde_json::to_vec(&serde_json::json!({
                "total_count": 2,
                "workflow_runs": [first],
            }))
            .unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "total_count": 2,
                "workflow_runs": [second],
            }))
            .unwrap(),
        ],
        requested_pages: Mutex::new(Vec::new()),
    };

    let records = collect_workflow_runs(
        &context(&scope, UtcMicros(i64::MAX)),
        &config(&fixture),
        &scope,
        &client,
    )
    .await
    .unwrap();

    assert_eq!(records.len(), 2);
    assert_eq!(*client.requested_pages.lock().unwrap(), vec![1, 2]);
}

#[tokio::test]
async fn discovery_collects_every_bounded_workflow_job_page_in_order() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let first = fixture.ci_provider_record.workflow_job.clone();
    let mut second = first.clone();
    second.id += 1;
    let client = PagedWorkflowJobDiscoveryClient {
        workflow_job_pages: vec![
            serde_json::to_vec(&serde_json::json!({
                "total_count": 2,
                "jobs": [first],
            }))
            .unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "total_count": 2,
                "jobs": [second],
            }))
            .unwrap(),
        ],
        requested_pages: Mutex::new(Vec::new()),
    };

    let records = collect_workflow_jobs(
        &context(&scope, UtcMicros(i64::MAX)),
        &config(&fixture),
        &scope,
        &client,
        fixture.ci_provider_record.workflow_run.id,
    )
    .await
    .unwrap();

    assert_eq!(records.len(), 2);
    assert_eq!(*client.requested_pages.lock().unwrap(), vec![1, 2]);
}

#[tokio::test]
async fn source_revocation_stops_before_the_next_page() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let first = fixture.ci_provider_record.workflow_run.clone();
    let source = SequencedSourceAccess::revoke_at(3);
    let config = config_with_source(&fixture, source.clone());
    let client = PagedDiscoveryClient {
        workflow_run_pages: vec![
            serde_json::to_vec(&serde_json::json!({
                "total_count": 2,
                "workflow_runs": [first.clone()],
            }))
            .unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "total_count": 2,
                "workflow_runs": [first],
            }))
            .unwrap(),
        ],
        requested_pages: Mutex::new(Vec::new()),
    };

    assert_eq!(
        collect_workflow_runs(
            &context(&scope, UtcMicros(i64::MAX)),
            &config,
            &scope,
            &client,
        )
        .await,
        Err(ProductionCiFailureDiscoveryOutcomeV1::Denied)
    );
    assert_eq!(*client.requested_pages.lock().unwrap(), vec![1]);
}

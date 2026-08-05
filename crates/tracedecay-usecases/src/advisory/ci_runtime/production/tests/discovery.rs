use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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

struct PendingRead {
    polled: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

impl Future for PendingRead {
    type Output = GitHubCiTransportOutcomeV1;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.polled.fetch_add(1, Ordering::SeqCst);
        std::task::Poll::Pending
    }
}

impl Drop for PendingRead {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn denied_context_performs_zero_ci_discovery_reads() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);

    assert_eq!(
        discover_production_ci_failure_request_v1(
            &context(&scope, UtcMicros(2)),
            &config(&fixture),
            &scope,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            &CancellationToken::new(),
        )
        .await,
        ProductionCiFailureDiscoveryOutcomeV1::Denied
    );
}

#[tokio::test]
async fn total_ci_discovery_deadline_drops_inflight_read_without_continuation() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let polled = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let read = PendingRead {
        polled: Arc::clone(&polled),
        dropped: Arc::clone(&dropped),
    };
    let deadline = UtcMicros(now_micros().0.saturating_add(10_000));
    let context =
        context(&scope, UtcMicros(i64::MAX)).with_deadline(Deadline::new(deadline).unwrap());
    let started = Instant::now();
    let outcome =
        bounded_ci_discovery_read(&context, Box::pin(read), &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        Err(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(polled.load(Ordering::SeqCst) > 0);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ci_discovery_cancellation_drops_inflight_read_without_continuation() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let polled = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let read = PendingRead {
        polled: Arc::clone(&polled),
        dropped: Arc::clone(&dropped),
    };
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let cancel_after_read = Arc::clone(&polled);
    tokio::spawn(async move {
        while cancel_after_read.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        cancel.cancel();
    });
    let outcome = bounded_ci_discovery_read(
        &context(&scope, UtcMicros(i64::MAX)),
        Box::pin(read),
        &cancellation,
    )
    .await;

    assert_eq!(
        outcome,
        Err(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)
    );
    assert!(polled.load(Ordering::SeqCst) > 0);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    tokio::task::yield_now().await;
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stale_ci_access_remains_stale_without_a_network_read() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);

    let outcome = discover_production_ci_failure_request_v1(
        &context(&scope, UtcMicros(i64::MAX)),
        &config_with_source(&fixture, Arc::new(StaleSourceAccess)),
        &scope,
        MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(outcome, ProductionCiFailureDiscoveryOutcomeV1::Stale);
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

#[tokio::test]
async fn live_archive_preserves_stale_source_access() {
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
                failure_anchor: RetrievalAnchorId::new("anchor.ci-stale-access").unwrap(),
                provider_head_commit_id: scope.head_commit_id.clone(),
                failure_kind: CiFailureKindV1::LintFailure,
                observed_at: UtcMicros(7),
            },
        })),
        target,
        source_access: Arc::new(StaleSourceAccess),
    };

    let read = archive
        .read_record(&context(&scope, UtcMicros(i64::MAX)), &request)
        .await;

    assert_eq!(read.state, CiFailureLocalizationStateV1::Stale);
    assert_eq!(read.coverage, CiFailureCoverageV1::Stale);
    assert!(read.validate_for(&request));

    use crate::advisory::{CiReadOnlyEvidenceSource, DaemonCiReadOnlyEvidenceSourceV1};
    use tracedecay_application::feedback::CiFailureLocalizationPortOutcomeV1;

    let outcome =
        DaemonCiReadOnlyEvidenceSourceV1::new(archive, UnavailableProductionCiExactEvidenceV1)
            .read_localization(&context(&scope, UtcMicros(i64::MAX)), &request)
            .await;

    assert_eq!(outcome, CiFailureLocalizationPortOutcomeV1::Stale);
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

#[test]
fn discovery_appends_every_bounded_page_in_order() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let first = fixture.ci_provider_record.workflow_run.clone();
    let mut second = first.clone();
    second.id += 1;
    let expected = [first.id, second.id];
    let mut records = Vec::new();
    let mut expected_total = None;

    assert!(
        !append_discovery_page(&mut records, &mut expected_total, 2, vec![first], |run| run
            .id)
        .unwrap()
    );
    assert!(
        append_discovery_page(&mut records, &mut expected_total, 2, vec![second], |run| {
            run.id
        })
        .unwrap()
    );
    assert_eq!(records.len(), 2);
    assert_eq!(
        records.iter().map(|run| run.id).collect::<Vec<_>>(),
        expected
    );
}

#[tokio::test]
async fn source_revocation_denies_the_next_authorization() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let source = SequencedSourceAccess::revoke_at(2);
    let config = config_with_source(&fixture, source);
    let context = context(&scope, UtcMicros(i64::MAX));
    let cancellation = CancellationToken::new();

    assert!(
        bounded_ci_source_authorization(&context, &config, &scope, &cancellation)
            .await
            .is_ok()
    );
    assert_eq!(
        bounded_ci_source_authorization(&context, &config, &scope, &cancellation).await,
        Err(ProductionCiFailureDiscoveryOutcomeV1::Denied)
    );
}

//! Strict advisory runtime acceptance over authentic provider response captures.

use std::collections::{BTreeSet, VecDeque};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay::tracedecay::TraceDecay;
#[cfg(feature = "test-transport")]
use tracedecay_application::feedback::{
    CiFailureLocalizationPort, CiFailureLocalizationPortOutcomeV1, CiFailureLocalizationRequestV1,
    GitHubReviewReadPort, GitHubReviewReadPortOutcomeV1,
};
use tracedecay_application::feedback::{FeedbackPortFuture, GitHubReviewReadRequestV1};
#[cfg(feature = "test-transport")]
use tracedecay_application::now_micros;
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::feedback::{
    FeedbackCycleTerminationV1, FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewCommentIdV1,
    GitHubReviewCurrentBranchRemapV1, GitHubReviewImmutableAnchorV1, GitHubReviewReadOperationV1,
    ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    ActorId, CommitId, ContentDigest, FileOccurrenceId, ManifestDigest, ProjectId, ProviderId,
    RefId, RepositoryId, RetrievalAnchorId, SourceSpan, UtcMicros, WorktreeId,
};
#[cfg(feature = "test-transport")]
use tracedecay_domain::{CanonicalObservationIdV1, canonical_sha256};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::advisory::ci_runtime::GitHubCiOfficialResponseDecoderV1;
#[cfg(feature = "test-transport")]
use tracedecay_usecases::advisory::ci_runtime::{
    CiCodeAnchorStoreV1, CiRetainedProviderObservationV1, CiRetainedProviderRecordV1,
    ProjectCiCodeAnchorStoreV1,
};
use tracedecay_usecases::advisory::github_runtime::{
    GitHubProviderLifecycleV1, GitHubReviewBodyReadOutcomeV1, GitHubSourceAccessAuthorityV1,
    ProjectGitHubAnchorAuthorityV1,
};
#[cfg(feature = "test-transport")]
use tracedecay_usecases::advisory::github_runtime::{
    GitHubReviewAtomicRefreshStoreV1, GitHubReviewRefreshCoordinatorV1,
    GitHubReviewRefreshOutcomeV1, GitHubReviewRefreshStateV1,
    GitHubReviewRefreshStoreCommitOutcomeV1, GitHubReviewRefreshStoreReadOutcomeV1,
};
#[cfg(feature = "test-transport")]
use tracedecay_usecases::advisory::{CiFailureLocalizationAdapter, CiReadOnlyEvidenceSource};
use tracedecay_usecases::advisory::{
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCanonicalReviewAnchorsV1, GitHubHttpReadConfigV1,
    GitHubOfficialResponseDecoderV1, GitHubReadNetworkMetadataV1, GitHubReadNetworkStatusV1,
    GitHubReadOnlyCredentialV1, GitHubReadResponseDecoderV1, GitHubRepositoryTargetV1,
    GitHubReviewAnchorSeedV1, GitHubReviewProviderIdentityV1,
};

#[cfg(feature = "test-transport")]
use tracedecay_application::feedback::{
    FeedbackCycleAdvisoryV1, FeedbackCycleControl, FeedbackCycleExecutionRequest,
    FeedbackCycleService, FeedbackDiagnosticsPort, FeedbackDiagnosticsRequest, FeedbackImpactPort,
    FeedbackImpactPortOutcome, FeedbackImpactRequest, FeedbackObservationPort,
    FeedbackRuntimeStateV1, ProximityEvaluationRequestV1, feedback_surface_operation,
};
#[cfg(feature = "test-transport")]
use tracedecay_application::{
    AdvisoryFindingContributorV1, AdvisoryFindingValidityWindowV1, DiagnosticProviderDescriptor,
    DiagnosticProviderIdentity, DiagnosticProviderIdentityParts, DiagnosticProviderResult,
    DiagnosticProviderState, PolicyDecisionRef, ProviderCoverage, ProviderDocumentIdentity,
    ProviderFreshness, ProviderOrigin, ProviderProvenance, ProviderSourceIdentity, RevisionDigest,
};
#[cfg(feature = "test-transport")]
use tracedecay_domain::configuration::{
    AuthorityRef, ScopeSourceBinding, SourceBindingId, SourceKindV1,
};
#[cfg(feature = "test-transport")]
use tracedecay_domain::feedback::{
    FeedbackAdvisoryProviderStateV1, FeedbackAuthoritativeRuntimeStateV1,
    FeedbackBaselineHorizonV1, FeedbackBaselineStateV1, FeedbackBudgetV1,
    FeedbackContentIdentityV1, FeedbackCycleId, FeedbackCycleRequestV1,
    FeedbackCycleRuntimeSnapshotV1, FeedbackDiagnosticBaselineIdentityV1,
    FeedbackDiagnosticBaselineV1, FeedbackDiagnosticProducerV1, FeedbackDiagnosticV1,
    FeedbackEvaluationInputV1, FeedbackFindingLifecycleV1, FeedbackImpactStateV1, FeedbackImpactV1,
    FeedbackTargetV1, FeedbackTriggerV1, ProximityBranchWorktreeIncompatibilityV1,
    ProximityCoverageV1, ProximityRelationStrengthV1, ProximityRiskInputsV1,
    ProximityWarningClassV1,
};
#[cfg(feature = "test-transport")]
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, CodeGenerationId,
    ComponentVersion, LocatorDigest, ObservationId, ObservationOrderingDomainV1,
    ObservationSourceRangeV1, SessionId, SymbolOccurrenceId,
};
#[cfg(feature = "test-transport")]
use tracedecay_usecases::ProjectSourceAccessSnapshot;
#[cfg(feature = "test-transport")]
use tracedecay_usecases::advisory::fixtures::AdvisoryProximityFixtureEvidenceV1;
#[cfg(feature = "test-transport")]
use tracedecay_usecases::advisory::proximity_runtime::{
    CanonicalProximityEvidenceAuthorityV1, CanonicalProximityEvidenceBatchV1,
    ProximityRuntimeOutcomeV1, ProximityRuntimeOwnerV1, ProximityThresholdPinV1,
};
#[cfg(feature = "test-transport")]
use tracedecay_usecases::feedback::concrete::open_feedback_runtime;

#[path = "advisory_runtime_acceptance/code_graph.rs"]
mod code_graph;
mod common;

use code_graph::hermetic_advisory_code_graph;
// Only the `test-transport` CI-localization tests build a hermetic CI graph.
#[cfg(feature = "test-transport")]
use code_graph::hermetic_ci_code_graph;

struct NoAnchors;

impl GitHubCanonicalReviewAnchorAuthorityV1 for NoAnchors {
    fn resolve<'a>(
        &'a self,
        _request: &'a GitHubReviewReadRequestV1,
        _seed: &'a GitHubReviewAnchorSeedV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubCanonicalReviewAnchorsV1>> {
        Box::pin(async { None })
    }
}

#[derive(Clone, Default)]
struct FixtureAnchors {
    seeds: Arc<Mutex<Vec<GitHubReviewAnchorSeedV1>>>,
}

impl GitHubCanonicalReviewAnchorAuthorityV1 for FixtureAnchors {
    fn resolve<'a>(
        &'a self,
        request: &'a GitHubReviewReadRequestV1,
        seed: &'a GitHubReviewAnchorSeedV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubCanonicalReviewAnchorsV1>> {
        Box::pin(async move {
            self.seeds.lock().ok()?.push(seed.clone());
            let original = GitHubReviewImmutableAnchorV1 {
                repository_id: request.scope.repository_id.clone(),
                commit_id: seed.original_commit_id.clone(),
                retrieval_anchor_id: RetrievalAnchorId::new("anchor.fixture.github.original")
                    .ok()?,
                file: FileOccurrenceId::new("file.fixture.github.review").ok()?,
                content_digest: ContentDigest::new(format!("sha256:{}", "c".repeat(64))).ok()?,
                span: None,
                symbol: None,
            };
            Some(GitHubCanonicalReviewAnchorsV1 {
                initial_remap: GitHubReviewCurrentBranchRemapV1::unmapped(
                    original.clone(),
                    request.scope.clone(),
                )
                .ok()?,
                original,
                author_anchor: RetrievalAnchorId::new("anchor.fixture.github.author").ok()?,
                body_anchor: RetrievalAnchorId::new("anchor.fixture.github.body").ok()?,
                safe_url_anchor: Some(RetrievalAnchorId::new("anchor.fixture.github.url").ok()?),
            })
        })
    }
}

#[cfg(feature = "test-transport")]
struct PanicCiSource;

#[cfg(feature = "test-transport")]
impl CiReadOnlyEvidenceSource for PanicCiSource {
    fn read_localization<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiFailureLocalizationPortOutcomeV1> {
        Box::pin(async { panic!("denied CI request reached provider source") })
    }
}

#[cfg(feature = "test-transport")]
struct PanicGitHubPort;

#[cfg(feature = "test-transport")]
impl GitHubReviewReadPort for PanicGitHubPort {
    fn read<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1> {
        Box::pin(async { panic!("denied GitHub request reached provider port") })
    }
}

#[cfg(feature = "test-transport")]
struct PanicGitHubStore;

#[cfg(feature = "test-transport")]
impl GitHubReviewAtomicRefreshStoreV1 for PanicGitHubStore {
    fn load<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreReadOutcomeV1> {
        Box::pin(async { panic!("denied GitHub request reached refresh store") })
    }

    fn compare_and_record<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
        _expected_revision: Option<&'a ManifestDigest>,
        _next: &'a GitHubReviewRefreshStateV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreCommitOutcomeV1> {
        Box::pin(async { panic!("denied GitHub request reached refresh store") })
    }
}

#[cfg(feature = "test-transport")]
struct PanicGitHubSourceAccess;

#[cfg(feature = "test-transport")]
impl GitHubSourceAccessAuthorityV1 for PanicGitHubSourceAccess {
    fn authorize<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, tracedecay_usecases::advisory::GitHubProviderLifecycleV1> {
        Box::pin(async { panic!("denied GitHub request reached source access authority") })
    }
}

struct SequencedGitHubSourceAccess {
    outcomes: Mutex<VecDeque<GitHubProviderLifecycleV1>>,
}

impl SequencedGitHubSourceAccess {
    fn new(outcomes: impl IntoIterator<Item = GitHubProviderLifecycleV1>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }
}

impl GitHubSourceAccessAuthorityV1 for SequencedGitHubSourceAccess {
    fn authorize<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubProviderLifecycleV1> {
        let outcome = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(GitHubProviderLifecycleV1::Unavailable);
        Box::pin(async move { outcome })
    }
}

fn captured_response(source: &str) -> Value {
    serde_json::from_str::<Value>(source).expect("capture parses")["response"].clone()
}

fn scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.advisory.runtime.capture").unwrap(),
        repository_id: RepositoryId::new("repository.advisory.runtime.capture").unwrap(),
        worktree_id: WorktreeId::new("worktree.advisory.runtime.capture").unwrap(),
        branch_ref: "refs/heads/codex/tracedecay-total-redesign-plan".to_owned(),
        head_commit_id: CommitId::new("e29900448db98ae58e90d08770a3bb8bfa710846").unwrap(),
    }
}

#[test]
fn github_source_access_uses_owner_bound_ureq_dtos() {
    let credential = GitHubReadOnlyCredentialV1::anonymous();
    let target = GitHubRepositoryTargetV1 {
        owner: "ScriptedAlchemy".to_owned(),
        repository: "tracedecay".to_owned(),
        pull_request_number: 421,
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    };
    assert!(target.validate());
    let _owner_inputs = (credential, target, GitHubHttpReadConfigV1::default());
}

#[tokio::test]
async fn authentic_github_and_ci_responses_use_production_decoders() {
    let pull_request = captured_response(include_str!(
        "../crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/pull_request.json"
    ));
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestGetPullRequest,
        scope: scope(),
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    };
    let decoder = GitHubOfficialResponseDecoderV1::new(
        GitHubReviewProviderIdentityV1 {
            provider: ProviderId::new("provider.github").unwrap(),
            repository_owner: "ScriptedAlchemy".to_owned(),
            repository_name: "tracedecay".to_owned(),
            pull_request_number: 421,
            base_commit_id: CommitId::new("8339371d01311289e2b7cd7b0669ea3549308b8c").unwrap(),
            head_commit_id: request.scope.head_commit_id.clone(),
            merge_base_commit_id: CommitId::new("8339371d01311289e2b7cd7b0669ea3549308b8c")
                .unwrap(),
        },
        NoAnchors,
    )
    .unwrap();
    let metadata = GitHubReadNetworkMetadataV1 {
        retry_at: None,
        status: GitHubReadNetworkStatusV1::Ok,
        etag: None,
        next_cursor: None,
        rate_limit: None,
    };
    assert!(
        decoder
            .decode(
                &request,
                &metadata,
                serde_json::to_vec(&pull_request).unwrap().as_slice(),
            )
            .await
            .is_some()
    );
    let review_request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
        ..request.clone()
    };
    let review = captured_response(include_str!(
        "../crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/review.json"
    ));
    assert!(
        decoder
            .decode(
                &review_request,
                &metadata,
                serde_json::to_vec(&vec![review]).unwrap().as_slice(),
            )
            .await
            .is_some()
    );

    let fixture_anchors = FixtureAnchors::default();
    let graphql_decoder = GitHubOfficialResponseDecoderV1::new(
        GitHubReviewProviderIdentityV1 {
            provider: ProviderId::new("provider.github").unwrap(),
            repository_owner: "ScriptedAlchemy".to_owned(),
            repository_name: "tracedecay".to_owned(),
            pull_request_number: 421,
            base_commit_id: CommitId::new("8339371d01311289e2b7cd7b0669ea3549308b8c").unwrap(),
            head_commit_id: request.scope.head_commit_id.clone(),
            merge_base_commit_id: CommitId::new("8339371d01311289e2b7cd7b0669ea3549308b8c")
                .unwrap(),
        },
        fixture_anchors.clone(),
    )
    .unwrap();
    let thread = captured_response(include_str!(
        "../crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/review_thread.graphql.json"
    ));
    let thread_request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
        ..request.clone()
    };
    let ingress = graphql_decoder
        .decode(
            &thread_request,
            &metadata,
            serde_json::to_vec(&thread).unwrap().as_slice(),
        )
        .await
        .expect("authentic GraphQL review thread decodes through sanitizer");
    assert_eq!(ingress.items.len(), 1);
    assert_eq!(ingress.items[0].comment_id.as_str(), "3556767423");
    assert_eq!(
        ingress.items[0].body_anchor.as_str(),
        "anchor.fixture.github.body"
    );
    let seeds = fixture_anchors.seeds.lock().unwrap();
    assert_eq!(seeds.len(), 1);
    assert_eq!(
        seeds[0].author_node_id, "chatgpt-codex-connector",
        "the production GraphQL decoder must retain the provider author identity used by canonical anchoring"
    );
    assert_eq!(
        seeds[0].body_digest.as_str(),
        "sha256:81b743cede9ff0d124beb58731d91c556343545f71c0ba51eb2b5378fdb95652"
    );
    assert_eq!(
        seeds[0].retained_body,
        thread
            .pointer("/data/repository/pullRequest/reviewThreads/nodes/0/comments/nodes/0/bodyText")
            .and_then(Value::as_str)
            .expect("authentic GraphQL review body"),
        "canonical body retention must preserve the decoded provider payload exactly"
    );

    let ci = GitHubCiOfficialResponseDecoderV1::decode(
        include_str!(
            "../crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/workflow_run.json"
        ),
        include_str!(
            "../crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/workflow_job.json"
        ),
        include_str!(
            "../crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/check_run.json"
        ),
        include_str!(
            "../crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/check_annotations.json"
        ),
    )
    .expect("authentic CI responses decode");
    assert!(ci.failed_step().is_some());
    assert!(ci.failed_annotation().is_some());
}

#[tokio::test]
async fn corrupt_provider_identity_fails_production_decoder() {
    let mut pull_request = captured_response(include_str!(
        "../crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/pull_request.json"
    ));
    pull_request["id"] = json!(0);
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestGetPullRequest,
        scope: scope(),
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    };
    let decoder = GitHubOfficialResponseDecoderV1::new(
        GitHubReviewProviderIdentityV1 {
            provider: ProviderId::new("provider.github").unwrap(),
            repository_owner: "ScriptedAlchemy".to_owned(),
            repository_name: "tracedecay".to_owned(),
            pull_request_number: 421,
            base_commit_id: CommitId::new("8339371d01311289e2b7cd7b0669ea3549308b8c").unwrap(),
            head_commit_id: request.scope.head_commit_id.clone(),
            merge_base_commit_id: CommitId::new("8339371d01311289e2b7cd7b0669ea3549308b8c")
                .unwrap(),
        },
        NoAnchors,
    )
    .unwrap();
    assert!(
        decoder
            .decode(
                &request,
                &GitHubReadNetworkMetadataV1 {
                    retry_at: None,
                    status: GitHubReadNetworkStatusV1::Ok,
                    etag: None,
                    next_cursor: None,
                    rate_limit: None,
                },
                serde_json::to_vec(&pull_request).unwrap().as_slice(),
            )
            .await
            .is_none()
    );
}

#[cfg(feature = "test-transport")]
fn proximity_context(scope: &FeedbackScopeV1, now: UtcMicros) -> RequestContext {
    let resolved = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).unwrap()),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.advisory.proximity").unwrap(),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        ActorId::new("actor.advisory.proximity.issuer").unwrap(),
        UtcMicros(now.0.saturating_sub(1_000_000)),
        UtcMicros(now.0.saturating_add(60_000_000)),
        resolved.clone(),
        BTreeSet::from([CapabilityId::new("capability.application.feedback.proximity").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.application.feedback.proximity").unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        ActorId::new("actor.advisory.proximity").unwrap(),
        resolved,
        grant,
        RequestId::new("request.advisory.proximity").unwrap(),
        Deadline::new(UtcMicros(now.0.saturating_add(30_000_000))).unwrap(),
        CancellationContext::active("cancel.advisory.proximity").unwrap(),
    )
    .unwrap()
}

#[cfg(feature = "test-transport")]
fn ci_context(scope: &FeedbackScopeV1, now: UtcMicros) -> RequestContext {
    let resolved = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).unwrap()),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.advisory.ci").unwrap(),
        1,
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        ActorId::new("actor.advisory.ci.issuer").unwrap(),
        UtcMicros(now.0.saturating_sub(1_000_000)),
        UtcMicros(now.0.saturating_add(60_000_000)),
        resolved.clone(),
        BTreeSet::from([
            CapabilityId::new("capability.application.feedback.ci-failure-localize").unwrap(),
        ]),
        BTreeSet::from([
            UseCaseId::new("use-case.application.feedback.ci-failure-localize").unwrap(),
        ]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        ActorId::new("actor.advisory.ci").unwrap(),
        resolved,
        grant,
        RequestId::new("request.advisory.ci").unwrap(),
        Deadline::new(UtcMicros(now.0.saturating_add(30_000_000))).unwrap(),
        CancellationContext::active("cancel.advisory.ci").unwrap(),
    )
    .unwrap()
}

fn github_context(scope: &FeedbackScopeV1, project_id: ProjectId) -> RequestContext {
    let resolved = ResolvedScope::new(
        project_id,
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).unwrap()),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.github.body").unwrap(),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        ActorId::new("actor.github.body.issuer").unwrap(),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        resolved.clone(),
        BTreeSet::from([
            CapabilityId::new("capability.application.feedback.github-review-ingest").unwrap(),
        ]),
        BTreeSet::from([
            UseCaseId::new("use-case.application.feedback.github-review-ingest").unwrap(),
        ]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        ActorId::new("actor.github.body").unwrap(),
        resolved,
        grant,
        RequestId::new("request.github.body").unwrap(),
        Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
        CancellationContext::active("cancel.github.body").unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn retained_review_body_expansion_rechecks_exact_scope_and_source_access() {
    let (_environment, project) = common::IsolatedEnv::acquire().await;
    std::fs::create_dir_all(project.join("src")).unwrap();
    let source = "pub fn reviewed() {}\npub fn batched() {}\n";
    std::fs::write(project.join("src/lib.rs"), source).unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "tests@example.invalid"],
        vec!["config", "user.name", "TraceDecay Tests"],
        vec!["add", "src/lib.rs"],
        vec!["commit", "-m", "test: seed reviewed source"],
    ] {
        let output = Command::new("git")
            .args(&args)
            .current_dir(&project)
            .output()
            .expect("git command runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&project)
        .output()
        .expect("read fixture head");
    assert!(output.status.success());
    let head = CommitId::new(String::from_utf8(output.stdout).unwrap().trim()).unwrap();
    let graph = TraceDecay::init(&project).await.unwrap();
    let database = graph.db().clone();
    let scope = FeedbackScopeV1 {
        project_id: ProjectId::new("project.github.body").unwrap(),
        repository_id: RepositoryId::new("repository.github.body").unwrap(),
        worktree_id: WorktreeId::new("worktree.github.body").unwrap(),
        branch_ref: "refs/heads/github-body".to_owned(),
        head_commit_id: head.clone(),
    };
    let code_graph = hermetic_advisory_code_graph(&scope, &project, "src/lib.rs");
    let authority =
        ProjectGitHubAnchorAuthorityV1::new(database, &project, scope.clone(), code_graph).unwrap();
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestListPullRequestReviewComments,
        scope: scope.clone(),
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    };
    let fixture: Value = serde_json::from_str(include_str!(
        "../crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/review_comment.json"
    ))
    .unwrap();
    let body = fixture.pointer("/response/body").unwrap().as_str().unwrap();
    let provider_body_digest =
        ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(body)))).unwrap();
    let retained_body = tracedecay::privacy::sanitize_provider_metadata_text(body).unwrap();
    let seed = GitHubReviewAnchorSeedV1 {
        comment_id: GitHubReviewCommentIdV1::new("3556767423").unwrap(),
        author_node_id: "BOT_kgDOC98s_g".to_owned(),
        body_digest: provider_body_digest.clone(),
        retained_body: retained_body.clone(),
        safe_url: "https://github.com/ScriptedAlchemy/tracedecay/pull/421#discussion_r3556767423"
            .to_owned(),
        path: "src/lib.rs".to_owned(),
        original_commit_id: head.clone(),
        observed_commit_id: head.clone(),
        original_start_line: Some(1),
        original_line: Some(1),
        current_start_line: Some(1),
        current_line: Some(1),
    };
    let second_seed = GitHubReviewAnchorSeedV1 {
        comment_id: GitHubReviewCommentIdV1::new("3556767424").unwrap(),
        author_node_id: "BOT_kgDOC98s_g".to_owned(),
        body_digest: provider_body_digest.clone(),
        retained_body: retained_body.clone(),
        safe_url: "https://github.com/ScriptedAlchemy/tracedecay/pull/421#discussion_r3556767424"
            .to_owned(),
        path: "src/lib.rs".to_owned(),
        original_commit_id: head.clone(),
        observed_commit_id: head,
        original_start_line: Some(2),
        original_line: Some(2),
        current_start_line: Some(2),
        current_line: Some(2),
    };
    let batch = authority
        .resolve_many(&request, &[seed, second_seed])
        .await
        .expect("canonical body anchors");
    assert_eq!(batch.len(), 2);
    assert_eq!(
        batch[0].original.span,
        Some(SourceSpan {
            start_byte: 0,
            end_byte: "pub fn reviewed() {}\n".len() as u64,
        })
    );
    assert_eq!(
        batch[1].original.span,
        Some(SourceSpan {
            start_byte: "pub fn reviewed() {}\n".len() as u64,
            end_byte: source.len() as u64,
        })
    );
    let unavailable_seed = GitHubReviewAnchorSeedV1 {
        comment_id: GitHubReviewCommentIdV1::new("3556767425").unwrap(),
        author_node_id: "BOT_kgDOC98s_g".to_owned(),
        body_digest: provider_body_digest.clone(),
        retained_body: retained_body.clone(),
        safe_url: "https://github.com/ScriptedAlchemy/tracedecay/pull/421#discussion_r3556767425"
            .to_owned(),
        path: "src/missing.rs".to_owned(),
        original_commit_id: scope.head_commit_id.clone(),
        observed_commit_id: scope.head_commit_id.clone(),
        original_start_line: Some(1),
        original_line: Some(1),
        current_start_line: Some(1),
        current_line: Some(1),
    };
    assert!(
        authority
            .resolve_many(&request, &[unavailable_seed])
            .await
            .is_none()
    );
    let anchors = batch[0].clone();
    let context = github_context(&scope, scope.project_id.clone());
    let access = SequencedGitHubSourceAccess::new([
        GitHubProviderLifecycleV1::Ready,
        GitHubProviderLifecycleV1::Ready,
    ]);
    let GitHubReviewBodyReadOutcomeV1::Current(evidence) = authority
        .read_body(&context, &request, &anchors.body_anchor, &access)
        .await
    else {
        panic!("authorized body evidence must expand");
    };
    assert_eq!(evidence.body(), retained_body);
    assert_eq!(evidence.provider_body_digest, provider_body_digest);

    let revoked = SequencedGitHubSourceAccess::new([
        GitHubProviderLifecycleV1::Ready,
        GitHubProviderLifecycleV1::Denied,
    ]);
    assert!(matches!(
        authority
            .read_body(&context, &request, &anchors.body_anchor, &revoked)
            .await,
        GitHubReviewBodyReadOutcomeV1::Denied
    ));
    let wrong_project =
        github_context(&scope, ProjectId::new("project.github.body.other").unwrap());
    let never_called = SequencedGitHubSourceAccess::new([]);
    assert!(matches!(
        authority
            .read_body(
                &wrong_project,
                &request,
                &anchors.body_anchor,
                &never_called,
            )
            .await,
        GitHubReviewBodyReadOutcomeV1::Denied
    ));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn unauthorized_ci_request_is_denied_before_provider_read() {
    let fixture =
        tracedecay_usecases::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1()
            .unwrap();
    let scope = scope();
    let request = CiFailureLocalizationRequestV1 {
        scope: scope.clone(),
        run: fixture.ci.run,
    };
    let context = proximity_context(&scope, now_micros());
    let adapter = CiFailureLocalizationAdapter::new(PanicCiSource);

    assert!(matches!(
        adapter.localize(&context, &request).await,
        CiFailureLocalizationPortOutcomeV1::Denied
    ));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn ci_localization_resolves_generation_symbol_callers_and_tests_from_canonical_graph() {
    let (_environment, project) = common::IsolatedEnv::acquire().await;
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        concat!(
            "pub fn caller() { failed_symbol(); }\n",
            "pub fn failed_symbol() {}\n",
            "#[test]\n",
            "fn failed_symbol_test() { failed_symbol(); }\n",
        ),
    )
    .unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "tests@example.invalid"],
        vec!["config", "user.name", "TraceDecay Tests"],
        vec!["add", "."],
        vec!["commit", "-m", "seed canonical graph"],
    ] {
        let output = Command::new("git")
            .args(&args)
            .current_dir(&project)
            .output()
            .expect("git command runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&project)
        .output()
        .expect("read fixture head");
    assert!(head.status.success());
    let head = String::from_utf8(head.stdout).unwrap().trim().to_owned();

    let mut scope = scope();
    scope.head_commit_id = CommitId::new(head).unwrap();
    let code_graph = hermetic_ci_code_graph(&scope, &project);
    let mut provider_record =
        tracedecay_usecases::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1()
            .unwrap()
            .ci_provider_record;
    provider_record.workflow_run.head_sha = scope.head_commit_id.as_str().to_owned();
    provider_record.workflow_job.head_sha = scope.head_commit_id.as_str().to_owned();
    provider_record.check_run.head_sha = scope.head_commit_id.as_str().to_owned();
    let annotation = provider_record
        .annotations
        .first_mut()
        .expect("failure annotation");
    annotation.path = "src/lib.rs".to_owned();
    annotation.start_line = 2;
    annotation.end_line = 2;
    annotation.start_column = Some(1);
    annotation.end_column = None;
    let request = CiFailureLocalizationRequestV1 {
        scope: scope.clone(),
        run: provider_record.run_identity(),
    };
    let retained = CiRetainedProviderRecordV1 {
        provider_record,
        observation: CiRetainedProviderObservationV1 {
            observation_id: CanonicalObservationIdV1::new(
                canonical_sha256(&"observation.advisory.ci.graph")
                    .unwrap()
                    .as_str()
                    .to_owned(),
            )
            .unwrap(),
            failure_anchor: RetrievalAnchorId::new("anchor.advisory.ci.graph").unwrap(),
            provider_head_commit_id: scope.head_commit_id.clone(),
            failure_kind: tracedecay_domain::feedback::CiFailureKindV1::TestFailure,
            observed_at: now_micros(),
        },
    };
    let store = ProjectCiCodeAnchorStoreV1::new(project.clone(), scope.clone(), code_graph.clone())
        .unwrap();
    let evidence = store
        .resolve(&ci_context(&scope, now_micros()), &request, &retained)
        .await
        .expect("canonical graph evidence");

    assert_eq!(
        evidence.state,
        tracedecay_domain::feedback::CiFailureLocalizationStateV1::Complete
    );
    assert_eq!(
        evidence.coverage,
        tracedecay_domain::feedback::CiFailureCoverageV1::Complete
    );
    assert!(evidence.generation.is_some());
    assert_eq!(
        evidence.symbol.as_ref().map(|symbol| symbol.file.as_str()),
        Some("src/lib.rs")
    );
    assert!(!evidence.callers.is_empty());
    assert!(!evidence.tests.is_empty());

    let mut stale_scope = scope;
    stale_scope.head_commit_id = CommitId::new("0000000000000000000000000000000000000000").unwrap();
    let stale_request = CiFailureLocalizationRequestV1 {
        scope: stale_scope.clone(),
        run: retained.provider_record.run_identity(),
    };
    let mut stale_record = retained;
    stale_record.observation.provider_head_commit_id = stale_scope.head_commit_id.clone();
    stale_record.provider_record.workflow_run.head_sha =
        stale_scope.head_commit_id.as_str().to_owned();
    stale_record.provider_record.workflow_job.head_sha =
        stale_scope.head_commit_id.as_str().to_owned();
    stale_record.provider_record.check_run.head_sha =
        stale_scope.head_commit_id.as_str().to_owned();
    let stale = ProjectCiCodeAnchorStoreV1::new(project, stale_scope.clone(), code_graph)
        .unwrap()
        .resolve(
            &ci_context(&stale_scope, now_micros()),
            &stale_request,
            &stale_record,
        )
        .await
        .expect("typed partial evidence");
    assert_eq!(
        stale.state,
        tracedecay_domain::feedback::CiFailureLocalizationStateV1::Partial
    );
    assert_eq!(
        stale.coverage,
        tracedecay_domain::feedback::CiFailureCoverageV1::Partial
    );
    assert!(stale.generation.is_none());
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn unauthorized_github_refresh_is_denied_before_port_or_store_access() {
    let fixture =
        tracedecay_usecases::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1()
            .unwrap();
    let scope = scope();
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
        scope: scope.clone(),
        pull_request_id: fixture.github.pull_request_id,
    };
    let context = proximity_context(&scope, now_micros());
    let coordinator = GitHubReviewRefreshCoordinatorV1::new(
        PanicGitHubPort,
        PanicGitHubStore,
        PanicGitHubSourceAccess,
    );

    assert_eq!(
        coordinator.refresh(&context, &request).await,
        GitHubReviewRefreshOutcomeV1::Denied
    );
}

/// Authentic Cursor and Codex hook packets must cross the packaged CLI,
/// daemon-owned Hook V2 admission/delivery ports, and the registered advisory
/// advisory owner. A committed ingest alone is insufficient: the same process
/// must then return a non-vacuous typed four-pillar terminal cycle.
#[tokio::test]
async fn packaged_host_ingest_delivers_a_registered_advisory_cycle() {
    let (environment, project) = common::IsolatedEnv::acquire().await;
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn shared_edit() {}\n").unwrap();
    // The advisory surface is scoped to a branch and head commit, so an
    // uncommitted directory has no canonical feedback scope for the daemon to
    // mount and the advisory cycle below would be legitimately unavailable.
    for args in [
        vec!["init"],
        vec!["config", "user.email", "tests@example.invalid"],
        vec!["config", "user.name", "TraceDecay Tests"],
        vec!["add", "."],
        vec!["commit", "-m", "seed canonical graph"],
    ] {
        let output = Command::new("git")
            .args(&args)
            .current_dir(&project)
            .output()
            .expect("git command runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let init = common::tracedecay_command_with_home(environment.home())
        .arg("init")
        .current_dir(&project)
        .output()
        .expect("initialize production project");
    assert!(
        init.status.success(),
        "tracedecay init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let _daemon = common::spawn_tracedecay_daemon(environment.home());
    let transcript = project.join("cursor-proximity.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Edit the shared file.\"}]}}\n",
            "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Saved src/lib.rs.\"}]}}\n"
        ),
    )
    .unwrap();
    let mut event: Value = serde_json::from_str(include_str!(
        "../crates/tracedecay-hooks/fixtures/host_events/cursor/after-file-edit.json"
    ))
    .unwrap();
    event["conversation_id"] = json!("conversation-advisory-proximity");
    event["generation_id"] = json!("generation-advisory-proximity");
    event["model"] = json!("cursor-fixture");
    event["file_path"] = json!(project.join("src/lib.rs"));
    event["edits"] = json!([{"old_string": "", "new_string": "pub fn shared_edit() {}"}]);
    event["session_id"] = json!("session-advisory-proximity");
    event["cursor_version"] = json!("fixture");
    event["workspace_roots"] = json!([project.clone()]);
    event["user_email"] = json!("redacted@example.invalid");
    event["transcript_path"] = json!(transcript);
    let project_arg = project.to_string_lossy().into_owned();
    let args = json!({
        "action": "ingest_transcript",
        "provider": "cursor",
        "user_scope": false,
        "event_json": event.to_string(),
        "format": "json",
    })
    .to_string();
    // The first tool call triggers the daemon's cold project open, and the
    // hook tool surface deliberately returns the typed warming state instead
    // of retrying internally. Waiting that documented retryable state out is
    // the client protocol, so only a non-warming failure is a test failure.
    let ingest_deadline = std::time::Instant::now() + Duration::from_secs(60);
    let output = loop {
        let output = common::tracedecay_command_with_home(environment.home())
            .args([
                "tool",
                "--project",
                project_arg.as_str(),
                "tracedecay_hook_runtime",
                "--args",
                args.as_str(),
                "--json",
            ])
            .current_dir(&project)
            .output()
            .expect("invoke registered daemon observation path");
        if output.status.success() {
            break output;
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            stderr.contains("is warming in the background"),
            "registered daemon ingest failed\nstdout:\n{}\nstderr:\n{stderr}",
            String::from_utf8_lossy(&output.stdout),
        );
        assert!(
            std::time::Instant::now() < ingest_deadline,
            "registered daemon project stayed warming past the ingest deadline\nstderr:\n{stderr}",
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let response: Value =
        serde_json::from_slice(&output.stdout).expect("registered daemon ingest response");
    let payload: Value = serde_json::from_str(
        response["content"][0]["text"]
            .as_str()
            .expect("registered daemon ingest response text"),
    )
    .expect("registered daemon ingest payload");
    assert_eq!(
        payload["status"], "committed",
        "registered daemon ingest did not commit: {response}"
    );

    // Codex records a turn in its rollout, not in the Stop event, so the
    // project-scoped Stop ingest below has to find a rollout whose `cwd` is
    // this project — exactly the shape the daemon's project scheduler admits.
    let codex_sessions = environment.home().join(".codex/sessions");
    std::fs::create_dir_all(&codex_sessions).unwrap();
    let mut codex_meta: Value = serde_json::from_str(include_str!(
        "fixtures/provider_normalization/codex/session_meta.input.json"
    ))
    .unwrap();
    codex_meta["payload"]["id"] = json!("session-advisory-proximity");
    codex_meta["payload"]["cwd"] = json!(project.clone());
    let codex_message: Value = serde_json::from_str(include_str!(
        "fixtures/provider_normalization/codex/agent_message.input.json"
    ))
    .unwrap();
    std::fs::write(
        codex_sessions.join("rollout-advisory-proximity.jsonl"),
        format!("{codex_meta}\n{codex_message}\n"),
    )
    .unwrap();

    let mut stop: Value = serde_json::from_str(include_str!(
        "../crates/tracedecay-hooks/fixtures/host_events/codex/stop.json"
    ))
    .unwrap();
    stop["session_id"] = json!("session-advisory-proximity");
    stop["turn_id"] = json!("turn-advisory-proximity-stop");
    stop["cwd"] = json!(project.clone());
    stop["model"] = json!("codex-fixture");
    stop["permission_mode"] = json!("default");
    stop["last_assistant_message"] = json!("Saved src/lib.rs.");
    let stop_args = json!({
        "action": "ingest_transcript",
        "provider": "codex",
        "user_scope": false,
        "event_json": stop.to_string(),
        "format": "json",
    })
    .to_string();
    let stop_output = common::tracedecay_command_with_home(environment.home())
        .args([
            "tool",
            "--project",
            project_arg.as_str(),
            "tracedecay_hook_runtime",
            "--args",
            stop_args.as_str(),
            "--json",
        ])
        .current_dir(&project)
        .output()
        .expect("invoke registered daemon stop path");
    assert!(
        stop_output.status.success(),
        "registered daemon stop ingest failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&stop_output.stdout),
        String::from_utf8_lossy(&stop_output.stderr)
    );
    let stop_response: Value =
        serde_json::from_slice(&stop_output.stdout).expect("registered daemon stop response");
    let stop_payload: Value = serde_json::from_str(
        stop_response["content"][0]["text"]
            .as_str()
            .expect("registered daemon stop response text"),
    )
    .expect("registered daemon stop payload");
    assert_eq!(
        stop_payload["status"], "committed",
        "registered daemon stop ingest did not commit: {stop_response}"
    );

    let advisory_args = json!({
        "document_uri": format!("file://{}", project.join("src/lib.rs").display()),
    })
    .to_string();
    // Feedback/advisory registration is a deferred background upgrade keyed
    // to the first published code-index generation, and the tool reports that
    // window as a typed retryable unavailable state. Retrying it out is the
    // client protocol; only a non-retryable failure is a test failure.
    let advisory_deadline = std::time::Instant::now() + Duration::from_secs(60);
    let advisory = loop {
        let advisory = common::tracedecay_command_with_home(environment.home())
            .args([
                "tool",
                "--project",
                project_arg.as_str(),
                "tracedecay_feedback_advisory_cycle",
                "--args",
                advisory_args.as_str(),
                "--json",
            ])
            .current_dir(&project)
            .output()
            .expect("invoke registered four-pillar advisory path");
        if advisory.status.success() {
            break advisory;
        }
        let stdout = String::from_utf8_lossy(&advisory.stdout).into_owned();
        let retryable_unavailable =
            serde_json::from_str::<Value>(&stdout)
                .ok()
                .is_some_and(|response| {
                    response["problem"]["code"] == "feedback.advisory-cycle.unavailable"
                        && response["problem"]["retryable"] == true
                });
        assert!(
            retryable_unavailable,
            "registered advisory cycle failed\nstdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&advisory.stderr),
        );
        assert!(
            std::time::Instant::now() < advisory_deadline,
            "advisory cycle stayed unavailable past the deferred registration deadline\nstdout:\n{stdout}",
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let advisory: Value = serde_json::from_slice(&advisory.stdout).unwrap();
    assert_four_pillar_terminal_cycle(&advisory);
}

/// Asserts the four-pillar terminal state Plan 37 requires of one advisory
/// cycle. `producer_contributions` is a fixed literal array in the daemon
/// payload, so searching the serialized response for pillar names proves
/// nothing; the terminal cycle object is the only place the pillars report
/// real state.
fn assert_four_pillar_terminal_cycle(advisory: &Value) {
    let cycle = find_advisory_cycle(advisory).unwrap_or_else(|| {
        panic!("advisory result carried no four-pillar terminal cycle object: {advisory}")
    });

    // Decoding through the domain enums keeps this gate bound to the real
    // closed state sets instead of a copy that can silently go stale.
    let termination: FeedbackCycleTerminationV1 =
        serde_json::from_value(cycle["termination"].clone()).unwrap_or_else(|error| {
            panic!("cycle termination is not a typed terminal state ({error}): {cycle}")
        });
    let provider_states: Vec<ProviderEvaluationStateV1> =
        serde_json::from_value(cycle["provider_states"].clone()).unwrap_or_else(|error| {
            panic!("cycle provider_states are not typed provider states ({error}): {cycle}")
        });
    assert!(
        !provider_states.is_empty(),
        "a terminated cycle always reports the state of every evaluated provider: {cycle}"
    );

    // The acceptance project has no GitHub or CI provider access, so those
    // pillars are genuinely unavailable. Plan 37 requires that to stay visible
    // per pillar rather than collapsing into one clean, empty answer.
    let incomplete = provider_states
        .iter()
        .filter(|state| **state != ProviderEvaluationStateV1::SupportedCompletedComplete)
        .count();
    assert!(
        incomplete > 0,
        "unreachable GitHub and CI pillars must report their own state, not complete coverage: {cycle}"
    );
    assert_ne!(
        termination,
        FeedbackCycleTerminationV1::Clean,
        "a cycle with {incomplete} non-complete provider states can never terminate clean: {cycle}"
    );

    let published = cycle["published"]
        .as_bool()
        .unwrap_or_else(|| panic!("cycle published is not a boolean: {cycle}"));
    if termination == FeedbackCycleTerminationV1::IncompleteCoverage {
        assert!(
            !published,
            "an incomplete-coverage cycle is not publishable: {cycle}"
        );
    }
}

/// The gate above only runs after a daemon round trip, so prove here that it
/// actually rejects every way a four-pillar result can lie. Without this the
/// gate could silently rot back into the substring check it replaced.
#[test]
fn four_pillar_gate_rejects_collapsed_or_untyped_cycle_states() {
    // MCP delivers the evidence document as JSON text inside the envelope.
    let envelope = |cycle: Value| json!({"content": [{"text": cycle.to_string()}]});
    let contributions = json!([
        "github_review_ingest",
        "ci_failure_localize",
        "feedback_proximity"
    ]);

    assert_four_pillar_terminal_cycle(&envelope(json!({
        "request_handle": "rh.fixture",
        "cycle": {
            "termination": "incomplete_coverage",
            "provider_states": ["unavailable", "unavailable", "supported_completed_complete"],
            "published": false,
        },
        "producer_contributions": contributions.clone(),
    })));

    // Every case below is expected to panic, so keep the default hook from
    // printing a backtrace per rejection.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcomes = [
        (
            "a payload carrying only the fixed contribution list",
            json!({ "producer_contributions": contributions }),
        ),
        (
            "a clean termination beside an unavailable pillar",
            json!({"cycle": {
                "termination": "clean",
                "provider_states": ["unavailable", "supported_completed_complete"],
                "published": true,
            }}),
        ),
        (
            "pillar states collapsed into complete coverage",
            json!({"cycle": {
                "termination": "clean",
                "provider_states": ["supported_completed_complete"],
                "published": true,
            }}),
        ),
        (
            "an incomplete-coverage cycle claiming publication",
            json!({"cycle": {
                "termination": "incomplete_coverage",
                "provider_states": ["unavailable"],
                "published": true,
            }}),
        ),
        (
            "an untyped termination",
            json!({"cycle": {
                "termination": "mostly_fine",
                "provider_states": ["unavailable"],
                "published": false,
            }}),
        ),
        (
            "an untyped provider state",
            json!({"cycle": {
                "termination": "blocked",
                "provider_states": ["mostly_fine"],
                "published": false,
            }}),
        ),
    ]
    .map(|(reason, payload)| {
        let envelope = envelope(payload);
        let rejected =
            std::panic::catch_unwind(|| assert_four_pillar_terminal_cycle(&envelope)).is_err();
        (reason, rejected)
    });
    std::panic::set_hook(previous_hook);

    for (reason, rejected) in outcomes {
        assert!(rejected, "the four-pillar gate accepted {reason}");
    }
}

/// The advisory payload is wrapped by the MCP envelope and the application
/// evidence contract, so locate the terminal cycle by its exact typed shape
/// rather than pinning a transport path that neither plan owns. MCP tool
/// content arrives as a JSON document inside a string field, so nested text is
/// parsed and searched too.
fn find_advisory_cycle(value: &Value) -> Option<Value> {
    match value {
        Value::Object(fields) => {
            if fields.contains_key("termination")
                && fields.contains_key("provider_states")
                && fields.contains_key("published")
            {
                return Some(value.clone());
            }
            fields.values().find_map(find_advisory_cycle)
        }
        Value::Array(items) => items.iter().find_map(find_advisory_cycle),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .as_ref()
            .and_then(find_advisory_cycle),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Plan 37's positive four-pillar scenario.
//
// `packaged_host_ingest_delivers_a_registered_advisory_cycle` above proves the
// truthful *negative*: a hermetic project has no GitHub/CI reachability, so
// those pillars report their own unavailable state. The plan additionally
// requires one scenario where a single saved-edit/stop boundary returns
// post-edit diagnostics/impact, a localized CI failure, an existing GitHub
// review finding, and a concurrent-agent proximity warning *together*, in one
// cycle result.
//
// The remote halves cannot come from the network in a hermetic test, and the
// plan rejects synthetic lookalike providers as acceptance evidence. So both
// remote pillars are replayed from the suite's checked-in recorded provider
// captures (`crates/tracedecay-usecases/src/advisory/fixtures/
// provider_branch_review/`, the same captures the decoder tests above consume)
// through the shipped decoders, sanitizer, and canonical anchor authorities.
// Only immutable *identity* (head commit, reviewed path and lines) is
// retargeted onto this test's real repository — the same retargeting
// `ci_localization_resolves_generation_symbol_callers_and_tests_from_canonical_graph`
// already performs — so the recorded protocol shape, bodies, digests, and
// lifecycle flags stay exactly as captured.
// ---------------------------------------------------------------------------

/// Recorded proximity evidence mounted behind the production provider seam.
///
/// The evidence is admitted by the composite fixture's own gate, which requires
/// at least two of the *recorded* concurrent sessions and a resolved agent
/// identity per observation. Tier, inclusion, coverage, contribution identity,
/// and the projected warning are all computed by the shipped proximity owner.
#[cfg(feature = "test-transport")]
struct RecordedProximityEvidenceAuthority {
    batch: CanonicalProximityEvidenceBatchV1,
}

#[cfg(feature = "test-transport")]
impl CanonicalProximityEvidenceAuthorityV1 for RecordedProximityEvidenceAuthority {
    fn current_evidence<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a ProximityEvaluationRequestV1,
    ) -> FeedbackPortFuture<'a, Option<CanonicalProximityEvidenceBatchV1>> {
        let batch = self.batch.clone();
        Box::pin(async move { Some(batch) })
    }
}

/// Diagnostics for the saved generation under evaluation. Classification,
/// finding identity, and anchoring stay with the production cycle service.
#[cfg(feature = "test-transport")]
struct SavedGenerationDiagnostics {
    results: Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>,
    baselines: Vec<FeedbackDiagnosticBaselineV1>,
}

#[cfg(feature = "test-transport")]
impl FeedbackDiagnosticsPort for SavedGenerationDiagnostics {
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

/// Impact projected from the canonical-graph evidence the production CI
/// localization store resolved for this generation, so the impact pillar is
/// graph-derived rather than invented.
#[cfg(feature = "test-transport")]
struct GraphDerivedImpact(FeedbackImpactV1);

#[cfg(feature = "test-transport")]
impl FeedbackImpactPort for GraphDerivedImpact {
    fn impact<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a FeedbackImpactRequest,
    ) -> FeedbackPortFuture<'a, FeedbackImpactPortOutcome> {
        let impact = self.0.clone();
        Box::pin(async move { FeedbackImpactPortOutcome::Complete(impact) })
    }
}

#[cfg(feature = "test-transport")]
struct SharedFeedbackObservations(Arc<dyn FeedbackObservationPort + Send + Sync>);

#[cfg(feature = "test-transport")]
impl FeedbackObservationPort for SharedFeedbackObservations {
    fn observe(
        &self,
        input: &tracedecay_domain::feedback::FeedbackEvaluationInputV1,
        observation: tracedecay_domain::feedback::FeedbackCycleObservationV1,
    ) {
        self.0.observe(input, observation);
    }
}

#[cfg(feature = "test-transport")]
fn four_pillar_digest(fill: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).expect("digest")
}

/// One grant covering every pillar of the same cycle. Production issues exactly
/// one request context per cycle, so the pillars below must all admit against
/// this single grant rather than one bespoke context each.
#[cfg(feature = "test-transport")]
fn four_pillar_context(
    resolved: &ResolvedScope,
    requester: &ActorId,
    operation: &tracedecay_application::ApplicationOperation,
    now: UtcMicros,
) -> RequestContext {
    let capabilities = BTreeSet::from([
        operation.capability_id().clone(),
        CapabilityId::new(
            tracedecay_application::feedback::CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1.to_owned(),
        )
        .expect("ci capability"),
        CapabilityId::new(
            tracedecay_application::feedback::GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1.to_owned(),
        )
        .expect("github capability"),
        CapabilityId::new(tracedecay_application::feedback::PROXIMITY_CAPABILITY_ID_V1.to_owned())
            .expect("proximity capability"),
    ]);
    let use_cases = BTreeSet::from([
        operation.use_case_id().clone(),
        UseCaseId::new(
            tracedecay_application::feedback::CI_FAILURE_LOCALIZE_USE_CASE_ID_V1.to_owned(),
        )
        .expect("ci use case"),
        UseCaseId::new(
            tracedecay_application::feedback::GITHUB_REVIEW_INGEST_USE_CASE_ID_V1.to_owned(),
        )
        .expect("github use case"),
        UseCaseId::new(tracedecay_application::feedback::PROXIMITY_USE_CASE_ID_V1.to_owned())
            .expect("proximity use case"),
    ]);
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.advisory.four-pillar").expect("grant id"),
        1,
        four_pillar_digest('a'),
        ActorId::new("actor.advisory.four-pillar.issuer").expect("issuer"),
        UtcMicros(now.0.saturating_sub(60_000_000)),
        UtcMicros(now.0.saturating_add(600_000_000)),
        resolved.clone(),
        capabilities,
        use_cases,
        DisclosureClass::Evidence,
    )
    .expect("four-pillar grant");
    RequestContext::new(
        requester.clone(),
        resolved.clone(),
        grant,
        RequestId::new("request.advisory.four-pillar").expect("request id"),
        Deadline::new(UtcMicros(now.0.saturating_add(300_000_000))).expect("deadline"),
        CancellationContext::active("cancel.advisory.four-pillar").expect("cancellation"),
    )
    .expect("four-pillar request context")
}

/// One canonical observation per recorded peer session. Session identity comes
/// from the checked-in `proximity_sessions.json` capture; the fixture's own
/// admission gate rejects anything that is not at least two of those recorded
/// sessions with resolved agent identity.
#[cfg(feature = "test-transport")]
fn recorded_peer_observation(session: &SessionId, ordinal: u64) -> CanonicalObservationEnvelopeV1 {
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new("provider.cursor").expect("provider"),
        "message",
        ObservationId::new(format!("observation.advisory.proximity.{ordinal}"))
            .expect("observation id"),
        CanonicalObservationRelationsV1::new(session.clone())
            .with_agent_id(ObservationId::new(format!("agent.advisory.peer.{ordinal}")).unwrap()),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({ "text": "Editing the shared symbol." }),
            model: None,
            timestamp: None,
        }],
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::SqliteRowId,
            ObservationSourceRangeV1::new(ordinal, ordinal.saturating_add(1))
                .expect("observation range"),
        ),
    )
    .expect("canonical peer observation")
}

/// A single saved-edit/stop boundary returns all four Plan 37 pillars in one
/// canonical cycle result, and provider outcome stays orthogonal to per-finding
/// lifecycle: the GitHub pillar is a *complete* provider read whose recorded
/// thread is `isOutdated`, so it contributes a `Superseded` finding without
/// degrading its own provider state.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn one_saved_edit_cycle_returns_all_four_advisory_pillars_together() {
    let (_environment, project) = common::IsolatedEnv::acquire().await;
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        concat!(
            "pub fn caller() { failed_symbol(); }\n",
            "pub fn failed_symbol() {}\n",
            "#[test]\n",
            "fn failed_symbol_test() { failed_symbol(); }\n",
        ),
    )
    .unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "tests@example.invalid"],
        vec!["config", "user.name", "TraceDecay Tests"],
        vec!["add", "."],
        vec!["commit", "-m", "seed four-pillar canonical graph"],
    ] {
        let output = Command::new("git")
            .args(&args)
            .current_dir(&project)
            .output()
            .expect("git command runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&project)
        .output()
        .expect("read fixture head");
    assert!(head.status.success());
    let head = String::from_utf8(head.stdout).unwrap().trim().to_owned();

    let graph = TraceDecay::init(&project).await.expect("canonical graph");
    let database = graph.db().clone();

    let resolved = ResolvedScope::new(
        ProjectId::new("project.advisory.four-pillar").unwrap(),
        RepositoryId::new("repository.advisory.four-pillar").unwrap(),
        WorktreeId::new("worktree.advisory.four-pillar").unwrap(),
        Some(RefId::new("refs/heads/advisory-four-pillar").unwrap()),
    )
    .unwrap();
    let scope = FeedbackScopeV1 {
        project_id: resolved.project_id.clone(),
        repository_id: resolved.repository_id.clone(),
        worktree_id: resolved.worktree_id.clone(),
        branch_ref: "refs/heads/advisory-four-pillar".to_owned(),
        head_commit_id: CommitId::new(head.clone()).unwrap(),
    };
    let requester = ActorId::new("actor.advisory.four-pillar").unwrap();
    let now = now_micros();
    let operation = feedback_surface_operation("feedback_diagnostics")
        .expect("feedback operation catalog")
        .expect("feedback diagnostics operation");
    let context = four_pillar_context(&resolved, &requester, &operation, now);

    // The one authentic capture set every remote pillar below replays. Loading
    // it re-verifies the recorded capture metadata, body digests, and
    // cross-response identity, so a drifted or hand-edited fixture fails here
    // rather than silently weakening the assertions.
    let fixture =
        tracedecay_usecases::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1()
            .expect("checked-in composite provider capture");

    // ---- Pillar 2: a localized CI failure, through the production store ----
    let mut provider_record = fixture.ci_provider_record.clone();
    provider_record.workflow_run.head_sha = head.clone();
    provider_record.workflow_job.head_sha = head.clone();
    provider_record.check_run.head_sha = head.clone();
    let annotation = provider_record
        .annotations
        .first_mut()
        .expect("recorded failure annotation");
    annotation.path = "src/lib.rs".to_owned();
    annotation.start_line = 2;
    annotation.end_line = 2;
    annotation.start_column = Some(1);
    annotation.end_column = None;
    let ci_request = CiFailureLocalizationRequestV1 {
        scope: scope.clone(),
        run: provider_record.run_identity(),
    };
    let retained = CiRetainedProviderRecordV1 {
        provider_record,
        observation: CiRetainedProviderObservationV1 {
            observation_id: CanonicalObservationIdV1::new(
                canonical_sha256(&"observation.advisory.four-pillar.ci")
                    .unwrap()
                    .as_str()
                    .to_owned(),
            )
            .unwrap(),
            failure_anchor: RetrievalAnchorId::new("anchor.advisory.four-pillar.ci").unwrap(),
            provider_head_commit_id: scope.head_commit_id.clone(),
            failure_kind: tracedecay_domain::feedback::CiFailureKindV1::TestFailure,
            observed_at: now,
        },
    };
    let code_graph = hermetic_ci_code_graph(&scope, &project);
    let ci_code_evidence =
        ProjectCiCodeAnchorStoreV1::new(project.clone(), scope.clone(), code_graph)
            .unwrap()
            .resolve(&context, &ci_request, &retained)
            .await
            .expect("production CI localization over the canonical graph");
    assert_eq!(
        ci_code_evidence.state,
        tracedecay_domain::feedback::CiFailureLocalizationStateV1::Complete,
        "the retargeted recorded CI run must localize completely against this graph"
    );
    let ci_symbol = ci_code_evidence
        .symbol
        .clone()
        .expect("canonical-graph symbol evidence");
    let ci_generation = ci_code_evidence
        .generation
        .clone()
        .expect("canonical-graph generation evidence");
    // The production owner that assembles this projection also performs a live
    // provider read, which a hermetic project has no reachability for. The
    // projection itself is a pure join of the recorded provider record with the
    // canonical-graph evidence resolved just above, so it is reproduced here
    // field-for-field; every value still originates from the shipped store or
    // the checked-in capture, never from an invented provider.
    let ci_localization = tracedecay_domain::feedback::CiFailureLocalizationResultV1 {
        provider: ProviderId::new("provider.github-actions").unwrap(),
        run: ci_request.run.clone(),
        parser: tracedecay_domain::feedback::CiFailureParserIdentityV1 {
            parser_id: "parser.github-actions.check-annotation".to_owned(),
            parser_version: "1".to_owned(),
        },
        state: ci_code_evidence.state,
        coverage: ci_code_evidence.coverage,
        source_degradation: None,
        failure_kind: retained.observation.failure_kind,
        failure_anchor: retained.observation.failure_anchor.clone(),
        branch: tracedecay_domain::feedback::CiFailureBranchEvidenceV1 {
            scope: scope.clone(),
            provider_head_commit_id: retained.observation.provider_head_commit_id.clone(),
        },
        generation: ci_code_evidence.generation.clone(),
        symbol: ci_code_evidence.symbol.clone(),
        callers: ci_code_evidence.callers.clone(),
        tests: ci_code_evidence.tests.clone(),
        rerun_hints: Vec::new(),
        observed_at: retained.observation.observed_at,
    };
    ci_localization
        .validate()
        .expect("assembled CI localization stays canonical");

    // ---- Pillar 3: an existing GitHub review finding, through the production
    // decoder, sanitizer, and canonical anchor authority ----
    let mut thread = captured_response(include_str!(
        "../crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/review_thread.graphql.json"
    ));
    let pull_request = thread
        .pointer_mut("/data/repository/pullRequest")
        .expect("recorded pull request");
    pull_request["headRefOid"] = json!(head.clone());
    let node = pull_request
        .pointer_mut("/reviewThreads/nodes/0")
        .expect("recorded review thread");
    node["path"] = json!("src/lib.rs");
    node["originalStartLine"] = json!(2);
    node["originalLine"] = json!(2);
    let comment = node
        .pointer_mut("/comments/nodes/0")
        .expect("recorded review comment");
    comment["originalCommit"]["oid"] = json!(head.clone());
    comment["pullRequestReview"]["commit"]["oid"] = json!(head.clone());
    let github_request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
        scope: scope.clone(),
        pull_request_id: fixture.github.pull_request_id.clone(),
    };
    let code_graph = hermetic_advisory_code_graph(&scope, &project, "src/lib.rs");
    let anchors = Arc::new(
        ProjectGitHubAnchorAuthorityV1::new(database.clone(), &project, scope.clone(), code_graph)
            .expect("production GitHub anchor authority"),
    );
    let decoder = GitHubOfficialResponseDecoderV1::new(
        GitHubReviewProviderIdentityV1 {
            provider: ProviderId::new("provider.github").unwrap(),
            repository_owner: "ScriptedAlchemy".to_owned(),
            repository_name: "tracedecay".to_owned(),
            pull_request_number: fixture.pull_request_number,
            base_commit_id: fixture.base_commit_id.clone(),
            head_commit_id: scope.head_commit_id.clone(),
            merge_base_commit_id: fixture.merge_base_commit_id.clone(),
        },
        anchors,
    )
    .unwrap();
    let github_ingress = decoder
        .decode(
            &github_request,
            &GitHubReadNetworkMetadataV1 {
                retry_at: None,
                status: GitHubReadNetworkStatusV1::Ok,
                etag: None,
                next_cursor: None,
                rate_limit: None,
            },
            serde_json::to_vec(&thread).unwrap().as_slice(),
        )
        .await
        .expect("recorded review thread decodes through the production path");
    assert_eq!(github_ingress.items.len(), 1);
    assert_eq!(
        github_ingress.items[0].lifecycle,
        tracedecay_domain::feedback::GitHubReviewLifecycleV1::Outdated,
        "the recorded thread is captured outdated; that lifecycle must survive decoding"
    );

    // ---- Pillar 4: a concurrent-agent proximity warning, through the shipped
    // proximity owner ----
    let peers = &fixture.proximity.source_sessions;
    assert!(
        peers.len() >= 2,
        "the recorded capture must carry concurrent peer sessions"
    );
    let proximity_request = ProximityEvaluationRequestV1 {
        scope: scope.clone(),
        observed_at: now,
    };
    let proximity_evidence = fixture
        .proximity_evidence(AdvisoryProximityFixtureEvidenceV1 {
            observations: peers
                .iter()
                .enumerate()
                .map(|(ordinal, session)| recorded_peer_observation(session, ordinal as u64 + 1))
                .collect(),
            retrieval_anchor_ids: vec![
                RetrievalAnchorId::new("anchor.advisory.four-pillar.proximity").unwrap(),
            ],
            address: tracedecay_domain::feedback::ProximityAddressV1 {
                scope: scope.clone(),
                file: ci_symbol.file.clone(),
                span: Some(ci_symbol.span),
                symbol: Some(ci_symbol.symbol.clone()),
            },
            relation_paths: Vec::new(),
            risk_inputs: ProximityRiskInputsV1 {
                overlap_size: 1,
                blast_radius_size: 2,
                relation_strength: ProximityRelationStrengthV1::Direct,
                branch_worktree_incompatibility:
                    ProximityBranchWorktreeIncompatibilityV1::Compatible,
                freshness_decay_basis_points: 0,
            },
            // Same file as the saved edit: the shipped owner classifies that as
            // the immediate tier, so inclusion never depends on the threshold.
            warning_class: ProximityWarningClassV1::SameFile,
            raw_risk_basis_points: 9_000,
            observed_at: UtcMicros(now.0.saturating_sub(1_000_000)),
            expires_at: UtcMicros(now.0.saturating_add(600_000_000)),
            coverage: ProximityCoverageV1::Complete,
        })
        .expect("recorded concurrent sessions admit proximity evidence");
    let proximity_owner = ProximityRuntimeOwnerV1::new(
        scope.clone(),
        RecordedProximityEvidenceAuthority {
            batch: CanonicalProximityEvidenceBatchV1::new(
                vec![proximity_evidence],
                ProximityCoverageV1::Complete,
            )
            .expect("proximity evidence batch"),
        },
        (),
    )
    .expect("proximity runtime owner");
    let threshold = ProximityThresholdPinV1::new(
        tracedecay_domain::configuration::ConfigurationRevisionId::new(
            "configuration.advisory.four-pillar",
        )
        .unwrap(),
        four_pillar_digest('2'),
        5_000,
    )
    .expect("effective Plan 20 threshold pin");
    let ProximityRuntimeOutcomeV1::Completed(proximity) = proximity_owner
        .evaluate_with_threshold_pin(&context, &proximity_request, &threshold)
        .await
    else {
        panic!("recorded concurrent-agent evidence must complete the proximity provider");
    };

    // ---- Compose the three advisory pillars for exactly one cycle ----
    let validity = AdvisoryFindingValidityWindowV1 {
        valid_at: now_micros(),
        expires_at: UtcMicros(now.0.saturating_add(300_000_000)),
    };
    let ci_batch = ci_localization
        .advisory_findings(validity)
        .expect("CI advisory contribution");
    let github_batch = github_ingress
        .advisory_findings(validity)
        .expect("GitHub advisory contribution");
    let proximity_batch = proximity
        .advisory_findings(validity)
        .expect("proximity advisory contribution");
    let advisory = FeedbackCycleAdvisoryV1 {
        providers: vec![
            FeedbackAdvisoryProviderStateV1 {
                producer: FeedbackDiagnosticProducerV1::GitHubReview,
                state: github_batch.provider_state,
            },
            FeedbackAdvisoryProviderStateV1 {
                producer: FeedbackDiagnosticProducerV1::CiLocalization,
                state: ci_batch.provider_state,
            },
            FeedbackAdvisoryProviderStateV1 {
                producer: FeedbackDiagnosticProducerV1::Proximity,
                state: proximity_batch.provider_state,
            },
        ],
        findings: github_batch
            .findings
            .iter()
            .chain(ci_batch.findings.iter())
            .chain(proximity_batch.findings.iter())
            .cloned()
            .collect(),
    };
    assert_eq!(
        advisory.findings.len(),
        3,
        "each remote pillar must contribute exactly one finding: {advisory:?}"
    );

    // ---- Pillar 1 + the single cycle: post-edit diagnostics and impact, then
    // one canonical result carrying all four ----
    let access = ProjectSourceAccessSnapshot {
        scope: resolved.clone(),
        requester: requester.clone(),
        binding: ScopeSourceBinding::new(
            SourceBindingId::new("binding.advisory.four-pillar").unwrap(),
            SourceKindV1::Cursor,
            LocatorDigest::new(format!("sha256:{}", "1".repeat(64))).unwrap(),
            AuthorityRef::Project(resolved.project_id.clone()),
        )
        .unwrap(),
        configuration_revision: tracedecay_domain::configuration::ConfigurationRevisionId::new(
            "configuration.advisory.four-pillar",
        )
        .unwrap(),
        configuration_digest: four_pillar_digest('2'),
        configuration_provenance_digest: four_pillar_digest('3'),
        effective_capabilities: BTreeSet::from([operation.capability_id().clone()]),
        grant_expires_at: UtcMicros(now.0.saturating_add(600_000_000)),
    };
    let feedback = open_feedback_runtime(database, &project, resolved.clone(), access)
        .await
        .expect("production feedback runtime");

    let generation = ci_generation.generation_id.clone();
    let file_digest = four_pillar_digest('4');
    let request = FeedbackCycleRequestV1::new(
        FeedbackCycleId::new("cycle.advisory.four-pillar").unwrap(),
        scope.clone(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: four_pillar_digest('5'),
            file_digest: file_digest.clone(),
        },
        // One saved-edit/stop boundary drives this cycle.
        FeedbackTriggerV1::AgentStopGate,
        four_pillar_digest('6'),
        four_pillar_digest('2'),
        FeedbackBudgetV1::bounded(600_000, 600_000, 1_000_000, 1_000_000),
    )
    .expect("saved-edit cycle request");
    let input = FeedbackEvaluationInputV1 {
        request,
        target: FeedbackTargetV1 {
            file: ci_symbol.file.clone(),
            span: Some(ci_symbol.span),
            symbol: Some(ci_symbol.symbol.clone()),
            generation_id: Some(generation.clone()),
        },
        actor: tracedecay_domain::feedback::FeedbackActorContextV1::default(),
        observed_at: now,
    };
    let provider = DiagnosticProviderIdentity::new(DiagnosticProviderIdentityParts {
        scope: resolved.clone(),
        source: ProviderSourceIdentity::CleanGeneration {
            generation: generation.clone(),
        },
        document: ProviderDocumentIdentity {
            file: ci_symbol.file.clone(),
            content_digest: ContentDigest::new(file_digest.as_str().to_owned()).unwrap(),
            document_version: None,
        },
        producer: DiagnosticProviderDescriptor {
            provider: ProviderId::new("provider.advisory.four-pillar").unwrap(),
            analyzer_revision: ComponentVersion::new("analyzer.advisory.four-pillar.v1").unwrap(),
            language: tracedecay_domain::LanguageId::new("rust").unwrap(),
            language_descriptor_revision: tracedecay_domain::LanguageDescriptorRevision::new(
                "language.rust.advisory.four-pillar.v1",
            )
            .unwrap(),
        },
        requested_capability: CapabilityId::new("capability.diagnostics.current").unwrap(),
        freshness: ProviderFreshness::current(now),
        coverage: ProviderCoverage::complete(1, 1),
        provenance: ProviderProvenance {
            origin: ProviderOrigin::ConfiguredAnalyzer,
            anchor: Some(RetrievalAnchorId::new("anchor.advisory.four-pillar.provider").unwrap()),
        },
        configuration: RevisionDigest {
            revision: ComponentVersion::new("configuration.advisory.four-pillar.v1").unwrap(),
            digest: four_pillar_digest('2'),
        },
        policy: PolicyDecisionRef::new(
            "policy.advisory.four-pillar",
            1,
            four_pillar_digest('6'),
            ComponentVersion::new("policy.evaluator.advisory.four-pillar.v1").unwrap(),
        )
        .unwrap(),
    })
    .expect("saved-generation diagnostic provider identity");

    let mut post_edit_diagnostic = tracedecay_domain::GenerationDiagnosticV1 {
        diagnostic_anchor: RetrievalAnchorId::new("anchor.advisory.four-pillar.diagnostic")
            .unwrap(),
        generation_id: generation.clone(),
        repository: scope.repository_id.clone(),
        worktree: Some(scope.worktree_id.clone()),
        reference: Some(RefId::new(scope.branch_ref.clone()).unwrap()),
        source_revision: Some(scope.head_commit_id.clone()),
        file_occurrence_id: ci_symbol.file.clone(),
        content_digest: ContentDigest::new(file_digest.as_str().to_owned()).unwrap(),
        span: ci_symbol.span,
        symbol_occurrence_id: Some(ci_symbol.symbol.clone()),
        code: "E0308".to_owned(),
        severity: tracedecay_domain::DiagnosticSeverityV1::Error,
        message: "mismatched types".to_owned(),
        message_digest: four_pillar_digest('7'),
        provenance: tracedecay_domain::DiagnosticProvenanceV1 {
            producer_kind: tracedecay_domain::DiagnosticProducerKindV1::UpstreamCompiler,
            producer: ProviderId::new("provider.advisory.four-pillar").unwrap(),
            analyzer_revision: ComponentVersion::new("analyzer.advisory.four-pillar.v1").unwrap(),
            configuration_revision: ComponentVersion::new("configuration.advisory.four-pillar.v1")
                .unwrap(),
            sanitization_receipt: None,
        },
        evidence_class: tracedecay_domain::DiagnosticEvidenceClassV1::ProducerReported,
        collected_at: now,
        state: tracedecay_domain::DiagnosticRecordStateV1::Current,
    };
    post_edit_diagnostic.message_digest = post_edit_diagnostic.compute_message_digest().unwrap();

    let baseline = FeedbackDiagnosticBaselineV1 {
        identity: FeedbackDiagnosticBaselineIdentityV1 {
            current_generation_id: generation.clone(),
            current_generation_digest: four_pillar_digest('5'),
            current_head_commit_id: scope.head_commit_id.clone(),
            current_content_digest: file_digest.clone(),
            provider_identity_digest: provider.compute_digest().unwrap(),
            horizon: FeedbackBaselineHorizonV1 {
                comparison_generation_id: CodeGenerationId::new(
                    "generation.advisory.four-pillar.previous",
                )
                .unwrap(),
                comparison_generation_digest: four_pillar_digest('8'),
                comparison_head_commit_id: CommitId::new(
                    "0000000000000000000000000000000000000001",
                )
                .unwrap(),
                comparison_content_digest: four_pillar_digest('8'),
                watermark: four_pillar_digest('8'),
            },
        },
        diagnostic_anchors: Vec::new(),
        state: FeedbackBaselineStateV1::Complete,
    };
    let runtime_state = FeedbackRuntimeStateV1::new(
        FeedbackAuthoritativeRuntimeStateV1 {
            snapshot: FeedbackCycleRuntimeSnapshotV1::from_request(&input.request),
            baseline_horizon: Some(baseline.identity.horizon.clone()),
            runtime_watermark: four_pillar_digest('9'),
        },
        Some(generation.clone()),
    )
    .expect("authoritative runtime state");

    let impact = FeedbackImpactV1 {
        target: input.target.clone(),
        affected_files: vec![ci_symbol.file.clone()],
        affected_callers: ci_localization
            .callers
            .iter()
            .map(|caller| caller.caller_symbol.clone())
            .collect::<Vec<SymbolOccurrenceId>>(),
        affected_tests: ci_localization
            .tests
            .iter()
            .map(|test| test.test_symbol.clone())
            .collect::<Vec<SymbolOccurrenceId>>(),
        evidence_anchors: vec![ci_symbol.retrieval_anchor_id.clone()],
        state: FeedbackImpactStateV1::Complete,
        affected_tests_state: FeedbackImpactStateV1::Complete,
    };
    assert!(
        !impact.affected_callers.is_empty() && !impact.affected_tests.is_empty(),
        "impact must come from real canonical-graph evidence: {impact:?}"
    );

    let service = FeedbackCycleService::new(
        move |_context: &RequestContext, _input: &FeedbackEvaluationInputV1| {
            Some(runtime_state.clone())
        },
        SavedGenerationDiagnostics {
            results: vec![
                DiagnosticProviderResult::new(
                    provider.clone(),
                    DiagnosticProviderState::SupportedComplete,
                    Some(vec![FeedbackDiagnosticV1::Saved(Box::new(
                        post_edit_diagnostic,
                    ))]),
                )
                .expect("saved diagnostics provider result"),
            ],
            baselines: vec![baseline],
        },
        GraphDerivedImpact(impact),
        feedback.publication_store(),
        SharedFeedbackObservations(feedback.observation_port()),
        feedback.route_authorization(),
        operation,
    );

    let execution = service
        .execute_with_advisory(
            &context,
            FeedbackCycleExecutionRequest {
                input,
                providers: vec![provider],
                maximum_returned_findings: 32,
                usage: tracedecay_application::feedback::FeedbackBudgetUsage {
                    completed_at: UtcMicros(now.0.saturating_add(1_000)),
                    tokens_consumed: 1,
                    cost_microunits: 1,
                },
                control: FeedbackCycleControl::Continue,
            },
            advisory,
        )
        .await
        .expect("one canonical four-pillar cycle");

    // Exactly one cycle result carries every pillar.
    let cycle = execution.cycle;
    let finding_ids = cycle
        .findings
        .iter()
        .map(|finding| finding.finding_id.as_str().to_owned())
        .collect::<Vec<_>>();
    for pillar in [
        "finding.ci-localization.",
        "finding.github-review.",
        "finding.proximity.",
    ] {
        assert!(
            finding_ids.iter().any(|id| id.starts_with(pillar)),
            "one cycle result must carry the {pillar} pillar: {finding_ids:?}"
        );
    }
    // The diagnostics pillar mints its finding id from the diagnostic anchor,
    // so it is exactly the finding that is not one of the three advisory
    // contributions above.
    let diagnostics_findings = finding_ids
        .iter()
        .filter(|id| {
            !id.starts_with("finding.ci-localization.")
                && !id.starts_with("finding.github-review.")
                && !id.starts_with("finding.proximity.")
        })
        .count();
    assert_eq!(
        diagnostics_findings, 1,
        "the post-edit diagnostics pillar must contribute its own finding: {finding_ids:?}"
    );
    assert!(
        cycle.impact.is_some(),
        "one cycle result must carry post-edit impact: {cycle:?}"
    );
    assert_eq!(
        cycle.impact_state,
        Some(FeedbackImpactStateV1::Complete),
        "graph-derived impact must report complete coverage: {cycle:?}"
    );

    // Provider outcome and per-finding lifecycle stay orthogonal: every remote
    // pillar completed, yet the recorded GitHub thread is outdated and so
    // contributes a superseded finding rather than an active one.
    assert!(
        cycle
            .provider_states
            .iter()
            .all(|state| *state == ProviderEvaluationStateV1::SupportedCompletedComplete),
        "every pillar of a positive cycle reports a complete provider state: {:?}",
        cycle.provider_states
    );
    let github_finding = cycle
        .findings
        .iter()
        .find(|finding| {
            finding
                .finding_id
                .as_str()
                .starts_with("finding.github-review.")
        })
        .expect("github pillar finding");
    assert_eq!(
        github_finding.lifecycle,
        FeedbackFindingLifecycleV1::Superseded,
        "an outdated review thread stays superseded even though its provider read completed"
    );
    assert_eq!(
        github_finding.provider_state,
        ProviderEvaluationStateV1::SupportedCompletedComplete,
        "provider outcome must not be degraded by a per-finding lifecycle"
    );
    let ci_finding = cycle
        .findings
        .iter()
        .find(|finding| {
            finding
                .finding_id
                .as_str()
                .starts_with("finding.ci-localization.")
        })
        .expect("ci pillar finding");
    assert_eq!(ci_finding.lifecycle, FeedbackFindingLifecycleV1::Active);
    let proximity_finding = cycle
        .findings
        .iter()
        .find(|finding| {
            finding
                .finding_id
                .as_str()
                .starts_with("finding.proximity.")
        })
        .expect("proximity pillar finding");
    assert_eq!(
        proximity_finding.lifecycle,
        FeedbackFindingLifecycleV1::Active
    );
    // `Clean` is reserved for a covered cycle that found nothing, so a positive
    // four-pillar cycle terminates `Blocked` — findings present, coverage
    // complete. The point of pinning it is that it is neither `Clean` (which
    // would mean the pillars produced nothing) nor any degraded terminal.
    assert_eq!(
        cycle.termination,
        FeedbackCycleTerminationV1::Blocked,
        "a covered cycle carrying four pillars of findings terminates blocked: {cycle:?}"
    );
    assert_eq!(
        (
            cycle.total_findings,
            cycle.returned_findings,
            cycle.omitted_findings
        ),
        (4, 4, 0),
        "all four pillars are accounted for and none is omitted: {cycle:?}"
    );
}

//! Strict PR13 runtime acceptance over authentic provider response captures.

use std::collections::{BTreeSet, VecDeque};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay::application::advisory::ci_runtime::{
    CiCodeAnchorStoreV1, CiRetainedProviderObservationV1, CiRetainedProviderRecordV1,
    GitHubCiOfficialResponseDecoderV1, ProjectCiCodeAnchorStoreV1,
};
use tracedecay::application::advisory::github_runtime::{
    GitHubProviderLifecycleV1, GitHubReviewAtomicRefreshStoreV1, GitHubReviewBodyReadOutcomeV1,
    GitHubReviewRefreshCoordinatorV1, GitHubReviewRefreshOutcomeV1, GitHubReviewRefreshStateV1,
    GitHubReviewRefreshStoreCommitOutcomeV1, GitHubReviewRefreshStoreReadOutcomeV1,
    GitHubSourceAccessAuthorityV1, ProjectGitHubAnchorAuthorityV1,
};
use tracedecay::application::advisory::{
    CiFailureLocalizationAdapter, CiReadOnlyEvidenceSource, GitHubCanonicalReviewAnchorAuthorityV1,
    GitHubCanonicalReviewAnchorsV1, GitHubHttpReadConfigV1, GitHubOfficialResponseDecoderV1,
    GitHubReadNetworkMetadataV1, GitHubReadNetworkStatusV1, GitHubReadOnlyCredentialV1,
    GitHubReadResponseDecoderV1, GitHubRepositoryTargetV1, GitHubReviewAnchorSeedV1,
    GitHubReviewProviderIdentityV1,
};
use tracedecay::tracedecay::TraceDecay;
use tracedecay_application::feedback::{
    CiFailureLocalizationPort, CiFailureLocalizationPortOutcomeV1, CiFailureLocalizationRequestV1,
    FeedbackPortFuture, GitHubReviewReadPort, GitHubReviewReadPortOutcomeV1,
    GitHubReviewReadRequestV1,
};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope, now_micros,
};
use tracedecay_domain::feedback::{
    FeedbackCycleTerminationV1, FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewCommentIdV1,
    GitHubReviewCurrentBranchRemapV1, GitHubReviewImmutableAnchorV1, GitHubReviewReadOperationV1,
    ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    ActorId, CanonicalObservationIdV1, CommitId, ContentDigest, FileOccurrenceId, ManifestDigest,
    ProjectId, ProviderId, RefId, RepositoryId, RetrievalAnchorId, SourceSpan, UtcMicros,
    WorktreeId, canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

mod common;

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

struct PanicCiSource;

impl CiReadOnlyEvidenceSource for PanicCiSource {
    fn read_localization<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiFailureLocalizationPortOutcomeV1> {
        Box::pin(async { panic!("denied CI request reached provider source") })
    }
}

struct PanicGitHubPort;

impl GitHubReviewReadPort for PanicGitHubPort {
    fn read<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1> {
        Box::pin(async { panic!("denied GitHub request reached provider port") })
    }
}

struct PanicGitHubStore;

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

struct PanicGitHubSourceAccess;

impl GitHubSourceAccessAuthorityV1 for PanicGitHubSourceAccess {
    fn authorize<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, tracedecay::application::advisory::GitHubProviderLifecycleV1> {
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
        project_id: ProjectId::new("project.pr13.runtime.capture").unwrap(),
        repository_id: RepositoryId::new("repository.pr13.runtime.capture").unwrap(),
        worktree_id: WorktreeId::new("worktree.pr13.runtime.capture").unwrap(),
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
        "../src/application/advisory/fixtures/pr13_branch_pr/pull_request.json"
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
        "../src/application/advisory/fixtures/pr13_branch_pr/review.json"
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
        "../src/application/advisory/fixtures/pr13_branch_pr/review_thread.graphql.json"
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
        include_str!("../src/application/advisory/fixtures/pr13_branch_pr/workflow_run.json"),
        include_str!("../src/application/advisory/fixtures/pr13_branch_pr/workflow_job.json"),
        include_str!("../src/application/advisory/fixtures/pr13_branch_pr/check_run.json"),
        include_str!("../src/application/advisory/fixtures/pr13_branch_pr/check_annotations.json"),
    )
    .expect("authentic CI responses decode");
    assert!(ci.failed_step().is_some());
    assert!(ci.failed_annotation().is_some());
}

#[tokio::test]
async fn corrupt_provider_identity_fails_production_decoder() {
    let mut pull_request = captured_response(include_str!(
        "../src/application/advisory/fixtures/pr13_branch_pr/pull_request.json"
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

fn proximity_context(scope: &FeedbackScopeV1, now: UtcMicros) -> RequestContext {
    let resolved = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).unwrap()),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.pr13.proximity").unwrap(),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        ActorId::new("actor.pr13.proximity.issuer").unwrap(),
        UtcMicros(now.0.saturating_sub(1_000_000)),
        UtcMicros(now.0.saturating_add(60_000_000)),
        resolved.clone(),
        BTreeSet::from([CapabilityId::new("capability.application.feedback.proximity").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.application.feedback.proximity").unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        ActorId::new("actor.pr13.proximity").unwrap(),
        resolved,
        grant,
        RequestId::new("request.pr13.proximity").unwrap(),
        Deadline::new(UtcMicros(now.0.saturating_add(30_000_000))).unwrap(),
        CancellationContext::active("cancel.pr13.proximity").unwrap(),
    )
    .unwrap()
}

fn ci_context(scope: &FeedbackScopeV1, now: UtcMicros) -> RequestContext {
    let resolved = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).unwrap()),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.pr13.ci").unwrap(),
        1,
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        ActorId::new("actor.pr13.ci.issuer").unwrap(),
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
        ActorId::new("actor.pr13.ci").unwrap(),
        resolved,
        grant,
        RequestId::new("request.pr13.ci").unwrap(),
        Deadline::new(UtcMicros(now.0.saturating_add(30_000_000))).unwrap(),
        CancellationContext::active("cancel.pr13.ci").unwrap(),
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
    let authority = ProjectGitHubAnchorAuthorityV1::new(database, &project, scope.clone()).unwrap();
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestListPullRequestReviewComments,
        scope: scope.clone(),
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    };
    let fixture: Value = serde_json::from_str(include_str!(
        "../crates/tracedecay-usecases/src/advisory/fixtures/pr13_branch_pr/review_comment.json"
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

#[tokio::test]
async fn unauthorized_ci_request_is_denied_before_provider_read() {
    let fixture =
        tracedecay::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
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
    let graph = TraceDecay::init(&project).await.expect("canonical graph");
    graph.index_all().await.expect("canonical graph index");
    let graph = Arc::new(graph);
    let mut provider_record =
        tracedecay::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
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
                canonical_sha256(&"observation.pr13.ci.graph")
                    .unwrap()
                    .as_str()
                    .to_owned(),
            )
            .unwrap(),
            failure_anchor: RetrievalAnchorId::new("anchor.pr13.ci.graph").unwrap(),
            provider_head_commit_id: scope.head_commit_id.clone(),
            failure_kind: tracedecay_domain::feedback::CiFailureKindV1::TestFailure,
            observed_at: now_micros(),
        },
    };
    let store = ProjectCiCodeAnchorStoreV1::new(graph.clone(), scope.clone()).unwrap();
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
    let stale = ProjectCiCodeAnchorStoreV1::new(graph, stale_scope.clone())
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

#[tokio::test]
async fn unauthorized_github_refresh_is_denied_before_port_or_store_access() {
    let fixture =
        tracedecay::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
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
/// daemon-owned Hook V2 admission/delivery ports, and the registered PR13
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
    event["conversation_id"] = json!("conversation-pr13-proximity");
    event["generation_id"] = json!("generation-pr13-proximity");
    event["model"] = json!("cursor-fixture");
    event["file_path"] = json!(project.join("src/lib.rs"));
    event["edits"] = json!([{"old_string": "", "new_string": "pub fn shared_edit() {}"}]);
    event["session_id"] = json!("session-pr13-proximity");
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
    assert!(
        output.status.success(),
        "registered daemon ingest failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
    codex_meta["payload"]["id"] = json!("session-pr13-proximity");
    codex_meta["payload"]["cwd"] = json!(project.clone());
    let codex_message: Value = serde_json::from_str(include_str!(
        "fixtures/provider_normalization/codex/agent_message.input.json"
    ))
    .unwrap();
    std::fs::write(
        codex_sessions.join("rollout-pr13-proximity.jsonl"),
        format!("{codex_meta}\n{codex_message}\n"),
    )
    .unwrap();

    let mut stop: Value = serde_json::from_str(include_str!(
        "../crates/tracedecay-hooks/fixtures/host_events/codex/stop.json"
    ))
    .unwrap();
    stop["session_id"] = json!("session-pr13-proximity");
    stop["turn_id"] = json!("turn-pr13-proximity-stop");
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
    assert!(
        advisory.status.success(),
        "registered advisory cycle failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&advisory.stdout),
        String::from_utf8_lossy(&advisory.stderr)
    );
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

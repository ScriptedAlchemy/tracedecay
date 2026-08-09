use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_application::feedback::{
    FeedbackPortFuture, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
    GitHubReviewReadRequestV1,
};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewReadOperationV1,
};
use tracedecay_domain::{
    ActorId, AnchorOwnerBindingV1, CapabilityId, CommitId, GitHubStackCapabilityStateV1,
    ManifestDigest, PrivacyDomainId, ProjectId, ProviderId, RefId, RepositoryId,
    RetrievalAnchorTargetV3, UseCaseId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_store::RetrievalAnchorOwnerV1;
use tracedecay_usecases::advisory::github_runtime::{
    GitHubProviderLifecycleV1, GitHubSourceAccessAuthorityV1,
};
use tracedecay_usecases::advisory::{
    GitHubStackAnchorPublicationOutcomeV1, GitHubStackAnchorReadOutcomeV1,
    ProjectGitHubStackAnchorAuthorityV1,
};
use tracedecay_usecases::stack_coordinator::{
    DaemonGitHubStackCoordinatorV1, GitHubStackProviderOutcomeV1,
};

const SHA: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

struct Ready;

impl GitHubSourceAccessAuthorityV1 for Ready {
    fn authorize<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubProviderLifecycleV1> {
        Box::pin(async { GitHubProviderLifecycleV1::Ready })
    }
}

fn scopes(suffix: &str) -> (ResolvedScope, FeedbackScopeV1) {
    let project_id =
        ProjectId::new(format!("project.github-stack-anchor.{suffix}")).expect("project id");
    let repository_id = RepositoryId::new(format!("repository.github-stack-anchor.{suffix}"))
        .expect("repository id");
    let worktree_id =
        WorktreeId::new(format!("worktree.github-stack-anchor.{suffix}")).expect("worktree id");
    let branch = format!("refs/heads/github-stack-anchor-{suffix}");
    let resolved = ResolvedScope::new(
        project_id.clone(),
        repository_id.clone(),
        worktree_id.clone(),
        Some(RefId::new(branch.clone()).expect("branch ref")),
    )
    .expect("resolved scope");
    let feedback = FeedbackScopeV1 {
        project_id,
        repository_id,
        worktree_id,
        branch_ref: branch,
        head_commit_id: CommitId::new(format!("commit.github-stack-anchor.{suffix}"))
            .expect("head commit"),
    };
    (resolved, feedback)
}

fn context_and_request(
    resolved: ResolvedScope,
    feedback: FeedbackScopeV1,
) -> (RequestContext, GitHubReviewReadRequestV1) {
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.github-stack-anchor").expect("grant id"),
        1,
        ManifestDigest::new(SHA).expect("grant digest"),
        ActorId::new("actor.github-stack-anchor.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        resolved.clone(),
        BTreeSet::from([
            CapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1).expect("capability")
        ]),
        BTreeSet::from([UseCaseId::new(GITHUB_REVIEW_INGEST_USE_CASE_ID_V1).expect("use case")]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    let context = RequestContext::new(
        ActorId::new("actor.github-stack-anchor").expect("actor"),
        resolved,
        grant,
        RequestId::new("request.github-stack-anchor").expect("request id"),
        Deadline::new(UtcMicros(i64::MAX - 1)).expect("deadline"),
        CancellationContext::active("cancel.github-stack-anchor").expect("cancellation"),
    )
    .expect("context");
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
        scope: feedback,
        pull_request_id: GitHubPullRequestIdV1::new("42").expect("pull request id"),
    };
    (context, request)
}

#[tokio::test]
async fn compare_unavailable_degraded_capability_persists_without_a_snapshot_anchor() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let (resolved, feedback) = scopes("degraded");
    let runtime = RegisteredGlobalDbTestRuntime::project(
        profile.path(),
        project.path(),
        resolved.project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let database = runtime.project_database_arc().expect("project database");
    let authority =
        ProjectGitHubStackAnchorAuthorityV1::new(Arc::clone(&database), feedback.clone())
            .expect("stack anchor authority");
    let (context, request) = context_and_request(resolved.clone(), feedback);
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    coordinator
        .register_scope(
            &resolved,
            tracedecay_domain::configuration::GitHubStackedPullRequestPolicyV1::ProbePrivatePreview,
        )
        .expect("register stack scope");
    let provider_outcome = GitHubStackProviderOutcomeV1::Degraded {
        response_digest: canonical_sha256(&"compare-unavailable").expect("response digest"),
    };
    let observed_at = UtcMicros(200);
    let source_binding = authority
        .source_binding(&context, &request, &provider_outcome, observed_at)
        .expect("degraded source binding");
    let observation = coordinator
        .observe_provider(
            resolved.clone(),
            ProviderId::new("provider.github").expect("provider"),
            provider_outcome,
            source_binding,
            observed_at,
        )
        .expect("degraded observation");
    assert!(observation.snapshot_anchor_id.is_none());
    assert_eq!(
        authority
            .publish(&context, &request, &observation, &Ready)
            .await,
        GitHubStackAnchorPublicationOutcomeV1::Published
    );
    assert!(matches!(
        authority
            .resolve(
                &context,
                &request,
                &observation.capability_anchor_id,
                &Ready,
            )
            .await,
        GitHubStackAnchorReadOutcomeV1::Current(ref record)
            if matches!(record.target(), RetrievalAnchorTargetV3::GitTopology(target)
                if matches!(target.as_ref(), tracedecay_domain::GitTopologyAnchorTargetV1::GitHubStackCapability(capability)
                    if capability.state == GitHubStackCapabilityStateV1::Degraded))
    ));
    let durable = ProjectGitHubStackAnchorAuthorityV1::resolve_published_observation(
        database.as_ref(),
        &resolved,
        observation.clone(),
    )
    .expect("generic durable read");
    assert!(durable.snapshot_anchor.is_none());
    let wrong_privacy_owner = AnchorOwnerBindingV1::for_project(
        database.binding().shard_id.profile_id.clone(),
        resolved.project_id.clone(),
        PrivacyDomainId::new("privacy.github-stack.wrong").expect("wrong privacy domain"),
    )
    .expect("wrong privacy owner");
    assert!(
        database
            .resolve_retrieval_anchor_record(
                RetrievalAnchorOwnerV1::from(wrong_privacy_owner),
                observation.capability_anchor_id.clone(),
            )
            .expect("wrong privacy read")
            .is_none()
    );

    let (wrong_resolved, wrong_feedback) = scopes("wrong");
    let (wrong_context, wrong_request) = context_and_request(wrong_resolved, wrong_feedback);
    assert_eq!(
        authority
            .resolve(
                &wrong_context,
                &wrong_request,
                &observation.capability_anchor_id,
                &Ready,
            )
            .await,
        GitHubStackAnchorReadOutcomeV1::Denied
    );
}

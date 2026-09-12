//! Closed GitHub review reads.

use tracedecay_contracts::feedback::GitHubReviewReadRequestV1;
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewReadOperationV1,
};
use tracedecay_domain::{CommitId, ProjectId, RepositoryId, WorktreeId};

fn scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.advisory.runtime").unwrap(),
        repository_id: RepositoryId::new("repository.advisory.runtime").unwrap(),
        worktree_id: WorktreeId::new("worktree.advisory.runtime").unwrap(),
        branch_ref: "refs/heads/advisory-runtime".to_owned(),
        head_commit_id: CommitId::new("commit.advisory.runtime").unwrap(),
    }
}

#[test]
fn github_request_only_admits_closed_read_operations() {
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
        scope: scope(),
        pull_request_id: GitHubPullRequestIdV1::new("pull-request.advisory.runtime").unwrap(),
    };
    request.validate().unwrap();
    for mutation in [
        "mutation",
        "rest_create_pull_request_review",
        "rest_update_review_comment",
        "graphql_add_pull_request_review",
        "graphql_resolve_review_thread",
    ] {
        assert!(
            serde_json::from_str::<GitHubReviewReadOperationV1>(&format!("\"{mutation}\""))
                .is_err(),
            "GitHub mutation operation {mutation} must stay unrepresentable"
        );
    }
}

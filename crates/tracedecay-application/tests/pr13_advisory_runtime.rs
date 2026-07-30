//! Behavioral acceptance for closed GitHub review reads and scope-bound CI/proximity.

use tracedecay_application::feedback::{
    CiFailureLocalizationRequestV1, GitHubReviewReadRequestV1, ProximityEvaluationRequestV1,
};
use tracedecay_domain::feedback::{
    CiFailureRunIdentityV1, FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewReadOperationV1,
};
use tracedecay_domain::{CommitId, ProjectId, RepositoryId, UtcMicros, WorktreeId};

fn scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.pr13.runtime").unwrap(),
        repository_id: RepositoryId::new("repository.pr13.runtime").unwrap(),
        worktree_id: WorktreeId::new("worktree.pr13.runtime").unwrap(),
        branch_ref: "refs/heads/pr13-runtime".to_owned(),
        head_commit_id: CommitId::new("commit.pr13.runtime").unwrap(),
    }
}

#[test]
fn github_request_only_admits_closed_read_operations() {
    let request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
        scope: scope(),
        pull_request_id: GitHubPullRequestIdV1::new("pull-request.pr13.runtime").unwrap(),
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

#[test]
fn ci_and_proximity_requests_are_exactly_scope_bound() {
    let scope = scope();
    CiFailureLocalizationRequestV1 {
        scope: scope.clone(),
        run: CiFailureRunIdentityV1 {
            workflow_id: "workflow.pr13.runtime".to_owned(),
            job_id: "job.pr13.runtime".to_owned(),
            check_suite_id: "suite.pr13.runtime".to_owned(),
            check_run_id: "check.pr13.runtime".to_owned(),
            run_id: "run.pr13.runtime".to_owned(),
            attempt_id: "attempt.pr13.runtime".to_owned(),
        },
    }
    .validate()
    .unwrap();
    ProximityEvaluationRequestV1 {
        scope,
        observed_at: UtcMicros(1),
    }
    .validate()
    .unwrap();
}

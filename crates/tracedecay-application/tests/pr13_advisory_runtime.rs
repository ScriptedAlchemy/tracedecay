//! Focused acceptance checks for the PR13 application runtime contracts.

use tracedecay_application::feedback::{
    CiFailureLocalizationRequestV1, GitHubReviewReadRequestV1, ProximityEvaluationRequestV1,
    feedback_surface_catalog_contribution,
};
use tracedecay_domain::feedback::{
    CiFailureRunIdentityV1, FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewReadOperationV1,
};
use tracedecay_domain::{CommitId, ProjectId, RepositoryId, UtcMicros, WorktreeId};
use tracedecay_tool_catalog::BindingSurface;

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

#[test]
fn feedback_catalog_binds_pr13_advisory_handlers_to_supported_surfaces() {
    const PR14_READ_TRANSPORT: [BindingSurface; 4] = [
        BindingSurface::Cli,
        BindingSurface::Mcp,
        BindingSurface::Http,
        BindingSurface::Dashboard,
    ];
    const PR13_ADVISORY_TRANSPORT: [BindingSurface; 4] = [
        BindingSurface::Cli,
        BindingSurface::Mcp,
        BindingSurface::Http,
        BindingSurface::Lsp,
    ];
    const PR12_READ_ONLY: [&str; 4] = [
        "capability.application.feedback.diagnostics",
        "capability.application.feedback.get",
        "capability.application.feedback.expand",
        "capability.application.feedback.list",
    ];
    const PR13_PROVIDER_CONTRIBUTIONS: [&str; 3] = [
        "capability.application.feedback.github-review-ingest",
        "capability.application.feedback.ci-failure-localize",
        "capability.application.feedback.proximity",
    ];
    let catalog = feedback_surface_catalog_contribution().unwrap();
    for capability_id in PR12_READ_ONLY {
        let capability = catalog
            .capabilities()
            .iter()
            .find(|capability| capability.capability_id().as_str() == capability_id)
            .expect("read-only capability");
        let surfaces = catalog
            .bindings()
            .iter()
            .filter(|binding| binding.capability_id() == capability.capability_id())
            .map(|binding| binding.surface())
            .collect::<Vec<_>>();
        assert_eq!(surfaces.len(), PR14_READ_TRANSPORT.len());
        for surface in PR14_READ_TRANSPORT {
            assert!(surfaces.contains(&surface));
        }
        assert!(!surfaces.contains(&BindingSurface::Lsp));
    }
    for capability_id in PR13_PROVIDER_CONTRIBUTIONS {
        let capability = catalog
            .capabilities()
            .iter()
            .find(|capability| capability.capability_id().as_str() == capability_id)
            .expect("advisory capability");
        assert!(capability.availability().is_callable());
        let surfaces = catalog
            .bindings()
            .iter()
            .filter(|binding| binding.capability_id() == capability.capability_id())
            .map(|binding| binding.surface())
            .collect::<Vec<_>>();
        assert!(surfaces.is_empty());
        assert!(!surfaces.contains(&BindingSurface::Dashboard));
    }
    let advisory = catalog
        .capabilities()
        .iter()
        .find(|capability| {
            capability.capability_id().as_str() == "capability.application.feedback.advisory-cycle"
        })
        .expect("combined advisory capability");
    let surfaces = catalog
        .bindings()
        .iter()
        .filter(|binding| binding.capability_id() == advisory.capability_id())
        .map(|binding| binding.surface())
        .collect::<Vec<_>>();
    assert_eq!(surfaces.len(), PR13_ADVISORY_TRANSPORT.len());
    for surface in PR13_ADVISORY_TRANSPORT {
        assert!(surfaces.contains(&surface));
    }
}

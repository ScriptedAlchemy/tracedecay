use super::*;
use tracedecay_domain::{ProjectId, RefId, RepositoryId, WorktreeId};

fn feedback_scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.advisory-owner").expect("project id"),
        repository_id: RepositoryId::new("repository.advisory-owner").expect("repository id"),
        worktree_id: WorktreeId::new("worktree.advisory-owner").expect("worktree id"),
        branch_ref: "refs/heads/main".to_owned(),
        head_commit_id: CommitId::new("a".repeat(40)).expect("commit id"),
    }
}

#[test]
fn hook_notice_registration_is_released_with_the_published_owner() {
    let scope = feedback_scope();
    let resolved = tracedecay_application::ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).expect("ref")),
    )
    .expect("scope");
    let (project, worktree) = crate::hooks::hook_scope_locators(&resolved);
    let first = AdvisoryHookNoticeQueueV1::new(scope.clone());
    assert!(register_advisory_hook_notice_queue(
        project, worktree, &first
    ));
    let registration = AdvisoryHookNoticeRegistrationV1 {
        hook_project_id: project,
        hook_worktree_id: worktree,
        hook_notices: first,
    };
    let conflicting = AdvisoryHookNoticeQueueV1::new(scope);
    assert!(!register_advisory_hook_notice_queue(
        project,
        worktree,
        &conflicting
    ));

    drop(registration);

    assert!(register_advisory_hook_notice_queue(
        project,
        worktree,
        &conflicting
    ));
    assert!(unregister_advisory_hook_notice_queue(
        project,
        worktree,
        &conflicting
    ));
}

#[test]
fn advisory_deadline_outside_monotonic_horizon_is_typed() {
    let Err(problem) =
        model::advisory_monotonic_deadline_from_remaining(Instant::now(), Duration::MAX)
    else {
        panic!("far-future deadline must not overflow Instant");
    };

    assert!(matches!(
        problem,
        ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic { ref code, .. },
            ..
        } if code == "feedback.advisory-cycle.deadline"
    ));
}

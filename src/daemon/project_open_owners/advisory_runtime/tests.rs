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

fn hook_binding(host: tracedecay_hooks::HookHostV1) -> tracedecay_hooks::HookScopeBindingV1 {
    let capabilities = [
        tracedecay_hooks::HookEventFamily::SessionBoundary,
        tracedecay_hooks::HookEventFamily::PromptBoundary,
        tracedecay_hooks::HookEventFamily::ToolLifecycle,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        tracedecay_hooks::HookEventFamily::TestLifecycle,
    ]
    .into_iter()
    .map(|family| tracedecay_hooks::HookCapabilityV1 {
        family,
        support: tracedecay_hooks::stock_event_support(host, family),
    })
    .collect();
    tracedecay_hooks::HookScopeBindingV1 {
        host,
        project_id: [1; 16],
        repository_id: [2; 16],
        worktree_id: [3; 16],
        worktree_epoch: 1,
        binding_token: [4; 32],
        capabilities,
    }
}

#[test]
fn hook_notice_dispatch_requires_a_live_daemon_binding() {
    let root = tempfile::tempdir().expect("hook config root");
    let published_at = UtcMicros(1_000_000);
    assert!(
        advisory_hook_notice_dispatch(root.path(), published_at).is_none(),
        "an unpublished binding set must stay typed unbound"
    );

    let host = tracedecay_hooks::HookHostV1::ClaudeCode;
    let expires_at = UtcMicros(published_at.0 + 60_000_000);
    tracedecay_hooks::HookConfigurationPublisherV1::new(
        tracedecay_hooks::HookConfigurationFileWriterV1::new(hook_configuration_path(
            root.path(),
            host,
        )),
    )
    .publish(tracedecay_hooks::HookConfigurationSnapshotV1 {
        schema_version: tracedecay_hooks::HOOK_CONFIGURATION_SCHEMA_VERSION,
        revision: 7,
        published_at,
        expires_at,
        binding: hook_binding(host),
    })
    .expect("published hook binding");

    let (kind, rollback) =
        advisory_hook_notice_dispatch(root.path(), UtcMicros(published_at.0 + 1))
            .expect("live binding authorizes hook notice dispatch");
    assert_eq!(kind, HostKindV1::ClaudeCode);
    assert_eq!(rollback.configuration_revision, 7);
    assert_eq!(rollback.route, HookFeedbackDeliveryRouteV1::HookV2);

    assert!(
        advisory_hook_notice_dispatch(root.path(), expires_at).is_none(),
        "an expired binding is not a live delivery authority"
    );
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

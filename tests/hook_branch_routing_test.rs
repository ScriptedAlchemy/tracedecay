use tokensave::hooks::{
    cursor_branch_switch_target, cursor_shell_sync_plan,
    cursor_shell_sync_plan_with_current_branch, CursorShellSyncPlan,
};

#[test]
fn git_global_c_option_still_extracts_branch_switch_target() {
    assert_eq!(
        cursor_branch_switch_target("git -C repo switch feature/foo"),
        Some("feature/foo".to_string())
    );
    assert_eq!(
        cursor_shell_sync_plan("git -C repo switch feature/foo"),
        CursorShellSyncPlan::BranchAdd("feature/foo".to_string())
    );
}

#[test]
fn quoted_branch_names_are_preserved_when_extracting_switch_target() {
    assert_eq!(
        cursor_branch_switch_target("git switch 'feature/quoted name'"),
        Some("feature/quoted name".to_string())
    );
    assert_eq!(
        cursor_branch_switch_target("git checkout -b \"feature/double quoted\""),
        Some("feature/double quoted".to_string())
    );
}

#[test]
fn checkout_path_restore_is_not_treated_as_branch_switch() {
    assert_eq!(
        cursor_branch_switch_target("git checkout -- src/lib.rs"),
        None
    );
    assert_eq!(
        cursor_shell_sync_plan_with_current_branch(
            "git checkout -- src/lib.rs",
            Some("feature/current")
        ),
        CursorShellSyncPlan::CurrentBranchSync("feature/current".to_string())
    );
}

#[test]
fn ambiguous_state_changes_fall_back_to_current_branch_when_available() {
    assert_eq!(
        cursor_shell_sync_plan_with_current_branch("git pull --rebase", Some("feature/current")),
        CursorShellSyncPlan::CurrentBranchSync("feature/current".to_string())
    );
    assert_eq!(
        cursor_shell_sync_plan_with_current_branch(
            "git merge origin/main",
            Some("feature/current")
        ),
        CursorShellSyncPlan::CurrentBranchSync("feature/current".to_string())
    );
    assert_eq!(
        cursor_shell_sync_plan_with_current_branch("git pull --rebase", None),
        CursorShellSyncPlan::IncrementalSync
    );
}

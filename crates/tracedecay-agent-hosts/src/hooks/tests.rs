use std::path::{Path, PathBuf};

use super::{hook_output_owner_event_id, run_with_test_env_lock, schedule_user_session_review};

fn canonical(path: &Path) -> PathBuf {
    tracedecay_runtime_core::path_safety::canonical_root_identity(path)
}

fn init_minimal_git_root(dir: &Path) {
    std::fs::create_dir_all(dir).expect("git root");
    gix::init(dir).expect("minimal git root");
}

fn project_like_root(start: &Path, temp_root: Option<&Path>) -> Option<PathBuf> {
    super::nearest_project_like_root_with_temp_root(start, temp_root)
}

fn init_nested_git_workspace(root: &Path) -> PathBuf {
    let repo = root.join("workspace");
    init_minimal_git_root(&repo);
    let nested = repo.join("src");
    std::fs::create_dir(&nested).expect("nested workspace");
    nested
}

#[test]
fn temp_git_root_is_refused_as_project_like() {
    let configured_temp = tempfile::tempdir().expect("configured temp root");
    let nested = init_nested_git_workspace(configured_temp.path());
    let repo = nested.parent().expect("repo");

    assert!(
        canonical(repo).starts_with(canonical(configured_temp.path())),
        "fixture git root must be a descendant of the injected temp root"
    );
    assert!(
        tracedecay_runtime_core::worktree::git_worktree_root(&nested).is_some(),
        "fixture must be a real git worktree root"
    );
    assert_eq!(
        project_like_root(&nested, Some(configured_temp.path())),
        None
    );
}

#[test]
fn git_root_outside_configured_temp_is_project_like() {
    let configured_temp = tempfile::tempdir().expect("configured temp root");
    let sibling = tempfile::tempdir().expect("sibling root");
    let nested = init_nested_git_workspace(sibling.path());
    let repo = nested.parent().expect("repo");

    let temp_root = canonical(configured_temp.path());
    assert!(
        !canonical(repo).starts_with(&temp_root),
        "sibling git root must sit outside the injected temp root"
    );
    let discovered = tracedecay_runtime_core::worktree::git_worktree_root(&nested)
        .expect("fixture must be a real git worktree root");
    assert_eq!(
        project_like_root(&nested, Some(configured_temp.path())),
        Some(discovered)
    );
}

#[test]
fn empty_temp_root_does_not_reject_a_git_repo() {
    let home = tempfile::tempdir().expect("workspace root");
    let nested = init_nested_git_workspace(home.path());
    let discovered = tracedecay_runtime_core::worktree::git_worktree_root(&nested)
        .expect("fixture must be a real git worktree root");
    assert_eq!(super::usable_absolute_temp_root_from(Path::new("")), None);
    assert_eq!(
        project_like_root(&nested, Some(Path::new(""))),
        Some(discovered)
    );
}

#[test]
fn relative_temp_root_does_not_reject_a_git_repo() {
    let home = tempfile::tempdir().expect("workspace root");
    let nested = init_nested_git_workspace(home.path());
    let discovered = tracedecay_runtime_core::worktree::git_worktree_root(&nested)
        .expect("fixture must be a real git worktree root");
    assert_eq!(super::usable_absolute_temp_root_from(Path::new(".")), None);
    assert_eq!(
        project_like_root(&nested, Some(Path::new("."))),
        Some(discovered)
    );
}

#[test]
fn absent_temp_root_does_not_reject_a_git_repo() {
    let home = tempfile::tempdir().expect("workspace root");
    let nested = init_nested_git_workspace(home.path());
    let discovered = tracedecay_runtime_core::worktree::git_worktree_root(&nested)
        .expect("fixture must be a real git worktree root");
    assert_eq!(project_like_root(&nested, None), Some(discovered));
}

#[test]
fn marker_at_temp_root_does_not_make_descendant_project_like() {
    let temp = tempfile::tempdir().expect("temporary root");
    std::fs::write(temp.path().join("package.json"), "{}").expect("temp-root marker");

    let generic = temp.path().join("generic");
    std::fs::create_dir(&generic).expect("generic directory");
    assert_eq!(project_like_root(&generic, Some(temp.path())), None);

    let project = temp.path().join("project");
    let nested = project.join("src");
    std::fs::create_dir_all(&nested).expect("nested project directory");
    std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"x\"\n")
        .expect("project marker");
    assert_eq!(project_like_root(&nested, Some(temp.path())), Some(project));
}

#[test]
fn direct_hook_owner_identity_is_stable_across_retry_time() {
    let host = tracedecay_hooks::HookHostV1::Codex;
    let event = r#"{"session_id":"session-1","hook_event_name":"Stop"}"#;
    let output = r#"{"hookSpecificOutput":{"hookEventName":"Stop"}}"#;
    let first = hook_output_owner_event_id(host, event, output).expect("owner identity");
    let retry = hook_output_owner_event_id(host, event, output).expect("owner identity");
    let expected = tracedecay_domain::canonical_sha256(&(
        "tracedecay.hook-output-delivery.v1",
        host.hook_key(),
        event,
        output,
    ))
    .expect("canonical owner digest");
    let expected = format!(
        "hook:output:{}",
        expected.as_str().trim_start_matches("sha256:")
    );
    assert_eq!(first, retry);
    assert_eq!(first, expected);
}

#[cfg(unix)]
#[test]
fn session_review_hint_routes_exact_identity_to_the_daemon() {
    run_with_test_env_lock(async {
        let daemon = super::TestDaemonHookActionGuard::install([serde_json::json!({
            "action": "user_review",
            "status": "accepted",
        })]);

        schedule_user_session_review(
            &crate::ports::hook_runtime::crate_test_runtime(),
            "claude",
            Some("session-native-17"),
        )
        .await;

        assert_eq!(
            daemon.calls(),
            [(
                None,
                serde_json::json!({
                    "action": "user_review",
                    "format": "json",
                    "provider": "claude",
                    "session_id": "session-native-17",
                }),
            )]
        );
    });
}

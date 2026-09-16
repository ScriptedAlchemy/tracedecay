use std::path::{Path, PathBuf};

use super::{
    hook_output_owner_event_id, run_with_test_env_lock, schedule_user_session_review, EnvGuard,
};

fn canonical(path: &Path) -> PathBuf {
    tracedecay_runtime_core::path_safety::canonical_root_identity(path)
}

fn system_temp_root() -> PathBuf {
    canonical(&std::env::temp_dir())
}

fn init_minimal_git_root(dir: &Path) {
    std::fs::create_dir_all(dir).expect("git root");
    gix::init(dir).expect("minimal git root");
}

fn pin_process_temp(path: &Path) -> (EnvGuard, EnvGuard, EnvGuard) {
    (
        EnvGuard::set_path("TMPDIR", path),
        EnvGuard::set_path("TEMP", path),
        EnvGuard::set_path("TMP", path),
    )
}

fn assert_is_project_like(start: &Path) {
    assert!(
        super::is_project_like_workspace(start),
        "workspace must stay project-like"
    );
    assert!(
        super::nearest_project_like_root(start).is_some(),
        "workspace must stay project-like"
    );
}

#[test]
fn temp_git_root_is_refused_as_project_like() {
    let _lock = super::lock_test_env();
    let temp = tempfile::tempdir().expect("temporary root");
    let repo = temp.path().join("ephemeral-agent");
    init_minimal_git_root(&repo);
    let nested = repo.join("src");
    std::fs::create_dir(&nested).expect("nested workspace");

    let repo_id = canonical(&repo);
    assert!(
        repo_id.starts_with(system_temp_root()),
        "fixture git root must be a descendant of the system temp root"
    );
    assert!(
        tracedecay_runtime_core::worktree::git_worktree_root(&nested).is_some(),
        "fixture must be a real git worktree root"
    );
    assert_eq!(super::nearest_project_like_root(&nested), None);
    assert!(!super::is_project_like_workspace(&nested));
}

#[test]
fn git_root_outside_configured_temp_is_project_like() {
    let _lock = super::lock_test_env();
    let configured_temp = tempfile::tempdir().expect("configured temp root");
    let sibling = tempfile::tempdir().expect("sibling root");
    let _pinned = pin_process_temp(configured_temp.path());

    let repo = sibling.path().join("workspace");
    init_minimal_git_root(&repo);
    let nested = repo.join("src");
    std::fs::create_dir(&nested).expect("nested workspace");

    let temp_root = system_temp_root();
    let repo_id = canonical(&repo);
    assert!(
        temp_root.is_absolute() && !temp_root.as_os_str().is_empty(),
        "configured temp root must be a usable absolute bound"
    );
    assert!(
        !repo_id.starts_with(&temp_root),
        "sibling git root must sit outside the configured temp root"
    );
    let discovered = tracedecay_runtime_core::worktree::git_worktree_root(&nested)
        .expect("fixture must be a real git worktree root");
    assert_eq!(
        super::nearest_project_like_root(&nested),
        Some(discovered)
    );
    assert!(super::is_project_like_workspace(&nested));
}

#[test]
fn empty_temp_dir_does_not_reject_a_git_repo() {
    let _lock = super::lock_test_env();
    let start = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let _pinned = pin_process_temp(Path::new(""));
    assert_is_project_like(&start);
}

#[test]
fn relative_temp_dir_does_not_reject_a_git_repo() {
    let _lock = super::lock_test_env();
    let start = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let _pinned = pin_process_temp(Path::new("."));
    assert_is_project_like(&start);
}

#[test]
fn marker_at_temp_root_does_not_make_descendant_project_like() {
    let _lock = super::lock_test_env();
    let temp = tempfile::tempdir().expect("temporary root");
    let _pinned = pin_process_temp(temp.path());
    std::fs::write(temp.path().join("package.json"), "{}").expect("temp-root marker");

    let generic = temp.path().join("generic");
    std::fs::create_dir(&generic).expect("generic directory");
    assert_eq!(super::nearest_project_like_root(&generic), None);
    assert!(!super::is_project_like_workspace(&generic));

    let project = temp.path().join("project");
    let nested = project.join("src");
    std::fs::create_dir_all(&nested).expect("nested project directory");
    std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"x\"\n")
        .expect("project marker");
    assert_eq!(super::nearest_project_like_root(&nested), Some(project));
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

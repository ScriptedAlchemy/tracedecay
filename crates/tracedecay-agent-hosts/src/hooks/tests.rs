use super::{hook_output_owner_event_id, run_with_test_env_lock, schedule_user_session_review};

/// `std::env::temp_dir` reads `TMPDIR` on unix and `TMP`/`TEMP` on Windows, so
/// every variable is redirected and the bound is exercised on both layouts.
#[cfg(test)]
fn redirect_temp_dir(root: &std::path::Path) -> Vec<super::EnvGuard> {
    ["TMPDIR", "TMP", "TEMP"]
        .into_iter()
        .map(|key| super::EnvGuard::set_path(key, root))
        .collect()
}

#[test]
fn project_marker_search_stops_at_the_system_temp_root() {
    let _lock = super::lock_test_env();
    let temp = tempfile::tempdir().expect("temporary root");
    let _temp_env = redirect_temp_dir(temp.path());
    std::fs::write(temp.path().join("package.json"), "{}").expect("temp-root marker");

    let generic = temp.path().join("generic");
    std::fs::create_dir(&generic).expect("generic directory");
    assert_eq!(super::nearest_project_like_root(&generic), None);

    let project = temp.path().join("project");
    let nested = project.join("src");
    std::fs::create_dir_all(&nested).expect("nested project directory");
    std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"x\"\n")
        .expect("project marker");
    assert_eq!(super::nearest_project_like_root(&nested), Some(project));
}

#[test]
fn a_git_repository_at_the_system_temp_root_is_not_project_like() {
    let _lock = super::lock_test_env();
    let temp = tempfile::tempdir().expect("temporary root");
    let _temp_env = redirect_temp_dir(temp.path());
    let git = tracedecay_runtime_core::git::try_git_program().expect("resolve the git program");
    let status = std::process::Command::new(&git)
        .args(["init", "--initial-branch=main"])
        .current_dir(temp.path())
        .status()
        .expect("run git init at the temp root");
    assert!(status.success(), "git init at the temp root failed");

    let workspace = temp.path().join("agent-workspace");
    std::fs::create_dir(&workspace).expect("ephemeral agent workspace");

    assert_eq!(super::nearest_project_like_root(&workspace), None);
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

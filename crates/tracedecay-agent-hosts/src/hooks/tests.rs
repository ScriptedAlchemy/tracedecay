use super::{hook_output_owner_event_id, run_with_test_env_lock, schedule_user_session_review};

#[cfg(unix)]
#[test]
fn project_marker_search_stops_at_the_system_temp_root() {
    let _lock = super::lock_test_env();
    let temp = tempfile::tempdir().expect("temporary root");
    let _temp_env = super::EnvGuard::set_path("TMPDIR", temp.path());
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

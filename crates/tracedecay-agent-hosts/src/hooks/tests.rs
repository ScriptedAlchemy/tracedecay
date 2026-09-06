use super::{
    hook_output_owner_event_id, hook_route_metadata_from_parsed, run_with_test_env_lock,
    schedule_user_session_review,
};

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

#[test]
fn hook_route_metadata_preserves_camel_case_session_ids() {
    let event = serde_json::json!({
        "sessionId": "session-camel",
        "conversationId": "conversation-camel",
        "cwd": "/tmp/project"
    })
    .to_string();

    let parsed: serde_json::Value = serde_json::from_str(&event).expect("event JSON");
    let route = hook_route_metadata_from_parsed(&parsed, std::path::Path::new("/tmp/project"));

    assert_eq!(route.session_id.as_deref(), Some("session-camel"));

    let event = serde_json::json!({
        "conversationId": "conversation-camel",
        "cwd": "/tmp/project"
    })
    .to_string();

    let parsed: serde_json::Value = serde_json::from_str(&event).expect("event JSON");
    let route = hook_route_metadata_from_parsed(&parsed, std::path::Path::new("/tmp/project"));

    assert_eq!(route.session_id.as_deref(), Some("conversation-camel"));
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

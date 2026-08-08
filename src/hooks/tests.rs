#[cfg(unix)]
use super::{daemon_tool_json, run_with_test_env_lock};
use super::{
    hook_output_owner_event_id, hook_route_metadata_from_event, parse_daemon_tool_json_content,
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

#[cfg(unix)]
#[test]
fn daemon_tool_json_returns_project_warming_without_retrying() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    struct SocketEnvGuard(Option<std::ffi::OsString>);

    impl Drop for SocketEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(value) => std::env::set_var(crate::daemon::SOCKET_ENV, value),
                    None => std::env::remove_var(crate::daemon::SOCKET_ENV),
                }
            }
        }
    }

    run_with_test_env_lock(async {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind daemon socket");
        let previous = std::env::var_os(crate::daemon::SOCKET_ENV);
        unsafe {
            std::env::set_var(crate::daemon::SOCKET_ENV, &socket);
        }
        let _socket_env = SocketEnvGuard(previous);

        let daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept hook client");
            let (reader, mut writer) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(reader).lines();
            lines
                .next_line()
                .await
                .expect("read handshake")
                .expect("handshake line");
            let request_line = lines
                .next_line()
                .await
                .expect("read request")
                .expect("request line");
            let request: serde_json::Value =
                serde_json::from_str(&request_line).expect("request JSON");
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "error": {
                    "code": -32603,
                    "message": "config error: project is warming in the background; retry the same tool shortly"
                }
            });
            writer
                .write_all(
                    serde_json::to_string(&response)
                        .expect("response JSON")
                        .as_bytes(),
                )
                .await
                .expect("write response");
            writer.write_all(b"\n").await.expect("write newline");
            writer.shutdown().await.expect("shutdown fake daemon");
        });

        let error = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            daemon_tool_json(None, "tracedecay_status", serde_json::json!({})),
        )
        .await
        .expect("hook daemon call retried project warming")
        .expect_err("warming should remain a typed hook failure");
        assert!(
            error.to_string().contains("warming in the background"),
            "{error}"
        );
        daemon.await.expect("fake daemon task");
    });
}

#[test]
fn daemon_tool_json_ignores_notices_and_returns_one_payload() {
    let response = serde_json::json!({
        "content": [
            { "type": "text", "text": "write already accepted by daemon" },
            { "type": "text", "text": r#"{"status":"ok"}"# },
            { "type": "text", "text": "informational notice" }
        ]
    });

    assert_eq!(
        parse_daemon_tool_json_content(&response, "test").unwrap(),
        serde_json::json!({ "status": "ok" })
    );
}

#[test]
fn daemon_tool_json_rejects_zero_or_multiple_payloads() {
    let no_payload = serde_json::json!({
        "content": [{ "type": "text", "text": "notice only" }]
    });
    let error = parse_daemon_tool_json_content(&no_payload, "test").unwrap_err();
    assert!(error.to_string().contains("returned no JSON payload"));

    let multiple = serde_json::json!({
        "content": [
            { "type": "text", "text": "{}" },
            { "type": "text", "text": "[]" }
        ]
    });
    let error = parse_daemon_tool_json_content(&multiple, "test").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("returned multiple JSON payloads (2)")
    );
}

#[test]
fn hook_route_metadata_preserves_camel_case_session_ids() {
    let event = serde_json::json!({
        "sessionId": "session-camel",
        "conversationId": "conversation-camel",
        "cwd": "/tmp/project"
    })
    .to_string();

    let Some(route) = hook_route_metadata_from_event(&event, std::path::Path::new("/tmp/project"))
    else {
        panic!("route metadata should parse");
    };

    assert_eq!(route.session_id.as_deref(), Some("session-camel"));

    let event = serde_json::json!({
        "conversationId": "conversation-camel",
        "cwd": "/tmp/project"
    })
    .to_string();

    let Some(route) = hook_route_metadata_from_event(&event, std::path::Path::new("/tmp/project"))
    else {
        panic!("route metadata should parse");
    };

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

        schedule_user_session_review("claude", Some("session-native-17")).await;

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

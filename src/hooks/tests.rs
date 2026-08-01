#[cfg(target_os = "linux")]
use super::spawn_reaped_hook_child;
#[cfg(unix)]
use super::{daemon_tool_json, run_with_test_env_lock};
use super::{hook_route_metadata_from_event, parse_daemon_tool_json_content};

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
            daemon_tool_json(None, "tracedecay_fact_store", serde_json::json!({})),
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

#[cfg(target_os = "linux")]
#[test]
fn detached_hook_child_is_reaped_after_exit() {
    let mut command = std::process::Command::new("sh");
    command.arg("-c").arg("exit 0");
    let pid = spawn_reaped_hook_child(command, b"").expect("spawn disposable hook child");
    let process_path = std::path::PathBuf::from(format!("/proc/{pid}"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while process_path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        !process_path.exists(),
        "the exited hook child remained as an unreaped process"
    );
}

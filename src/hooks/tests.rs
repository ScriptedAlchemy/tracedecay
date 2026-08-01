use super::{
    hook_route_metadata_from_event, parse_daemon_tool_json_content, spawn_reaped_hook_child,
};

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

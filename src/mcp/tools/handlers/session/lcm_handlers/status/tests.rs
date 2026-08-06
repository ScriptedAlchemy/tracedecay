use std::path::Path;

use tempfile::TempDir;

use super::super::super::*;
use super::super::shared::lcm_status_payload;
use super::super::test_support::*;
use super::*;

#[tokio::test]
async fn non_read_only_doctor_controls_are_rejected_before_storage_open() {
    for (args, field) in [
        (
            json!({"provider": "claude", "mode": false, "format": "json"}),
            "mode",
        ),
        (
            json!({"provider": "claude", "mode": "repair", "format": "json"}),
            "mode",
        ),
        (
            json!({"provider": "claude", "apply": "yes", "format": "json"}),
            "apply",
        ),
    ] {
        let error = handle_lcm_doctor(
            LcmHandlerContext::user(Path::new("/missing"), None, None),
            args,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains(field), "{error}");
    }

    let error = handle_lcm_status(
        LcmHandlerContext::user(Path::new("/missing"), None, None),
        json!({"gc_config": {}, "format": "json"}),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("gc_config"), "{error}");
}

#[tokio::test]
async fn status_without_registered_authority_is_fast_and_does_not_open_a_store() {
    let temp = TempDir::new().unwrap();
    let missing = temp.path().join("sessions.db");
    let started = std::time::Instant::now();
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        handle_lcm_status(
            LcmHandlerContext::user(&missing, None, None),
            json!({"format": "json"}),
        ),
    )
    .await
    .expect("unavailable LCM status should finish within the fast-path budget")
    .expect("unavailable LCM status is a typed response");
    eprintln!("unavailable LCM status latency: {:?}", started.elapsed());
    assert_eq!(payload(response)["status"], "unavailable");
    assert!(!missing.exists());
}

#[test]
fn status_envelope_preserves_exact_json_and_markdown_rendering() {
    let status = json!({
        "raw_message_count": 12,
        "payload": {"externalized_count": 2}
    });
    let expected = json!({
        "status": "ok",
        "provider": "all",
        "session_id": "session-1",
        "deep": true,
        "lcm": status,
    });
    let value = lcm_status_payload("all", Some("session-1"), true, status);
    assert_eq!(value, expected);

    let json_result = tool_json(None, &json!({"format": "json"}), &value);
    assert_eq!(payload(json_result), expected);

    let markdown_result = tool_json(None, &json!({"format": "markdown"}), &value);
    let markdown = markdown_result.value["content"][0]["text"]
        .as_str()
        .expect("markdown tool result text");
    assert_eq!(markdown, crate::mcp::tools::render::generic_md(&expected));
}

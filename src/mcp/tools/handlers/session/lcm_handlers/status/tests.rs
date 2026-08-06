use std::path::Path;

use super::super::super::*;
use super::super::shared::lcm_status_payload;
use super::super::test_support::*;
use super::*;

#[tokio::test]
async fn doctor_requires_a_specific_provider_before_storage_open() {
    for (args, field) in [
        (json!({"format": "json"}), "provider"),
        (json!({"provider": "all", "format": "json"}), "provider"),
    ] {
        let error = handle_lcm_doctor(
            LcmHandlerContext::user(Path::new("/missing"), None, None),
            args,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains(field), "{error}");
    }
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

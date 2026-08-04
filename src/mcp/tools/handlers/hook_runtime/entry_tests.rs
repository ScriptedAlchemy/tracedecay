use super::*;

#[test]
fn required_str_rejects_missing_and_empty_values() {
    assert!(required_str(&json!({}), "action").is_err());
    assert!(required_str(&json!({ "action": "" }), "action").is_err());
    assert_eq!(
        required_str(&json!({ "action": "reset_counter" }), "action").unwrap(),
        "reset_counter"
    );
}

#[test]
fn projectless_runtime_rejects_project_database_actions() {
    assert!(!projectless_action_allowed("reset_counter", &json!({})));
    assert!(!projectless_action_allowed(
        "ingest_transcript",
        &json!({ "user_scope": false }),
    ));
    assert!(projectless_action_allowed(
        "ingest_transcript",
        &json!({ "user_scope": true }),
    ));
}

#[test]
fn session_authority_roles_fail_closed_independently() {
    let none = SessionAuthorities::default();
    assert!(required_project_db(none).is_err());
    assert!(required_user_db(none).is_err());
}

#[test]
fn cursor_event_redacts_host_content_before_durable_admission() {
    let event = json!({
        "session_id": "cursor-session-1",
        "transcript_path": "/tmp/cursor-session-1.jsonl",
        "prompt": "do not persist this prompt",
        "command": "do not persist this command",
        "edits": [{ "new_string": "do not persist this edit" }],
        "messages_to_compact": 12,
    });

    let queued = cursor_event::sanitize_cursor_event("preCompact", &event).unwrap();
    let persisted = serde_json::to_value(queued).unwrap();

    assert_eq!(persisted["event_name"], "preCompact");
    assert_eq!(persisted["event"]["session_id"], "cursor-session-1");
    assert!(persisted["event"].get("prompt").is_none());
    assert!(persisted["event"].get("command").is_none());
    assert!(persisted["event"].get("edits").is_none());

    let queued = cursor_event::sanitize_cursor_event("preCompact", &event).unwrap();
    let plan = crate::mcp::hook_events::HookEventPlan::CursorEvent(queued);
    let encoded = crate::mcp::hook_events::encode_durable_hook_event_plan(&plan).unwrap();
    assert_eq!(
        crate::mcp::hook_events::decode_durable_hook_event_plan(&encoded).unwrap(),
        plan
    );
}

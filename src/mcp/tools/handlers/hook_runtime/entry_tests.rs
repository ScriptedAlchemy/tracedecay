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
    assert!(!projectless_action_allowed(
        "claude_compact",
        &json!({ "user_scope": false }),
    ));
    assert!(projectless_action_allowed(
        "claude_compact",
        &json!({ "user_scope": true }),
    ));
}

#[test]
fn claude_compaction_requires_native_event_before_ingest() {
    let args = json!({
        "event_json": serde_json::to_string(&json!({
            "hook_event_name": "PreCompact",
            "trigger": "manual",
            "session_id": "session-1",
            "transcript_path": "/tmp/session-1.jsonl",
            "compact_summary": "summary"
        })).unwrap()
    });

    assert!(claude_compact_ingest_args(&args, false).is_err());
}

#[test]
fn incomplete_claude_transcript_is_typed_unavailable() {
    let outcome = incomplete_claude_compaction_source(&json!({
        "messages_upserted": 7,
        "deferred_sources": 1
    }));

    assert_eq!(outcome["status"], "unavailable");
    assert_eq!(outcome["reason"], "canonical_transcript_incomplete");
    assert_eq!(outcome["messages_upserted"], 7);
    assert_eq!(outcome["deferred_sources"], 1);
}

#[test]
fn session_authority_roles_fail_closed_independently() {
    let none = SessionAuthorities::default();
    assert!(required_project_db(none).is_err());
    assert!(required_user_db(none).is_err());
}

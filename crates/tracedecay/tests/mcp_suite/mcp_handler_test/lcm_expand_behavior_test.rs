//! Caller-visible `tracedecay_lcm_expand` behavior on the real MCP `tools/call` path.
//!
//! These tests send the same JSON-RPC request a host sends and compare the
//! text the host reads with literals. They do not inspect store tables or
//! which helper ran.

#![cfg(feature = "test-transport")]

use crate::support::{
    activate_test_temporal_generation, extract_real_server_text, handle_real_server_tool_call,
    handle_real_server_tool_call_raw, lcm_raw_store_id, open_active_project_session_db,
    real_mcp_server, seed_temporal_lcm_session_message, setup_empty_project,
};
use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_lcm::{LcmSourceRef, LcmSummaryNodeDraft};
use tracedecay_sessions::admission::HostAdmissionScope;

const BODY: &str = "orchard dispatch marker alpha-brass-seven";
/// Offset 8 skips `orchard `; the next 16 characters are this window,
/// including the trailing space after `marker`.
const WINDOW: &str = "dispatch marker ";
const SESSION: &str = "expand-proof-session";
const MESSAGE: &str = "expand-proof-message";
const SUMMARY_TEXT: &str = "alpha-brass summary of the orchard dispatch";
const NOT_FOUND: &str = "The requested resource was not found or is not authorized";
const INVALID: &str = "The retained operation request is invalid.";

async fn expand(server: &McpServer, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_lcm_expand", arguments).await;
    serde_json::from_str(extract_real_server_text(&result)).expect("expand JSON")
}

fn assert_problem(payload: &Value, kind: &str, code: &str, message: &str) {
    assert_eq!(payload["problem"]["kind"], kind, "{payload}");
    assert_eq!(payload["problem"]["code"], code, "{payload}");
    assert_eq!(payload["problem"]["message"], message, "{payload}");
    assert!(payload.get("expansion").is_none(), "{payload}");
}

#[tokio::test]
async fn lcm_expand_returns_the_seeded_message_and_refuses_the_wrong_target() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let projection = seed_temporal_lcm_session_message(&cg, SESSION, MESSAGE, BODY, 1).await;
    let store_id = lcm_raw_store_id(&cg, MESSAGE).await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, SESSION, vec![projection]).await;
    let server = real_mcp_server(cg).await;

    let full = expand(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "raw_message", "store_id": store_id}
        }),
    )
    .await;
    assert_eq!(full["status"], "partial", "{full}");
    assert_eq!(full["provider"], "cursor");
    assert_eq!(full["session_id"], SESSION);
    assert_eq!(full["grain"], "occurrence");
    assert_eq!(full["state"], "available");
    assert_eq!(full["omitted"], 1);
    assert_eq!(full["retrieval"]["outcome"], "partial");
    assert_eq!(full["expansion"]["kind"], "raw_message");
    assert_eq!(full["expansion"]["content"], BODY);
    assert_eq!(full["expansion"]["from_current_session"], true);
    assert_eq!(full["expansion"]["content_range"]["offset"], 0);
    assert_eq!(full["expansion"]["content_range"]["returned_chars"], 41);
    assert_eq!(full["expansion"]["content_range"]["total_chars"], 41);
    assert_eq!(full["expansion"]["content_range"]["truncated"], false);
    assert_eq!(full["expansion"]["raw_message"]["message_id"], MESSAGE);
    assert_eq!(full["expansion"]["raw_message"]["content"], BODY);
    assert_eq!(
        full["expansion"]["raw_message"]["store_id"],
        json!(store_id)
    );

    let by_message = expand(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "canonical_occurrence", "message_id": MESSAGE}
        }),
    )
    .await;
    assert_eq!(by_message["status"], "partial", "{by_message}");
    assert_eq!(by_message["expansion"]["content"], BODY);
    assert_eq!(
        by_message["expansion"]["raw_message"]["message_id"],
        MESSAGE
    );
    assert_eq!(
        by_message["expansion"]["raw_message"]["store_id"],
        json!(store_id)
    );
    assert_eq!(full["expansion"]["raw_message"]["role"], "assistant");
    assert_eq!(full["expansion"]["raw_message"]["session_id"], SESSION);
    assert_eq!(full["expansion"]["raw_message"]["provider"], "cursor");
    assert_eq!(full["expansion"]["raw_message"]["storage_kind"], "inline");

    let window = expand(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "raw_message", "store_id": store_id},
            "content_offset": 8,
            "content_limit": 16
        }),
    )
    .await;
    assert_eq!(window["status"], "partial", "{window}");
    assert_eq!(window["expansion"]["kind"], "raw_message");
    assert_eq!(window["expansion"]["content"], WINDOW);
    assert_eq!(window["expansion"]["raw_message"]["content"], WINDOW);
    assert_eq!(window["expansion"]["raw_message"]["message_id"], MESSAGE);
    assert_eq!(window["expansion"]["raw_message"]["store_id"], store_id);
    assert_eq!(window["expansion"]["from_current_session"], true);
    assert_eq!(window["expansion"]["content_range"]["offset"], 8);
    assert_eq!(window["expansion"]["content_range"]["limit"], 16);
    assert_eq!(window["expansion"]["content_range"]["returned_chars"], 16);
    assert_eq!(window["expansion"]["content_range"]["total_chars"], 41);
    assert_eq!(window["expansion"]["content_range"]["truncated"], true);

    let past_end = expand(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "canonical_occurrence", "message_id": MESSAGE},
            "content_offset": 100,
            "content_limit": 16
        }),
    )
    .await;
    assert_eq!(past_end["expansion"]["content"], "");
    assert_eq!(past_end["expansion"]["content_range"]["offset"], 41);
    assert_eq!(past_end["expansion"]["content_range"]["returned_chars"], 0);
    assert_eq!(past_end["expansion"]["content_range"]["total_chars"], 41);
    assert_eq!(past_end["expansion"]["content_range"]["truncated"], true);
    assert_eq!(past_end["expansion"]["raw_message"]["content"], "");

    let missing = expand(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "canonical_occurrence", "message_id": "missing-expand-message"}
        }),
    )
    .await;
    assert_problem(
        &missing,
        "not_found_or_not_authorized",
        "not_found_or_not_authorized",
        NOT_FOUND,
    );
    assert!(missing["problem"]["diagnostic"].is_null(), "{missing}");

    let wrong_provider = expand(
        &server,
        json!({
            "provider": "codex",
            "session_id": SESSION,
            "target": {"kind": "canonical_occurrence", "message_id": MESSAGE}
        }),
    )
    .await;
    assert_problem(
        &wrong_provider,
        "not_found_or_not_authorized",
        "not_found_or_not_authorized",
        NOT_FOUND,
    );
    assert!(
        !wrong_provider.to_string().contains(BODY),
        "a wrong provider must not receive the cursor body: {wrong_provider}"
    );

    let over_limit = expand(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "canonical_occurrence", "message_id": MESSAGE},
            "content_limit": 8193
        }),
    )
    .await;
    assert_eq!(
        over_limit["problem"]["kind"], "invalid_request",
        "{over_limit}"
    );
    assert_eq!(
        over_limit["problem"]["code"],
        "application.retained.invalid-request"
    );
    assert_eq!(over_limit["problem"]["message"], INVALID);
    assert_eq!(
        over_limit["problem"]["diagnostic"]["code"],
        "application.retained.invalid-request"
    );
    assert_eq!(over_limit["problem"]["diagnostic"]["message"], INVALID);
    assert_eq!(
        over_limit["problem"]["legal_actions"],
        json!(["correct_request"])
    );
    assert_eq!(over_limit["problem"]["retry"], "never");
    assert!(over_limit.get("expansion").is_none(), "{over_limit}");

    let zero_limit = expand(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "canonical_occurrence", "message_id": MESSAGE},
            "content_limit": 0
        }),
    )
    .await;
    assert_eq!(
        zero_limit["problem"]["kind"], "invalid_request",
        "{zero_limit}"
    );
    assert_eq!(zero_limit["problem"]["message"], INVALID);

    let source_limit_on_message = expand(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "canonical_occurrence", "message_id": MESSAGE},
            "source_limit": 1
        }),
    )
    .await;
    assert_eq!(
        source_limit_on_message["problem"]["kind"], "invalid_request",
        "{source_limit_on_message}"
    );
    assert_eq!(source_limit_on_message["problem"]["message"], INVALID);

    let missing_target = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_lcm_expand",
        json!({"provider": "cursor", "session_id": SESSION}),
    )
    .await;
    let missing_target_message = missing_target["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("missing-target rejection: {missing_target}"));
    assert!(
        missing_target_message.starts_with(
            "tool execution failed: config error: invalid retained application request for tracedecay_lcm_expand: missing field `target`"
        ),
        "{missing_target_message}"
    );
    assert_eq!(missing_target["error"]["code"], -32603);

    let unknown_field = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_lcm_expand",
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "canonical_occurrence", "message_id": MESSAGE},
            "not_a_field": true
        }),
    )
    .await;
    assert_eq!(
        unknown_field["error"]["message"],
        "tool execution failed: config error: invalid retained application request for tracedecay_lcm_expand: not_a_field: unknown field `not_a_field`, expected one of `provider`, `session_id`, `target`, `content_offset`, `content_limit`, `source_limit`, `cursor`, `format`"
    );
    assert_eq!(unknown_field["error"]["code"], -32603);

    server.shutdown().await;
}

#[tokio::test]
async fn lcm_expand_returns_summary_text_and_the_source_body() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let projection = seed_temporal_lcm_session_message(&cg, SESSION, MESSAGE, BODY, 1).await;
    let store_id = lcm_raw_store_id(&cg, MESSAGE).await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, SESSION, vec![projection]).await;
    let summary = db
        .lcm_insert_summary_node_for_test(
            HostAdmissionScope::Project,
            LcmSummaryNodeDraft {
                provider: "cursor".to_string(),
                conversation_id: SESSION.to_string(),
                session_id: SESSION.to_string(),
                depth: 0,
                summary_text: SUMMARY_TEXT.to_string(),
                source_refs: vec![LcmSourceRef::RawMessage { store_id }],
                source_token_count: 9,
                summary_token_count: 6,
                source_time_start: Some(1),
                source_time_end: Some(1),
                expand_hint: Some("expand the orchard dispatch".to_string()),
                metadata_json: None,
            },
        )
        .await
        .expect("summary publication");
    let summary_id = summary.node_id;
    db.poison_lcm_raw_projection_for_test(
        HostAdmissionScope::Project,
        store_id,
        "projection poison",
    )
    .await
    .expect("legacy projection poison");
    let server = real_mcp_server(cg).await;

    let expanded = expand(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "summary_node", "node_id": summary_id}
        }),
    )
    .await;
    assert_eq!(expanded["status"], "ok", "{expanded}");
    assert_eq!(expanded["grain"], "summary");
    assert_eq!(expanded["state"], "available");
    assert_eq!(expanded["provider"], "cursor");
    assert_eq!(expanded["session_id"], SESSION);
    assert_eq!(expanded["expansion"]["kind"], "summary_node");
    assert_eq!(expanded["expansion"]["content"], SUMMARY_TEXT);
    assert_eq!(expanded["expansion"]["summary_node"]["node_id"], summary_id);
    assert_eq!(
        expanded["expansion"]["summary_node"]["summary_text"],
        SUMMARY_TEXT
    );
    assert_eq!(
        expanded["expansion"]["summary_node"]["expand_hint"],
        "expand the orchard dispatch"
    );
    assert_eq!(expanded["expansion"]["summary_sources"][0]["content"], BODY);
    assert_eq!(
        expanded["expansion"]["summary_sources"][0]["state"],
        "available"
    );
    assert_eq!(
        expanded["expansion"]["summary_sources"][0]["raw_message"]["content"],
        BODY
    );
    assert_eq!(
        expanded["expansion"]["summary_sources"][0]["raw_message"]["message_id"],
        MESSAGE
    );
    assert_eq!(
        expanded["expansion"]["source_pagination"]["returned_sources"],
        1
    );
    assert_eq!(
        expanded["expansion"]["source_pagination"]["total_sources"],
        1
    );
    assert_eq!(
        expanded["expansion"]["source_pagination"]["has_more"],
        false
    );
    assert_eq!(
        expanded["expansion"]["source_pagination"]["remaining_sources"],
        0
    );

    server.shutdown().await;
}

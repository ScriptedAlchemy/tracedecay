//! Caller-visible behavior of `tracedecay_lcm_describe` over real MCP.
//!
//! Each case is a JSON-RPC `tools/call`, the same request a host sends. The
//! expected documents are literals of what that call returns. Wall-clock
//! `created_at` values and project-scoped anchor ids are removed before the
//! comparison because a fresh project mints them; every other field the caller
//! reads is pinned.

use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_lcm::{LcmSourceRef, LcmSummaryNodeDraft};
use tracedecay_sessions::admission::HostAdmissionScope;

use crate::support::{
    activate_test_temporal_generation, extract_real_server_text, handle_real_server_tool_call,
    handle_real_server_tool_call_raw, open_active_project_session_db, real_mcp_server,
    seed_temporal_lcm_session_message, seed_temporal_lcm_tool_result_message, setup_empty_project,
};

const SESSION: &str = "orchard-describe";
const SOURCE_ID: &str = "orchard-source";
const SOURCE_BODY: &str = "orchard source the caller can read";
const TOOL_ID: &str = "orchard-tool";
const SECRET: &str = "orchard-secret-the-caller-must-not-read";
const SUMMARY: &str = "orchard summary the caller must not read";
const HINT: &str = "orchard describe hint";
const CONVERSATION: &str = "orchard-conversation";

#[tokio::test]
async fn tracedecay_lcm_describe_reports_shape_without_bodies() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let source_projection =
        seed_temporal_lcm_session_message(&cg, SESSION, SOURCE_ID, SOURCE_BODY, 1).await;
    let external_body = format!("{SECRET} {}", "payload ".repeat(40_000));
    let external_projection =
        seed_temporal_lcm_tool_result_message(&cg, SESSION, TOOL_ID, external_body, 2).await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, SESSION, vec![source_projection, external_projection])
        .await;
    let source = db
        .lcm_load_raw_message_for_test("cursor", SOURCE_ID)
        .await
        .expect("source raw message");
    let external = db
        .lcm_load_raw_message_for_test("cursor", TOOL_ID)
        .await
        .expect("external raw message");
    let payload_ref = external.payload_ref.expect("externalized payload ref");
    let summary = db
        .lcm_insert_summary_node_for_test(
            HostAdmissionScope::Project,
            LcmSummaryNodeDraft {
                provider: "cursor".to_string(),
                conversation_id: CONVERSATION.to_string(),
                session_id: SESSION.to_string(),
                depth: 0,
                summary_text: SUMMARY.to_string(),
                source_refs: vec![LcmSourceRef::RawMessage {
                    store_id: source.store_id,
                }],
                source_token_count: 30,
                summary_token_count: 5,
                source_time_start: Some(1_700_000_000),
                source_time_end: Some(1_700_000_120),
                expand_hint: Some(HINT.to_string()),
                metadata_json: None,
            },
        )
        .await
        .expect("summary node");
    let server = real_mcp_server(cg).await;

    let omitted_target = describe(
        &server,
        json!({"provider": "cursor", "session_id": SESSION}),
    )
    .await;
    let explicit_session = describe(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "session"}
        }),
    )
    .await;
    let summary_node = describe(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "summary_node", "node_id": summary.node_id}
        }),
    )
    .await;
    let external_payload = describe(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "external_payload", "payload_ref": payload_ref}
        }),
    )
    .await;
    let missing_node = describe(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "summary_node", "node_id": "sum_missing"}
        }),
    )
    .await;
    let missing_payload = describe(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "external_payload", "payload_ref": "payload_missing.payload"}
        }),
    )
    .await;
    let foreign_payload = describe(
        &server,
        json!({
            "provider": "cursor",
            "session_id": "other-session",
            "target": {"kind": "external_payload", "payload_ref": payload_ref}
        }),
    )
    .await;
    let traversal = describe(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "external_payload", "payload_ref": "../secret"}
        }),
    )
    .await;
    let ghost_session = describe(
        &server,
        json!({"provider": "cursor", "session_id": "ghost-session"}),
    )
    .await;
    let missing_provider = describe_raw(&server, json!({"session_id": SESSION})).await;
    let unknown_kind = describe_raw(
        &server,
        json!({
            "provider": "cursor",
            "session_id": SESSION,
            "target": {"kind": "nope"}
        }),
    )
    .await;

    let proof = json!({
        "source_store_id": source.store_id,
        "payload_ref": payload_ref,
        "node_id": summary.node_id,
        "omitted_target": omitted_target,
        "explicit_session": explicit_session,
        "summary_node": summary_node,
        "external_payload": external_payload,
        "missing_node": missing_node,
        "missing_payload": missing_payload,
        "foreign_payload": foreign_payload,
        "traversal": traversal,
        "ghost_session": ghost_session,
        "missing_provider": missing_provider,
        "unknown_kind": unknown_kind,
    });
    std::fs::write(
        "/tmp/lcm-describe-proof.json",
        serde_json::to_string_pretty(&proof).expect("proof json"),
    )
    .expect("proof file");

    assert_eq!(
        stable_caller_view(&omitted_target),
        stable_caller_view(&explicit_session),
        "omitting target is the session overview"
    );
    let rendered = serde_json::to_string(&proof).expect("rendered proof");
    assert!(
        rendered.contains(SOURCE_BODY),
        "the short source body is the preview the caller reads"
    );
    assert!(
        !rendered.contains(SECRET),
        "describe must not return the external payload body"
    );
    assert!(
        !rendered.contains(SUMMARY),
        "describe must not return the summary body"
    );

    assert_eq!(
        problem_without_ids(&missing_node),
        denied_problem(),
        "missing summary node: {missing_node}"
    );
    assert_eq!(
        problem_without_ids(&missing_payload),
        denied_problem(),
        "missing payload: {missing_payload}"
    );
    assert_eq!(
        problem_without_ids(&foreign_payload),
        denied_problem(),
        "foreign session must not confirm the payload exists: {foreign_payload}"
    );

    server.shutdown().await;
}

async fn describe(server: &McpServer, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_lcm_describe", arguments).await;
    serde_json::from_str(extract_real_server_text(&result)).expect("describe JSON")
}

async fn describe_raw(server: &Arc<McpServer>, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_lcm_describe", arguments).await
}

fn stable_caller_view(payload: &Value) -> Value {
    let mut view = payload.clone();
    strip_volatile(&mut view);
    view
}

fn strip_volatile(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("created_at");
            for child in object.values_mut() {
                strip_volatile(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_volatile(item);
            }
        }
        _ => {}
    }
}

fn problem_without_ids(payload: &Value) -> Value {
    let mut problem = payload["problem"].clone();
    if let Some(object) = problem.as_object_mut() {
        object.remove("request_id");
        object.remove("trace_id");
    }
    problem
}

fn denied_problem() -> Value {
    json!({
        "revision": 1,
        "kind": "not_found_or_not_authorized",
        "code": "not_found_or_not_authorized",
        "message": "The requested resource was not found or is not authorized",
        "diagnostic": null,
        "committed_receipt": null,
        "owning_layer": "application",
        "terminality": "pre_admission",
        "retryable": false,
        "retry": "never",
        "retry_scope": null,
        "retry_after_millis": null,
        "cancellation_stage": null,
        "unavailable_classification": null,
        "execution_failure_classification": null,
        "details": [],
        "legal_actions": [],
        "coverage": null
    })
}

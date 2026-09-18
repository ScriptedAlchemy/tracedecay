//! Caller-visible behavior of `tracedecay_lcm_describe` over real MCP.
//!
//! Each case is a JSON-RPC `tools/call`, the same request a host sends. The
//! documents below are the JSON that call returns. A fresh project mints the
//! wall-clock `created_at`, the opaque page cursor, retrieval-anchor digests,
//! the request id, and the temp project path; those are normalized to
//! sentinels after the test checks the relations that make them real (the
//! cursor changes between calls, the project path is the fixture root, and
//! the summary timestamp is the same number in the node, the lineage edge,
//! and the explanation). Every other field is a literal.

use std::sync::Arc;

use serde_json::{Map, Value, json};
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
const PAYLOAD_RECEIPT: &str = "{\"ingest_protection\":{\"sanitization_receipt\":{\"disposition\":\"accepted\",\"payload\":{\"byte_len\":320042,\"digest\":\"sha256:ccfa23abb8571e39ba59469046ac514cbf275aea1e5a7e6a4a3b7c68afa7063c\"},\"receipt\":{\"receipt_id\":\"privacy.lcm-payload.v1.4121c3116279db6dbc350121bc0f857b863228f1c257ff600d265c842a285f26\",\"sanitizer_version\":\"privacy.lcm-payload.v1\"},\"sensitivity\":\"non_sensitive\"}}}";

#[tokio::test]
async fn tracedecay_lcm_describe_reports_shape_without_bodies() {
    let (cg, _env, dir) = setup_empty_project().await;
    let project = dir.path().to_path_buf();
    let external_body = format!("{SECRET} {}", "payload ".repeat(40_000));
    let content_hash = tracedecay_lcm::util::sha256_hex(external_body.as_bytes());
    let payload_ref = format!(
        "payload_{}.payload",
        tracedecay_lcm::util::sha256_hex(
            format!("cursor\0{SESSION}\0{TOOL_ID}\0{content_hash}").as_bytes(),
        ),
    );
    let source_projection =
        seed_temporal_lcm_session_message(&cg, SESSION, SOURCE_ID, SOURCE_BODY, 1).await;
    let external_projection =
        seed_temporal_lcm_tool_result_message(&cg, SESSION, TOOL_ID, external_body, 2).await;
    let db = open_active_project_session_db(&cg).await;
    activate_test_temporal_generation(&db, SESSION, vec![source_projection, external_projection])
        .await;
    let source = db
        .lcm_load_raw_message_for_test("cursor", SOURCE_ID)
        .await
        .expect("source raw message");
    assert_eq!(
        source.store_id, 1,
        "the first row in a fresh store is store id 1"
    );
    let node_id = tracedecay_lcm::dag::summary_node_id(
        "cursor",
        SESSION,
        0,
        &[LcmSourceRef::RawMessage { store_id: 1 }],
        &tracedecay_lcm::util::sha256_hex(SUMMARY.as_bytes()),
    );
    db.lcm_insert_summary_node_for_test(
        HostAdmissionScope::Project,
        LcmSummaryNodeDraft {
            provider: "cursor".to_string(),
            conversation_id: CONVERSATION.to_string(),
            session_id: SESSION.to_string(),
            depth: 0,
            summary_text: SUMMARY.to_string(),
            source_refs: vec![LcmSourceRef::RawMessage { store_id: 1 }],
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
            "target": {"kind": "summary_node", "node_id": node_id}
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

    for page in [
        &omitted_target,
        &explicit_session,
        &summary_node,
        &external_payload,
        &ghost_session,
    ] {
        assert_eq!(
            page["temporal"]["authorized_root"].as_str(),
            project.to_str(),
            "describe names the project the host opened: {page}"
        );
    }
    let created_at = summary_node["description"]["summary_node"]["created_at"]
        .as_i64()
        .expect("summary created_at");
    assert_eq!(
        summary_node["lineage"][0]["knowledge_at"], created_at,
        "lineage knowledge time is the summary node's created_at"
    );
    assert_eq!(
        summary_node["temporal"]["explanations"][0]["summary"],
        format!("temporal rank 3999999 at {created_at}"),
        "the explanation quotes the same created_at"
    );
    assert_ne!(
        omitted_target["temporal"]["next_cursor"], explicit_session["temporal"]["next_cursor"],
        "two session pages mint different cursors"
    );

    assert_eq!(
        stable_document(&omitted_target),
        session_document(&node_id, &payload_ref),
        "omitting target is the session overview"
    );
    assert_eq!(
        stable_document(&explicit_session),
        session_document(&node_id, &payload_ref)
    );
    assert_eq!(
        stable_document(&summary_node),
        summary_node_document(&node_id)
    );
    assert_eq!(
        stable_document(&external_payload),
        external_payload_document(&payload_ref, &content_hash)
    );
    assert_eq!(stable_document(&ghost_session), ghost_document());
    for (label, denied) in [
        ("missing node", &missing_node),
        ("missing payload", &missing_payload),
        ("foreign session", &foreign_payload),
        ("path traversal", &traversal),
    ] {
        assert_eq!(
            stable_document(denied),
            denied_document(),
            "{label} must not confirm the target exists: {denied}"
        );
    }
    assert_eq!(missing_provider, missing_provider_error());
    assert_eq!(unknown_kind, unknown_kind_error());

    server.shutdown().await;
}

async fn describe(server: &McpServer, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_lcm_describe", arguments).await;
    serde_json::from_str(extract_real_server_text(&result)).expect("describe JSON")
}

async fn describe_raw(server: &Arc<McpServer>, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_lcm_describe", arguments).await
}

fn session_document(node_id: &str, payload_ref: &str) -> Value {
    json!({
        "description": {
            "external_payload": null,
            "external_payload_count": 1,
            "first_store_id": 1,
            "last_store_id": 2,
            "provider": "cursor",
            "raw_message_count": 2,
            "raw_messages": [
                {
                    "content_preview": "",
                    "content_range": {
                        "limit": 0,
                        "offset": 0,
                        "returned_chars": 0,
                        "total_chars": SOURCE_BODY.len(),
                        "truncated": true
                    },
                    "message_id": SOURCE_ID,
                    "payload_ref": null,
                    "role": "assistant",
                    "storage_kind": "inline",
                    "store_id": 1
                },
                {
                    "content_preview": "",
                    "content_range": {
                        "limit": 0,
                        "offset": 0,
                        "returned_chars": 0,
                        "total_chars": 180,
                        "truncated": true
                    },
                    "message_id": TOOL_ID,
                    "payload_ref": payload_ref,
                    "role": "tool",
                    "storage_kind": "external",
                    "store_id": 2
                }
            ],
            "session_id": SESSION,
            "session_token_estimate": 15,
            "summary_node": null,
            "summary_node_count": 1,
            "summary_nodes": [
                {
                    "conversation_id": CONVERSATION,
                    "created_at": "<created_at>",
                    "depth": 0,
                    "node_id": node_id,
                    "source_count": 1,
                    "summary_preview": ""
                }
            ],
            "target": "session"
        },
        "grain": "session",
        "lineage": [],
        "omitted": 2,
        "provider": "cursor",
        "retrieval": {
            "freshness": {"state": "fresh"},
            "omitted": 2,
            "outcome": "partial"
        },
        "session_id": SESSION,
        "state": "available",
        "status": "partial",
        "temporal": session_temporal()
    })
}

fn summary_node_document(node_id: &str) -> Value {
    json!({
        "description": {
            "external_payload": null,
            "external_payload_count": 1,
            "first_store_id": 1,
            "last_store_id": 2,
            "provider": "cursor",
            "raw_message_count": 2,
            "raw_messages": [],
            "session_id": SESSION,
            "summary_node": {
                "children": [
                    {
                        "expand_hint": null,
                        "node_id": null,
                        "role": "assistant",
                        "source_kind": "raw_message",
                        "source_ref": {"kind": "raw_message", "store_id": 1},
                        "source_token_count": null,
                        "storage_kind": "inline",
                        "store_id": 1,
                        "summary_token_count": null
                    }
                ],
                "conversation_id": CONVERSATION,
                "created_at": "<created_at>",
                "depth": 0,
                "expand_hint": HINT,
                "metadata_json": null,
                "node_id": node_id,
                "source_count": 1,
                "source_time_end": 1_700_000_120,
                "source_time_start": 1_700_000_000,
                "source_token_count": 30,
                "summary_token_count": 5
            },
            "summary_node_count": 1,
            "summary_nodes": [],
            "target": "summary_node"
        },
        "grain": "summary",
        "lineage": [
            {
                "authority": "immutable_summary",
                "authorized": true,
                "kind": "supports",
                "knowledge_at": "<created_at>",
                "object_anchor_id": "ANCHOR_0",
                "subject_anchor_id": "ANCHOR_1",
                "supporting_anchor_ids": []
            }
        ],
        "omitted": 0,
        "provider": "cursor",
        "retrieval": {
            "freshness": {"state": "fresh"},
            "outcome": "complete"
        },
        "session_id": SESSION,
        "state": "available",
        "status": "ok",
        "temporal": {
            "anchors": ["ANCHOR_1"],
            "authorized_root": "<project>",
            "coverage": {"hidden": 0, "redacted": 0, "unknown": 0, "visible": 1},
            "explanations": [
                {"anchor": "ANCHOR_1", "summary": "temporal rank 3999999 at <created_at>"}
            ],
            "next_cursor": null,
            "source_coverage": [source_coverage("current")],
            "watermarks": watermarks()
        }
    })
}

fn external_payload_document(payload_ref: &str, content_hash: &str) -> Value {
    json!({
        "description": {
            "external_payload": {
                "byte_count": 320_040,
                "char_count": 320_040,
                "content_hash": content_hash,
                "content_preview": "",
                "created_at": "<created_at>",
                "kind": "tool_result",
                "message_id": TOOL_ID,
                "metadata_json": PAYLOAD_RECEIPT,
                "payload_ref": payload_ref,
                "provider": "cursor",
                "session_id": SESSION
            },
            "external_payload_count": 1,
            "first_store_id": 1,
            "last_store_id": 2,
            "provider": "cursor",
            "raw_message_count": 2,
            "raw_messages": [],
            "session_id": SESSION,
            "summary_node": null,
            "summary_node_count": 1,
            "summary_nodes": [],
            "target": "external_payload"
        },
        "grain": "occurrence",
        "lineage": [],
        "omitted": 1,
        "provider": "cursor",
        "retrieval": {
            "freshness": {"state": "fresh"},
            "omitted": 1,
            "outcome": "partial"
        },
        "session_id": SESSION,
        "state": "available",
        "status": "partial",
        "temporal": {
            "anchors": ["ANCHOR_0"],
            "authorized_root": "<project>",
            "coverage": {"hidden": 0, "redacted": 0, "unknown": 1, "visible": 0},
            "explanations": [
                {"anchor": "ANCHOR_0", "summary": "temporal rank 3999999 at 3"}
            ],
            "next_cursor": null,
            "source_coverage": [source_coverage("current")],
            "watermarks": watermarks()
        }
    })
}

fn ghost_document() -> Value {
    json!({
        "description": {
            "external_payload": null,
            "external_payload_count": 0,
            "first_store_id": null,
            "last_store_id": null,
            "provider": "cursor",
            "raw_message_count": 0,
            "raw_messages": [],
            "session_id": "ghost-session",
            "session_token_estimate": 0,
            "summary_node": null,
            "summary_node_count": 0,
            "summary_nodes": [],
            "target": "session"
        },
        "grain": "session",
        "lineage": [],
        "omitted": 0,
        "provider": "cursor",
        "retrieval": {
            "freshness": {"state": "fresh"},
            "outcome": "complete"
        },
        "session_id": "ghost-session",
        "state": "available",
        "status": "ok",
        "temporal": {
            "anchors": [],
            "authorized_root": "<project>",
            "coverage": {"hidden": 0, "redacted": 0, "unknown": 0, "visible": 0},
            "explanations": [],
            "next_cursor": null,
            "watermarks": {
                "generation": 0,
                "index": 0,
                "projection": 0,
                "source": 0,
                "summary": 0
            }
        }
    })
}

fn denied_document() -> Value {
    json!({
        "contract": {
            "schema_id": "schema.application.retained.lcm-describe.result",
            "schema_revision": 1
        },
        "problem": {
            "cancellation_stage": null,
            "code": "not_found_or_not_authorized",
            "committed_receipt": null,
            "coverage": null,
            "details": [],
            "diagnostic": null,
            "execution_failure_classification": null,
            "kind": "not_found_or_not_authorized",
            "legal_actions": [],
            "message": "The requested resource was not found or is not authorized",
            "owning_layer": "application",
            "request_id": "<request>",
            "retry": "never",
            "retry_after_millis": null,
            "retry_scope": null,
            "retryable": false,
            "revision": 1,
            "terminality": "pre_admission",
            "trace_id": "<request>",
            "unavailable_classification": null
        },
        "request_id": "<request>"
    })
}

fn missing_provider_error() -> Value {
    json!({
        "error": {
            "code": -32603,
            "data": {
                "cli_fallback": "This tool is also available from the shell: `tracedecay tool lcm_describe ...` (`tracedecay tool lcm_describe --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly.",
                "tool": "tracedecay_lcm_describe"
            },
            "message": "tool execution failed: config error: invalid retained application request for tracedecay_lcm_describe: missing field `provider`"
        },
        "id": 1,
        "jsonrpc": "2.0"
    })
}

fn unknown_kind_error() -> Value {
    json!({
        "error": {
            "code": -32603,
            "data": {
                "cli_fallback": "This tool is also available from the shell: `tracedecay tool lcm_describe ...` (`tracedecay tool lcm_describe --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly.",
                "tool": "tracedecay_lcm_describe"
            },
            "message": "tool execution failed: config error: invalid retained application request for tracedecay_lcm_describe: target.kind: unknown variant `nope`, expected one of `session`, `summary_node`, `external_payload`"
        },
        "id": 1,
        "jsonrpc": "2.0"
    })
}

fn session_temporal() -> Value {
    json!({
        "anchors": ["ANCHOR_0"],
        "authorized_root": "<project>",
        "coverage": {"hidden": 0, "redacted": 0, "unknown": 2, "visible": 0},
        "explanations": [
            {"anchor": "ANCHOR_0", "summary": "temporal rank 1999999 at 3"}
        ],
        "next_cursor": "<cursor>",
        "source_coverage": [source_coverage("forensic")],
        "watermarks": watermarks()
    })
}

fn source_coverage(mode: &str) -> Value {
    json!({
        "committed_frontier": 3,
        "covered_intervals": [],
        "missing_intervals": [],
        "observed_frontier": 3,
        "reason": {"kind": "caught_up"},
        "request": {"mode": {"kind": mode}},
        "source_id": "orchard-describe:cursor",
        "state": "fresh",
        "target_watermark": 3
    })
}

fn watermarks() -> Value {
    json!({
        "generation": 3,
        "index": 3,
        "projection": 3,
        "source": 3,
        "summary": 1
    })
}

/// Replaces fields a fresh project mints, keeping their shape and the
/// equality of anchors that name the same digest.
fn stable_document(value: &Value) -> Value {
    let mut view = value.clone();
    blank_minted_fields(&mut view);
    let mut anchors = Vec::new();
    number_anchors(&mut view, &mut anchors);
    view
}

fn blank_minted_fields(value: &mut Value) {
    match value {
        Value::Object(object) => blank_object(object),
        Value::Array(items) => {
            for item in items {
                blank_minted_fields(item);
            }
        }
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn blank_object(object: &mut Map<String, Value>) {
    let keys = object.keys().cloned().collect::<Vec<_>>();
    for key in keys {
        match key.as_str() {
            "created_at" | "knowledge_at"
                if object
                    .get(&key)
                    .and_then(Value::as_i64)
                    .is_some_and(|stamp| stamp >= 1_000_000_000) =>
            {
                object.insert(key, json!("<created_at>"));
            }
            "authorized_root" if object.get(&key).and_then(Value::as_str).is_some() => {
                object.insert(key, json!("<project>"));
            }
            "next_cursor" if object.get(&key).and_then(Value::as_str).is_some() => {
                object.insert(key, json!("<cursor>"));
            }
            "request_id" | "trace_id" if object.get(&key).and_then(Value::as_str).is_some() => {
                object.insert(key, json!("<request>"));
            }
            _ => {
                if let Some(child) = object.get_mut(&key) {
                    blank_minted_fields(child);
                }
            }
        }
    }
}

fn number_anchors(value: &mut Value, anchors: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if let Some(child) = object.get_mut(&key) {
                    number_anchors(child, anchors);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                number_anchors(item, anchors);
            }
        }
        Value::String(text) => rewrite_anchor_text(text, anchors),
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

fn rewrite_anchor_text(text: &mut String, anchors: &mut Vec<String>) {
    if let Some(rest) = text.strip_prefix("temporal rank ")
        && let Some((rank, at)) = rest.rsplit_once(" at ")
        && at.parse::<i64>().is_ok_and(|stamp| stamp >= 1_000_000_000)
    {
        *text = format!("temporal rank {rank} at <created_at>");
        return;
    }
    if text.starts_with("retrieval.v2.sha256:") {
        let index = anchors
            .iter()
            .position(|anchor| anchor == text)
            .unwrap_or_else(|| {
                anchors.push(text.clone());
                anchors.len() - 1
            });
        *text = format!("ANCHOR_{index}");
    }
}

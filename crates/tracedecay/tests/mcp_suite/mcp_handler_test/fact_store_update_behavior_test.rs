#![cfg(feature = "test-transport")]

//! Behavioral proof of `tracedecay_fact_store_update` on the production MCP
//! server. Add seeds the row and get reads it back; every assertion is the
//! update a caller observes, including refusals that must not replace it.

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call_raw, production_composition_fixture,
};

const SEEDED_CONTENT: &str = "Project Phoenix ships on the first of the month";
const REWRITTEN_CONTENT: &str = "Project Phoenix uses deterministic Amari Memory";
const REVIEWED_CONTENT: &str = "Project Phoenix uses deterministic Amari Memory after review";
const STALE_CONTENT: &str = "This stale write must not land";

const SORTED_TAGS: &[&str] = &["holographic", "memory"];
const SORTED_ENTITIES: &[&str] = &["Amari Memory", "Project Phoenix"];

fn success_envelope(response: &Value, tool: &str) -> Value {
    assert!(
        response["error"].is_null(),
        "{tool} returned a JSON-RPC error: {response}"
    );
    assert_ne!(
        response["result"]["isError"],
        json!(true),
        "{tool} returned a semantic error: {response}"
    );
    let text = extract_real_server_text(&response["result"]);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON: {error}: {text}"))
}

fn retained_payload(response: &Value, tool: &str) -> Value {
    let envelope = success_envelope(response, tool);
    envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("{tool} omitted its payload: {envelope}"))
}

fn assert_problem(response: &Value, expected: Value) {
    assert!(
        response["error"].is_null(),
        "typed refusals stay on the tool result, not a JSON-RPC error: {response}"
    );
    assert_eq!(
        response["result"]["isError"],
        json!(true),
        "refused update must set MCP isError: {response}"
    );
    let envelope = success_text(response);
    let problem = &envelope["problem"];
    assert_eq!(problem["kind"], expected["kind"], "{problem}");
    assert_eq!(problem["code"], expected["code"], "{problem}");
    assert_eq!(problem["message"], expected["message"], "{problem}");
    assert_eq!(problem["retry"], expected["retry"], "{problem}");
    assert_eq!(
        problem["legal_actions"], expected["legal_actions"],
        "{problem}"
    );
    assert_eq!(problem["diagnostic"], expected["diagnostic"], "{problem}");
    assert_eq!(
        response["result"]["structuredContent"]["problem"]["kind"], problem["kind"],
        "the structured MCP problem must match the text envelope: {response}"
    );
    assert_eq!(
        response["result"]["structuredContent"]["problem"]["message"], problem["message"],
        "the structured MCP problem must match the text envelope: {response}"
    );
}

fn success_text(response: &Value) -> Value {
    let text = extract_real_server_text(&response["result"]);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("invalid tool JSON: {error}: {text}"))
}

/// A well-formed id for this owner that was never stored. Flipping the last
/// identity nibble keeps the owner binding and rejects a non-canonical token
/// such as `fact.missing` before the store is consulted.
fn unknown_fact_id(known: &Value) -> String {
    let known = known
        .as_str()
        .unwrap_or_else(|| panic!("seeded fact id must be a string: {known}"));
    let mut id = known.to_owned();
    let last = id
        .pop()
        .unwrap_or_else(|| panic!("seeded fact id must be non-empty: {known}"));
    id.push(if last == 'a' { 'b' } else { 'a' });
    id
}

fn available_fact(projection: &Value) -> &Value {
    assert_eq!(projection["kind"], "available", "{projection}");
    projection
        .get("fact")
        .unwrap_or_else(|| panic!("available projection omitted its fact: {projection}"))
}

fn assert_fact_snapshot(projection: &Value, snapshot: &Value) {
    let fact = available_fact(projection);
    assert_eq!(fact["fact_id"], snapshot["fact_id"], "{fact}");
    assert_eq!(fact["content"], snapshot["content"], "{fact}");
    assert_eq!(fact["category"], snapshot["category"], "{fact}");
    assert_eq!(fact["tags"], snapshot["tags"], "{fact}");
    assert_eq!(fact["entities"], snapshot["entities"], "{fact}");
    assert_eq!(
        fact["trust_score_millionths"], snapshot["trust_score_millionths"],
        "{fact}"
    );
    assert_eq!(fact["metadata"], snapshot["metadata"], "{fact}");
    assert_eq!(fact["source_label"], snapshot["source_label"], "{fact}");
    assert_eq!(fact["source"]["kind"], "application", "{fact}");
    assert_eq!(
        fact["source"]["operation_id"], snapshot["operation_id"],
        "{fact}"
    );
    assert_eq!(fact["owner"]["kind"], "project", "{fact}");
    assert_eq!(
        fact["owner"]["project_id"], snapshot["project_id"],
        "{fact}"
    );
}

fn assert_committed_update(payload: &Value, fact_id: &Value, project_id: &Value, event_count: u64) {
    assert_eq!(payload["commit"]["disposition"], "committed", "{payload}");
    assert_eq!(payload["commit"]["fact_id"], *fact_id, "{payload}");
    assert_eq!(
        payload["commit"]["owner"],
        json!({"kind": "project", "project_id": project_id}),
        "{payload}"
    );
    let last_event_id = payload["commit"]["last_event_id"]
        .as_str()
        .unwrap_or_else(|| panic!("commit omitted last_event_id: {payload}"));
    assert!(
        !last_event_id.is_empty(),
        "commit last_event_id must be a durable id: {payload}"
    );
    assert_eq!(
        payload["fact"]["fact"]["last_event_id"],
        json!(last_event_id),
        "{payload}"
    );
    let active_assertion_id = payload["commit"]["active_assertion_id"]
        .as_str()
        .unwrap_or_else(|| panic!("commit omitted active_assertion_id: {payload}"));
    assert!(
        !active_assertion_id.is_empty(),
        "commit active_assertion_id must be a durable id: {payload}"
    );
    assert_eq!(
        payload["fact"]["fact"]["active_assertion_id"],
        json!(active_assertion_id),
        "{payload}"
    );
    let events = payload["commit"]["committed_event_ids"]
        .as_array()
        .unwrap_or_else(|| panic!("commit omitted event ids: {payload}"));
    assert_eq!(events.len() as u64, event_count, "{payload}");
    assert_eq!(
        events.last().cloned(),
        Some(payload["commit"]["last_event_id"].clone()),
        "{payload}"
    );
}

/// Update rewrites the supplied fields, keeps the fact id, and reports the
/// exact trust delta. A stale compare-and-swap token, an empty patch, blank
/// content, and an unknown id are refused and leave the stored fact alone.
#[tokio::test]
async fn fact_store_update_preserves_identity_and_rejects_stale_or_empty_writes() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production fact-store MCP server");

    let added = retained_payload(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_add",
            json!({
                "content": SEEDED_CONTENT,
                "category": "project",
                "tags": ["setup"],
                "entities": ["Setup Entity"],
                "source_label": "setup-label",
                "metadata": {"origin": "setup"}
            }),
        )
        .await,
        "tracedecay_fact_store_add",
    );
    assert_eq!(added["outcome"], "committed", "{added}");
    assert_eq!(added["result"]["disposition"], "added", "{added}");
    let seeded = available_fact(&added["result"]["fact"]);
    assert_eq!(seeded["content"], SEEDED_CONTENT, "{seeded}");
    assert_eq!(seeded["trust_score_millionths"], json!(500_000), "{seeded}");
    let fact_id = seeded["fact_id"].clone();
    let project_id = seeded["owner"]["project_id"].clone();
    let operation_id = seeded["source"]["operation_id"].clone();
    let seeded_event_id = seeded["last_event_id"].clone();
    let seeded_assertion_id = seeded["active_assertion_id"].clone();
    let seeded_created_at = seeded["telemetry"]["created_at"].clone();

    let rewritten = json!({
        "fact_id": fact_id,
        "project_id": project_id,
        "operation_id": operation_id,
        "content": REWRITTEN_CONTENT,
        "category": "decision",
        "tags": SORTED_TAGS,
        "entities": SORTED_ENTITIES,
        "trust_score_millionths": 800_000,
        "source_label": "operator-note",
        "metadata": {"lane": "proof", "updated": true}
    });
    let update_response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_store_update",
        json!({
            "fact_id": fact_id,
            "expected_last_event_id": seeded_event_id,
            "content": REWRITTEN_CONTENT,
            "category": "decision",
            "tags": ["memory", "holographic"],
            "entities": ["Project Phoenix", "Amari Memory"],
            "trust": 0.8,
            "source_label": {"kind": "set", "value": "operator-note"},
            "metadata": {"updated": true, "lane": "proof"}
        }),
    )
    .await;
    let update_envelope = success_envelope(&update_response, "tracedecay_fact_store_update");
    assert_eq!(
        update_envelope["outcome"]["outcome"], "effect",
        "{update_envelope}"
    );
    assert_eq!(
        update_envelope["outcome"]["value"]["effect_class"], "administrative",
        "{update_envelope}"
    );
    assert_eq!(
        update_envelope["outcome"]["value"]["reconciliation"], "reconciled",
        "{update_envelope}"
    );
    assert_eq!(
        update_envelope["outcome"]["value"]["receipt"]["outcome"], "completed",
        "{update_envelope}"
    );
    let updated = update_envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .expect("update payload");
    assert_eq!(
        updated["trust_delta_millionths"],
        json!(300_000),
        "{updated}"
    );
    assert_fact_snapshot(&updated["fact"], &rewritten);
    assert_committed_update(&updated, &fact_id, &project_id, 2);
    let rewritten_fact = available_fact(&updated["fact"]);
    assert_eq!(
        rewritten_fact["telemetry"]["created_at"], seeded_created_at,
        "update must not rewrite the fact's creation time: {rewritten_fact}"
    );
    assert_ne!(
        rewritten_fact["telemetry"]["updated_at"], seeded_created_at,
        "update must advance updated_at: {rewritten_fact}"
    );
    assert_eq!(
        rewritten_fact["telemetry"]["retrieval_count"],
        json!(0),
        "{rewritten_fact}"
    );
    assert_eq!(
        rewritten_fact["telemetry"]["access_count"],
        json!(0),
        "{rewritten_fact}"
    );
    assert_eq!(
        rewritten_fact["telemetry"]["helpful_count"],
        json!(0),
        "{rewritten_fact}"
    );
    assert_eq!(
        rewritten_fact["telemetry"]["unhelpful_count"],
        json!(0),
        "{rewritten_fact}"
    );
    assert!(
        rewritten_fact["telemetry"]["last_retrieved_at"].is_null(),
        "{rewritten_fact}"
    );
    assert!(
        rewritten_fact["telemetry"]["last_recalled_at"].is_null(),
        "{rewritten_fact}"
    );
    assert!(
        rewritten_fact["telemetry"]["last_feedback_at"].is_null(),
        "{rewritten_fact}"
    );
    assert_ne!(
        rewritten_fact["last_event_id"], seeded_event_id,
        "{rewritten_fact}"
    );
    assert_ne!(
        rewritten_fact["active_assertion_id"], seeded_assertion_id,
        "{rewritten_fact}"
    );

    let stored = retained_payload(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": fact_id}),
        )
        .await,
        "tracedecay_fact_store_get",
    );
    assert_fact_snapshot(&stored["fact"], &rewritten);

    let reviewed = json!({
        "fact_id": fact_id,
        "project_id": project_id,
        "operation_id": operation_id,
        "content": REVIEWED_CONTENT,
        "category": "decision",
        "tags": SORTED_TAGS,
        "entities": SORTED_ENTITIES,
        "trust_score_millionths": 800_000,
        "source_label": "operator-note",
        "metadata": {"lane": "proof", "updated": true}
    });
    let narrowed = retained_payload(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_update",
            json!({
                "fact_id": fact_id,
                "content": REVIEWED_CONTENT
            }),
        )
        .await,
        "tracedecay_fact_store_update",
    );
    assert_eq!(narrowed["trust_delta_millionths"], json!(0), "{narrowed}");
    assert_fact_snapshot(&narrowed["fact"], &reviewed);
    assert_committed_update(&narrowed, &fact_id, &project_id, 1);
    let reviewed_event_id = available_fact(&narrowed["fact"])["last_event_id"].clone();

    let cleared = json!({
        "fact_id": fact_id,
        "project_id": project_id,
        "operation_id": operation_id,
        "content": REVIEWED_CONTENT,
        "category": "decision",
        "tags": SORTED_TAGS,
        "entities": SORTED_ENTITIES,
        "trust_score_millionths": 800_000,
        "source_label": null,
        "metadata": {"lane": "proof", "updated": true}
    });
    let cleared_payload = retained_payload(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_update",
            json!({
                "fact_id": fact_id,
                "expected_last_event_id": reviewed_event_id,
                "source_label": {"kind": "clear"}
            }),
        )
        .await,
        "tracedecay_fact_store_update",
    );
    assert_eq!(
        cleared_payload["trust_delta_millionths"],
        json!(0),
        "{cleared_payload}"
    );
    assert_fact_snapshot(&cleared_payload["fact"], &cleared);
    assert_committed_update(&cleared_payload, &fact_id, &project_id, 1);

    let stale = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_store_update",
        json!({
            "fact_id": fact_id,
            "expected_last_event_id": seeded_event_id,
            "content": STALE_CONTENT
        }),
    )
    .await;
    assert_problem(
        &stale,
        json!({
            "kind": "conflict",
            "code": "application.retained.conflict",
            "message": "The retained operation conflicts with current state.",
            "retry": "after_revalidate",
            "legal_actions": ["refresh"],
            "diagnostic": {
                "code": "application.retained.conflict",
                "message": "The retained operation conflicts with current state."
            }
        }),
    );

    let empty = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_store_update",
        json!({"fact_id": fact_id}),
    )
    .await;
    assert_problem(
        &empty,
        json!({
            "kind": "invalid_request",
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid.",
            "retry": "never",
            "legal_actions": ["correct_request"],
            "diagnostic": {
                "code": "application.retained.invalid-request",
                "message": "The retained operation request is invalid."
            }
        }),
    );

    let blank = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_store_update",
        json!({
            "fact_id": fact_id,
            "content": "   "
        }),
    )
    .await;
    assert_problem(
        &blank,
        json!({
            "kind": "invalid_request",
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid.",
            "retry": "never",
            "legal_actions": ["correct_request"],
            "diagnostic": {
                "code": "application.retained.invalid-request",
                "message": "The retained operation request is invalid."
            }
        }),
    );

    let missing = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_store_update",
        json!({
            "fact_id": unknown_fact_id(&fact_id),
            "content": "no such fact"
        }),
    )
    .await;
    assert_problem(
        &missing,
        json!({
            "kind": "not_found_or_not_authorized",
            "code": "not_found_or_not_authorized",
            "message": "The requested resource was not found or is not authorized",
            "retry": "never",
            "legal_actions": [],
            "diagnostic": null
        }),
    );

    let untouched = retained_payload(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": fact_id}),
        )
        .await,
        "tracedecay_fact_store_get",
    );
    assert_fact_snapshot(&untouched["fact"], &cleared);
    assert_eq!(
        available_fact(&untouched["fact"])["content"],
        REVIEWED_CONTENT,
        "refused updates must leave the reviewed content stored: {untouched}"
    );

    fixture.harness.shutdown().await;
}

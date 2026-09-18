#![cfg(feature = "test-transport")]

//! Behavioral proof of `tracedecay_fact_feedback` on the production MCP server.
//! Add seeds the row and get reads the durable history back; every assertion is
//! the rating a caller observes, including refusals that must not change trust.

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call_raw, production_composition_fixture,
};

const SEEDED_CONTENT: &str = "Amari Memory starts feedback proofs at the default half trust";
const SEEDED_CATEGORY: &str = "decision";
const SEEDED_SOURCE: &str = "seed-label";
const REVIEWER: &str = "reviewer";
const REVIEW_REASON: &str = "matched the install path";

const DEFAULT_TRUST: u64 = 500_000;
const HELPFUL_TRUST: u64 = 550_000;
const HELPFUL_DELTA: i64 = 50_000;
const AFTER_UNHELPFUL_TRUST: u64 = 450_000;
const UNHELPFUL_DELTA: i64 = -100_000;

fn success_text(response: &Value) -> Value {
    let text = extract_real_server_text(&response["result"]);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("invalid tool JSON: {error}: {text}"))
}

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
    success_text(response)
}

fn retained_payload(response: &Value, tool: &str) -> Value {
    let envelope = success_envelope(response, tool);
    envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("{tool} omitted its payload: {envelope}"))
}

fn assert_effect(envelope: &Value) {
    assert_eq!(envelope["outcome"]["outcome"], "effect", "{envelope}");
    assert_eq!(
        envelope["outcome"]["value"]["effect_class"], "administrative",
        "{envelope}"
    );
    assert_eq!(
        envelope["outcome"]["value"]["reconciliation"], "reconciled",
        "{envelope}"
    );
    assert_eq!(
        envelope["outcome"]["value"]["receipt"]["outcome"], "completed",
        "{envelope}"
    );
}

fn available_fact(projection: &Value) -> &Value {
    assert_eq!(projection["kind"], "available", "{projection}");
    projection
        .get("fact")
        .unwrap_or_else(|| panic!("available projection omitted its fact: {projection}"))
}

fn assert_problem(response: &Value, expected: Value) {
    assert!(
        response["error"].is_null(),
        "typed refusals stay on the tool result, not a JSON-RPC error: {response}"
    );
    assert_eq!(
        response["result"]["isError"],
        json!(true),
        "refused feedback must set MCP isError: {response}"
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
        response["result"]["problem"]["kind"], problem["kind"],
        "the structured MCP problem must match the text envelope: {response}"
    );
    assert_eq!(
        response["result"]["problem"]["message"], problem["message"],
        "the structured MCP problem must match the text envelope: {response}"
    );
}

fn not_found_problem() -> Value {
    json!({
        "kind": "not_found_or_not_authorized",
        "code": "not_found_or_not_authorized",
        "message": "The requested resource was not found or is not authorized",
        "retry": "never",
        "legal_actions": [],
        "diagnostic": null
    })
}

fn conflict_problem() -> Value {
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
    })
}

fn assert_schema_refusal(response: &Value, detail: &str) {
    let message = format!(
        "tool execution failed: invalid retained application request for tracedecay_fact_feedback: {detail}"
    );
    assert!(
        response.get("result").is_none(),
        "schema refusals are JSON-RPC errors, not tool results: {response}"
    );
    assert_eq!(response["jsonrpc"], "2.0", "{response}");
    assert_eq!(response["id"], json!(1), "{response}");
    assert_eq!(response["error"]["code"], json!(-32603), "{response}");
    assert_eq!(response["error"]["message"], message, "{response}");
    assert_eq!(
        response["error"]["data"]["tool"], "tracedecay_fact_feedback",
        "{response}"
    );
}

fn assert_stored_identity(fact: &Value, fact_id: &Value, project_id: &Value, trust: u64) {
    assert_eq!(fact["fact_id"], *fact_id, "{fact}");
    assert_eq!(fact["content"], SEEDED_CONTENT, "{fact}");
    assert_eq!(fact["category"], SEEDED_CATEGORY, "{fact}");
    assert_eq!(fact["tags"], json!([]), "{fact}");
    assert_eq!(fact["entities"], json!([]), "{fact}");
    assert_eq!(fact["metadata"], json!({}), "{fact}");
    assert_eq!(fact["source_label"], SEEDED_SOURCE, "{fact}");
    assert_eq!(fact["source"]["kind"], "application", "{fact}");
    assert_eq!(fact["owner"]["kind"], "project", "{fact}");
    assert_eq!(fact["owner"]["project_id"], *project_id, "{fact}");
    assert_eq!(fact["trust_score_millionths"], json!(trust), "{fact}");
    assert_eq!(fact["telemetry"]["retrieval_count"], json!(0), "{fact}");
    assert_eq!(fact["telemetry"]["access_count"], json!(0), "{fact}");
    assert!(fact["telemetry"]["last_retrieved_at"].is_null(), "{fact}");
    assert!(fact["telemetry"]["last_recalled_at"].is_null(), "{fact}");
}

fn assert_rating(
    payload: &Value,
    fact_id: &Value,
    project_id: &Value,
    action: &str,
    old_trust: u64,
    new_trust: u64,
    delta: i64,
    helpful_count: u64,
    unhelpful_count: u64,
) {
    let feedback = &payload["feedback"];
    assert_eq!(feedback["action"], action, "{payload}");
    assert_eq!(feedback["fact_id"], *fact_id, "{payload}");
    assert_eq!(
        feedback["old_trust_millionths"],
        json!(old_trust),
        "{payload}"
    );
    assert_eq!(
        feedback["new_trust_millionths"],
        json!(new_trust),
        "{payload}"
    );
    assert_eq!(
        feedback["trust_delta_millionths"],
        json!(delta),
        "{payload}"
    );
    assert_eq!(feedback["helpful_count"], json!(helpful_count), "{payload}");
    assert_eq!(
        feedback["unhelpful_count"],
        json!(unhelpful_count),
        "{payload}"
    );
    let fact = available_fact(&payload["fact"]);
    assert_stored_identity(fact, fact_id, project_id, new_trust);
    assert_eq!(
        fact["telemetry"]["helpful_count"],
        json!(helpful_count),
        "{fact}"
    );
    assert_eq!(
        fact["telemetry"]["unhelpful_count"],
        json!(unhelpful_count),
        "{fact}"
    );
    assert_eq!(payload["commit"]["disposition"], "committed", "{payload}");
    assert_eq!(payload["commit"]["fact_id"], *fact_id, "{payload}");
    assert_eq!(payload["commit"]["owner"], fact["owner"], "{payload}");
    assert_eq!(
        payload["commit"]["last_event_id"], feedback["event_id"],
        "{payload}"
    );
    assert_eq!(
        payload["commit"]["last_event_id"], fact["last_event_id"],
        "{payload}"
    );
    assert_eq!(
        payload["commit"]["active_assertion_id"], fact["active_assertion_id"],
        "{payload}"
    );
    let events = payload["commit"]["committed_event_ids"]
        .as_array()
        .unwrap_or_else(|| panic!("commit omitted event ids: {payload}"));
    assert_eq!(events, &vec![feedback["event_id"].clone()], "{payload}");
}

/// Helpful feedback raises trust by exactly 50000 millionths and unhelpful
/// feedback lowers it by exactly 100000. A stale compare-and-swap token, a
/// missing or removed fact, another memory scope, and rejected request shapes
/// leave the stored fact on the last committed trust.
#[tokio::test]
async fn fact_feedback_rates_trust_and_refuses_the_other_inputs() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production fact-feedback MCP server");

    let added = retained_payload(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_add",
            json!({
                "content": SEEDED_CONTENT,
                "category": SEEDED_CATEGORY,
                "source_label": SEEDED_SOURCE
            }),
        )
        .await,
        "tracedecay_fact_store_add",
    );
    assert_eq!(added["outcome"], "committed", "{added}");
    assert_eq!(added["result"]["disposition"], "added", "{added}");
    let seeded = available_fact(&added["result"]["fact"]);
    assert_stored_identity(
        seeded,
        &seeded["fact_id"],
        &seeded["owner"]["project_id"],
        DEFAULT_TRUST,
    );
    assert_eq!(seeded["telemetry"]["helpful_count"], json!(0), "{seeded}");
    assert_eq!(seeded["telemetry"]["unhelpful_count"], json!(0), "{seeded}");
    assert!(
        seeded["telemetry"]["last_feedback_at"].is_null(),
        "{seeded}"
    );
    let fact_id = seeded["fact_id"].clone();
    let project_id = seeded["owner"]["project_id"].clone();
    let seeded_event_id = seeded["last_event_id"].clone();
    let seeded_assertion_id = seeded["active_assertion_id"].clone();
    let seeded_created_at = seeded["telemetry"]["created_at"].clone();

    assert_schema_refusal(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_feedback",
            json!({"fact_id": fact_id}),
        )
        .await,
        "missing field `action`",
    );
    assert_schema_refusal(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_feedback",
            json!({"fact_id": 41, "action": "helpful"}),
        )
        .await,
        "fact_id: invalid type: integer `41`, expected a string",
    );
    assert_schema_refusal(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_feedback",
            json!({"fact_id": fact_id, "helpful": true}),
        )
        .await,
        "unknown field `helpful`, expected `fact_id` or `expected_last_event_id` or `action` or `source_label` or `reason` or `memory_scope` or `project_selector`",
    );
    assert_schema_refusal(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_feedback",
            json!({"fact_id": fact_id, "action": "helpful", "source": "legacy"}),
        )
        .await,
        "unknown field `source`, expected `fact_id` or `expected_last_event_id` or `action` or `source_label` or `reason` or `memory_scope` or `project_selector`",
    );
    assert_schema_refusal(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_feedback",
            json!({"fact_id": fact_id, "action": "maybe"}),
        )
        .await,
        "action: unknown variant `maybe`, expected `helpful` or `unhelpful`",
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
    assert_eq!(untouched["trust_history"], json!([]), "{untouched}");
    let untouched_fact = available_fact(&untouched["fact"]);
    assert_stored_identity(untouched_fact, &fact_id, &project_id, DEFAULT_TRUST);
    assert_eq!(
        untouched_fact["last_event_id"], seeded_event_id,
        "{untouched}"
    );
    assert_eq!(
        untouched_fact["telemetry"]["helpful_count"],
        json!(0),
        "{untouched}"
    );
    assert_eq!(
        untouched_fact["telemetry"]["unhelpful_count"],
        json!(0),
        "{untouched}"
    );

    let helpful_response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": fact_id,
            "action": "helpful",
            "source_label": REVIEWER,
            "reason": REVIEW_REASON
        }),
    )
    .await;
    let helpful_envelope = success_envelope(&helpful_response, "tracedecay_fact_feedback");
    assert_effect(&helpful_envelope);
    let helpful = helpful_envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .expect("helpful feedback payload");
    assert_rating(
        &helpful,
        &fact_id,
        &project_id,
        "helpful",
        DEFAULT_TRUST,
        HELPFUL_TRUST,
        HELPFUL_DELTA,
        1,
        0,
    );
    let helpful_event_id = helpful["feedback"]["event_id"].clone();
    let helpful_fact = available_fact(&helpful["fact"]);
    let helpful_at = helpful_fact["telemetry"]["last_feedback_at"].clone();
    assert_eq!(
        helpful_fact["telemetry"]["created_at"], seeded_created_at,
        "{helpful_fact}"
    );
    assert_eq!(
        helpful_fact["active_assertion_id"], seeded_assertion_id,
        "feedback must not replace the fact assertion: {helpful_fact}"
    );
    assert_ne!(
        helpful_fact["last_event_id"], seeded_event_id,
        "{helpful_fact}"
    );

    let unhelpful_response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": fact_id,
            "expected_last_event_id": helpful_event_id,
            "action": "unhelpful"
        }),
    )
    .await;
    let unhelpful_envelope = success_envelope(&unhelpful_response, "tracedecay_fact_feedback");
    assert_effect(&unhelpful_envelope);
    let unhelpful = unhelpful_envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .expect("unhelpful feedback payload");
    assert_rating(
        &unhelpful,
        &fact_id,
        &project_id,
        "unhelpful",
        HELPFUL_TRUST,
        AFTER_UNHELPFUL_TRUST,
        UNHELPFUL_DELTA,
        1,
        1,
    );
    let unhelpful_event_id = unhelpful["feedback"]["event_id"].clone();
    let unhelpful_fact = available_fact(&unhelpful["fact"]);
    let rated_at = unhelpful_fact["telemetry"]["last_feedback_at"].clone();
    assert_eq!(
        unhelpful_fact["telemetry"]["created_at"], seeded_created_at,
        "{unhelpful_fact}"
    );
    assert_eq!(
        unhelpful_fact["active_assertion_id"], seeded_assertion_id,
        "{unhelpful_fact}"
    );
    assert_ne!(unhelpful_event_id, helpful_event_id, "{unhelpful}");

    let stored = retained_payload(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": fact_id}),
        )
        .await,
        "tracedecay_fact_store_get",
    );
    let stored_fact = available_fact(&stored["fact"]);
    assert_stored_identity(stored_fact, &fact_id, &project_id, AFTER_UNHELPFUL_TRUST);
    assert_eq!(
        stored_fact["telemetry"]["helpful_count"],
        json!(1),
        "{stored}"
    );
    assert_eq!(
        stored_fact["telemetry"]["unhelpful_count"],
        json!(1),
        "{stored}"
    );
    assert_eq!(
        stored_fact["telemetry"]["last_feedback_at"], rated_at,
        "{stored}"
    );
    assert_eq!(stored_fact["last_event_id"], unhelpful_event_id, "{stored}");
    let history = stored["trust_history"]
        .as_array()
        .unwrap_or_else(|| panic!("get omitted trust history: {stored}"));
    assert_eq!(
        history,
        &vec![
            json!({
                "event_id": helpful_event_id,
                "occurred_at": helpful_at,
                "action": "helpful",
                "old_trust_millionths": DEFAULT_TRUST,
                "new_trust_millionths": HELPFUL_TRUST,
                "source_label": REVIEWER,
                "reason": REVIEW_REASON,
                "details_availability": "available"
            }),
            json!({
                "event_id": unhelpful_event_id,
                "occurred_at": rated_at,
                "action": "unhelpful",
                "old_trust_millionths": HELPFUL_TRUST,
                "new_trust_millionths": AFTER_UNHELPFUL_TRUST,
                "source_label": null,
                "reason": null,
                "details_availability": "unknown"
            })
        ],
        "{stored}"
    );

    let stale = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": fact_id,
            "expected_last_event_id": seeded_event_id,
            "action": "helpful"
        }),
    )
    .await;
    assert_problem(&stale, conflict_problem());

    let missing = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_feedback",
        json!({"fact_id": "fact.missing", "action": "helpful"}),
    )
    .await;
    assert_problem(&missing, not_found_problem());

    let other_scope = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": fact_id,
            "action": "helpful",
            "memory_scope": "user"
        }),
    )
    .await;
    assert_problem(&other_scope, not_found_problem());

    let still_rated = retained_payload(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": fact_id}),
        )
        .await,
        "tracedecay_fact_store_get",
    );
    assert_stored_identity(
        available_fact(&still_rated["fact"]),
        &fact_id,
        &project_id,
        AFTER_UNHELPFUL_TRUST,
    );
    assert_eq!(
        still_rated["trust_history"][1]["event_id"], unhelpful_event_id,
        "refused feedback must not append history: {still_rated}"
    );

    let removed = retained_payload(
        &handle_real_server_tool_call_raw(
            &server,
            "tracedecay_fact_store_remove",
            json!({"fact_id": fact_id}),
        )
        .await,
        "tracedecay_fact_store_remove",
    );
    assert_eq!(removed["outcome"], "removed", "{removed}");

    let removed_feedback = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_feedback",
        json!({"fact_id": fact_id, "action": "helpful"}),
    )
    .await;
    assert_problem(&removed_feedback, not_found_problem());

    fixture.harness.shutdown().await;
}

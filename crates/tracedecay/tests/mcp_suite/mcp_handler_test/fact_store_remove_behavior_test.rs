#![cfg(feature = "test-transport")]

//! Caller-visible `tracedecay_fact_store_remove` behavior through the
//! production MCP `tools/call` path.
//!
//! Expected payloads, refusal records, and JSON-RPC messages are literals
//! this test owns. Generated fact and event identities are the handles the
//! caller received from the preceding call, not values read back out of the
//! assertion.

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call_raw, production_composition_fixture,
    retained_envelope_payload,
};

const REMOVED_CONTENT: &str = "Cerulean ledger stores the quay invoice under dock 17.";
const SURVIVOR_CONTENT: &str = "Amber kiln keeps the glaze recipe for the west firing.";
const SOURCE_LABEL: &str = "mcp-remove-proof";
const STALE_EVENT_ID: &str = "event.stale-remove-token";
const TOOL: &str = "tracedecay_fact_store_remove";
const REMOVE_RESULT_SCHEMA: &str = "schema.application.retained.fact-store-remove.result";

const MISSING_FACT_ID_MESSAGE: &str = "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_remove: missing field `fact_id`";
const NUMERIC_FACT_ID_MESSAGE: &str = "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_remove: fact_id: invalid type: integer `41`, expected a string";
const UNKNOWN_FIELD_MESSAGE: &str = "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_remove: unknown field `action`, expected `fact_id` or `expected_last_event_id` or `memory_scope` or `project_selector`";

struct AddedFact {
    fact_id: String,
    last_event_id: String,
    project_id: String,
}

enum ToolAnswer {
    Payload(Value),
    Problem(Value),
    Protocol {
        code: i64,
        message: String,
        tool: String,
    },
}

async fn call_tool(
    server: &tracedecay::mcp::McpServer,
    tool_name: &str,
    arguments: Value,
) -> ToolAnswer {
    let response = handle_real_server_tool_call_raw(server, tool_name, arguments).await;
    if !response["error"].is_null() {
        let error = &response["error"];
        return ToolAnswer::Protocol {
            code: error["code"]
                .as_i64()
                .unwrap_or_else(|| panic!("JSON-RPC error code: {response}")),
            message: error["message"]
                .as_str()
                .unwrap_or_else(|| panic!("JSON-RPC error message: {response}"))
                .to_owned(),
            tool: error["data"]["tool"]
                .as_str()
                .unwrap_or_else(|| panic!("JSON-RPC error tool: {response}"))
                .to_owned(),
        };
    }
    let result = &response["result"];
    let text = extract_real_server_text(result);
    if result.get("isError") == Some(&Value::Bool(true)) {
        let envelope: Value = serde_json::from_str(text)
            .unwrap_or_else(|error| panic!("{tool_name} problem is not JSON: {error}: {text}"));
        return ToolAnswer::Problem(envelope);
    }
    let payload = retained_envelope_payload(text)
        .unwrap_or_else(|| panic!("{tool_name} omitted its canonical payload: {text}"));
    ToolAnswer::Payload(payload)
}

fn payload(answer: ToolAnswer) -> Value {
    match answer {
        ToolAnswer::Payload(payload) => payload,
        ToolAnswer::Problem(problem) => panic!("expected a payload, got a problem: {problem}"),
        ToolAnswer::Protocol {
            code,
            message,
            tool,
        } => {
            panic!("expected a payload, got JSON-RPC {code} from {tool}: {message}")
        }
    }
}

fn assert_protocol_error(answer: ToolAnswer, message: &str) {
    match answer {
        ToolAnswer::Protocol {
            code,
            message: actual,
            tool,
        } => {
            assert_eq!(code, -32603, "{actual}");
            assert_eq!(tool, TOOL);
            assert_eq!(actual, message);
        }
        ToolAnswer::Payload(payload) => {
            panic!("expected a protocol error, got a payload: {payload}")
        }
        ToolAnswer::Problem(problem) => {
            panic!("expected a protocol error, got a problem: {problem}")
        }
    }
}

fn assert_remove_contract(envelope: &Value) {
    assert_eq!(
        envelope["contract"]["schema_id"], REMOVE_RESULT_SCHEMA,
        "{envelope}"
    );
    assert_eq!(envelope["contract"]["schema_revision"], 1, "{envelope}");
    assert_eq!(
        envelope["request_id"], envelope["problem"]["request_id"],
        "{envelope}"
    );
    assert_eq!(
        envelope["problem"]["request_id"], envelope["problem"]["trace_id"],
        "{envelope}"
    );
}

fn assert_problem(answer: ToolAnswer, expected: Value) {
    let ToolAnswer::Problem(mut envelope) = answer else {
        panic!("expected a retained problem, got {answer:?}");
    };
    assert_remove_contract(&envelope);
    let problem = envelope["problem"].as_object_mut().expect("problem record");
    problem.remove("request_id");
    problem.remove("trace_id");
    assert_eq!(envelope["problem"], expected);
}

fn conflict_problem() -> Value {
    json!({
        "revision": 1,
        "kind": "conflict",
        "code": "application.retained.conflict",
        "message": "The retained operation conflicts with current state.",
        "diagnostic": {
            "code": "application.retained.conflict",
            "message": "The retained operation conflicts with current state."
        },
        "committed_receipt": null,
        "owning_layer": "application",
        "terminality": "pre_admission",
        "retryable": true,
        "retry": "after_revalidate",
        "retry_scope": "fresh_request",
        "retry_after_millis": null,
        "cancellation_stage": null,
        "unavailable_classification": null,
        "execution_failure_classification": null,
        "details": [],
        "legal_actions": ["refresh"],
        "coverage": null
    })
}

fn hidden_fact_problem() -> Value {
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

fn foreign_fact_id() -> String {
    format!("fact.v1.{}.{}", "0".repeat(64), "1".repeat(64))
}

fn missing_sibling_id(fact_id: &str) -> String {
    let rest = fact_id
        .strip_prefix("fact.v1.")
        .unwrap_or_else(|| panic!("fact id must use the fact.v1 namespace: {fact_id}"));
    let (owner, identity) = rest
        .split_once('.')
        .unwrap_or_else(|| panic!("fact id must bind an owner and an identity: {fact_id}"));
    assert_eq!(owner.len(), 64, "{fact_id}");
    assert_eq!(identity.len(), 64, "{fact_id}");
    assert!(
        owner
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{fact_id}"
    );
    let mut identity = identity.to_owned();
    let last = identity.pop().expect("identity nibble");
    identity.push(if last == '0' { '1' } else { '0' });
    format!("fact.v1.{owner}.{identity}")
}

fn listed_contents(list: &Value) -> Vec<String> {
    list["facts"]
        .as_array()
        .unwrap_or_else(|| panic!("list facts: {list}"))
        .iter()
        .map(|projection| {
            assert_eq!(projection["kind"], "available", "{projection}");
            projection["fact"]["content"]
                .as_str()
                .unwrap_or_else(|| panic!("listed content: {projection}"))
                .to_owned()
        })
        .collect()
}

async fn add_fact(server: &tracedecay::mcp::McpServer, content: &str) -> AddedFact {
    let added = payload(
        call_tool(
            server,
            "tracedecay_fact_store_add",
            json!({
                "content": content,
                "category": "project",
                "source_label": SOURCE_LABEL
            }),
        )
        .await,
    );
    assert_eq!(added["outcome"], "committed", "{added}");
    assert_eq!(added["result"]["disposition"], "added", "{added}");
    let fact = &added["result"]["fact"];
    assert_eq!(fact["kind"], "available", "{added}");
    assert_eq!(fact["fact"]["content"], content, "{added}");
    assert_eq!(fact["fact"]["category"], "project", "{added}");
    assert_eq!(fact["fact"]["source_label"], SOURCE_LABEL, "{added}");
    assert_eq!(fact["fact"]["owner"]["kind"], "project", "{added}");
    AddedFact {
        fact_id: fact["fact"]["fact_id"]
            .as_str()
            .unwrap_or_else(|| panic!("added fact id: {added}"))
            .to_owned(),
        last_event_id: added["result"]["commit"]["last_event_id"]
            .as_str()
            .unwrap_or_else(|| panic!("added event id: {added}"))
            .to_owned(),
        project_id: fact["fact"]["owner"]["project_id"]
            .as_str()
            .unwrap_or_else(|| panic!("added project id: {added}"))
            .to_owned(),
    }
}

fn deleted_status(project_id: &str, fact_id: &str) -> Value {
    json!({
        "owner": {"kind": "project", "project_id": project_id},
        "fact_id": fact_id,
        "payload_access": "deleted"
    })
}

fn assert_deleted_projection(fact: &Value, project_id: &str, fact_id: &str) {
    assert_eq!(fact["kind"], "unavailable", "{fact}");
    let mut status = fact["status"].clone();
    let projected_as_of = status
        .as_object_mut()
        .expect("status object")
        .remove("projected_as_of")
        .unwrap_or_else(|| panic!("deleted status is missing projected_as_of: {fact}"));
    assert!(
        projected_as_of.as_i64().is_some(),
        "projected_as_of must be a timestamp: {projected_as_of}"
    );
    assert_eq!(status, deleted_status(project_id, fact_id));
    assert!(
        !fact.to_string().contains(REMOVED_CONTENT),
        "a deleted projection must not echo the removed content: {fact}"
    );
}

/// One production MCP journey for `tracedecay_fact_store_remove`.
///
/// A matching removal deletes that fact only. A later removal of the same id
/// reports `already_removed` and writes nothing. A well-formed id this owner
/// never stored is `not_found`. An id that does not belong to the owner, a
/// stale compare-and-swap token on a live fact, and a request the schema
/// rejects each refuse without deleting the remaining fact.
#[tokio::test]
async fn fact_store_remove_deletes_only_the_named_fact() {
    let production = production_composition_fixture().await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production fact-store MCP server");

    assert_protocol_error(
        call_tool(&server, TOOL, json!({})).await,
        MISSING_FACT_ID_MESSAGE,
    );
    assert_protocol_error(
        call_tool(&server, TOOL, json!({"fact_id": 41})).await,
        NUMERIC_FACT_ID_MESSAGE,
    );
    assert_protocol_error(
        call_tool(
            &server,
            TOOL,
            json!({"fact_id": "not-a-fact", "action": "remove"}),
        )
        .await,
        UNKNOWN_FIELD_MESSAGE,
    );

    let empty = payload(
        call_tool(
            &server,
            "tracedecay_fact_store_list",
            json!({"category": "project", "min_trust": 0}),
        )
        .await,
    );
    assert_eq!(empty["facts"], json!([]), "{empty}");

    let removed = add_fact(&server, REMOVED_CONTENT).await;
    let survivor = add_fact(&server, SURVIVOR_CONTENT).await;
    assert_ne!(removed.fact_id, survivor.fact_id);
    assert_eq!(removed.project_id, survivor.project_id);

    assert_problem(
        call_tool(&server, TOOL, json!({"fact_id": foreign_fact_id()})).await,
        hidden_fact_problem(),
    );
    assert_problem(
        call_tool(&server, TOOL, json!({"fact_id": "not-a-fact"})).await,
        hidden_fact_problem(),
    );
    let missing = payload(
        call_tool(
            &server,
            TOOL,
            json!({"fact_id": missing_sibling_id(&removed.fact_id)}),
        )
        .await,
    );
    assert_eq!(
        missing,
        json!({"outcome": "not_found", "remaining_fact_count": 2}),
        "{missing}"
    );

    assert_problem(
        call_tool(
            &server,
            TOOL,
            json!({
                "fact_id": survivor.fact_id,
                "expected_last_event_id": STALE_EVENT_ID
            }),
        )
        .await,
        conflict_problem(),
    );
    let untouched = payload(
        call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": survivor.fact_id}),
        )
        .await,
    );
    assert_eq!(untouched["fact"]["kind"], "available", "{untouched}");
    assert_eq!(untouched["fact"]["fact"]["content"], SURVIVOR_CONTENT);
    assert_eq!(
        untouched["fact"]["fact"]["last_event_id"], survivor.last_event_id,
        "a refused remove must not append a lineage event: {untouched}"
    );

    let removed_response = handle_real_server_tool_call_raw(
        &server,
        TOOL,
        json!({
            "fact_id": removed.fact_id,
            "expected_last_event_id": removed.last_event_id
        }),
    )
    .await;
    assert!(removed_response["error"].is_null(), "{removed_response}");
    assert_ne!(
        removed_response["result"]["isError"],
        Value::Bool(true),
        "{removed_response}"
    );
    let removed_text = extract_real_server_text(&removed_response["result"]);
    let removed_envelope: Value = serde_json::from_str(removed_text)
        .unwrap_or_else(|error| panic!("remove envelope is not JSON: {error}: {removed_text}"));
    assert_eq!(
        removed_envelope["contract"]["schema_id"], REMOVE_RESULT_SCHEMA,
        "{removed_envelope}"
    );
    assert_eq!(removed_envelope["contract"]["schema_revision"], 1);
    assert_eq!(removed_envelope["outcome"]["outcome"], "effect");
    let deleted = removed_envelope["outcome"]["value"]["payload"].clone();
    assert_eq!(deleted["outcome"], "removed", "{deleted}");
    assert_eq!(deleted["remaining_fact_count"], 1, "{deleted}");
    assert_deleted_projection(&deleted["fact"], &removed.project_id, &removed.fact_id);
    assert_eq!(deleted["commit"]["disposition"], "committed", "{deleted}");
    assert_eq!(deleted["commit"]["fact_id"], removed.fact_id);
    assert_eq!(deleted["commit"]["owner"]["kind"], "project");
    assert_eq!(deleted["commit"]["owner"]["project_id"], removed.project_id);
    assert!(
        deleted["commit"]["active_assertion_id"].is_null(),
        "{deleted}"
    );
    let removal_event = deleted["commit"]["last_event_id"]
        .as_str()
        .unwrap_or_else(|| panic!("removal event id: {deleted}"))
        .to_owned();
    assert_ne!(removal_event, removed.last_event_id);
    assert_eq!(
        deleted["commit"]["committed_event_ids"],
        json!([removal_event]),
        "{deleted}"
    );

    let listed = payload(
        call_tool(
            &server,
            "tracedecay_fact_store_list",
            json!({"category": "project", "min_trust": 0}),
        )
        .await,
    );
    assert_eq!(listed_contents(&listed), vec![SURVIVOR_CONTENT.to_owned()]);
    assert!(listed["next_after_fact_id"].is_null(), "{listed}");

    let tombstone = payload(
        call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": removed.fact_id}),
        )
        .await,
    );
    assert_deleted_projection(&tombstone["fact"], &removed.project_id, &removed.fact_id);
    assert_eq!(tombstone["trust_history"], json!([]), "{tombstone}");

    let gone = payload(
        call_tool(
            &server,
            "tracedecay_fact_store_search",
            json!({"query": REMOVED_CONTENT, "min_trust": 0}),
        )
        .await,
    );
    assert_eq!(gone["hits"], json!([]), "{gone}");
    assert!(gone["next_after"].is_null(), "{gone}");
    assert_eq!(
        gone["retrieval_telemetry"],
        json!({"kind": "not_applicable"}),
        "{gone}"
    );

    let kept = payload(
        call_tool(
            &server,
            "tracedecay_fact_store_search",
            json!({"query": SURVIVOR_CONTENT, "min_trust": 0}),
        )
        .await,
    );
    let hits = kept["hits"].as_array().expect("survivor hits");
    assert_eq!(hits.len(), 1, "{kept}");
    assert_eq!(hits[0]["fact"]["content"], SURVIVOR_CONTENT);
    assert_eq!(hits[0]["fact"]["fact_id"], survivor.fact_id);
    assert_eq!(hits[0]["fact"]["category"], "project");
    assert_eq!(hits[0]["fact"]["source_label"], SOURCE_LABEL);

    let again = payload(call_tool(&server, TOOL, json!({"fact_id": removed.fact_id})).await);
    assert_eq!(again["outcome"], "already_removed", "{again}");
    assert_eq!(again["remaining_fact_count"], 1, "{again}");
    assert!(again.get("commit").is_none(), "{again}");
    assert_deleted_projection(&again["fact"], &removed.project_id, &removed.fact_id);

    let retried = payload(
        call_tool(
            &server,
            TOOL,
            json!({
                "fact_id": removed.fact_id,
                "expected_last_event_id": removal_event
            }),
        )
        .await,
    );
    assert_eq!(retried["outcome"], "already_removed", "{retried}");
    assert_eq!(retried["remaining_fact_count"], 1, "{retried}");
    assert!(retried.get("commit").is_none(), "{retried}");

    let still_there = payload(
        call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": survivor.fact_id}),
        )
        .await,
    );
    assert_eq!(still_there["fact"]["kind"], "available", "{still_there}");
    assert_eq!(still_there["fact"]["fact"]["content"], SURVIVOR_CONTENT);
    assert_eq!(still_there["fact"]["fact"]["category"], "project");
    assert_eq!(still_there["fact"]["fact"]["source_label"], SOURCE_LABEL);
    assert_eq!(
        still_there["fact"]["fact"]["last_event_id"], survivor.last_event_id,
        "{still_there}"
    );

    let status = payload(call_tool(&server, "tracedecay_memory_status", json!({})).await);
    assert_eq!(status["memory"]["fact_count"], 1, "{status}");

    production.harness.shutdown().await;
}

impl std::fmt::Debug for ToolAnswer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Payload(payload) => formatter.debug_tuple("Payload").field(payload).finish(),
            Self::Problem(problem) => formatter.debug_tuple("Problem").field(problem).finish(),
            Self::Protocol {
                code,
                message,
                tool,
            } => formatter
                .debug_struct("Protocol")
                .field("code", code)
                .field("message", message)
                .field("tool", tool)
                .finish(),
        }
    }
}

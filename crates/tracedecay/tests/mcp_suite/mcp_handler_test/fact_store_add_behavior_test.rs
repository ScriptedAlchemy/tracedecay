#![cfg(feature = "test-transport")]

//! Caller-visible `tracedecay_fact_store_add` results, invoked through the
//! production MCP `tools/call` path.
//!
//! Minted fact, event, and operation ids are required to agree across the
//! returned fact and its commit receipt. The remaining payload is then
//! compared to a literal, so a missing, extra, or rewritten field fails.

use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    ProductionCompositionFixture, handle_real_server_tool_call_raw, production_composition_fixture,
};

const TOOL: &str = "tracedecay_fact_store_add";
const SCHEMA_ID: &str = "schema.application.retained.fact-store-add.result";
const USE_CASE: &str = "use-case.application.retained.fact-store-add";
const GENERATED: &str = "<generated>";

/// Similarity the production add call reports for the Redis negation pair.
/// Holographic similarity outranks the token Jaccard of those two sentences.
const REDIS_CONFLICT_SIMILARITY: u64 = 907_172;

struct AddFixture {
    production: ProductionCompositionFixture,
    server: Arc<McpServer>,
}

async fn open_fixture() -> AddFixture {
    let production = production_composition_fixture().await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production fact-store MCP server");
    AddFixture { production, server }
}

async fn close_fixture(fixture: AddFixture) {
    fixture.production.harness.shutdown().await;
}

async fn call_add(server: &McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, TOOL, arguments).await
}

fn tool_text<'a>(response: &'a Value) -> &'a str {
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "JSON-RPC error: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("MCP tool text: {response}"))
}

fn effect_payload(response: &Value) -> Value {
    assert_ne!(
        response["result"]["isError"],
        json!(true),
        "fact add must not be an MCP error: {response}"
    );
    assert_eq!(response["result"]["content"][0]["type"], "text");
    let envelope: Value = serde_json::from_str(tool_text(response)).expect("application JSON");
    assert_eq!(envelope["contract"]["schema_id"], SCHEMA_ID);
    assert_eq!(envelope["contract"]["schema_revision"], 1);
    assert_eq!(envelope["outcome"]["outcome"], "effect");
    let effect = &envelope["outcome"]["value"];
    assert_eq!(effect["effect_class"], "administrative");
    assert_eq!(effect["reconciliation"], "reconciled");
    assert_eq!(effect["execution"]["termination"], "completed");
    assert_eq!(effect["receipt"]["outcome"], "completed");
    assert_eq!(effect["receipt"]["effect_class"], "administrative");
    assert_eq!(effect["receipt"]["operation"], USE_CASE);
    effect["payload"].clone()
}

fn assert_fresh_fact(fact: &Value) {
    let fact_id = fact["fact_id"]
        .as_str()
        .unwrap_or_else(|| panic!("fact id: {fact}"));
    assert!(
        fact_id.starts_with("fact.v1."),
        "fact id must use the canonical namespace: {fact_id}"
    );
    // The projection clock and the telemetry clock are independent, so these
    // stamps are not a single literal. Require a real instant before blanking.
    let created_at = fact["telemetry"]["created_at"]
        .as_i64()
        .unwrap_or_else(|| panic!("created_at: {fact}"));
    let projected_as_of = fact["projected_as_of"]
        .as_i64()
        .unwrap_or_else(|| panic!("projected_as_of: {fact}"));
    assert!(
        created_at > 1_000_000_000_000,
        "created_at must be a real timestamp, not a placeholder: {created_at}"
    );
    assert!(
        projected_as_of > 1_000_000_000_000,
        "projected_as_of must be a real timestamp, not a placeholder: {projected_as_of}"
    );
    let updated_at = fact["telemetry"]["updated_at"]
        .as_i64()
        .unwrap_or_else(|| panic!("updated_at: {fact}"));
    assert!(
        updated_at > 1_000_000_000_000,
        "updated_at must be a real timestamp, not a placeholder: {updated_at}"
    );
    assert_eq!(fact["source"]["kind"], "application");
}

fn assert_commit_binds_fact(payload: &Value, event_count: usize) {
    let fact = &payload["result"]["fact"]["fact"];
    let commit = &payload["result"]["commit"];
    assert_fresh_fact(fact);
    assert_eq!(payload["result"]["fact"]["kind"], "available");
    assert_eq!(commit["fact_id"], fact["fact_id"]);
    assert_eq!(commit["owner"], fact["owner"]);
    assert_eq!(commit["active_assertion_id"], fact["active_assertion_id"]);
    assert_eq!(commit["last_event_id"], fact["last_event_id"]);
    let events = commit["committed_event_ids"]
        .as_array()
        .unwrap_or_else(|| panic!("committed events: {commit}"));
    assert_eq!(
        events.len(),
        event_count,
        "content {}: {commit}",
        fact["content"]
    );
    assert_eq!(events.last(), Some(&fact["last_event_id"]));
}

fn scrub(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut scrubbed = serde_json::Map::new();
            for (key, child) in map {
                let next = match key.as_str() {
                    "fact_id"
                    | "closest_fact_id"
                    | "active_assertion_id"
                    | "last_event_id"
                    | "operation_id"
                    | "project_id" => json!(GENERATED),
                    "created_at" | "updated_at" | "projected_as_of" => json!(0),
                    "committed_event_ids" => Value::Array(
                        child
                            .as_array()
                            .unwrap_or_else(|| panic!("committed_event_ids: {child}"))
                            .iter()
                            .map(|_| json!(GENERATED))
                            .collect(),
                    ),
                    _ => scrub(child),
                };
                scrubbed.insert(key.clone(), next);
            }
            Value::Object(scrubbed)
        }
        Value::Array(items) => Value::Array(items.iter().map(scrub).collect()),
        other => other.clone(),
    }
}

fn project_owner() -> Value {
    json!({"kind": "project", "project_id": GENERATED})
}

fn literal_fact(
    content: &str,
    category: &str,
    tags: Value,
    entities: Value,
    trust_score_millionths: u64,
    source_label: Value,
    metadata: Value,
    owner: Value,
) -> Value {
    json!({
        "owner": owner,
        "fact_id": GENERATED,
        "content": content,
        "category": category,
        "tags": tags,
        "entities": entities,
        "trust_score_millionths": trust_score_millionths,
        "source": {"kind": "application", "operation_id": GENERATED},
        "source_label": source_label,
        "active_assertion_id": GENERATED,
        "last_event_id": GENERATED,
        "projected_as_of": 0,
        "telemetry": {
            "retrieval_count": 0,
            "access_count": 0,
            "helpful_count": 0,
            "unhelpful_count": 0,
            "created_at": 0,
            "updated_at": 0,
            "last_retrieved_at": null,
            "last_recalled_at": null,
            "last_feedback_at": null
        },
        "metadata": metadata
    })
}

fn literal_commit(disposition: &str, owner: Value, event_count: usize) -> Value {
    json!({
        "disposition": disposition,
        "fact_id": GENERATED,
        "owner": owner,
        "committed_event_ids": vec![json!(GENERATED); event_count],
        "last_event_id": GENERATED,
        "active_assertion_id": GENERATED
    })
}

fn assert_added(
    payload: &Value,
    content: &str,
    category: &str,
    tags: Value,
    entities: Value,
    trust_score_millionths: u64,
    source_label: Value,
    metadata: Value,
    owner: Value,
    commit_disposition: &str,
    event_count: usize,
) {
    assert_commit_binds_fact(payload, event_count);
    assert_eq!(
        scrub(payload),
        json!({
            "outcome": "committed",
            "result": {
                "disposition": "added",
                "fact": {
                    "kind": "available",
                    "fact": literal_fact(
                        content,
                        category,
                        tags,
                        entities,
                        trust_score_millionths,
                        source_label,
                        metadata,
                        owner.clone(),
                    )
                },
                "commit": literal_commit(commit_disposition, owner, event_count)
            }
        })
    );
}

#[tokio::test]
async fn fact_store_add_commits_supplied_fields_and_replays_the_same_request() {
    let fixture = open_fixture().await;
    let supplied = json!({
        "content": "Project Phoenix builds with pnpm rather than npm",
        "category": "decision",
        "tags": ["zeta", "alpha"],
        "entities": ["pnpm", "Project Phoenix"],
        "trust": 0.75,
        "source_label": "operator-note",
        "metadata": {"lane": "workspace"}
    });

    let added = effect_payload(&call_add(&fixture.server, supplied.clone()).await);
    assert_added(
        &added,
        "Project Phoenix builds with pnpm rather than npm",
        "decision",
        json!(["alpha", "zeta"]),
        json!(["Project Phoenix", "pnpm"]),
        750_000,
        json!("operator-note"),
        json!({"lane": "workspace"}),
        project_owner(),
        "committed",
        2,
    );
    let fact_id = added["result"]["fact"]["fact"]["fact_id"].clone();

    let replay = effect_payload(&call_add(&fixture.server, supplied).await);
    assert_eq!(replay["result"]["fact"]["fact"]["fact_id"], fact_id);
    assert_added(
        &replay,
        "Project Phoenix builds with pnpm rather than npm",
        "decision",
        json!(["alpha", "zeta"]),
        json!(["Project Phoenix", "pnpm"]),
        750_000,
        json!("operator-note"),
        json!({"lane": "workspace"}),
        project_owner(),
        "idempotent_replay",
        2,
    );

    let defaults = effect_payload(
        &call_add(
            &fixture.server,
            json!({"content": "Remember the default category and trust"}),
        )
        .await,
    );
    assert_added(
        &defaults,
        "Remember the default category and trust",
        "general",
        json!([]),
        json!([]),
        500_000,
        Value::Null,
        json!({}),
        project_owner(),
        "committed",
        1,
    );

    let zero_trust = effect_payload(
        &call_add(
            &fixture.server,
            json!({
                "content": "Zero trust is stored as zero millionths",
                "category": "tool",
                "trust": 0.0
            }),
        )
        .await,
    );
    assert_added(
        &zero_trust,
        "Zero trust is stored as zero millionths",
        "tool",
        json!([]),
        json!([]),
        0,
        Value::Null,
        json!({}),
        project_owner(),
        "committed",
        2,
    );

    close_fixture(fixture).await;
}

#[tokio::test]
async fn fact_store_add_reports_a_normalized_duplicate_without_a_second_fact() {
    let fixture = open_fixture().await;
    let original = "Use pnpm for workspace installs";
    let added = effect_payload(
        &call_add(
            &fixture.server,
            json!({
                "content": original,
                "category": "decision",
                "source_label": "first"
            }),
        )
        .await,
    );
    let fact_id = added["result"]["fact"]["fact"]["fact_id"].clone();
    assert_eq!(added["result"]["fact"]["fact"]["content"], original);

    for arguments in [
        json!({
            "content": "  use   PNPM for workspace installs  ",
            "category": "decision",
            "source_label": "first"
        }),
        json!({
            "content": original,
            "category": "decision",
            "source_label": "second"
        }),
    ] {
        let duplicate = effect_payload(&call_add(&fixture.server, arguments).await);
        assert_eq!(duplicate["closest_fact_id"], fact_id);
        assert_eq!(duplicate["fact"]["fact"]["fact_id"], fact_id);
        assert_fresh_fact(&duplicate["fact"]["fact"]);
        assert_eq!(
            scrub(&duplicate),
            json!({
                "outcome": "normalized_duplicate",
                "fact": {
                    "kind": "available",
                    "fact": literal_fact(
                        original,
                        "decision",
                        json!([]),
                        json!([]),
                        500_000,
                        json!("first"),
                        json!({}),
                        project_owner(),
                    )
                },
                "closest_fact_id": GENERATED
            })
        );
    }

    close_fixture(fixture).await;
}

#[tokio::test]
async fn fact_store_add_keeps_a_trailing_period_as_its_own_fact() {
    let fixture = open_fixture().await;
    let base = "The deployment uses PostgreSQL for durable state in production";
    let variant = "The deployment uses PostgreSQL for durable state in production.";
    let added = effect_payload(
        &call_add(
            &fixture.server,
            json!({"content": base, "category": "decision"}),
        )
        .await,
    );
    let fact_id = added["result"]["fact"]["fact"]["fact_id"].clone();

    let punctuated = effect_payload(
        &call_add(
            &fixture.server,
            json!({"content": variant, "category": "decision"}),
        )
        .await,
    );
    assert_ne!(punctuated["result"]["fact"]["fact"]["fact_id"], fact_id);
    assert_added(
        &punctuated,
        variant,
        "decision",
        json!([]),
        json!([]),
        500_000,
        Value::Null,
        json!({}),
        project_owner(),
        "committed",
        1,
    );

    close_fixture(fixture).await;
}

#[tokio::test]
async fn fact_store_add_reports_a_possible_conflict_for_a_negated_fact() {
    let fixture = open_fixture().await;
    let base = "The deployment uses Redis for durable cache state in production";
    let negated = "The deployment no longer uses Redis for durable cache state in production";
    let added = effect_payload(
        &call_add(
            &fixture.server,
            json!({"content": base, "category": "decision"}),
        )
        .await,
    );
    let fact_id = added["result"]["fact"]["fact"]["fact_id"].clone();

    let conflict = effect_payload(
        &call_add(
            &fixture.server,
            json!({"content": negated, "category": "decision"}),
        )
        .await,
    );
    assert_eq!(conflict["result"]["closest_fact_id"], fact_id);
    assert_ne!(conflict["result"]["fact"]["fact"]["fact_id"], fact_id);
    assert_eq!(conflict["result"]["fact"]["fact"]["content"], negated);
    assert_commit_binds_fact(&conflict, 1);
    assert_eq!(
        scrub(&conflict),
        json!({
            "outcome": "committed",
            "result": {
                "disposition": "possible_conflict",
                "fact": {
                    "kind": "available",
                    "fact": literal_fact(
                        negated,
                        "decision",
                        json!([]),
                        json!([]),
                        500_000,
                        Value::Null,
                        json!({}),
                        project_owner(),
                    )
                },
                "closest_fact_id": GENERATED,
                "similarity_millionths": REDIS_CONFLICT_SIMILARITY,
                "commit": literal_commit("committed", project_owner(), 1)
            }
        })
    );

    close_fixture(fixture).await;
}

#[tokio::test]
async fn fact_store_add_rejects_secret_like_content_without_echoing_it() {
    let fixture = open_fixture().await;
    let secret = "api_key=sk-test-742913 must not be persisted";
    let response = call_add(
        &fixture.server,
        json!({"content": secret, "category": "decision"}),
    )
    .await;
    let serialized = response.to_string();
    assert!(
        !serialized.contains("sk-test-742913"),
        "the add response must not echo the rejected secret: {serialized}"
    );
    assert_eq!(
        effect_payload(&response),
        json!({"outcome": "secret_rejected"})
    );

    let again = call_add(
        &fixture.server,
        json!({"content": secret, "category": "project"}),
    )
    .await;
    assert!(
        !again.to_string().contains("sk-test-742913"),
        "a repeated secret add must still omit the secret"
    );
    assert_eq!(
        effect_payload(&again),
        json!({"outcome": "secret_rejected"})
    );

    close_fixture(fixture).await;
}

#[tokio::test]
async fn fact_store_add_user_scope_stores_a_profile_owned_fact() {
    let fixture = open_fixture().await;
    let added = effect_payload(
        &call_add(
            &fixture.server,
            json!({
                "content": "User prefers concise technical answers",
                "category": "user_pref",
                "memory_scope": "user",
                "trust": 0.25
            }),
        )
        .await,
    );
    assert_eq!(
        added["result"]["fact"]["fact"]["owner"],
        json!({"kind": "profile"})
    );
    assert_added(
        &added,
        "User prefers concise technical answers",
        "user_pref",
        json!([]),
        json!([]),
        250_000,
        Value::Null,
        json!({}),
        json!({"kind": "profile"}),
        "committed",
        2,
    );

    close_fixture(fixture).await;
}

fn assert_argument_error(response: &Value, message: &str) {
    assert!(
        response.get("result").is_none(),
        "a decode rejection must be a JSON-RPC error, not a tool result: {response}"
    );
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(
        response["error"],
        json!({
            "code": -32603,
            "message": message,
            "data": {
                "tool": TOOL,
                "cli_fallback": "This tool is also available from the shell: `tracedecay tool fact_store_add ...` (`tracedecay tool fact_store_add --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
            }
        })
    );
}

fn assert_invalid_request(response: &Value) {
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert_eq!(response["result"]["content"][0]["type"], "text");
    let envelope: Value = serde_json::from_str(tool_text(response)).expect("problem envelope JSON");
    assert_eq!(envelope["contract"]["schema_id"], SCHEMA_ID);
    assert_eq!(envelope["contract"]["schema_revision"], 1);
    assert_eq!(envelope["request_id"], envelope["problem"]["request_id"]);
    assert_eq!(
        envelope["problem"]["request_id"],
        envelope["problem"]["trace_id"]
    );
    let mut problem = envelope["problem"].clone();
    problem["request_id"] = json!(GENERATED);
    problem["trace_id"] = json!(GENERATED);
    assert_eq!(
        problem,
        json!({
            "revision": 1,
            "kind": "invalid_request",
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid.",
            "diagnostic": {
                "code": "application.retained.invalid-request",
                "message": "The retained operation request is invalid."
            },
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
            "request_id": GENERATED,
            "trace_id": GENERATED,
            "details": [],
            "legal_actions": ["correct_request"],
            "coverage": null
        })
    );
}

#[tokio::test]
async fn fact_store_add_rejects_unknown_fields_categories_and_out_of_range_trust() {
    let fixture = open_fixture().await;

    assert_argument_error(
        &call_add(
            &fixture.server,
            json!({
                "content": "Category outside the closed vocabulary must be rejected",
                "category": "pitfall"
            }),
        )
        .await,
        "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_add: category: unknown variant `pitfall`, expected one of `general`, `user_pref`, `project`, `tool`, `decision`, `code_area`",
    );
    assert_argument_error(
        &call_add(
            &fixture.server,
            json!({
                "action": "add",
                "content": "The retired action tag is not a fact field"
            }),
        )
        .await,
        "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_add: action: unknown field `action`, expected one of `content`, `memory_scope`, `category`, `tags`, `entities`, `trust`, `source_label`, `metadata`, `project_selector`",
    );
    assert_argument_error(
        &call_add(&fixture.server, json!({"category": "decision"})).await,
        "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_add: missing field `content`",
    );

    for trust in [1.5, -0.1] {
        assert_invalid_request(
            &call_add(
                &fixture.server,
                json!({
                    "content": "Trust outside zero to one is rejected",
                    "trust": trust
                }),
            )
            .await,
        );
    }

    assert_invalid_request(&call_add(&fixture.server, json!({"content": "   "})).await);

    close_fixture(fixture).await;
}

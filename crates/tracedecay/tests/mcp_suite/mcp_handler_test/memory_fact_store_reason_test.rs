//! Behavior of `tracedecay_fact_store_reason` through the production MCP server.
//!
//! Reason is a read over stored entity names: a fact is a connection only when
//! it names every requested entity. Prose that merely mentions those names,
//! and facts that name only some of them, stay out. Callers see that page, or
//! a typed refusal, on the JSON-RPC tool result.

use serde_json::{Value, json};

use super::memory_facts_test::{
    close_test_graph, invoke_production_tool, production_server, setup_project,
};
use crate::support::handle_real_server_tool_call_raw;

const RELEASE_FACT: &str = "Phoenix release binds Amari Memory on the fifteenth";
const TENTATIVE_FACT: &str = "A tentative note also links Phoenix and Amari Memory";
const DECISION_FACT: &str = "The decision record links Phoenix and Amari Memory";
const PHOENIX_ONLY_FACT: &str = "Phoenix stores session history separately";
const AMARI_ONLY_FACT: &str = "Amari Memory encodes role bindings";
const PROSE_ONLY_FACT: &str =
    "The note mentions Project Phoenix and Amari Memory without linking them";

async fn call_reason(
    fixture: &super::memory_facts_test::FactStoreMcpFixture,
    arguments: Value,
) -> Value {
    handle_real_server_tool_call_raw(
        production_server(fixture),
        "tracedecay_fact_store_reason",
        arguments,
    )
    .await
}

fn reason_payload(response: &Value) -> Value {
    assert!(
        response["error"].is_null(),
        "reason must answer on the tool result, not a transport error: {response}"
    );
    assert_ne!(
        response["result"]["isError"],
        json!(true),
        "a served reason page is not a tool error: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("reason MCP text");
    let body: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("reason returned invalid JSON ({error}): {text}"));
    assert_eq!(
        body["outcome"]["outcome"], "evidence",
        "reason is a read, not an effect: {body}"
    );
    body.pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("reason omitted its payload: {body}"))
}

fn reason_problem(response: &Value) -> Value {
    assert!(
        response["error"].is_null(),
        "an invalid selection is a tool result, not a transport error: {response}"
    );
    assert_eq!(
        response["result"]["isError"],
        json!(true),
        "an invalid selection must be a semantic tool error: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("reason refusal text");
    let body: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("reason refusal was not JSON ({error}): {text}"));
    body["problem"].clone()
}

fn assert_invalid_entity_selection(problem: &Value) {
    assert_eq!(
        json!({
            "kind": problem["kind"],
            "code": problem["code"],
            "message": problem["message"],
            "diagnostic": problem["diagnostic"],
            "retry": problem["retry"],
            "retryable": problem["retryable"],
            "owning_layer": problem["owning_layer"],
            "terminality": problem["terminality"],
            "legal_actions": problem["legal_actions"],
            "committed_receipt": problem["committed_receipt"],
        }),
        json!({
            "kind": "invalid_request",
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid.",
            "diagnostic": {
                "code": "application.retained.invalid-request",
                "message": "The retained operation request is invalid."
            },
            "retry": "never",
            "retryable": false,
            "owning_layer": "application",
            "terminality": "pre_admission",
            "legal_actions": ["correct_request"],
            "committed_receipt": null
        }),
        "invalid entity selection problem: {problem}"
    );
}

fn decode_refusal(response: &Value) -> String {
    assert_eq!(
        response["error"]["code"], -32603,
        "a request the reason schema rejects is an internal tool error: {response}"
    );
    assert_eq!(
        response["error"]["data"]["tool"], "tracedecay_fact_store_reason",
        "the refusal must name the tool: {response}"
    );
    response["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("decode refusal has no message: {response}"))
        .to_owned()
}

fn observed_hit(hit: &Value) -> Value {
    json!({
        "content": hit["fact"]["content"],
        "category": hit["fact"]["category"],
        "tags": hit["fact"]["tags"],
        "entities": hit["fact"]["entities"],
        "trust_score_millionths": hit["fact"]["trust_score_millionths"],
        "source_kind": hit["fact"]["source"]["kind"],
        "source_label": hit["fact"]["source_label"],
        "metadata": hit["fact"]["metadata"],
        "retrieval_count": hit["fact"]["telemetry"]["retrieval_count"],
        "last_retrieved_at": hit["fact"]["telemetry"]["last_retrieved_at"],
        "why": hit["why"],
        "scores": hit["scores"],
    })
}

fn connection(content: &str, category: &str, trust_millionths: u64, tags: &[&str]) -> Value {
    json!({
        "content": content,
        "category": category,
        "tags": tags,
        "entities": ["Amari Memory", "Project Phoenix"],
        "trust_score_millionths": trust_millionths,
        "source_kind": "application",
        "source_label": "reason-proof",
        "metadata": {"plan": "release"},
        "retrieval_count": 0,
        "last_retrieved_at": null,
        "why": "entity reasoning",
        "scores": {
            "score_millionths": trust_millionths,
            "fts_score_millionths": 0,
            "jaccard_score_millionths": 0,
            "holographic_score_millionths": 1_000_000,
            "trust_score_millionths": trust_millionths
        }
    })
}

fn assert_reason_frame(page: &Value, hit: &Value) {
    assert_eq!(page["owner"]["kind"], "project");
    assert_eq!(hit["fact"]["owner"], page["owner"]);
    assert!(
        hit["fact"]["fact_id"]
            .as_str()
            .is_some_and(|fact_id| fact_id.starts_with("fact.v1.")),
        "reason must return a canonical fact id: {hit}"
    );
    assert_eq!(page["graph_coverage"], json!({"kind": "not_applicable"}));
    assert!(page.get("retrieval_telemetry").is_none(), "{page}");
}

async fn store_fact(
    fixture: &super::memory_facts_test::FactStoreMcpFixture,
    content: &str,
    category: &str,
    entities: &[&str],
    trust: f64,
) {
    invoke_production_tool(
        fixture,
        "tracedecay_fact_store_add",
        json!({
            "content": content,
            "category": category,
            "entities": entities,
            "trust": trust,
            "tags": ["memory", "holographic"],
            "source_label": "reason-proof",
            "metadata": {"plan": "release"}
        }),
    )
    .await
    .unwrap_or_else(|error| panic!("store {content:?} failed: {error}"));
}

/// A fact connects the requested entities only when its stored names include
/// every one of them. Folding, trust, category, and page order are part of
/// that result.
#[tokio::test]
async fn fact_store_reason_returns_facts_that_name_every_entity() {
    let fixture = setup_project().await;
    store_fact(
        &fixture,
        RELEASE_FACT,
        "project",
        &["Project Phoenix", "Amari Memory"],
        0.75,
    )
    .await;
    store_fact(
        &fixture,
        TENTATIVE_FACT,
        "project",
        &["Project Phoenix", "Amari Memory"],
        0.25,
    )
    .await;
    store_fact(
        &fixture,
        DECISION_FACT,
        "decision",
        &["Amari Memory", "Project Phoenix"],
        1.0,
    )
    .await;
    store_fact(
        &fixture,
        PHOENIX_ONLY_FACT,
        "project",
        &["Project Phoenix", "Session History"],
        0.5,
    )
    .await;
    store_fact(&fixture, AMARI_ONLY_FACT, "project", &["Amari Memory"], 0.5).await;
    store_fact(&fixture, PROSE_ONLY_FACT, "project", &["Ledger"], 0.5).await;

    let folded = reason_payload(
        &call_reason(
            &fixture,
            json!({
                "entities": ["'Project Phoenix'", "amari  memory"],
                "category": "project"
            }),
        )
        .await,
    );
    let hits = folded["hits"].as_array().expect("reason hits");
    assert_eq!(
        hits.len(),
        1,
        "default trust keeps the 0.25 link out: {folded}"
    );
    assert_reason_frame(&folded, &hits[0]);
    assert_eq!(folded["next_after"], Value::Null);
    assert_eq!(
        observed_hit(&hits[0]),
        connection(RELEASE_FACT, "project", 750_000, &["holographic", "memory"])
    );

    let reversed = reason_payload(
        &call_reason(
            &fixture,
            json!({
                "entities": ["AMARI MEMORY", "project phoenix"],
                "category": "project"
            }),
        )
        .await,
    );
    assert_eq!(
        reversed["hits"]
            .as_array()
            .expect("reversed hits")
            .iter()
            .map(observed_hit)
            .collect::<Vec<_>>(),
        vec![connection(
            RELEASE_FACT,
            "project",
            750_000,
            &["holographic", "memory"]
        )]
    );

    let including_tentative = reason_payload(
        &call_reason(
            &fixture,
            json!({
                "entities": ["Project Phoenix", "Amari Memory"],
                "category": "project",
                "min_trust": 0.0,
                "limit": 1
            }),
        )
        .await,
    );
    let first = including_tentative["hits"].as_array().expect("first page");
    assert_eq!(first.len(), 1, "{including_tentative}");
    assert_eq!(
        observed_hit(&first[0]),
        connection(RELEASE_FACT, "project", 750_000, &["holographic", "memory"])
    );
    assert_eq!(
        including_tentative["next_after"]["score_millionths"],
        750_000
    );
    assert_eq!(
        including_tentative["next_after"]["fact_id"],
        first[0]["fact"]["fact_id"]
    );
    assert_eq!(
        including_tentative["next_after"]["updated_at"],
        first[0]["fact"]["telemetry"]["updated_at"]
    );

    let second = reason_payload(
        &call_reason(
            &fixture,
            json!({
                "entities": ["Project Phoenix", "Amari Memory"],
                "category": "project",
                "min_trust": 0.0,
                "limit": 1,
                "after": including_tentative["next_after"]
            }),
        )
        .await,
    );
    let second_hits = second["hits"].as_array().expect("second page");
    assert_eq!(second_hits.len(), 1, "{second}");
    assert_eq!(second["next_after"], Value::Null);
    assert_eq!(
        observed_hit(&second_hits[0]),
        connection(
            TENTATIVE_FACT,
            "project",
            250_000,
            &["holographic", "memory"]
        )
    );

    let decision = reason_payload(
        &call_reason(
            &fixture,
            json!({
                "entities": ["Project Phoenix", "Amari Memory"],
                "category": "decision"
            }),
        )
        .await,
    );
    assert_eq!(
        decision["hits"]
            .as_array()
            .expect("decision hits")
            .iter()
            .map(observed_hit)
            .collect::<Vec<_>>(),
        vec![connection(
            DECISION_FACT,
            "decision",
            1_000_000,
            &["holographic", "memory"]
        )]
    );

    let unrelated = reason_payload(
        &call_reason(
            &fixture,
            json!({"entities": ["Session History", "Amari Memory"], "min_trust": 0.0}),
        )
        .await,
    );
    assert_eq!(unrelated["hits"], json!([]));
    assert_eq!(unrelated["next_after"], Value::Null);
    assert_eq!(
        unrelated["graph_coverage"],
        json!({"kind": "not_applicable"})
    );
    assert_eq!(unrelated["owner"]["kind"], "project");
    assert!(unrelated.get("retrieval_telemetry").is_none());

    close_test_graph(fixture).await;
}

/// A missing, empty, duplicated, untrimmed, or out-of-range selection is the
/// retained invalid-request problem. Schema mistakes name the offending
/// argument instead.
#[tokio::test]
async fn fact_store_reason_refuses_an_unusable_entity_selection() {
    let fixture = setup_project().await;

    for arguments in [
        json!({"entities": []}),
        json!({"entities": ["same", "same"]}),
        json!({"entities": ["Amari Memory"], "limit": 0}),
        json!({"entities": ["Amari Memory"], "limit": 201}),
        json!({"entities": ["Amari Memory"], "min_trust": 1.5}),
        json!({"entities": ["   "]}),
        json!({"entities": ["  Project Phoenix  ", "Amari Memory"]}),
    ] {
        let problem = reason_problem(&call_reason(&fixture, arguments.clone()).await);
        assert_invalid_entity_selection(&problem);
    }

    assert_eq!(
        decode_refusal(&call_reason(&fixture, json!({})).await),
        "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_reason: missing field `entities`"
    );
    assert_eq!(
        decode_refusal(&call_reason(&fixture, json!({"entities": "Project Phoenix"})).await),
        "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_reason: entities: invalid type: string \"Project Phoenix\", expected a sequence"
    );
    assert_eq!(
        decode_refusal(
            &call_reason(
                &fixture,
                json!({"entities": ["Project Phoenix", "Amari Memory"], "query": "release"}),
            )
            .await,
        ),
        "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_reason: unknown field `query`"
    );
    assert_eq!(
        decode_refusal(
            &call_reason(
                &fixture,
                json!({"entities": ["Project Phoenix"], "category": "pitfall"}),
            )
            .await,
        ),
        "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_reason: unknown variant `pitfall`, expected one of `general`, `user_pref`, `project`, `tool`, `decision`, `code_area`"
    );

    close_test_graph(fixture).await;
}

//! Production `tools/call` proof for `tracedecay_fact_store_supersede`.
//!
//! The tool retires one fact from the default list, search, and probe
//! surfaces while leaving its payload and trust readable by id. A second
//! successor, a missing successor, a self-target, and a stale generation are
//! typed refusals that must not rewrite the current fact.

#![cfg(feature = "test-transport")]

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, handle_real_server_tool_call, production_composition_fixture,
};

const FIRST_CUTOFF: &str = "qxkelpfirst ships on the first of the month";
const FIFTEENTH_CUTOFF: &str = "qxkelpfifteenth ships on the fifteenth after the retro";
const ORION_OWNER: &str = "qxkelporion is owned by the platform guild";

struct SupersedeFixture {
    production: ProductionCompositionFixture,
    server: Arc<tracedecay::mcp::McpServer>,
}

async fn open_fixture() -> SupersedeFixture {
    let production = production_composition_fixture().await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production fact-store MCP server");
    SupersedeFixture { production, server }
}

async fn close_fixture(fixture: SupersedeFixture) {
    fixture.production.harness.shutdown().await;
}

async fn call_tool(
    server: &tracedecay::mcp::McpServer,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let result = handle_real_server_tool_call(server, tool_name, arguments).await;
    assert_ne!(
        result.get("isError").and_then(Value::as_bool),
        Some(true),
        "{tool_name} refused: {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool_name} returned no text: {result}"));
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{tool_name} returned invalid JSON: {error}: {result}"))
}

fn added_fact(payload: &Value) -> (String, Value) {
    assert_eq!(payload["outcome"], "committed", "{payload}");
    assert_eq!(payload["result"]["disposition"], "added", "{payload}");
    assert_eq!(payload["result"]["fact"]["kind"], "available", "{payload}");
    let fact_id = payload["result"]["fact"]["fact"]["fact_id"]
        .as_str()
        .unwrap_or_else(|| panic!("add omitted fact_id: {payload}"))
        .to_owned();
    let last_event_id = payload["result"]["commit"]["last_event_id"].clone();
    (fact_id, last_event_id)
}

fn listed_contents(payload: &Value) -> BTreeSet<String> {
    payload["facts"]
        .as_array()
        .unwrap_or_else(|| panic!("list omitted facts: {payload}"))
        .iter()
        .map(|projection| {
            assert_eq!(projection["kind"], "available", "{projection}");
            projection["fact"]["content"]
                .as_str()
                .unwrap_or_else(|| panic!("listed fact omitted content: {projection}"))
                .to_owned()
        })
        .collect()
}

fn hit_contents(payload: &Value) -> BTreeSet<String> {
    payload["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("read omitted hits: {payload}"))
        .iter()
        .map(|hit| {
            hit["fact"]["content"]
                .as_str()
                .unwrap_or_else(|| panic!("hit omitted content: {hit}"))
                .to_owned()
        })
        .collect()
}

fn same_owner_absent_fact_id(fact_id: &str) -> String {
    let (prefix, identity) = fact_id
        .rsplit_once('.')
        .unwrap_or_else(|| panic!("fact id {fact_id} has no identity segment"));
    let mut identity = identity.to_owned();
    let last = identity
        .pop()
        .unwrap_or_else(|| panic!("fact id {fact_id} has an empty identity"));
    identity.push(if last == '0' { '1' } else { '0' });
    format!("{prefix}.{identity}")
}

fn assert_problem(
    result: &Value,
    kind: &str,
    code: &str,
    message: &str,
    retry: &str,
    legal_actions: Value,
) {
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(result["structuredContent"]["problem"]["kind"], kind, "{result}");
    assert_eq!(result["structuredContent"]["problem"]["code"], code, "{result}");
    assert_eq!(result["structuredContent"]["problem"]["message"], message, "{result}");
    assert_eq!(result["structuredContent"]["problem"]["retry"], retry, "{result}");
    assert_eq!(
        result["structuredContent"]["problem"]["legal_actions"], legal_actions,
        "{result}"
    );
}

#[tokio::test]
async fn fact_store_supersede_retires_old_fact_and_keeps_its_payload() {
    let fixture = open_fixture().await;
    let server = Arc::clone(&fixture.server);

    let old = call_tool(
        &server,
        "tracedecay_fact_store_add",
        json!({
            "content": FIRST_CUTOFF,
            "category": "project",
            "entities": ["Qxkelp Schedule"],
            "trust": 0.5
        }),
    )
    .await;
    let (old_fact_id, old_event_id) = added_fact(&old);
    assert_eq!(
        old["result"]["fact"]["fact"]["content"], FIRST_CUTOFF,
        "{old}"
    );
    assert_eq!(
        old["result"]["fact"]["fact"]["trust_score_millionths"], 500_000,
        "{old}"
    );
    assert_eq!(
        old["result"]["fact"]["fact"]["entities"],
        json!(["Qxkelp Schedule"]),
        "{old}"
    );

    let successor = call_tool(
        &server,
        "tracedecay_fact_store_add",
        json!({
            "content": FIFTEENTH_CUTOFF,
            "category": "decision",
            "entities": ["Qxkelp Train"],
            "trust": 1.0
        }),
    )
    .await;
    let (successor_fact_id, _) = added_fact(&successor);
    assert_eq!(
        successor["result"]["fact"]["fact"]["content"], FIFTEENTH_CUTOFF,
        "{successor}"
    );
    assert_eq!(
        successor["result"]["fact"]["fact"]["trust_score_millionths"], 1_000_000,
        "{successor}"
    );

    assert_eq!(
        listed_contents(&call_tool(&server, "tracedecay_fact_store_list", json!({})).await),
        BTreeSet::from([FIRST_CUTOFF.to_owned(), FIFTEENTH_CUTOFF.to_owned()])
    );
    assert_eq!(
        hit_contents(
            &call_tool(
                &server,
                "tracedecay_fact_store_search",
                json!({"query": "qxkelpfirst", "min_trust": 0.0}),
            )
            .await
        ),
        BTreeSet::from([FIRST_CUTOFF.to_owned()])
    );
    assert_eq!(
        hit_contents(
            &call_tool(
                &server,
                "tracedecay_fact_store_probe",
                json!({"entity": "Qxkelp Schedule", "min_trust": 0.0}),
            )
            .await
        ),
        BTreeSet::from([FIRST_CUTOFF.to_owned()])
    );

    let superseded = call_tool(
        &server,
        "tracedecay_fact_store_supersede",
        json!({
            "fact_id": old_fact_id,
            "superseded_by": successor_fact_id
        }),
    )
    .await;
    assert_eq!(superseded["outcome"], "superseded", "{superseded}");
    assert_eq!(superseded["fact_id"], old_fact_id, "{superseded}");
    assert_eq!(
        superseded["superseded_by"], successor_fact_id,
        "{superseded}"
    );
    assert_eq!(
        superseded["commit"]["disposition"], "committed",
        "{superseded}"
    );
    assert_eq!(superseded["commit"]["fact_id"], old_fact_id, "{superseded}");
    assert_eq!(
        superseded["commit"]["owner"], old["result"]["commit"]["owner"],
        "{superseded}"
    );
    assert_eq!(
        superseded["commit"]["active_assertion_id"],
        Value::Null,
        "{superseded}"
    );
    assert_ne!(
        superseded["commit"]["last_event_id"], old_event_id,
        "supersession must append its own lineage event: {superseded}"
    );

    let listed = call_tool(&server, "tracedecay_fact_store_list", json!({})).await;
    assert_eq!(listed["next_after_fact_id"], Value::Null, "{listed}");
    assert_eq!(
        listed_contents(&listed),
        BTreeSet::from([FIFTEENTH_CUTOFF.to_owned()])
    );
    assert_eq!(
        hit_contents(
            &call_tool(
                &server,
                "tracedecay_fact_store_search",
                json!({"query": "qxkelpfirst", "min_trust": 0.0}),
            )
            .await
        ),
        BTreeSet::new()
    );
    assert_eq!(
        hit_contents(
            &call_tool(
                &server,
                "tracedecay_fact_store_search",
                json!({"query": "qxkelpfifteenth", "min_trust": 0.0}),
            )
            .await
        ),
        BTreeSet::from([FIFTEENTH_CUTOFF.to_owned()])
    );
    assert_eq!(
        hit_contents(
            &call_tool(
                &server,
                "tracedecay_fact_store_probe",
                json!({"entity": "Qxkelp Schedule", "min_trust": 0.0}),
            )
            .await
        ),
        BTreeSet::new()
    );
    assert_eq!(
        hit_contents(
            &call_tool(
                &server,
                "tracedecay_fact_store_probe",
                json!({"entity": "Qxkelp Train", "min_trust": 0.0}),
            )
            .await
        ),
        BTreeSet::from([FIFTEENTH_CUTOFF.to_owned()])
    );

    let retired = call_tool(
        &server,
        "tracedecay_fact_store_get",
        json!({"fact_id": old_fact_id}),
    )
    .await;
    assert_eq!(retired["fact"]["kind"], "superseded", "{retired}");
    assert_eq!(
        retired["fact"]["superseded_by"], successor_fact_id,
        "{retired}"
    );
    assert_eq!(
        retired["fact"]["fact"]["content"], FIRST_CUTOFF,
        "{retired}"
    );
    assert_eq!(retired["fact"]["fact"]["category"], "project", "{retired}");
    assert_eq!(
        retired["fact"]["fact"]["entities"],
        json!(["Qxkelp Schedule"]),
        "{retired}"
    );
    assert_eq!(
        retired["fact"]["fact"]["trust_score_millionths"], 500_000,
        "{retired}"
    );
    assert_eq!(retired["trust_history"], json!([]), "{retired}");

    let current = call_tool(
        &server,
        "tracedecay_fact_store_get",
        json!({"fact_id": successor_fact_id}),
    )
    .await;
    assert_eq!(current["fact"]["kind"], "available", "{current}");
    assert_eq!(
        current["fact"]["fact"]["content"], FIFTEENTH_CUTOFF,
        "{current}"
    );
    assert_eq!(current["fact"]["fact"]["category"], "decision", "{current}");
    assert_eq!(
        current["fact"]["fact"]["trust_score_millionths"], 1_000_000,
        "{current}"
    );

    let replayed = call_tool(
        &server,
        "tracedecay_fact_store_supersede",
        json!({
            "fact_id": old_fact_id,
            "superseded_by": successor_fact_id
        }),
    )
    .await;
    assert_eq!(replayed["outcome"], "superseded", "{replayed}");
    assert_eq!(
        replayed["commit"]["disposition"], "idempotent_replay",
        "{replayed}"
    );
    assert_eq!(
        replayed["commit"]["last_event_id"], superseded["commit"]["last_event_id"],
        "{replayed}"
    );

    let observed = call_tool(
        &server,
        "tracedecay_fact_store_supersede",
        json!({
            "fact_id": old_fact_id,
            "superseded_by": successor_fact_id,
            "expected_last_event_id": superseded["commit"]["last_event_id"]
        }),
    )
    .await;
    assert_eq!(
        observed,
        json!({
            "outcome": "already_superseded",
            "fact_id": old_fact_id,
            "superseded_by": successor_fact_id
        }),
        "{observed}"
    );

    let other = call_tool(
        &server,
        "tracedecay_fact_store_add",
        json!({
            "content": ORION_OWNER,
            "category": "tool",
            "entities": ["Qxkelp Orion"],
            "trust": 0.5
        }),
    )
    .await;
    let (other_fact_id, _) = added_fact(&other);
    let refused = handle_real_server_tool_call(
        &server,
        "tracedecay_fact_store_supersede",
        json!({
            "fact_id": old_fact_id,
            "superseded_by": other_fact_id
        }),
    )
    .await;
    assert_problem(
        &refused,
        "unavailable",
        "application.retained.authority-unavailable",
        &format!(
            "The retained operation authority is unavailable: canonical fact was already superseded by {successor_fact_id}"
        ),
        "after_delay",
        json!(["retry"]),
    );

    let still_retired = call_tool(
        &server,
        "tracedecay_fact_store_get",
        json!({"fact_id": old_fact_id}),
    )
    .await;
    assert_eq!(
        still_retired["fact"]["superseded_by"], successor_fact_id,
        "{still_retired}"
    );
    assert_eq!(
        still_retired["fact"]["fact"]["content"], FIRST_CUTOFF,
        "{still_retired}"
    );
    assert_eq!(
        listed_contents(&call_tool(&server, "tracedecay_fact_store_list", json!({})).await),
        BTreeSet::from([FIFTEENTH_CUTOFF.to_owned(), ORION_OWNER.to_owned()])
    );

    close_fixture(fixture).await;
}

#[tokio::test]
async fn fact_store_supersede_refusals_leave_the_current_fact_unchanged() {
    let fixture = open_fixture().await;
    let server = Arc::clone(&fixture.server);

    let added = call_tool(
        &server,
        "tracedecay_fact_store_add",
        json!({
            "content": FIRST_CUTOFF,
            "category": "project",
            "entities": ["Qxkelp Schedule"],
            "trust": 0.5
        }),
    )
    .await;
    let (fact_id, _) = added_fact(&added);
    let successor = call_tool(
        &server,
        "tracedecay_fact_store_add",
        json!({
            "content": FIFTEENTH_CUTOFF,
            "category": "decision",
            "entities": ["Qxkelp Train"],
            "trust": 1.0
        }),
    )
    .await;
    let (successor_fact_id, _) = added_fact(&successor);

    let missing = call_tool(
        &server,
        "tracedecay_fact_store_supersede",
        json!({
            "fact_id": same_owner_absent_fact_id(&fact_id),
            "superseded_by": successor_fact_id
        }),
    )
    .await;
    assert_eq!(missing, json!({"outcome": "not_found"}), "{missing}");

    let self_supersede = handle_real_server_tool_call(
        &server,
        "tracedecay_fact_store_supersede",
        json!({
            "fact_id": fact_id,
            "superseded_by": fact_id
        }),
    )
    .await;
    assert_problem(
        &self_supersede,
        "invalid_request",
        "application.retained.invalid-request",
        "The retained operation request is invalid.",
        "never",
        json!(["correct_request"]),
    );

    let missing_successor = handle_real_server_tool_call(
        &server,
        "tracedecay_fact_store_supersede",
        json!({
            "fact_id": fact_id,
            "superseded_by": same_owner_absent_fact_id(&successor_fact_id)
        }),
    )
    .await;
    assert_problem(
        &missing_successor,
        "not_found_or_not_authorized",
        "not_found_or_not_authorized",
        "The requested resource was not found or is not authorized",
        "never",
        json!([]),
    );

    let stale = handle_real_server_tool_call(
        &server,
        "tracedecay_fact_store_supersede",
        json!({
            "fact_id": fact_id,
            "superseded_by": successor_fact_id,
            "expected_last_event_id": "event.stale-supersede"
        }),
    )
    .await;
    assert_problem(
        &stale,
        "conflict",
        "application.retained.conflict",
        "The retained operation conflicts with current state.",
        "after_revalidate",
        json!(["refresh"]),
    );

    let unchanged = call_tool(
        &server,
        "tracedecay_fact_store_get",
        json!({"fact_id": fact_id}),
    )
    .await;
    assert_eq!(unchanged["fact"]["kind"], "available", "{unchanged}");
    assert_eq!(
        unchanged["fact"]["fact"]["content"], FIRST_CUTOFF,
        "{unchanged}"
    );
    assert_eq!(
        unchanged["fact"]["fact"]["category"], "project",
        "{unchanged}"
    );
    assert_eq!(
        unchanged["fact"]["fact"]["trust_score_millionths"], 500_000,
        "{unchanged}"
    );
    assert_eq!(
        listed_contents(&call_tool(&server, "tracedecay_fact_store_list", json!({})).await),
        BTreeSet::from([FIRST_CUTOFF.to_owned(), FIFTEENTH_CUTOFF.to_owned()])
    );

    close_fixture(fixture).await;
}

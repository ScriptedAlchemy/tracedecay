//! Production MCP behavior of `tracedecay_fact_store_probe`.
//!
//! Callers pass one entity and observe the facts connected to it. Matching is
//! the normalized entity, ranking is trust, and a request the reviewed schema
//! refuses never falls through to an empty success.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};

use super::memory_facts_test::{
    close_test_graph, fact_store_cross_project_fixture, invoke_exact_tool, invoke_production_tool,
    setup_project,
};
use crate::support::{handle_real_server_tool_call, handle_real_server_tool_call_raw};

const FRIDAY: &str = "Northwind closes the ledger on Friday";
const AUDIT: &str = "Northwind records the audit trail after close";
const SCRATCH: &str = "Northwind keeps a low-trust scratch note";
const ORCHARD: &str = "Orchard rain stays off the ledger";
const USER_PREF: &str = "User prefers the Northwind ledger in dark type";
const MONDAY: &str = "Northwind now closes the ledger on Monday";
const LEDGER: &str = "Northwind Ledger";

#[tokio::test]
async fn fact_store_probe_returns_connected_facts_and_typed_refusals() {
    let cg = setup_project().await;
    let project_id = cg
        .server
        .cg()
        .await
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("probe fixture project id")
        .to_owned();

    let friday_id = add_fact(
        &cg.server,
        json!({
            "content": FRIDAY,
            "category": "project",
            "entities": [LEDGER],
            "tags": ["books"],
            "source_label": "probe-ledger",
            "metadata": {"plan": "probe"},
            "trust": 0.75
        }),
    )
    .await;
    add_fact(
        &cg.server,
        json!({
            "content": AUDIT,
            "category": "decision",
            "entities": [LEDGER],
            "trust": 0.5
        }),
    )
    .await;
    add_fact(
        &cg.server,
        json!({
            "content": SCRATCH,
            "category": "project",
            "entities": [LEDGER],
            "trust": 0.25
        }),
    )
    .await;
    add_fact(
        &cg.server,
        json!({
            "content": ORCHARD,
            "category": "project",
            "entities": ["Orchard Weather"],
            "trust": 0.75
        }),
    )
    .await;
    add_fact(
        &cg.server,
        json!({
            "content": USER_PREF,
            "category": "user_pref",
            "entities": [LEDGER],
            "memory_scope": "user",
            "trust": 0.75
        }),
    )
    .await;

    let default_probe = probe(&cg.server, json!({"entity": LEDGER})).await;
    assert_eq!(
        default_probe["owner"],
        json!({"kind": "project", "project_id": project_id})
    );
    assert_eq!(default_probe["next_after"], Value::Null);
    assert_probe_hits(
        &default_probe,
        &[
            (FRIDAY, "project", &[LEDGER], 750_000),
            (AUDIT, "decision", &[LEDGER], 500_000),
        ],
    );
    let friday = &default_probe["hits"][0];
    assert_eq!(friday["fact"]["fact_id"], friday_id);
    assert_eq!(friday["fact"]["tags"], json!(["books"]));
    assert_eq!(friday["fact"]["source_label"], "probe-ledger");
    assert_eq!(friday["fact"]["metadata"], json!({"plan": "probe"}));
    assert_eq!(friday["fact"]["source"]["kind"], "application");

    // Surrounding punctuation is stripped before the case-insensitive compare,
    // so this is the same entity as `Northwind Ledger`.
    let folded = probe(&cg.server, json!({"entity": "(northwind ledger)"})).await;
    assert_probe_hits(
        &folded,
        &[
            (FRIDAY, "project", &[LEDGER], 750_000),
            (AUDIT, "decision", &[LEDGER], 500_000),
        ],
    );

    let trusted = probe(&cg.server, json!({"entity": LEDGER, "min_trust": 0.6})).await;
    assert_eq!(trusted["next_after"], Value::Null);
    assert_probe_hits(&trusted, &[(FRIDAY, "project", &[LEDGER], 750_000)]);

    let including_scratch = probe(&cg.server, json!({"entity": LEDGER, "min_trust": 0.0})).await;
    assert_probe_hits(
        &including_scratch,
        &[
            (FRIDAY, "project", &[LEDGER], 750_000),
            (AUDIT, "decision", &[LEDGER], 500_000),
            (SCRATCH, "project", &[LEDGER], 250_000),
        ],
    );

    let decisions = probe(
        &cg.server,
        json!({"entity": LEDGER, "category": "decision", "min_trust": 0.0}),
    )
    .await;
    assert_probe_hits(&decisions, &[(AUDIT, "decision", &[LEDGER], 500_000)]);

    let orchard = probe(&cg.server, json!({"entity": "Orchard Weather"})).await;
    assert_eq!(orchard["next_after"], Value::Null);
    assert_probe_hits(
        &orchard,
        &[(ORCHARD, "project", &["Orchard Weather"], 750_000)],
    );

    let user = probe(
        &cg.server,
        json!({"entity": LEDGER, "memory_scope": "user"}),
    )
    .await;
    assert_eq!(user["owner"], json!({"kind": "profile"}));
    assert_eq!(user["next_after"], Value::Null);
    assert_probe_hits(&user, &[(USER_PREF, "user_pref", &[LEDGER], 750_000)]);

    let first_page = probe(&cg.server, json!({"entity": LEDGER, "limit": 1})).await;
    assert_probe_hits(&first_page, &[(FRIDAY, "project", &[LEDGER], 750_000)]);
    assert_eq!(first_page["next_after"]["score_millionths"], 750_000);
    assert_eq!(first_page["next_after"]["fact_id"], friday_id);
    let second_page = probe(
        &cg.server,
        json!({
            "entity": LEDGER,
            "limit": 1,
            "after": first_page["next_after"].clone()
        }),
    )
    .await;
    assert_eq!(second_page["next_after"], Value::Null);
    assert_probe_hits(&second_page, &[(AUDIT, "decision", &[LEDGER], 500_000)]);

    let unknown = probe(&cg.server, json!({"entity": "Missing Beacon"})).await;
    assert_eq!(
        unknown["owner"],
        json!({"kind": "project", "project_id": project_id})
    );
    assert_eq!(unknown["hits"], json!([]));
    assert_eq!(unknown["next_after"], Value::Null);
    assert_eq!(unknown["graph_coverage"], json!({"kind": "not_applicable"}));
    for entity in ["", "   "] {
        let blank = handle_real_server_tool_call(
            &cg.server,
            "tracedecay_fact_store_probe",
            json!({"entity": entity}),
        )
        .await;
        assert_invalid_request(&blank);
    }

    let missing_entity = handle_real_server_tool_call_raw(
        &cg.server,
        "tracedecay_fact_store_probe",
        json!({"category": "project"}),
    )
    .await;
    assert_schema_refusal(&missing_entity, "missing field `entity`");
    let unknown_field = handle_real_server_tool_call_raw(
        &cg.server,
        "tracedecay_fact_store_probe",
        json!({"entity": LEDGER, "not_a_probe_field": true}),
    )
    .await;
    assert_schema_refusal(&unknown_field, "unknown field `not_a_probe_field`");

    let invalid_limit = handle_real_server_tool_call(
        &cg.server,
        "tracedecay_fact_store_probe",
        json!({"entity": LEDGER, "limit": 0}),
    )
    .await;
    assert_invalid_request(&invalid_limit);

    let monday_id = add_fact(
        &cg.server,
        json!({
            "content": MONDAY,
            "category": "project",
            "entities": [LEDGER],
            "trust": 1.0
        }),
    )
    .await;
    let superseded = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_supersede",
        json!({"fact_id": friday_id, "superseded_by": monday_id}),
    )
    .await
    .expect("supersede the Friday fact");
    assert_eq!(superseded["outcome"], "superseded");
    let after_supersede = probe(&cg.server, json!({"entity": LEDGER})).await;
    assert_probe_hits(
        &after_supersede,
        &[
            (MONDAY, "project", &[LEDGER], 1_000_000),
            (AUDIT, "decision", &[LEDGER], 500_000),
        ],
    );

    close_test_graph(cg).await;
}

#[tokio::test]
async fn fact_store_probe_reads_only_the_selected_registered_project() {
    let fixture = fact_store_cross_project_fixture().await;
    let active_project_id = fixture
        .active_server
        .cg()
        .await
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("active probe project id")
        .to_owned();
    let target_project_id = fixture
        .target_server
        .cg()
        .await
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("target probe project id")
        .to_owned();

    invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_add",
        json!({
            "content": "Active beacon stays on the active project",
            "category": "project",
            "entities": ["Selector Beacon"],
            "trust": 0.75
        }),
    )
    .await
    .expect("active beacon");
    invoke_exact_tool(
        &fixture.target_server,
        "tracedecay_fact_store_add",
        json!({
            "content": "Target beacon stays on the registered project",
            "category": "project",
            "entities": ["Selector Beacon"],
            "trust": 0.75
        }),
    )
    .await
    .expect("target beacon");

    let active = probe(&fixture.active_server, json!({"entity": "Selector Beacon"})).await;
    assert_eq!(
        active["owner"],
        json!({"kind": "project", "project_id": active_project_id})
    );
    assert_probe_hits(
        &active,
        &[(
            "Active beacon stays on the active project",
            "project",
            &["Selector Beacon"],
            750_000,
        )],
    );

    let selected = probe(
        &fixture.active_server,
        json!({
            "entity": "Selector Beacon",
            "project_selector": {"project_id": target_project_id}
        }),
    )
    .await;
    assert_eq!(
        selected["owner"],
        json!({"kind": "project", "project_id": target_project_id})
    );
    assert_probe_hits(
        &selected,
        &[(
            "Target beacon stays on the registered project",
            "project",
            &["Selector Beacon"],
            750_000,
        )],
    );

    let missing = handle_real_server_tool_call_raw(
        &fixture.active_server,
        "tracedecay_fact_store_probe",
        json!({
            "entity": "Selector Beacon",
            "project_selector": {"project_id": "project.missing"}
        }),
    )
    .await;
    let error = &missing["error"];
    assert_eq!(error["code"], -32602, "{missing}");
    assert_eq!(
        error["message"],
        "tool project route failed: reason_code=project_route_not_found retryable=false: registered project not found for project_selector.project_id=project.missing; run tracedecay_project_search"
    );
    assert_eq!(error["data"]["tool"], "tracedecay_fact_store_probe");
    assert_eq!(error["data"]["reason_code"], "project_route_not_found");
    assert_eq!(error["data"]["retryable"], false);
    assert_eq!(
        error["data"]["detail"],
        "registered project not found for project_selector.project_id=project.missing; run tracedecay_project_search"
    );

    fixture.harness.shutdown().await;
}

async fn add_fact(server: &tracedecay::mcp::McpServer, arguments: Value) -> String {
    let added = invoke_exact_tool(server, "tracedecay_fact_store_add", arguments)
        .await
        .expect("fact_store_add");
    assert_eq!(added["outcome"], "committed");
    assert_eq!(added["result"]["disposition"], "added");
    added["result"]["fact"]["fact"]["fact_id"]
        .as_str()
        .expect("added fact id")
        .to_owned()
}

async fn probe(server: &tracedecay::mcp::McpServer, arguments: Value) -> Value {
    invoke_exact_tool(server, "tracedecay_fact_store_probe", arguments)
        .await
        .expect("fact_store_probe")
}

fn assert_probe_hits(payload: &Value, expected: &[(&str, &str, &[&str], u64)]) {
    assert_eq!(
        payload["graph_coverage"],
        json!({"kind": "not_applicable"}),
        "{payload}"
    );
    let hits = payload["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("probe hits: {payload}"));
    assert_eq!(hits.len(), expected.len(), "{payload}");
    for (hit, (content, category, entities, trust)) in hits.iter().zip(expected) {
        assert_eq!(hit["fact"]["content"], *content, "{hit}");
        assert_eq!(hit["fact"]["category"], *category, "{hit}");
        assert_eq!(hit["fact"]["entities"], json!(entities), "{hit}");
        assert_eq!(hit["fact"]["trust_score_millionths"], *trust, "{hit}");
        assert_eq!(hit["why"], "entity probe", "{hit}");
        assert_eq!(
            hit["scores"],
            json!({
                "score_millionths": trust,
                "fts_score_millionths": 0,
                "jaccard_score_millionths": 0,
                "holographic_score_millionths": 1_000_000,
                "trust_score_millionths": trust,
            }),
            "{hit}"
        );
    }
}

fn assert_invalid_request(result: &Value) {
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(result["structuredContent"]["problem"]["kind"], "invalid_request", "{result}");
    assert_eq!(
        result["structuredContent"]["problem"]["code"],
        "application.retained.invalid-request"
    );
    assert_eq!(
        result["structuredContent"]["problem"]["message"],
        "The retained operation request is invalid."
    );
    assert_eq!(
        result["structuredContent"]["problem"]["diagnostic"],
        json!({
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid."
        })
    );
    assert_eq!(result["structuredContent"]["problem"]["retry"], "never");
    assert_eq!(result["structuredContent"]["problem"]["retryable"], false);
    assert_eq!(
        result["structuredContent"]["problem"]["legal_actions"],
        json!(["correct_request"])
    );
}

fn assert_schema_refusal(response: &Value, detail: &str) {
    let error = &response["error"];
    assert_eq!(error["code"], -32603, "{response}");
    assert_eq!(
        error["message"],
        format!(
            "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_probe: {detail}"
        ),
        "{response}"
    );
    assert_eq!(error["data"]["tool"], "tracedecay_fact_store_probe");
}

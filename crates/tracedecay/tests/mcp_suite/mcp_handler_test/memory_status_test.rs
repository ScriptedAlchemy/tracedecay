#![cfg(feature = "test-transport")]

//! `tracedecay_memory_status` as a caller sees it: one production MCP call,
//! one concrete memory, one literal report.

use serde_json::{Value, json};

use super::memory_facts_test::{
    FactStoreMcpFixture, active_project_id, close_test_graph, invoke_production_tool,
    invoke_production_tool_response, setup_project,
};

fn committed_fact_id(added: &Value) -> String {
    assert_eq!(added["outcome"], "committed");
    assert_eq!(added["result"]["disposition"], "added");
    added
        .pointer("/result/fact/fact/fact_id")
        .and_then(Value::as_str)
        .expect("committed add returns an available fact id")
        .to_owned()
}

fn quiet_funnel() -> Value {
    json!({
        "retrieval_count_total": 0,
        "access_count_total": 0,
        "retrieved_fact_count": 0,
        "rated_fact_count": 0,
        "feedback_total": 0,
        "seen_to_feedback_ratio": null,
    })
}

fn expect_memory_report(status: &Value, expected: Value, context: &str) {
    let Some(memory) = status.get("memory").and_then(Value::as_object) else {
        panic!("memory status omitted its report: {status}");
    };
    let mut fields: Vec<_> = memory.keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "algebra",
            "below_default_recall_threshold_count",
            "entity_count",
            "fact_count",
            "feedback_funnel",
            "helpful_count",
            "owner",
            "trust_025_050_count",
            "trust_050_075_count",
            "trust_075_100_count",
            "trust_0_025_count",
            "unhelpful_count",
        ]
    );
    assert_eq!(
        json!({
            "owner": memory["owner"],
            "fact_count": memory["fact_count"],
            "entity_count": memory["entity_count"],
            "trust_0_025_count": memory["trust_0_025_count"],
            "trust_025_050_count": memory["trust_025_050_count"],
            "trust_050_075_count": memory["trust_050_075_count"],
            "trust_075_100_count": memory["trust_075_100_count"],
            "below_default_recall_threshold_count": memory["below_default_recall_threshold_count"],
            "helpful_count": memory["helpful_count"],
            "unhelpful_count": memory["unhelpful_count"],
            "feedback_funnel": memory["feedback_funnel"],
        }),
        expected,
        "{context}: {status}"
    );
}

async fn memory_status(fixture: &FactStoreMcpFixture, arguments: Value) -> Value {
    invoke_production_tool(fixture, "tracedecay_memory_status", arguments)
        .await
        .expect("tracedecay_memory_status")
}

async fn add_fact(fixture: &FactStoreMcpFixture, arguments: Value) -> String {
    let added = invoke_production_tool(fixture, "tracedecay_fact_store_add", arguments)
        .await
        .expect("fact add");
    committed_fact_id(&added)
}

#[tokio::test]
async fn memory_status_reports_the_seeded_project_and_keeps_user_memory_separate() {
    let fixture = setup_project().await;
    let project_id = active_project_id(&fixture).await;
    let project_owner = json!({"kind": "project", "project_id": project_id});

    let empty = memory_status(&fixture, json!({})).await;
    expect_memory_report(
        &empty,
        json!({
            "owner": project_owner,
            "fact_count": 0,
            "entity_count": 0,
            "trust_0_025_count": 0,
            "trust_025_050_count": 0,
            "trust_050_075_count": 0,
            "trust_075_100_count": 0,
            "below_default_recall_threshold_count": 0,
            "helpful_count": 0,
            "unhelpful_count": 0,
            "feedback_funnel": quiet_funnel(),
        }),
        "empty project",
    );
    let explicit_project = memory_status(&fixture, json!({"memory_scope": "project"})).await;
    expect_memory_report(
        &explicit_project,
        json!({
            "owner": project_owner,
            "fact_count": 0,
            "entity_count": 0,
            "trust_0_025_count": 0,
            "trust_025_050_count": 0,
            "trust_050_075_count": 0,
            "trust_075_100_count": 0,
            "below_default_recall_threshold_count": 0,
            "helpful_count": 0,
            "unhelpful_count": 0,
            "feedback_funnel": quiet_funnel(),
        }),
        "explicit project scope on an empty store",
    );

    let low_id = add_fact(
        &fixture,
        json!({
            "content": "Billing hold is manual until finance confirms",
            "category": "project",
            "trust": 0.24,
            "entities": ["Billing"]
        }),
    )
    .await;
    let mid_id = add_fact(
        &fixture,
        json!({
            "content": "Routing prefers the regional gateway",
            "category": "project",
            "trust": 0.36,
            "entities": ["Routing"]
        }),
    )
    .await;
    add_fact(
        &fixture,
        json!({
            "content": "Cache keys include the tenant id",
            "category": "project",
            "trust": 0.55,
            "entities": ["billing", "Cache"]
        }),
    )
    .await;
    let high_id = add_fact(
        &fixture,
        json!({
            "content": "Deploy gate quark-9042 blocks unsigned artifacts",
            "category": "project",
            "trust": 0.90,
            "entities": ["Deploy Gate"]
        }),
    )
    .await;

    let seeded = json!({
        "owner": project_owner,
        "fact_count": 4,
        "entity_count": 4,
        "trust_0_025_count": 1,
        "trust_025_050_count": 1,
        "trust_050_075_count": 1,
        "trust_075_100_count": 1,
        "below_default_recall_threshold_count": 1,
        "helpful_count": 0,
        "unhelpful_count": 0,
        "feedback_funnel": quiet_funnel(),
    });
    expect_memory_report(
        &memory_status(&fixture, json!({})).await,
        seeded.clone(),
        "four seeded project facts",
    );

    let searched = invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_search",
        json!({
            "query": "quark-9042",
            "min_trust": 0.8,
            "limit": 1
        }),
    )
    .await
    .expect("fact search");
    let recalled = json!({
        "owner": project_owner,
        "fact_count": 4,
        "entity_count": 4,
        "trust_0_025_count": 1,
        "trust_025_050_count": 1,
        "trust_050_075_count": 1,
        "trust_075_100_count": 1,
        "below_default_recall_threshold_count": 1,
        "helpful_count": 0,
        "unhelpful_count": 0,
        "feedback_funnel": {
            "retrieval_count_total": 1,
            "access_count_total": 1,
            "retrieved_fact_count": 1,
            "rated_fact_count": 0,
            "feedback_total": 0,
            "seen_to_feedback_ratio": null,
        },
    });
    let after_search = memory_status(&fixture, json!({})).await;
    let search_context = format!("after searching quark-9042: {searched}");
    expect_memory_report(&after_search, recalled, &search_context);

    invoke_production_tool(
        &fixture,
        "tracedecay_fact_feedback",
        json!({"fact_id": low_id, "action": "helpful"}),
    )
    .await
    .expect("helpful feedback");
    invoke_production_tool(
        &fixture,
        "tracedecay_fact_feedback",
        json!({"fact_id": mid_id, "action": "unhelpful"}),
    )
    .await
    .expect("unhelpful feedback");

    // Feedback moves 0.24 to 0.29 and 0.36 to 0.26. Both scores sit in the
    // 0.25 bucket and under the 0.30 recall floor, so the floor count rises
    // from the single untouched 0.24 fact to both rated facts.
    let rated = json!({
        "owner": project_owner,
        "fact_count": 4,
        "entity_count": 4,
        "trust_0_025_count": 0,
        "trust_025_050_count": 2,
        "trust_050_075_count": 1,
        "trust_075_100_count": 1,
        "below_default_recall_threshold_count": 2,
        "helpful_count": 1,
        "unhelpful_count": 1,
        "feedback_funnel": {
            "retrieval_count_total": 1,
            "access_count_total": 1,
            "retrieved_fact_count": 1,
            "rated_fact_count": 2,
            "feedback_total": 2,
            "seen_to_feedback_ratio": 1,
        },
    });
    expect_memory_report(
        &memory_status(&fixture, json!({})).await,
        rated.clone(),
        "after helpful and unhelpful feedback",
    );
    expect_memory_report(
        &memory_status(&fixture, json!({})).await,
        rated.clone(),
        "a second status read must not change the report",
    );

    add_fact(
        &fixture,
        json!({
            "content": "User prefers terse status lines",
            "category": "user_pref",
            "memory_scope": "user",
            "entities": ["Voice"]
        }),
    )
    .await;
    expect_memory_report(
        &memory_status(&fixture, json!({})).await,
        rated.clone(),
        "project status ignores the user-scoped fact",
    );
    expect_memory_report(
        &memory_status(&fixture, json!({"memory_scope": "user"})).await,
        json!({
            "owner": {"kind": "profile"},
            "fact_count": 1,
            "entity_count": 1,
            "trust_0_025_count": 0,
            "trust_025_050_count": 0,
            "trust_050_075_count": 1,
            "trust_075_100_count": 0,
            "below_default_recall_threshold_count": 0,
            "helpful_count": 0,
            "unhelpful_count": 0,
            "feedback_funnel": quiet_funnel(),
        }),
        "user scope",
    );

    let denied = invoke_production_tool_response(
        &fixture,
        "tracedecay_memory_status",
        json!({"project_selector": {"project_id": "project.missing"}}),
    )
    .await;
    assert_eq!(
        denied["error"]["data"]["tool"], "tracedecay_memory_status",
        "denied selector response: {denied}"
    );
    assert_eq!(
        denied["error"]["data"]["reason_code"],
        "application_surface_not_found_or_not_authorized"
    );
    assert_eq!(denied["error"]["data"]["kind"], "denied");
    assert_eq!(denied["error"]["data"]["retryable"], false);
    assert_eq!(denied.get("result"), None);
    expect_memory_report(
        &memory_status(
            &fixture,
            json!({"project_selector": {"project_id": project_id}}),
        )
        .await,
        rated.clone(),
        "registered project selector",
    );

    let invalid = invoke_production_tool_response(
        &fixture,
        "tracedecay_memory_status",
        json!({"memory_scope": "galaxy"}),
    )
    .await;
    assert_eq!(
        invalid["error"]["data"]["tool"], "tracedecay_memory_status",
        "invalid scope response: {invalid}"
    );
    assert_eq!(
        invalid["error"]["data"]["reason_code"],
        "application_surface_invalid_request"
    );
    assert_eq!(invalid["error"]["data"]["kind"], "invalid_request");

    invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_remove",
        json!({"fact_id": high_id}),
    )
    .await
    .expect("fact remove");
    expect_memory_report(
        &memory_status(&fixture, json!({})).await,
        json!({
            "owner": project_owner,
            "fact_count": 3,
            "entity_count": 3,
            "trust_0_025_count": 0,
            "trust_025_050_count": 2,
            "trust_050_075_count": 1,
            "trust_075_100_count": 0,
            "below_default_recall_threshold_count": 2,
            "helpful_count": 1,
            "unhelpful_count": 1,
            "feedback_funnel": {
                "retrieval_count_total": 0,
                "access_count_total": 0,
                "retrieved_fact_count": 0,
                "rated_fact_count": 2,
                "feedback_total": 2,
                "seen_to_feedback_ratio": 0,
            },
        }),
        "after removing the only recalled fact",
    );

    close_test_graph(fixture).await;
}

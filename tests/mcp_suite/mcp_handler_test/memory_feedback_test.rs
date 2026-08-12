#![cfg(feature = "test-transport")]

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay::mcp::get_tool_definitions;

use super::memory_facts_test::{close_test_graph, invoke_production_tool, setup_project};

fn committed_fact_id(added: &Value) -> String {
    added
        .pointer("/result/fact/fact/fact_id")
        .and_then(Value::as_str)
        .expect("committed add must return an available canonical fact")
        .to_owned()
}

#[test]
fn fact_feedback_schema_is_canonical_and_excludes_legacy_aliases() {
    let tools = get_tool_definitions().expect("tool definitions");
    let feedback = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_fact_feedback")
        .expect("tracedecay_fact_feedback definition");
    let properties = feedback.input_schema["properties"]
        .as_object()
        .expect("fact feedback properties");

    assert_eq!(
        feedback.annotations.as_ref().unwrap()["readOnlyHint"],
        false
    );
    assert_eq!(
        feedback.input_schema["required"],
        json!(["fact_id", "action"])
    );
    assert_eq!(properties["fact_id"]["type"], "string");
    for field in [
        "expected_last_event_id",
        "source_label",
        "reason",
        "memory_scope",
        "project_selector",
    ] {
        assert!(
            properties.contains_key(field),
            "missing canonical field {field}"
        );
    }
    for alias in [
        "helpful",
        "unhelpful",
        "trust_delta",
        "source",
        "metadata",
        "note",
    ] {
        assert!(
            !properties.contains_key(alias),
            "legacy alias survived: {alias}"
        );
    }
}

#[tokio::test]
async fn fact_feedback_persists_canonical_trust_events_and_status() {
    let fixture = setup_project().await;
    let added = invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_add",
        json!({
            "content": "Helpful memory fact for canonical feedback",
            "category": "general"
        }),
    )
    .await
    .expect("canonical fact add");
    let fact_id = committed_fact_id(&added);

    let helpful = invoke_production_tool(
        &fixture,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": fact_id.clone(),
            "action": "helpful",
            "source_label": "mcp-test",
            "reason": "matched"
        }),
    )
    .await
    .expect("canonical helpful feedback");
    let helpful_event_id = helpful["feedback"]["event_id"]
        .as_str()
        .expect("canonical feedback event id")
        .to_owned();
    assert_eq!(helpful["feedback"]["fact_id"], fact_id);
    assert_eq!(helpful["feedback"]["action"], "helpful");
    assert_eq!(helpful["feedback"]["old_trust_millionths"], 500_000);
    assert!(
        helpful["feedback"]["new_trust_millionths"]
            .as_u64()
            .unwrap()
            > 500_000
    );
    assert!(
        helpful["feedback"]["trust_delta_millionths"]
            .as_i64()
            .unwrap()
            > 0
    );
    assert_eq!(helpful["feedback"]["helpful_count"], 1);
    assert_eq!(helpful["feedback"]["unhelpful_count"], 0);
    assert_eq!(helpful["fact"]["kind"], "available");
    assert_eq!(helpful["fact"]["fact"]["fact_id"], fact_id);
    assert_eq!(helpful["commit"]["fact_id"], fact_id);
    assert_eq!(helpful["commit"]["last_event_id"], helpful_event_id);

    let unhelpful = invoke_production_tool(
        &fixture,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": fact_id.clone(),
            "expected_last_event_id": helpful_event_id,
            "action": "unhelpful"
        }),
    )
    .await
    .expect("canonical unhelpful feedback");
    assert_eq!(unhelpful["feedback"]["action"], "unhelpful");
    assert!(
        unhelpful["feedback"]["new_trust_millionths"]
            .as_u64()
            .unwrap()
            < helpful["feedback"]["new_trust_millionths"]
                .as_u64()
                .unwrap()
    );
    assert_eq!(unhelpful["feedback"]["helpful_count"], 1);
    assert_eq!(unhelpful["feedback"]["unhelpful_count"], 1);

    let fetched = invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_get",
        json!({"fact_id": fact_id.clone()}),
    )
    .await
    .expect("canonical fact get");
    assert_eq!(fetched["fact"]["kind"], "available");
    assert_eq!(fetched["fact"]["fact"]["fact_id"], fact_id);
    let history = fetched["trust_history"]
        .as_array()
        .expect("canonical trust history");
    assert_eq!(history.len(), 2);
    let helpful_history = history
        .iter()
        .find(|entry| entry["action"] == "helpful")
        .expect("helpful history entry");
    assert_eq!(helpful_history["source_label"], "mcp-test");
    assert_eq!(helpful_history["reason"], "matched");
    assert!(helpful_history.get("note").is_none());

    let status = invoke_production_tool(&fixture, "tracedecay_memory_status", json!({}))
        .await
        .expect("canonical memory status");
    assert!(status.get("status").is_none());
    assert!(status["memory"]["fact_count"].as_u64().unwrap() >= 1);
    assert_eq!(status["memory"]["helpful_count"], 1);
    assert_eq!(status["memory"]["unhelpful_count"], 1);

    close_test_graph(fixture).await;
}

#[tokio::test]
async fn fact_feedback_rejects_missing_action_numeric_ids_and_legacy_aliases() {
    let fixture = setup_project().await;
    let added = invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_add",
        json!({"content": "Canonical feedback validation fact"}),
    )
    .await
    .expect("canonical fact add");
    let fact_id = committed_fact_id(&added);

    for invalid in [
        json!({"fact_id": fact_id.clone()}),
        json!({"fact_id": 41, "action": "helpful"}),
        json!({"fact_id": fact_id.clone(), "helpful": true}),
        json!({"fact_id": fact_id, "action": "helpful", "source": "legacy"}),
    ] {
        assert!(
            invoke_production_tool(&fixture, "tracedecay_fact_feedback", invalid)
                .await
                .is_err()
        );
    }

    close_test_graph(fixture).await;
}

#[tokio::test]
async fn fact_feedback_on_a_removed_fact_fails_fast() {
    let fixture = setup_project().await;
    let added = invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_add",
        json!({"content": "A removed fact must reject later feedback"}),
    )
    .await
    .expect("canonical fact add");
    let fact_id = committed_fact_id(&added);
    invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_remove",
        json!({"fact_id": fact_id.clone()}),
    )
    .await
    .expect("canonical fact remove");

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        invoke_production_tool(
            &fixture,
            "tracedecay_fact_feedback",
            json!({"fact_id": fact_id, "action": "helpful"}),
        ),
    )
    .await
    .expect("fact feedback must not wait for the client deadline");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(result.is_err());

    close_test_graph(fixture).await;
}

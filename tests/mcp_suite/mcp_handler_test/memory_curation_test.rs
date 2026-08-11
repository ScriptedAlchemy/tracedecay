#![cfg(feature = "test-transport")]

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
fn fact_store_curate_is_discoverable_with_the_canonical_schema() {
    let tools = get_tool_definitions().expect("tool definitions");
    let curate = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_fact_store_curate")
        .expect("fact_store_curate must be advertised for MCP and generic CLI discovery");
    let properties = curate.input_schema["properties"]
        .as_object()
        .expect("fact-store curation schema properties");

    assert_eq!(curate.annotations.as_ref().unwrap()["readOnlyHint"], false);
    assert_eq!(
        curate.input_schema["required"],
        json!(["min_confidence", "operations"])
    );
    for field in [
        "memory_scope",
        "project_selector",
        "min_confidence",
        "operations",
    ] {
        assert!(
            properties.contains_key(field),
            "missing canonical field {field}"
        );
    }
    for retired in ["action", "ops", "project_id", "project_path"] {
        assert!(
            !properties.contains_key(retired),
            "retired curation field survived: {retired}"
        );
    }
}

#[tokio::test]
async fn fact_store_curate_normalizes_and_links_user_facts_with_durable_replay() {
    let fixture = setup_project().await;
    let source_id = committed_fact_id(
        &invoke_production_tool(
            &fixture,
            "tracedecay_fact_store_add",
            json!({
                "content": "User cache policy requires canonical curation",
                "category": "user_pref",
                "entities": ["user cache policy"],
                "memory_scope": "user"
            }),
        )
        .await
        .expect("add source user fact"),
    );
    let target_id = committed_fact_id(
        &invoke_production_tool(
            &fixture,
            "tracedecay_fact_store_add",
            json!({
                "content": "Canonical curation preserves user memory lineage",
                "category": "user_pref",
                "entities": ["user cache policy"],
                "memory_scope": "user"
            }),
        )
        .await
        .expect("add target user fact"),
    );
    let request = json!({
        "memory_scope": "user",
        "min_confidence": 0.9,
        "operations": [
            {
                "kind": "normalize_tags",
                "fact_id": source_id,
                "tags": ["Canonical Tag", "cache-policy", "canonical tag"],
                "evidence_fact_ids": [target_id],
                "confidence": 0.98
            },
            {
                "kind": "link_facts",
                "source_fact_id": source_id,
                "target_fact_id": target_id,
                "relation": "supports",
                "evidence_fact_ids": [source_id, target_id],
                "confidence": 0.97,
                "source_label": "mcp-canonical-curation-test",
                "metadata": {"basis": "two retained user facts"}
            }
        ]
    });

    let applied = invoke_production_tool(&fixture, "tracedecay_fact_store_curate", request.clone())
        .await
        .expect("curate retained user facts through production MCP");
    assert_eq!(applied["owner"]["kind"], "profile");
    assert_eq!(applied["normalized_tags"], 1);
    assert_eq!(applied["facts_linked"], 1);
    assert_eq!(applied["changed_fact_ids"], json!([source_id, target_id]));
    let commit_receipts = applied["commit_receipts"]
        .as_array()
        .expect("one durable commit receipt per curation operation");
    assert_eq!(commit_receipts.len(), 2);
    assert_eq!(
        commit_receipts[0]["committed_event_ids"]
            .as_array()
            .map(Vec::len),
        Some(2),
        "tag normalization must commit assertion and tags_normalized lineage events"
    );
    assert_eq!(
        commit_receipts[1]["committed_event_ids"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "fact linking must commit its canonical linked lineage event"
    );
    assert!(commit_receipts.iter().all(|receipt| {
        receipt["disposition"] == "committed"
            && receipt["committed_event_ids"]
                .as_array()
                .is_some_and(|events| !events.is_empty())
    }));
    assert!(applied["operation_id"].is_string());
    assert!(applied["replay_fact_id"].is_string());
    assert!(applied["replay_event_id"].is_string());

    let replayed = invoke_production_tool(&fixture, "tracedecay_fact_store_curate", request)
        .await
        .expect("replay identical retained user curation");
    assert_eq!(replayed["operation_id"], applied["operation_id"]);
    assert_eq!(replayed["input_digest"], applied["input_digest"]);
    assert_eq!(replayed["replay_fact_id"], applied["replay_fact_id"]);
    assert_eq!(replayed["replay_event_id"], applied["replay_event_id"]);
    assert!(
        replayed["commit_receipts"]
            .as_array()
            .is_some_and(|receipts| receipts.len() == 2
                && receipts
                    .iter()
                    .all(|receipt| receipt["disposition"] == "idempotent_replay"))
    );

    let fetched = invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_get",
        json!({"memory_scope": "user", "fact_id": source_id}),
    )
    .await
    .expect("reread curated source fact from retained user memory");
    assert_eq!(
        fetched["fact"]["fact"]["tags"],
        json!(["cache_policy", "canonical_tag"])
    );

    close_test_graph(fixture).await;
}

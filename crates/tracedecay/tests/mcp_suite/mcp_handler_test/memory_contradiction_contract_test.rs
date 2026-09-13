use serde_json::json;

use super::memory_facts_test::{close_test_graph, invoke_production_tool, setup_project};

#[tokio::test]
async fn fact_store_contradict_accepts_only_the_exact_bounded_contract() {
    let fixture = setup_project().await;

    let accepted = invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_contradict",
        json!({
            "format": "json",
            "memory_scope": "project",
            "category": "project",
            "threshold_millionths": 300_000,
            "limit": 1
        }),
    )
    .await
    .expect("exact bounded contradiction query");
    assert_eq!(accepted["owner"]["kind"], "project");
    assert!(accepted["contradictions"].is_array());
    assert!(accepted.get("next_after").is_none());

    for rejected in [
        json!({"threshold": 0.3}),
        json!({"min_trust": 0.5}),
        json!({"after": {"fact_id": "fact.v1.invalid"}}),
        json!({"threshold_millionths": 1_000_001}),
        json!({"limit": 0}),
        json!({"limit": 201}),
        json!({"category": "legacy-generalized"}),
    ] {
        assert!(
            invoke_production_tool(
                &fixture,
                "tracedecay_fact_store_contradict",
                rejected.clone()
            )
            .await
            .is_err(),
            "contradiction route accepted noncanonical input: {rejected}"
        );
    }

    close_test_graph(fixture).await;
}

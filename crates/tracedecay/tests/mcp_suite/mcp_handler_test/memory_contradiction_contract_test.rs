use std::fmt::Display;

use serde_json::{Value, json};

use super::memory_facts_test::{
    FactStoreMcpFixture, close_test_graph, invoke_production_tool, setup_project,
};

const SOURCE_LABEL: &str = "contradiction-proof";
const TRUST_MILLIONTHS: u32 = 800_000;

/// Shared-entity Jaccard of the Ledger/Harbor pairs: three overlapping tokens
/// out of four, so divergence is exactly 0.25.
const POLARITY_SCORE_MILLIONTHS: u32 = 250_000;
const POLARITY_WHY: &str = "shared entities with content divergence=0.250";

/// `alpha` against `alpha zebra` is one shared token out of two, divergence 0.5.
const SAME_POLARITY_SCORE_MILLIONTHS: u32 = 500_000;
const SAME_POLARITY_WHY: &str = "shared entities with content divergence=0.500";

#[tokio::test]
async fn fact_store_contradict_reports_the_opposing_fact_above_or_below_threshold() {
    let fixture = setup_project().await;

    let empty = contradict(
        &fixture,
        json!({
            "memory_scope": "project",
            "category": "project",
            "threshold_millionths": 1_000_000,
            "limit": 1
        }),
    )
    .await;
    let owner = empty["owner"].clone();
    assert_eq!(owner["kind"], "project");
    assert_eq!(empty["contradictions"], json!([]));

    let ledger = store_fact(&fixture, "Ledger ships nightly", "project", "Ledger").await;
    store_fact(&fixture, "Ledger never ships nightly", "project", "Ledger").await;
    let harbor = store_fact(&fixture, "Harbor opens daily", "project", "Harbor").await;
    store_fact(&fixture, "Harbor never opens daily", "project", "Harbor").await;
    let alpha = store_fact(&fixture, "alpha", "decision", "alpha").await;
    let alpha_zebra = store_fact(&fixture, "alpha zebra", "decision", "alpha").await;

    // Opposite polarity is reported even when the score is under the threshold.
    let project = contradict(
        &fixture,
        json!({
            "memory_scope": "project",
            "category": "project",
            "threshold_millionths": 1_000_000,
            "limit": 5
        }),
    )
    .await;
    assert_eq!(project["owner"], owner);
    assert_eq!(project["contradictions"].as_array().map(Vec::len), Some(2));
    assert_polarity_contradiction(
        &project,
        "Ledger never ships nightly",
        "Ledger ships nightly",
        "Ledger",
        &ledger,
        &owner,
    );
    assert_polarity_contradiction(
        &project,
        "Harbor never opens daily",
        "Harbor opens daily",
        "Harbor",
        &harbor,
        &owner,
    );

    let limited = contradict(
        &fixture,
        json!({
            "memory_scope": "project",
            "category": "project",
            "threshold_millionths": 1_000_000,
            "limit": 1
        }),
    )
    .await;
    assert_eq!(limited["owner"], owner);
    assert_eq!(limited["contradictions"].as_array().map(Vec::len), Some(1));
    let only = &limited["contradictions"][0];
    match only["new_content"].as_str() {
        Some("Ledger never ships nightly") => assert_polarity_contradiction(
            &limited,
            "Ledger never ships nightly",
            "Ledger ships nightly",
            "Ledger",
            &ledger,
            &owner,
        ),
        Some("Harbor never opens daily") => assert_polarity_contradiction(
            &limited,
            "Harbor never opens daily",
            "Harbor opens daily",
            "Harbor",
            &harbor,
            &owner,
        ),
        other => panic!("limit 1 returned an unknown contradiction {other:?}: {limited}"),
    }

    // Same polarity is included only when the score meets the threshold.
    let decision = contradict(
        &fixture,
        json!({
            "memory_scope": "project",
            "category": "decision",
            "threshold_millionths": 500_000,
            "limit": 5
        }),
    )
    .await;
    assert_eq!(decision["owner"], owner);
    assert_eq!(decision["contradictions"].as_array().map(Vec::len), Some(1));
    assert_same_polarity_contradiction(
        &decision["contradictions"][0],
        &alpha,
        &alpha_zebra,
        &owner,
    );

    let below = contradict(
        &fixture,
        json!({
            "memory_scope": "project",
            "category": "decision",
            "threshold_millionths": 500_001,
            "limit": 5
        }),
    )
    .await;
    assert_eq!(below["owner"], owner);
    assert_eq!(below["contradictions"], json!([]));

    let other_category = contradict(
        &fixture,
        json!({
            "memory_scope": "project",
            "category": "general",
            "threshold_millionths": 0,
            "limit": 5
        }),
    )
    .await;
    assert_eq!(other_category["owner"], owner);
    assert_eq!(other_category["contradictions"], json!([]));

    close_test_graph(fixture).await;
}

#[tokio::test]
async fn fact_store_contradict_rejects_noncanonical_arguments() {
    let fixture = setup_project().await;

    for (arguments, field) in [
        (json!({"threshold": 0.3}), "threshold"),
        (json!({"min_trust": 0.5}), "min_trust"),
        (json!({"after": {"fact_id": "fact.v1.invalid"}}), "after"),
    ] {
        let body = rejection_body(&fixture, arguments).await;
        assert_eq!(body["code"], -32603, "{body}");
        assert_eq!(
            body["message"],
            format!(
                "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_contradict: {field}: unknown field `{field}`, expected one of `threshold_millionths`, `memory_scope`, `category`, `limit`, `project_selector`"
            ),
            "{body}"
        );
    }

    let category = rejection_body(&fixture, json!({"category": "legacy-generalized"})).await;
    assert_eq!(category["code"], -32603, "{category}");
    assert_eq!(
        category["message"],
        "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_contradict: category: unknown variant `legacy-generalized`, expected one of `general`, `user_pref`, `project`, `tool`, `decision`, `code_area`",
        "{category}"
    );

    for arguments in [
        json!({"threshold_millionths": 1_000_001}),
        json!({"limit": 0}),
        json!({"limit": 201}),
    ] {
        let body = rejection_body(&fixture, arguments.clone()).await;
        assert_eq!(
            body["problem"]["kind"], "invalid_request",
            "{arguments}: {body}"
        );
        assert_eq!(
            body["problem"]["code"], "application.retained.invalid-request",
            "{arguments}: {body}"
        );
        assert_eq!(
            body["problem"]["message"], "The retained operation request is invalid.",
            "{arguments}: {body}"
        );
        assert_eq!(body["problem"]["retry"], "never", "{arguments}: {body}");
        assert_eq!(body["problem"]["retryable"], false, "{arguments}: {body}");
        assert_eq!(
            body["problem"]["legal_actions"],
            json!(["correct_request"]),
            "{arguments}: {body}"
        );
    }

    close_test_graph(fixture).await;
}

async fn contradict(fixture: &FactStoreMcpFixture, arguments: Value) -> Value {
    invoke_production_tool(fixture, "tracedecay_fact_store_contradict", arguments)
        .await
        .expect("fact store contradict")
}

async fn store_fact(
    fixture: &FactStoreMcpFixture,
    content: &str,
    category: &str,
    entity: &str,
) -> String {
    let added = invoke_production_tool(
        fixture,
        "tracedecay_fact_store_add",
        json!({
            "content": content,
            "category": category,
            "entities": [entity],
            "tags": [],
            "trust": 0.8,
            "source_label": SOURCE_LABEL,
            "metadata": {}
        }),
    )
    .await
    .unwrap_or_else(|error| panic!("store {content:?}: {error}"));
    assert_eq!(added["outcome"], "committed", "{added}");
    assert_eq!(added["result"]["fact"]["kind"], "available", "{added}");
    assert_eq!(
        added["result"]["fact"]["fact"]["content"], content,
        "{added}"
    );
    added["result"]["fact"]["fact"]["fact_id"]
        .as_str()
        .unwrap_or_else(|| panic!("stored fact id: {added}"))
        .to_owned()
}

fn assert_polarity_contradiction(
    page: &Value,
    new_content: &str,
    existing_content: &str,
    entity: &str,
    existing_fact_id: &str,
    owner: &Value,
) {
    let item = page["contradictions"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["new_content"].as_str() == Some(new_content))
        })
        .unwrap_or_else(|| panic!("missing contradiction for {new_content}: {page}"));
    assert_eq!(item["new_content"], new_content);
    assert_eq!(item["score_millionths"], POLARITY_SCORE_MILLIONTHS);
    assert_eq!(item["why"], POLARITY_WHY);
    assert_eq!(item["existing_fact"]["content"], existing_content);
    assert_eq!(item["existing_fact"]["fact_id"], existing_fact_id);
    assert_eq!(item["existing_fact"]["category"], "project");
    assert_eq!(item["existing_fact"]["entities"], json!([entity]));
    assert_stored_fact_shape(&item["existing_fact"], owner);
}

fn assert_same_polarity_contradiction(
    item: &Value,
    alpha_id: &str,
    alpha_zebra_id: &str,
    owner: &Value,
) {
    assert_eq!(item["score_millionths"], SAME_POLARITY_SCORE_MILLIONTHS);
    assert_eq!(item["why"], SAME_POLARITY_WHY);
    let existing_content = item["existing_fact"]["content"].as_str().unwrap_or("");
    let new_content = item["new_content"].as_str().unwrap_or("");
    let mut contents = [existing_content, new_content];
    contents.sort_unstable();
    assert_eq!(contents, ["alpha", "alpha zebra"]);
    let expected_id = if existing_content == "alpha" {
        alpha_id
    } else {
        alpha_zebra_id
    };
    assert_eq!(item["existing_fact"]["fact_id"], expected_id);
    assert_eq!(item["existing_fact"]["category"], "decision");
    assert_eq!(item["existing_fact"]["entities"], json!(["alpha"]));
    assert_stored_fact_shape(&item["existing_fact"], owner);
}

fn assert_stored_fact_shape(fact: &Value, owner: &Value) {
    assert_eq!(fact["owner"], *owner);
    assert_eq!(fact["tags"], json!([]));
    assert_eq!(fact["trust_score_millionths"], TRUST_MILLIONTHS);
    assert_eq!(fact["source"]["kind"], "application");
    assert_eq!(fact["source_label"], SOURCE_LABEL);
    assert_eq!(fact["metadata"], json!({}));
    assert_eq!(fact["telemetry"]["retrieval_count"], 0);
    assert_eq!(fact["telemetry"]["access_count"], 0);
    assert_eq!(fact["telemetry"]["helpful_count"], 0);
    assert_eq!(fact["telemetry"]["unhelpful_count"], 0);
}

async fn rejection_body(fixture: &FactStoreMcpFixture, arguments: Value) -> Value {
    let error = invoke_production_tool(fixture, "tracedecay_fact_store_contradict", arguments)
        .await
        .expect_err("noncanonical contradiction input must be rejected");
    parse_rejection(&error)
}

fn parse_rejection(error: &impl Display) -> Value {
    let rendered = error.to_string();
    let json_at = rendered
        .find('{')
        .unwrap_or_else(|| panic!("contradiction rejection was not JSON: {rendered}"));
    serde_json::from_str(&rendered[json_at..]).unwrap_or_else(|parse_error| {
        panic!("contradiction rejection was not JSON ({parse_error}): {rendered}")
    })
}

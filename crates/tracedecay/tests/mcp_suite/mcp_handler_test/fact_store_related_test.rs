#![cfg(feature = "test-transport")]

//! Behavioral proof of `tracedecay_fact_store_related` through the production
//! MCP tool call.
//!
//! The mounted project-memory graph expands the source fact's component after
//! the trust floor is applied, so a co-occurring fact below that floor still
//! comes back, ranked by trust. An entity with no stored fact returns no hits.
//! A fact outside the component stays out of that page.

use serde_json::{Value, json};

use super::memory_facts_test::{
    FactStoreMcpFixture, close_test_graph, invoke_production_tool, setup_project,
};

const SOURCE: &str = "Harbor ledger binds rust crates through a shared dock";
const NEIGHBOR: &str = "Rust crates feed the compiler cache nightly";
const QUIET: &str = "A quiet rust-crate note stays below the default trust floor";
const ISOLATED: &str = "Weather stays isolated from the harbor ledger";
const SOURCE_ENTITY: &str = "Harbor Ledger";
const SHARED_ENTITY: &str = "Rust Crates";
const NEIGHBOR_ENTITY: &str = "Compiler Cache";
const ISOLATED_ENTITY: &str = "Weather Desk";
const SOURCE_WHY: &str = "entity/relation co-occurrence from Harbor Ledger";
const FOLDED_WHY: &str = "entity/relation co-occurrence from harbor ledger";
const ISOLATED_WHY: &str = "entity/relation co-occurrence from Weather Desk";

fn component_graph(expanded_fact_count: usize) -> Value {
    json!({
        "kind": "complete",
        "root_count": 1,
        "relation_count": 8,
        "expanded_fact_count": expanded_fact_count,
    })
}

struct StoredFact {
    fact_id: String,
    owner: Value,
}

async fn store_fact(
    fixture: &FactStoreMcpFixture,
    content: &str,
    category: &str,
    entities: &[&str],
    trust: f64,
) -> StoredFact {
    let added = invoke_production_tool(
        fixture,
        "tracedecay_fact_store_add",
        json!({
            "content": content,
            "category": category,
            "entities": entities,
            "trust": trust,
            "source_label": "related-proof",
        }),
    )
    .await
    .expect("fact store add");
    assert_eq!(added["outcome"], "committed", "{added}");
    assert_eq!(added["result"]["disposition"], "added", "{added}");
    let fact = &added["result"]["fact"]["fact"];
    assert_eq!(fact["content"], content, "{added}");
    StoredFact {
        fact_id: fact["fact_id"].as_str().expect("added fact id").to_owned(),
        owner: fact["owner"].clone(),
    }
}

async fn related(fixture: &FactStoreMcpFixture, arguments: Value) -> Value {
    invoke_production_tool(fixture, "tracedecay_fact_store_related", arguments)
        .await
        .expect("fact store related")
}

fn observed_hit(hit: &Value) -> Value {
    json!({
        "fact_id": hit["fact"]["fact_id"],
        "content": hit["fact"]["content"],
        "category": hit["fact"]["category"],
        "entities": hit["fact"]["entities"],
        "tags": hit["fact"]["tags"],
        "trust_score_millionths": hit["fact"]["trust_score_millionths"],
        "source_kind": hit["fact"]["source"]["kind"],
        "source_label": hit["fact"]["source_label"],
        "retrieval_count": hit["fact"]["telemetry"]["retrieval_count"],
        "access_count": hit["fact"]["telemetry"]["access_count"],
        "helpful_count": hit["fact"]["telemetry"]["helpful_count"],
        "unhelpful_count": hit["fact"]["telemetry"]["unhelpful_count"],
        "metadata": hit["fact"]["metadata"],
        "why": hit["why"],
        "scores": hit["scores"],
    })
}

fn expected_hit(
    fact_id: &str,
    content: &str,
    category: &str,
    entities: &[&str],
    trust_millionths: u32,
    why: &str,
) -> Value {
    json!({
        "fact_id": fact_id,
        "content": content,
        "category": category,
        "entities": entities,
        "tags": [],
        "trust_score_millionths": trust_millionths,
        "source_kind": "application",
        "source_label": "related-proof",
        "retrieval_count": 0,
        "access_count": 0,
        "helpful_count": 0,
        "unhelpful_count": 0,
        "metadata": {},
        "why": why,
        "scores": {
            "score_millionths": trust_millionths,
            "fts_score_millionths": 0,
            "jaccard_score_millionths": 0,
            "holographic_score_millionths": 1_000_000,
            "trust_score_millionths": trust_millionths,
        },
    })
}

fn assert_related_page(page: &Value, owner: &Value, hits: Value, graph_coverage: Value) {
    assert!(
        page.get("retrieval_telemetry").is_none(),
        "related does not record search retrieval telemetry: {page}"
    );
    assert_eq!(
        json!({
            "owner": page["owner"],
            "next_after": page["next_after"],
            "graph_coverage": page["graph_coverage"],
            "hits": page["hits"].as_array().expect("hits").iter().map(observed_hit).collect::<Vec<_>>(),
        }),
        json!({
            "owner": owner,
            "next_after": Value::Null,
            "graph_coverage": graph_coverage,
            "hits": hits,
        }),
        "related page: {page}"
    );
}

fn component_hits(source_id: &str, neighbor_id: &str, quiet_id: &str, why: &str) -> Value {
    json!([
        expected_hit(
            source_id,
            SOURCE,
            "decision",
            &[SOURCE_ENTITY, SHARED_ENTITY],
            910_000,
            why,
        ),
        expected_hit(
            neighbor_id,
            NEIGHBOR,
            "project",
            &[NEIGHBOR_ENTITY, SHARED_ENTITY],
            620_000,
            why,
        ),
        expected_hit(quiet_id, QUIET, "decision", &[SHARED_ENTITY], 200_000, why),
    ])
}

fn rejected_problem(error: &tracedecay_domain::errors::TraceDecayError) -> Value {
    let rendered = error.to_string();
    let json_start = rendered
        .find('{')
        .unwrap_or_else(|| panic!("related rejection was not a problem envelope: {rendered}"));
    serde_json::from_str(&rendered[json_start..])
        .unwrap_or_else(|parse_error| panic!("related rejection JSON ({parse_error}): {rendered}"))
}

#[tokio::test]
async fn fact_store_related_lists_cooccurring_facts_and_omits_isolated_ones() {
    let fixture = setup_project().await;
    let source = store_fact(
        &fixture,
        SOURCE,
        "decision",
        &[SOURCE_ENTITY, SHARED_ENTITY],
        0.91,
    )
    .await;
    let neighbor = store_fact(
        &fixture,
        NEIGHBOR,
        "project",
        &[SHARED_ENTITY, NEIGHBOR_ENTITY],
        0.62,
    )
    .await;
    let quiet = store_fact(&fixture, QUIET, "decision", &[SHARED_ENTITY], 0.2).await;
    let isolated = store_fact(&fixture, ISOLATED, "decision", &[ISOLATED_ENTITY], 0.99).await;

    let hits = component_hits(
        &source.fact_id,
        &neighbor.fact_id,
        &quiet.fact_id,
        SOURCE_WHY,
    );
    let page = related(&fixture, json!({"entity": SOURCE_ENTITY, "limit": 10})).await;
    assert_related_page(&page, &source.owner, hits.clone(), component_graph(1));

    let above_floor = related(
        &fixture,
        json!({"entity": SOURCE_ENTITY, "min_trust": 0.8, "limit": 10}),
    )
    .await;
    assert_related_page(
        &above_floor,
        &source.owner,
        hits.clone(),
        component_graph(2),
    );

    let below_default = related(
        &fixture,
        json!({"entity": SOURCE_ENTITY, "min_trust": 0.1, "limit": 10}),
    )
    .await;
    assert_related_page(&below_default, &source.owner, hits, component_graph(0));

    let folded = related(&fixture, json!({"entity": "harbor ledger", "limit": 10})).await;
    assert_related_page(
        &folded,
        &source.owner,
        component_hits(
            &source.fact_id,
            &neighbor.fact_id,
            &quiet.fact_id,
            FOLDED_WHY,
        ),
        component_graph(1),
    );

    let isolated_page = related(&fixture, json!({"entity": ISOLATED_ENTITY, "limit": 10})).await;
    assert_related_page(
        &isolated_page,
        &isolated.owner,
        json!([expected_hit(
            &isolated.fact_id,
            ISOLATED,
            "decision",
            &[ISOLATED_ENTITY],
            990_000,
            ISOLATED_WHY,
        )]),
        json!({
            "kind": "complete",
            "root_count": 1,
            "relation_count": 2,
            "expanded_fact_count": 1,
        }),
    );

    let missing = related(&fixture, json!({"entity": "Unlisted Beacon", "limit": 10})).await;
    assert_related_page(
        &missing,
        &source.owner,
        json!([]),
        json!({
            "kind": "complete",
            "root_count": 0,
            "relation_count": 0,
            "expanded_fact_count": 0,
        }),
    );

    let first_page = related(&fixture, json!({"entity": SOURCE_ENTITY, "limit": 1})).await;
    assert_eq!(
        first_page["graph_coverage"],
        component_graph(1),
        "{first_page}"
    );
    assert_eq!(
        first_page["hits"]
            .as_array()
            .expect("first page hits")
            .iter()
            .map(observed_hit)
            .collect::<Vec<_>>(),
        vec![expected_hit(
            &source.fact_id,
            SOURCE,
            "decision",
            &[SOURCE_ENTITY, SHARED_ENTITY],
            910_000,
            SOURCE_WHY,
        )],
        "first related page: {first_page}"
    );
    assert_eq!(
        first_page["next_after"]["score_millionths"], 910_000,
        "{first_page}"
    );
    assert_eq!(
        first_page["next_after"]["fact_id"], source.fact_id,
        "{first_page}"
    );
    let second_page = related(
        &fixture,
        json!({
            "entity": SOURCE_ENTITY,
            "limit": 1,
            "after": first_page["next_after"],
        }),
    )
    .await;
    assert_eq!(
        second_page["hits"]
            .as_array()
            .expect("second page hits")
            .iter()
            .map(observed_hit)
            .collect::<Vec<_>>(),
        vec![expected_hit(
            &neighbor.fact_id,
            NEIGHBOR,
            "project",
            &[NEIGHBOR_ENTITY, SHARED_ENTITY],
            620_000,
            SOURCE_WHY,
        )],
        "second related page: {second_page}"
    );
    assert_eq!(
        second_page["next_after"]["score_millionths"], 620_000,
        "{second_page}"
    );
    assert_eq!(
        second_page["next_after"]["fact_id"], neighbor.fact_id,
        "{second_page}"
    );
    let third_page = related(
        &fixture,
        json!({
            "entity": SOURCE_ENTITY,
            "limit": 1,
            "after": second_page["next_after"],
        }),
    )
    .await;
    assert_eq!(
        third_page["hits"]
            .as_array()
            .expect("third page hits")
            .iter()
            .map(observed_hit)
            .collect::<Vec<_>>(),
        vec![expected_hit(
            &quiet.fact_id,
            QUIET,
            "decision",
            &[SHARED_ENTITY],
            200_000,
            SOURCE_WHY,
        )],
        "third related page: {third_page}"
    );
    assert_eq!(third_page["next_after"], Value::Null, "{third_page}");

    let rejected = invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_related",
        json!({"entity": SOURCE_ENTITY, "limit": 0}),
    )
    .await
    .expect_err("a zero page size is not a related result");
    let problem = &rejected_problem(&rejected)["problem"];
    assert_eq!(
        json!({
            "kind": problem["kind"],
            "code": problem["code"],
            "message": problem["message"],
            "retry": problem["retry"],
            "retryable": problem["retryable"],
            "terminality": problem["terminality"],
            "legal_actions": problem["legal_actions"],
            "diagnostic": problem["diagnostic"],
        }),
        json!({
            "kind": "invalid_request",
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid.",
            "retry": "never",
            "retryable": false,
            "terminality": "pre_admission",
            "legal_actions": ["correct_request"],
            "diagnostic": {
                "code": "application.retained.invalid-request",
                "message": "The retained operation request is invalid.",
            },
        }),
        "{rejected}"
    );

    close_test_graph(fixture).await;
}

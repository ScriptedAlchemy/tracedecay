//! The trailers and footer a typed result carries beside its body, as an MCP
//! client receives them from the production server for `search`, `context`,
//! `callers` and `callees`, in both output formats.

use super::{
    GraphQueryFixture, call_production_tool, graph_query_fixture_with_sources,
    shutdown_graph_fixture,
};
use crate::support::extract_text;
use serde_json::{Value, json};
use std::fs;
use std::time::Duration;

/// 143 bytes: the raw read of `src/walk.rs` prices at 35 tokens.
const WALK_RS: &str = "pub struct Walk;\nimpl Walk {\n    pub fn read(&self) {}\n}\n\npub trait Step {\n    fn step(&self);\n}\n\nimpl Step for Walk {\n    fn step(&self) {}\n}\n";

/// 2550 bytes: the raw read of `src/lib.rs` prices at 637 tokens.
fn lib_rs() -> String {
    format!(
        "mod walk;\nuse walk::Walk;\n{}\npub fn known(walk: &Walk) {{ walk.read(); }}\n",
        "// padding so the raw read clearly costs more than the answer\n".repeat(40)
    )
}

async fn trailer_fixture() -> GraphQueryFixture {
    graph_query_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/walk.rs"), WALK_RS).unwrap();
        fs::write(project.join("src/lib.rs"), lib_rs()).unwrap();
    })
    .await
}

async fn call(fixture: &GraphQueryFixture, tool: &str, arguments: Value) -> Vec<String> {
    let result = call_production_tool(fixture, tool, arguments, None, None)
        .await
        .unwrap_or_else(|error| panic!("{tool}: {error}"));
    result.value["content"]
        .as_array()
        .unwrap_or_else(|| panic!("{tool} content blocks: {}", result.value))
        .iter()
        .map(|block| block["text"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Checks the last block is the token-accounting footer pricing `raw_tokens`
/// of touched files against the body it closes (chars / 4), and that no
/// stale-graph trailer rides a current seat. Returns the body blocks.
fn body_before_footer(texts: &[String], raw_tokens: u64) -> Vec<String> {
    let (footer, body) = texts
        .split_last()
        .unwrap_or_else(|| panic!("no content blocks: {texts:?}"));
    let body_chars: usize = body.iter().map(String::len).sum();
    assert_eq!(
        footer,
        &format!(
            "\ntracedecay_metrics: before={raw_tokens} after={}",
            body_chars / 4
        ),
        "{texts:?}"
    );
    assert!(
        body.iter()
            .all(|text| !text.contains("code_graph_freshness:")),
        "a current seat adds no stale trailer: {texts:?}"
    );
    body.to_vec()
}

#[tokio::test]
async fn search_results_end_with_the_accounting_footer_for_their_files() {
    let fixture = trailer_fixture().await;
    let arguments = json!({"query": "known", "prefer_symbol": true, "limit": 1, "format": "json"});
    let mut texts = call(&fixture, "tracedecay_search", arguments.clone()).await;
    for _ in 0..60 {
        let payload: Value = serde_json::from_str(&texts[0]).unwrap();
        if payload["freshness"] == json!({"state": "fresh"}) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        texts = call(&fixture, "tracedecay_search", arguments.clone()).await;
    }
    let body = body_before_footer(&texts, 637);
    let payload: Value = serde_json::from_str(&body[0]).unwrap();
    assert_eq!(
        payload["results"][0]["display"]["name"], "known",
        "{payload:#}"
    );

    let markdown = call(
        &fixture,
        "tracedecay_search",
        json!({"query": "known", "prefer_symbol": true, "limit": 1, "format": "markdown"}),
    )
    .await;
    let body = body_before_footer(&markdown, 637);
    assert!(
        body[0].starts_with("freshness: fresh\n## Search Results\n"),
        "{body:?}"
    );
    shutdown_graph_fixture(fixture).await;
}

#[tokio::test]
async fn plan_context_returns_its_plan_sections_and_the_accounting_footer() {
    let fixture = trailer_fixture().await;
    let arguments = json!({
        "task": "Step",
        "mode": "plan",
        "max_nodes": 1,
        "include_code": true,
        "max_code_blocks": 1,
    });

    let mut json_arguments = arguments.clone();
    json_arguments["format"] = json!("json");
    let texts = call(&fixture, "tracedecay_context", json_arguments).await;
    let body = body_before_footer(&texts, 35);
    let payload: Value = serde_json::from_str(&body[0]).unwrap();
    assert_eq!(payload["mode"], "plan", "{payload:#}");
    assert_eq!(
        payload["plan"]["extension_points"],
        json!([{
            "name": "Step",
            "kind": "trait",
            "file": "src/walk.rs",
            "line": 6,
            "implementor_count": 1,
        }]),
        "{payload:#}"
    );
    assert_eq!(
        payload["retrieval"],
        json!({
            "search": {"state": "ran", "budget": 1, "admitted": 1, "truncated": true},
            "graph": {"state": "ran", "budget": 1, "admitted": 1, "truncated": false},
            "related": {"state": "ran", "budget": 1, "admitted": 1, "truncated": true},
            "code": {"state": "ran", "budget": 1, "admitted": 1, "truncated": false},
            "memory": {"state": "ran", "budget": 3, "admitted": 0, "truncated": false},
        }),
        "{payload:#}"
    );

    let mut markdown_arguments = arguments;
    markdown_arguments["format"] = json!("markdown");
    let texts = call(&fixture, "tracedecay_context", markdown_arguments).await;
    let body = body_before_footer(&texts, 35);
    let markdown = body.concat();
    for line in [
        "### Extension Points",
        "- **Step** (trait) - src/walk.rs:6 (1 implementors)",
        "_Budget-bound, more may exist: search 1/1 (`max_nodes`), related 1/1 (`max_nodes`)._",
    ] {
        assert!(
            markdown.lines().any(|candidate| candidate == line),
            "missing line {line:?} in:\n{markdown}"
        );
    }
    shutdown_graph_fixture(fixture).await;
}

/// A typed symbol-graph read reports the files it answered from and the
/// generation it served on its envelope, so the MCP response carries the
/// same footer as every other code read.
#[tokio::test]
async fn typed_callers_carry_their_envelope_files_and_the_accounting_footer() {
    let fixture = trailer_fixture().await;
    let target = call_production_tool(
        &fixture,
        "tracedecay_by_qualified_name",
        json!({"qualified_name": "src/walk.rs::Walk::read", "format": "json"}),
        None,
        None,
    )
    .await
    .expect("exact method");
    let target: Value = serde_json::from_str(extract_text(&target.value)).unwrap();
    let node_id = target[0]["node_id"].as_str().expect("node id").to_owned();

    let texts = call(
        &fixture,
        "tracedecay_callers",
        json!({"node_id": node_id, "maximum_depth": 1, "format": "json"}),
    )
    .await;
    let body = body_before_footer(&texts, 637);
    let payload: Value = serde_json::from_str(&body[0]).unwrap();
    assert_eq!(
        payload["touched_files"],
        json!(["src/lib.rs"]),
        "{payload:#}"
    );
    assert_eq!(
        payload["code_graph"]["freshness"],
        json!({"state": "current"}),
        "{payload:#}"
    );
    assert!(
        payload["code_graph"]["generation"]
            .as_str()
            .is_some_and(|generation| generation.starts_with("generation.v1.")),
        "{payload:#}"
    );

    let markdown = call(
        &fixture,
        "tracedecay_callers",
        json!({"node_id": node_id, "maximum_depth": 1}),
    )
    .await;
    let body = body_before_footer(&markdown, 637);
    assert!(body.concat().contains("known"), "{body:?}");
    shutdown_graph_fixture(fixture).await;
}

/// The `tracedecay_cost` trailer block among the body blocks.
fn cost_trailer(body: &[String]) -> &str {
    let trailers: Vec<&str> = body
        .iter()
        .map(String::as_str)
        .filter(|text| text.starts_with("\ntracedecay_cost: "))
        .collect();
    assert_eq!(trailers.len(), 1, "one cost trailer: {body:?}");
    trailers[0]
}

/// A metered code read carries its store cost on the envelope and renders
/// the same receipt as a trailer, so an operator reading either surface sees
/// what the call read.
#[tokio::test]
async fn typed_callees_carry_their_read_cost_on_the_envelope_and_the_trailer() {
    let fixture = trailer_fixture().await;
    let target = call_production_tool(
        &fixture,
        "tracedecay_by_qualified_name",
        json!({"qualified_name": "src/lib.rs::known", "format": "json"}),
        None,
        None,
    )
    .await
    .expect("exact function");
    let target: Value = serde_json::from_str(extract_text(&target.value)).unwrap();
    let node_id = target[0]["node_id"].as_str().expect("node id").to_owned();

    let texts = call(
        &fixture,
        "tracedecay_callees",
        json!({"node_id": node_id, "maximum_depth": 1, "format": "json"}),
    )
    .await;
    let payload: Value = serde_json::from_str(&texts[0]).unwrap();
    assert_eq!(
        payload["outcome"]["value"]["payload"]["items"][0]["symbol"]["qualified_name"],
        "src/walk.rs::Walk::read",
        "{payload:#}"
    );
    // Six point reads: the seed `known`; the trait-dispatch check on its one
    // callee (the callee's summary, its two incoming edges, and the impl that
    // contains it); and the callee's summary for the page. Three fan-outs:
    // `known`'s call relations and their targets (one row each), then the
    // callee's incoming edges (two rows).
    let cost = &payload["cost"];
    assert_eq!(
        (
            &cost["point_reads"],
            &cost["adjacency_queries"],
            &cost["adjacency_rows"],
        ),
        (
            &json!({"graph_sealed": 6, "graph_staging": 0}),
            &json!(3),
            &json!(4),
        ),
        "{payload:#}"
    );
    assert_eq!(
        cost_trailer(&texts),
        format!(
            "\ntracedecay_cost: wall_us={} graph_sealed_reads=6 graph_staging_reads=0 \
             adjacency_queries=3 adjacency_rows=4 bytes_hydrated={}",
            cost["wall_micros"], cost["bytes_hydrated"]
        ),
        "the trailer renders the envelope's receipt"
    );

    let markdown = call(
        &fixture,
        "tracedecay_callees",
        json!({"node_id": node_id, "maximum_depth": 1}),
    )
    .await;
    assert!(
        cost_trailer(&markdown).contains(" graph_sealed_reads=6 graph_staging_reads=0 "),
        "{markdown:?}"
    );
    shutdown_graph_fixture(fixture).await;
}

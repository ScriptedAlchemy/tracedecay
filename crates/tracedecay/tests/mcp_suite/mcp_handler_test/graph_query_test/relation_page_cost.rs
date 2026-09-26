//! What one page of a large relation listing costs the graph store, read
//! from the cost receipt the production MCP server returns with it.

use super::{
    GraphQueryFixture, call_production_tool, graph_query_fixture_with_sources,
    shutdown_graph_fixture,
};
use crate::support::extract_text;
use serde_json::{Value, json};
use std::fs;

const CALLEES: usize = 105;

/// `hub` calls 105 distinct functions.
fn hub_source() -> String {
    let calls: String = (0..CALLEES)
        .map(|index| format!("    leaf_{index}();\n"))
        .collect();
    let leaves: String = (0..CALLEES)
        .map(|index| format!("pub fn leaf_{index}() {{}}\n"))
        .collect();
    format!("pub fn hub() {{\n{calls}}}\n\n{leaves}")
}

async fn callees_page(fixture: &GraphQueryFixture, node_id: &str, cursor: Option<&Value>) -> Value {
    let mut arguments = json!({
        "node_id": node_id,
        "maximum_depth": 1,
        "resolve_trait_dispatch": false,
        "format": "json",
    });
    if let Some(cursor) = cursor {
        arguments["meta"] = json!({
            "projection": "evidence",
            "order": "source_position",
            "cursor": cursor,
        });
    }
    let result = call_production_tool(fixture, "tracedecay_callees", arguments, None, None)
        .await
        .expect("callees page");
    serde_json::from_str(extract_text(&result.value)).expect("callees envelope")
}

/// `(point reads, adjacency queries, adjacency rows)` of one page.
fn page_cost(envelope: &Value) -> (u64, u64, u64) {
    let cost = &envelope["cost"];
    let reads = |store: &str| cost["point_reads"][store].as_u64().expect("point reads");
    (
        reads("graph_sealed") + reads("graph_staging"),
        cost["adjacency_queries"]
            .as_u64()
            .expect("adjacency queries"),
        cost["adjacency_rows"].as_u64().expect("adjacency rows"),
    )
}

/// A page of a 105-callee listing reads the seed and the page's own ten
/// callees, not every relation it pages over: at most `2 * page_size + 1`
/// entities on every page, including the continuation.
#[tokio::test]
async fn a_callees_page_reads_its_own_rows_not_every_relation() {
    let fixture = graph_query_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), hub_source()).unwrap();
    })
    .await;
    let target = call_production_tool(
        &fixture,
        "tracedecay_by_qualified_name",
        json!({"qualified_name": "src/lib.rs::hub", "format": "json"}),
        None,
        None,
    )
    .await
    .expect("exact function");
    let target: Value = serde_json::from_str(extract_text(&target.value)).unwrap();
    let node_id = target[0]["node_id"].as_str().expect("node id").to_owned();

    let first = callees_page(&fixture, &node_id, None).await;
    let payload = &first["outcome"]["value"]["payload"];
    assert_eq!(
        (
            payload["total"].as_u64(),
            payload["items"].as_array().map(Vec::len)
        ),
        (Some(105), Some(10)),
        "{first:#}"
    );
    // Eleven reads: the seed, then the ten callees the page returns. The
    // 105 relations are enumerated from two batched fan-outs (the calls, then
    // each call's target) that decode no entity.
    assert_eq!(page_cost(&first), (11, 2, 210), "{first:#}");

    let second = callees_page(&fixture, &node_id, Some(&payload["next_cursor"])).await;
    assert_eq!(
        second["outcome"]["value"]["payload"]["items"]
            .as_array()
            .map(Vec::len),
        Some(10),
        "{second:#}"
    );
    assert_eq!(page_cost(&second), (11, 2, 210), "{second:#}");
    shutdown_graph_fixture(fixture).await;
}

#![cfg(feature = "test-transport")]

//! Focused MCP renderer/default-format tests through the production daemon
//! composition and its mounted MCP server.

use serde_json::{Value, json};
use tracedecay_mcp::ToolResult;

use super::support::{
    ProductionCompositionFixture, extract_json, extract_text, production_composition_fixture,
    wait_for_current_graph,
};

async fn call_tool(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> ToolResult {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} production invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "{tool_name} returned a production MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    ToolResult::new(
        response
            .result
            .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result")),
        Vec::new(),
    )
}

async fn resolve_node_id_over_mcp(fixture: &ProductionCompositionFixture, name: &str) -> String {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production rendering server");
    wait_for_current_graph(&server).await;
    let result = call_tool(
        fixture,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20, "format": "json"}),
    )
    .await;
    let payload = extract_json(&result.value);
    payload["matches"]
        .as_array()
        .and_then(|matches| matches.iter().find(|result| result["name"] == name))
        .and_then(|result| result["id"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            panic!("node '{name}' missing from production exact-symbol response: {payload}")
        })
}

#[tokio::test]
async fn type_hierarchy_defaults_to_markdown_and_supports_json() {
    let fixture = production_composition_fixture().await;
    let node_id = resolve_node_id_over_mcp(&fixture, "helper").await;

    let markdown = call_tool(
        &fixture,
        "tracedecay_type_hierarchy",
        json!({"node_id": node_id.clone()}),
    )
    .await;
    let markdown = extract_text(&markdown.value);
    assert!(
        markdown.starts_with("## code\\_type\\_hierarchy"),
        "{markdown}"
    );
    assert!(
        markdown.contains("(function) src/utils.rs:") && markdown.contains(&node_id),
        "the payload leads with the root symbol line: {markdown}"
    );
    assert!(
        !markdown.contains("|- "),
        "helper has no subtypes: {markdown}"
    );
    assert!(serde_json::from_str::<Value>(markdown).is_err());

    let json_result = call_tool(
        &fixture,
        "tracedecay_type_hierarchy",
        json!({"node_id": node_id, "format": "json"}),
    )
    .await;
    let parsed: Value = serde_json::from_str(extract_text(&json_result.value)).unwrap();
    let items = &parsed["outcome"]["value"]["payload"]["items"];
    assert_eq!(items[0]["symbol"]["name"], "helper", "{parsed}");
    assert_eq!(items[0]["edge_kind"], "root", "{parsed}");
    fixture.harness.shutdown().await;
}

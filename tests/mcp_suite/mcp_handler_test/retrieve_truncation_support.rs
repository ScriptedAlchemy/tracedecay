use serde_json::{Value, json};

pub(super) fn retrieve_json_arguments(handle: &str) -> Value {
    json!({ "format": "json", "handle": handle })
}

#[cfg(feature = "test-transport")]
pub(super) async fn call_production_tool(
    fixture: &crate::support::ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> tracedecay::mcp::ToolResult {
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
    tracedecay::mcp::ToolResult::new(
        response
            .result
            .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result")),
        Vec::new(),
    )
}

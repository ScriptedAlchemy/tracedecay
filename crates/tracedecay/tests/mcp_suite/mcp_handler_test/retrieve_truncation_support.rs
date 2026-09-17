use serde_json::{Value, json};

pub(super) fn retrieve_json_arguments(handle: &str) -> Value {
    json!({ "format": "json", "handle": handle })
}

#[cfg(feature = "test-transport")]
pub(super) async fn retrieve_all_json_pages(
    fixture: &crate::support::ProductionCompositionFixture,
    handle: &str,
) -> String {
    let mut offset = 0usize;
    let mut content = String::new();
    loop {
        let page = call_production_tool(
            fixture,
            "tracedecay_retrieve",
            json!({"format": "json", "handle": handle, "offset": offset}),
        )
        .await;
        let page: Value = serde_json::from_str(crate::support::extract_text(&page.value))
            .expect("retrieve page JSON");
        let page_content = page["content"].as_str().expect("retrieve page content");
        content.push_str(page_content);
        if !page["has_more"].as_bool().expect("retrieve has_more") {
            return content;
        }
        offset = page["next_offset"].as_u64().expect("retrieve next_offset") as usize;
    }
}

#[cfg(feature = "test-transport")]
pub(super) async fn call_production_tool(
    fixture: &crate::support::ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> tracedecay_mcp::ToolResult {
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
    tracedecay_mcp::ToolResult::new(
        response
            .result
            .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result")),
        Vec::new(),
    )
}

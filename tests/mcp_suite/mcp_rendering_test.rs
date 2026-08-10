#![cfg(feature = "test-transport")]

//! Focused MCP renderer/default-format tests through the production daemon
//! composition and its mounted MCP server.

use std::fs;

use serde_json::{Value, json};
use tracedecay::mcp::ToolResult;

use super::support::{
    ProductionCompositionFixture, extract_json, extract_text, production_composition_fixture,
    production_composition_fixture_with_sources,
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
    let result = call_tool(
        fixture,
        "tracedecay_grep",
        json!({
            "pattern": format!("fn {name}"),
            "fixed_strings": true,
            "format": "json"
        }),
    )
    .await;
    let payload = extract_json(&result.value);
    payload["results"]
        .as_array()
        .and_then(|results| {
            results.iter().find_map(|result| {
                (result["symbol"].as_str() == Some(name))
                    .then(|| result["node_id"].as_str().map(str::to_owned))
                    .flatten()
            })
        })
        .unwrap_or_else(|| panic!("node '{name}' missing from production grep response: {payload}"))
}

#[tokio::test]
async fn read_cache_default_response_stays_markdown() {
    let fixture = production_composition_fixture().await;
    let args = json!({"file": "src/main.rs", "mode": "full"});

    call_tool(&fixture, "tracedecay_read", args.clone()).await;
    let cached = call_tool(&fixture, "tracedecay_read", args).await;
    let cached_text = extract_text(&cached.value);
    assert!(cached_text.starts_with("## src/main.rs (full)"));
    assert!(cached_text.contains("**unchanged:** true"));
    assert!(!cached_text.trim_start().starts_with('{'));
    assert!(!cached_text.contains('|'));

    let cached_json = call_tool(
        &fixture,
        "tracedecay_read",
        json!({"file": "src/main.rs", "mode": "full", "format": "json"}),
    )
    .await;
    let cached_json = extract_json(&cached_json.value);
    assert_eq!(cached_json["unchanged"], true);
    assert_eq!(cached_json["file"], "src/main.rs");
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn read_lines_includes_overlapping_symbol_context() {
    let fixture = production_composition_fixture().await;

    let result = call_tool(
        &fixture,
        "tracedecay_read",
        json!({"file": "src/main.rs", "mode": "lines", "lines": "5-7"}),
    )
    .await;
    let text = extract_text(&result.value);

    assert!(text.contains("### Context"), "got: {text}");
    assert!(text.contains("fn main()"), "got: {text}");
    assert!(text.contains("main 5-8"), "got: {text}");
    assert!(text.contains("let result = helper();"), "got: {text}");

    let json_result = call_tool(
        &fixture,
        "tracedecay_read",
        json!({
            "file": "src/main.rs",
            "mode": "lines",
            "lines": "5-7",
            "format": "json"
        }),
    )
    .await;
    let parsed = extract_json(&json_result.value);
    assert_eq!(parsed["context"]["symbol_count"], 1);
    assert_eq!(parsed["context"]["symbols"][0]["name"], "main");
    assert_eq!(parsed["context"]["symbols"][0]["signature"], "fn main()");

    let cached = call_tool(
        &fixture,
        "tracedecay_read",
        json!({"file": "src/main.rs", "mode": "lines", "lines": "5-7"}),
    )
    .await;
    let cached_text = extract_text(&cached.value);
    assert!(
        cached_text.contains("**unchanged:** true"),
        "got: {cached_text}"
    );
    assert!(cached_text.contains("### Context"), "got: {cached_text}");
    assert!(cached_text.contains("fn main()"), "got: {cached_text}");
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn simplify_scan_fails_closed_without_verified_similarity_authority() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/dead.rs"),
            r#"
fn abandoned_helper() -> usize {
    7
}
"#,
        )
        .unwrap();
    })
    .await;

    let response = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_simplify_scan",
            json!({"files": ["src/dead.rs"], "format": "markdown"}),
        )
        .await
        .unwrap();
    let error = response
        .error
        .expect("simplify scan must fail closed without verified similarity authority");
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data["reason_code"].as_str()),
        Some("verified-simplify-similarity-unavailable")
    );
    fixture.harness.shutdown().await;
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
    assert!(markdown.starts_with("## Type Hierarchy"));
    assert!(markdown.contains("```text"));
    assert!(!markdown.contains("|"));
    assert!(serde_json::from_str::<Value>(markdown).is_err());

    let json_result = call_tool(
        &fixture,
        "tracedecay_type_hierarchy",
        json!({"node_id": node_id, "format": "json"}),
    )
    .await;
    let parsed: Value = serde_json::from_str(extract_text(&json_result.value)).unwrap();
    assert_eq!(parsed["root"]["name"], "helper");
    assert!(parsed["tree"].as_str().unwrap().contains("helper"));
    fixture.harness.shutdown().await;
}

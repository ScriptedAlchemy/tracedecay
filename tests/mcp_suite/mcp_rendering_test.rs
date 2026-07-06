//! Focused MCP renderer/default-format tests.

use std::fs;

use serde_json::{Value, json};

use super::mcp_handler_test::{
    extract_json, extract_text, find_node_id, handle_tool_call, index_all_retrying_sync_lock,
    setup_empty_project, setup_project,
};

#[tokio::test]
async fn read_cache_default_response_stays_markdown() {
    let (cg, _dir) = setup_project().await;
    let args = json!({"file": "src/main.rs", "mode": "full"});

    tracedecay::mcp::handle_tool_call(&cg, "tracedecay_read", args.clone(), None, None)
        .await
        .unwrap();
    let cached = tracedecay::mcp::handle_tool_call(&cg, "tracedecay_read", args, None, None)
        .await
        .unwrap();
    let cached_text = extract_text(&cached.value);
    assert!(cached_text.starts_with("## src/main.rs (full)"));
    assert!(cached_text.contains("**unchanged:** true"));
    assert!(!cached_text.trim_start().starts_with('{'));
    assert!(!cached_text.contains('|'));

    let cached_json = handle_tool_call(
        &cg,
        "tracedecay_read",
        json!({"file": "src/main.rs", "mode": "full", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let cached_json = extract_json(&cached_json.value);
    assert_eq!(cached_json["unchanged"], true);
    assert_eq!(cached_json["file"], "src/main.rs");
}

#[tokio::test]
async fn simplify_scan_markdown_visible_output_is_not_escaped_blob() {
    let (cg, _env, dir) = setup_empty_project().await;
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/dead.rs"),
        r#"
fn abandoned_helper() -> usize {
    7
}
"#,
    )
    .unwrap();
    index_all_retrying_sync_lock(&cg).await;

    let result = handle_tool_call(
        &cg,
        "tracedecay_simplify_scan",
        json!({"files": ["src/dead.rs"], "format": "markdown"}),
        None,
        None,
    )
    .await
    .unwrap();

    let text = extract_text(&result.value);
    assert!(text.contains("# Simplify Scan"), "got: {text}");
    assert!(text.contains("## Potential Dead Code"), "got: {text}");
    assert!(text.contains("abandoned_helper"), "got: {text}");
    assert!(
        serde_json::from_str::<Value>(text).is_err(),
        "visible markdown should not be a JSON envelope: {text}"
    );
    assert!(
        !text.contains("\\n") && !text.contains("\\\""),
        "visible markdown should not contain escaped markdown/json: {text}"
    );
    assert!(
        !text.contains("\"content\""),
        "visible markdown should not contain a nested MCP envelope: {text}"
    );
}

#[tokio::test]
async fn type_hierarchy_defaults_to_markdown_and_supports_json() {
    let (cg, _dir) = setup_project().await;
    let node_id = find_node_id(&cg, "helper").await;

    let markdown = handle_tool_call(
        &cg,
        "tracedecay_type_hierarchy",
        json!({"node_id": node_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let markdown = extract_text(&markdown.value);
    assert!(markdown.starts_with("## Type Hierarchy"));
    assert!(markdown.contains("```text"));
    assert!(!markdown.contains("|"));
    assert!(serde_json::from_str::<Value>(markdown).is_err());

    let json_result = handle_tool_call(
        &cg,
        "tracedecay_type_hierarchy",
        json!({"node_id": node_id, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let parsed: Value = serde_json::from_str(extract_text(&json_result.value)).unwrap();
    assert_eq!(parsed["root"]["name"], "helper");
    assert!(parsed["tree"].as_str().unwrap().contains("helper"));
}

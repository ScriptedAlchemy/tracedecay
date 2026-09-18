#![cfg(feature = "test-transport")]

//! `tracedecay_signature` as an MCP client observes it.
//!
//! Calls go through JSON-RPC `tools/call` on the production server. Expected
//! rows are the declaration in `SOURCE`, not a value read back out of the
//! handler. `full_file` is that file's 414 bytes divided by 4. `body` is the
//! declaration's line count times 20.

use std::fs;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    dispatch_mcp_tool_call, production_composition_fixture_with_sources, warm_code_index_search,
};

const SOURCE: &str = r#"/// Loads the current value.
pub async fn fetch_value(key: &str) -> u32 {
    FETCH_BODY_NOT_IN_SIGNATURE
}

fn cached_value() -> u32 {
    CACHED_BODY_NOT_IN_SIGNATURE
}

pub fn parse<'a, T>(input: &'a str) -> Result<&'a str, T> where T: Copy {
    Ok(input)
}

pub struct Widget;

impl Widget {
    /// Paints the widget.
    pub fn render(&self) -> &'static str {
        "WIDGET_BODY_NOT_IN_SIGNATURE"
    }
}
"#;

#[tokio::test]
async fn tracedecay_signature_returns_the_declared_signature() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), SOURCE).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production signature server");
    warm_code_index_search(&server, "fetch_value").await;

    let fetch_response = signature_call(
        &server,
        json!({"qualified_name": "src/lib.rs::fetch_value", "format": "json"}),
    )
    .await;
    let fetch_text = tool_text(&fetch_response);
    assert!(
        !fetch_text.contains("FETCH_BODY_NOT_IN_SIGNATURE"),
        "signature lookup must not return the body: {fetch_text}"
    );
    let fetch = parse_json(fetch_text);
    assert_one_surface(&fetch, &fetch_surface());
    let fetch_id = node_id(&fetch);

    let by_node = signature_json(&server, json!({"node_id": fetch_id})).await;
    assert_one_surface(&by_node, &fetch_surface());
    let by_alias = signature_json(&server, json!({"id": node_id(&by_node)})).await;
    assert_one_surface(&by_alias, &fetch_surface());
    let node_wins = signature_json(
        &server,
        json!({
            "node_id": fetch_id,
            "qualified_name": "src/lib.rs::cached_value",
        }),
    )
    .await;
    assert_one_surface(&node_wins, &fetch_surface());

    let cached = signature_json(
        &server,
        json!({"qualified_name": "src/lib.rs::cached_value"}),
    )
    .await;
    assert_one_surface(&cached, &cached_surface());
    let parse = signature_json(&server, json!({"qualified_name": "src/lib.rs::parse"})).await;
    assert_one_surface(&parse, &parse_surface());
    assert!(
        !serde_json::to_string(&parse).unwrap().contains("Ok(input)"),
        "where-clause signature must not include the function body: {parse}"
    );

    let render = signature_json(
        &server,
        json!({"qualified_name": "src/lib.rs::Widget::render"}),
    )
    .await;
    assert_one_surface(&render, &render_surface());
    assert!(
        !serde_json::to_string(&render)
            .unwrap()
            .contains("WIDGET_BODY_NOT_IN_SIGNATURE"),
        "method signature must not include the body: {render}"
    );

    let widget = signature_json(&server, json!({"qualified_name": "src/lib.rs::Widget"})).await;
    let rows = widget.as_array().expect("widget rows");
    assert_eq!(
        rows.len(),
        2,
        "struct and impl share one qualified name: {widget}"
    );
    assert_signature_surface(row_by_kind(&widget, "struct"), &struct_surface());
    assert_signature_surface(row_by_kind(&widget, "impl"), &impl_surface());

    let default_text = tool_text(
        &signature_call(
            &server,
            json!({"qualified_name": "src/lib.rs::fetch_value"}),
        )
        .await,
    );
    let markdown_text = tool_text(
        &signature_call(
            &server,
            json!({"qualified_name": "src/lib.rs::fetch_value", "format": "markdown"}),
        )
        .await,
    );
    let expected_markdown = fetch_markdown(&fetch_id);
    assert_eq!(default_text, expected_markdown);
    assert_eq!(markdown_text, expected_markdown);
    assert!(!default_text.contains("FETCH_BODY_NOT_IN_SIGNATURE"));

    let missing = signature_json(
        &server,
        json!({"qualified_name": "src/lib.rs::missing_symbol"}),
    )
    .await;
    assert_eq!(missing, json!([]));
    let missing_markdown = tool_text(
        &signature_call(
            &server,
            json!({"qualified_name": "src/lib.rs::missing_symbol"}),
        )
        .await,
    );
    assert_eq!(missing_markdown, "_None._\n");
    let unknown_node = signature_json(&server, json!({"node_id": "missing-symbol"})).await;
    assert_eq!(unknown_node, json!([]));

    let omitted = signature_call(&server, json!({})).await;
    assert_eq!(omitted["error"]["code"], -32602);
    assert_eq!(
        omitted["error"]["message"],
        "missing required parameter: qualified_name or node_id"
    );
    assert_eq!(
        omitted["error"]["data"],
        json!({
            "tool": "tracedecay_signature",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: qualified_name or node_id"
        })
    );

    let blank = signature_call(&server, json!({"node_id": ""})).await;
    assert_eq!(blank["error"]["code"], -32603);
    assert_eq!(
        blank["error"]["message"],
        "tool execution failed: config error: invalid parameter: node_id must not be empty"
    );
    assert_eq!(blank["error"]["data"]["tool"], "tracedecay_signature");

    fixture.harness.shutdown().await;
}

fn fetch_surface() -> Value {
    json!({
        "name": "fetch_value",
        "qualified_name": "src/lib.rs::fetch_value",
        "kind": "function",
        "visibility": "public",
        "signature": "pub async fn fetch_value(key: &str) -> u32",
        "docstring": "Loads the current value.",
        "is_async": true,
        "file": "src/lib.rs",
        "start_line": 2,
        "end_line": 4,
        "cost_to_expand": {"body": 60, "full_file": 103},
        "unavailable_fields": ["attrs_start_line"]
    })
}

fn cached_surface() -> Value {
    json!({
        "name": "cached_value",
        "qualified_name": "src/lib.rs::cached_value",
        "kind": "function",
        "visibility": "private",
        "signature": "fn cached_value() -> u32",
        "docstring": null,
        "is_async": false,
        "file": "src/lib.rs",
        "start_line": 6,
        "end_line": 8,
        "cost_to_expand": {"body": 60, "full_file": 103},
        "unavailable_fields": ["attrs_start_line"]
    })
}

fn parse_surface() -> Value {
    json!({
        "name": "parse",
        "qualified_name": "src/lib.rs::parse",
        "kind": "function",
        "visibility": "public",
        "signature": "pub fn parse<'a, T>(input: &'a str) -> Result<&'a str, T> where T: Copy",
        "docstring": null,
        "is_async": false,
        "file": "src/lib.rs",
        "start_line": 10,
        "end_line": 12,
        "cost_to_expand": {"body": 60, "full_file": 103},
        "unavailable_fields": ["attrs_start_line"]
    })
}

fn render_surface() -> Value {
    json!({
        "name": "render",
        "qualified_name": "src/lib.rs::Widget::render",
        "kind": "method",
        "visibility": "public",
        "signature": "pub fn render(&self) -> &'static str",
        "docstring": "Paints the widget.",
        "is_async": false,
        "file": "src/lib.rs",
        "start_line": 18,
        "end_line": 20,
        "cost_to_expand": {"body": 60, "full_file": 103},
        "unavailable_fields": ["attrs_start_line"]
    })
}

fn struct_surface() -> Value {
    json!({
        "name": "Widget",
        "qualified_name": "src/lib.rs::Widget",
        "kind": "struct",
        "visibility": "public",
        "signature": "pub struct Widget;",
        "docstring": null,
        "is_async": false,
        "file": "src/lib.rs",
        "start_line": 14,
        "end_line": 14,
        "cost_to_expand": {"body": 20, "full_file": 103},
        "unavailable_fields": ["attrs_start_line"]
    })
}

fn impl_surface() -> Value {
    json!({
        "name": "Widget",
        "qualified_name": "src/lib.rs::Widget",
        "kind": "impl",
        "visibility": "private",
        "signature": "impl Widget",
        "docstring": null,
        "is_async": false,
        "file": "src/lib.rs",
        "start_line": 16,
        "end_line": 21,
        "cost_to_expand": {"body": 120, "full_file": 103},
        "unavailable_fields": ["attrs_start_line"]
    })
}

fn fetch_markdown(node_id: &str) -> String {
    format!(
        "\
- **fetch_value**
  **kind:** function
  **file:** src/lib.rs
  **signature:** `pub async fn fetch_value(key: &str) -> u32`
  **cost_to_expand:** body=60, full_file=103
  **docstring:** Loads the current value.
  **end_line:** 4
  **is_async:** true
  **node_id:** `{node_id}`
  **qualified_name:** `src/lib.rs::fetch_value`
  **start_line:** 2
  **unavailable_fields:** attrs_start_line
  **visibility:** public
"
    )
}

async fn signature_call(server: &McpServer, arguments: Value) -> Value {
    dispatch_mcp_tool_call(server, "tracedecay_signature", arguments).await
}

async fn signature_json(server: &McpServer, mut arguments: Value) -> Value {
    arguments
        .as_object_mut()
        .expect("signature arguments")
        .entry("format")
        .or_insert_with(|| json!("json"));
    parse_json(&tool_text(&signature_call(server, arguments).await))
}

fn tool_text(response: &Value) -> String {
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "MCP tools/call failed: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("MCP result has no text: {response}"))
        .to_owned()
}

fn parse_json(text: &str) -> Value {
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("MCP text was not JSON: {error}\n{text}"))
}

fn assert_one_surface(payload: &Value, expected: &Value) {
    let rows = payload
        .as_array()
        .unwrap_or_else(|| panic!("signature payload should be an array: {payload}"));
    assert_eq!(rows.len(), 1, "one addressed symbol: {payload}");
    assert_signature_surface(&rows[0], expected);
}

fn assert_signature_surface(actual: &Value, expected: &Value) {
    let node_id = actual["node_id"]
        .as_str()
        .unwrap_or_else(|| panic!("signature row is missing node_id: {actual}"));
    assert!(
        node_id.starts_with("symbol.v1."),
        "node_id should be a graph occurrence: {node_id}"
    );
    let mut actual = actual.clone();
    actual
        .as_object_mut()
        .expect("signature row")
        .remove("node_id");
    assert_eq!(actual, *expected);
}

fn node_id(payload: &Value) -> String {
    payload[0]["node_id"].as_str().expect("node_id").to_owned()
}

fn row_by_kind<'a>(payload: &'a Value, kind: &str) -> &'a Value {
    payload
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["kind"] == kind))
        .unwrap_or_else(|| panic!("missing {kind} row in {payload}"))
}

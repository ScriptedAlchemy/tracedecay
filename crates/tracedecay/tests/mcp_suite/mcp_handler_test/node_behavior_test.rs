#![cfg(feature = "test-transport")]

//! `tracedecay_node` as an MCP client observes it.
//!
//! Calls go through JSON-RPC `tools/call` on the production server. Expected
//! rows are the declarations in `SOURCE`, not values read back out of the
//! handler. `full_file` is that file's 488 bytes divided by 4. `body` is the
//! declaration's line count times 20. Node ids come from
//! `tracedecay_find_exact_symbol` only so the call has an address; every
//! assertion is on `tracedecay_node`.

use std::fs;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    dispatch_mcp_tool_call, production_composition_fixture_with_sources, wait_for_current_graph,
};

const SOURCE: &str = r#"/// Loads the current value.
pub async fn fetch_value(key: &str) -> u32 {
    if key.is_empty() {
        return 0;
    }
    let mut total = 0;
    for byte in key.bytes() {
        total += u32::from(byte);
    }
    total
}

fn cached_value() -> u32 {
    CACHED_BODY_NOT_IN_NODE
}

#[derive(Debug, Clone)]
pub struct Widget {
    pub label: &'static str,
}

impl Widget {
    /// Paints the widget.
    pub fn render(&self) -> &'static str {
        "WIDGET_BODY_NOT_IN_NODE"
    }
}
"#;

const SOURCE_BYTES: usize = 488;
const FULL_FILE_COST: u64 = 122;
const MISSING_NODE: &str =
    "symbol.v1.sha256:0000000000000000000000000000000000000000000000000000000000000000";
const EVIDENCE_ANCHOR: &str =
    "code-graph:symbol.v1.sha256:0000000000000000000000000000000000000000000000000000000000000000";
const NODE_CLI_FALLBACK: &str = "This tool is also available from the shell: `tracedecay tool node ...` (`tracedecay tool node --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly.";

#[tokio::test]
async fn tracedecay_node_reports_declared_symbols_and_typed_refusals() {
    assert_eq!(
        SOURCE.len(),
        SOURCE_BYTES,
        "full_file literal must match SOURCE"
    );
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), SOURCE).unwrap();
    })
    .await;
    let on_disk = fs::read(fixture.project_root.join("src/lib.rs")).expect("fixture source");
    assert_eq!(on_disk.len(), SOURCE_BYTES);
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production node server");
    wait_for_current_graph(&server).await;

    let fetch_id = occurrence_id(&server, "fetch_value", "function").await;
    let cached_id = occurrence_id(&server, "cached_value", "function").await;
    let widget_id = occurrence_id(&server, "Widget", "struct").await;
    let label_id = occurrence_id(&server, "label", "field").await;
    let impl_id = occurrence_id(&server, "Widget", "impl").await;
    let render_id = occurrence_id(&server, "render", "method").await;

    assert_node(&server, &fetch_id, &fetch_details(&fetch_id)).await;
    assert_node(&server, &cached_id, &cached_details(&cached_id)).await;
    assert_node(&server, &widget_id, &widget_details(&widget_id)).await;
    assert_node(&server, &label_id, &label_details(&label_id)).await;
    assert_node(&server, &impl_id, &impl_details(&impl_id)).await;
    assert_node(&server, &render_id, &render_details(&render_id)).await;

    let anchored = node_json(
        &server,
        json!({"node_id": format!("code-symbol:{fetch_id}")}),
    )
    .await;
    assert_eq!(anchored, fetch_details(&fetch_id));
    let upper_json = node_json(&server, json!({"node_id": fetch_id, "format": "JSON"})).await;
    assert_eq!(upper_json, fetch_details(&fetch_id));

    let fetch_text = tool_text(&node_call(&server, json!({"node_id": fetch_id})).await);
    let fetch_markdown_text =
        tool_text(&node_call(&server, json!({"node_id": fetch_id, "format": "markdown"})).await);
    let fetch_text_format =
        tool_text(&node_call(&server, json!({"node_id": fetch_id, "format": "text"})).await);
    let expected_fetch_markdown = fetch_markdown(&fetch_id);
    assert_eq!(fetch_text, expected_fetch_markdown);
    assert_eq!(fetch_markdown_text, expected_fetch_markdown);
    assert_eq!(fetch_text_format, expected_fetch_markdown);
    assert_eq!(
        tool_text(&node_call(&server, json!({"node_id": widget_id})).await),
        widget_markdown(&widget_id)
    );
    assert_eq!(
        tool_text(&node_call(&server, json!({"node_id": cached_id})).await),
        cached_markdown(&cached_id)
    );

    for text in [
        tool_text(&node_call(&server, json!({"node_id": fetch_id, "format": "json"})).await),
        tool_text(&node_call(&server, json!({"node_id": cached_id, "format": "json"})).await),
        tool_text(&node_call(&server, json!({"node_id": render_id, "format": "json"})).await),
        expected_fetch_markdown,
    ] {
        assert!(
            !text.contains("key.is_empty()")
                && !text.contains("CACHED_BODY_NOT_IN_NODE")
                && !text.contains("WIDGET_BODY_NOT_IN_NODE"),
            "node details must not return the body: {text}"
        );
    }

    let missing = node_call(&server, json!({"node_id": MISSING_NODE})).await;
    assert!(
        missing.get("error").is_none() || missing["error"].is_null(),
        "a missing node is a tool result, not a transport error: {missing}"
    );
    assert_eq!(missing["result"]["isError"], true);
    assert_eq!(
        parse_json(&tool_text(&missing)),
        json!({
            "status": "not_found",
            "reason_code": "node_not_found",
            "node_id": MISSING_NODE,
            "message": format!("Node not found: {MISSING_NODE}")
        })
    );

    assert_execution_failed(
        &node_call(&server, json!({})).await,
        "tool execution failed: config error: invalid arguments for tracedecay_node: missing field `node_id`",
    );
    assert_execution_failed(
        &node_call(&server, json!({"node_id": ""})).await,
        "tool execution failed: config error: invalid parameter: node_id must not be empty",
    );
    assert_execution_failed(
        &node_call(&server, json!({"node_id": "   "})).await,
        "tool execution failed: config error: invalid parameter: node_id must not be empty",
    );
    assert_execution_failed(
        &node_call(&server, json!({"id": fetch_id})).await,
        "tool execution failed: config error: invalid arguments for tracedecay_node: unknown field `id`, expected `node_id`",
    );
    assert_execution_failed(
        &node_call(&server, json!({"node_id": fetch_id, "limit": 1})).await,
        "tool execution failed: config error: invalid arguments for tracedecay_node: unknown field `limit`, expected `node_id`",
    );
    assert_execution_failed(
        &node_call(&server, json!([fetch_id])).await,
        "tool execution failed: config error: invalid arguments: tracedecay_node expects a JSON object",
    );
    assert_execution_failed(
        &node_call(&server, json!({"node_id": EVIDENCE_ANCHOR})).await,
        &format!(
            "tool execution failed: config error: invalid parameter: node_id `{EVIDENCE_ANCHOR}` is an evidence anchor, not a graph symbol occurrence"
        ),
    );

    fixture.harness.shutdown().await;
}

fn fetch_details(id: &str) -> Value {
    details(
        id,
        "fetch_value",
        "function",
        "src/lib.rs::fetch_value",
        "pub async fn fetch_value(key: &str) -> u32",
        Some("Loads the current value."),
        true,
        &[],
        "public",
        2,
        11,
        1,
        1,
        2,
        2,
        200,
    )
}

fn cached_details(id: &str) -> Value {
    details(
        id,
        "cached_value",
        "function",
        "src/lib.rs::cached_value",
        "fn cached_value() -> u32",
        None,
        false,
        &[],
        "private",
        13,
        15,
        0,
        0,
        1,
        1,
        60,
    )
}

fn widget_details(id: &str) -> Value {
    details(
        id,
        "Widget",
        "struct",
        "src/lib.rs::Widget",
        "pub struct Widget",
        None,
        false,
        &["Clone", "Debug"],
        "public",
        18,
        20,
        0,
        0,
        0,
        1,
        60,
    )
}

fn label_details(id: &str) -> Value {
    details(
        id,
        "label",
        "field",
        "src/lib.rs::Widget::label",
        "pub label: &'static str",
        None,
        false,
        &[],
        "public",
        19,
        19,
        0,
        0,
        0,
        1,
        20,
    )
}

fn impl_details(id: &str) -> Value {
    details(
        id,
        "Widget",
        "impl",
        "src/lib.rs::Widget",
        "impl Widget",
        None,
        false,
        &[],
        "private",
        22,
        27,
        0,
        0,
        0,
        1,
        120,
    )
}

fn render_details(id: &str) -> Value {
    details(
        id,
        "render",
        "method",
        "src/lib.rs::Widget::render",
        "pub fn render(&self) -> &'static str",
        Some("Paints the widget."),
        false,
        &[],
        "public",
        24,
        26,
        0,
        0,
        1,
        1,
        60,
    )
}

#[allow(clippy::too_many_arguments)]
fn details(
    id: &str,
    name: &str,
    kind: &str,
    qualified_name: &str,
    signature: &str,
    docstring: Option<&str>,
    is_async: bool,
    derives: &[&str],
    visibility: &str,
    start_line: u64,
    end_line: u64,
    branches: u64,
    loops: u64,
    max_nesting: u64,
    cyclomatic: u64,
    body_cost: u64,
) -> Value {
    json!({
        "id": id,
        "name": name,
        "kind": kind,
        "qualified_name": qualified_name,
        "file": "src/lib.rs",
        "start_line": start_line,
        "end_line": end_line,
        "signature": signature,
        "docstring": docstring,
        "is_async": is_async,
        "derives": derives,
        "visibility": visibility,
        "branches": branches,
        "loops": loops,
        "max_nesting": max_nesting,
        "cyclomatic_complexity": cyclomatic,
        "complexity_analysis": "complete",
        "cost_to_expand": {"body": body_cost, "full_file": FULL_FILE_COST},
        "unavailable_fields": [
            "assertions",
            "attrs_start_line",
            "returns",
            "unchecked_calls",
            "unsafe_blocks"
        ]
    })
}

fn fetch_markdown(id: &str) -> String {
    format!(
        "\
**branches:** 1
**complexity_analysis:** complete
**cyclomatic_complexity:** 2
**docstring:** Loads the current value.
**end_line:** 11
**file:** src/lib.rs
**id:** `{id}`
**is_async:** true
**kind:** function
**loops:** 1
**max_nesting:** 2
**name:** fetch_value
**qualified_name:** `src/lib.rs::fetch_value`
**signature:** `pub async fn fetch_value(key: &str) -> u32`
**start_line:** 2
**visibility:** public

## cost_to_expand
**body:** 200
**full_file:** 122
derives: none

## unavailable_fields
- assertions
- attrs_start_line
- returns
- unchecked_calls
- unsafe_blocks
"
    )
}

fn cached_markdown(id: &str) -> String {
    format!(
        "\
**branches:** 0
**complexity_analysis:** complete
**cyclomatic_complexity:** 1
**end_line:** 15
**file:** src/lib.rs
**id:** `{id}`
**is_async:** false
**kind:** function
**loops:** 0
**max_nesting:** 1
**name:** cached_value
**qualified_name:** `src/lib.rs::cached_value`
**signature:** `fn cached_value() -> u32`
**start_line:** 13
**visibility:** private

## cost_to_expand
**body:** 60
**full_file:** 122
derives: none

## unavailable_fields
- assertions
- attrs_start_line
- returns
- unchecked_calls
- unsafe_blocks
"
    )
}

fn widget_markdown(id: &str) -> String {
    format!(
        "\
**branches:** 0
**complexity_analysis:** complete
**cyclomatic_complexity:** 1
**end_line:** 20
**file:** src/lib.rs
**id:** `{id}`
**is_async:** false
**kind:** struct
**loops:** 0
**max_nesting:** 0
**name:** Widget
**qualified_name:** `src/lib.rs::Widget`
**signature:** `pub struct Widget`
**start_line:** 18
**visibility:** public

## cost_to_expand
**body:** 60
**full_file:** 122

## derives
- Clone
- Debug

## unavailable_fields
- assertions
- attrs_start_line
- returns
- unchecked_calls
- unsafe_blocks
"
    )
}

async fn assert_node(server: &McpServer, id: &str, expected: &Value) {
    let actual = node_json(server, json!({"node_id": id})).await;
    assert_eq!(actual, *expected);
}

async fn node_json(server: &McpServer, mut arguments: Value) -> Value {
    arguments
        .as_object_mut()
        .expect("node arguments")
        .entry("format")
        .or_insert_with(|| json!("json"));
    parse_json(&tool_text(&node_call(server, arguments).await))
}

async fn node_call(server: &McpServer, arguments: Value) -> Value {
    dispatch_mcp_tool_call(server, "tracedecay_node", arguments).await
}

async fn occurrence_id(server: &McpServer, name: &str, kind: &str) -> String {
    let response = dispatch_mcp_tool_call(
        server,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "format": "json"}),
    )
    .await;
    let payload = parse_json(&tool_text(&response));
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches
                .iter()
                .find(|row| row["name"] == name && row["kind"] == kind)
        })
        .and_then(|row| row["id"].as_str())
        .unwrap_or_else(|| panic!("no {kind} named {name} in {payload}"))
        .to_owned()
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

fn assert_execution_failed(response: &Value, message: &str) {
    assert_eq!(response["error"]["code"], -32603, "{response}");
    assert_eq!(response["error"]["message"], message, "{response}");
    assert_eq!(
        response["error"]["data"],
        json!({
            "tool": "tracedecay_node",
            "cli_fallback": NODE_CLI_FALLBACK,
        }),
        "{response}"
    );
}

#![cfg(feature = "test-transport")]

//! `tracedecay_signature` through the production MCP `tools/call` path.
//!
//! Expected strings are the surface a caller observes for this source, not
//! values recomputed by the handler under test.

use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    extract_text, handle_real_server_tool_call_raw, production_composition_fixture_with_sources,
    warm_code_index_search,
};

const SOURCE: &str = "\
/// Doubles a copyable value.
pub async fn scale<T>(value: T, factor: u32) -> T
where
    T: Copy,
{
    BODY_TOKEN_917
}

impl Counter {
    pub(crate) fn bump(&mut self, by: u32) -> u32 {
        BODY_TOKEN_917
    }
}
";

const SCALE_SIGNATURE: &str = "\
pub async fn scale<T>(value: T, factor: u32) -> T
where
    T: Copy,";

const BUMP_SIGNATURE: &str = "pub(crate) fn bump(&mut self, by: u32) -> u32";

#[tokio::test]
async fn tracedecay_signature_returns_literal_surface_and_typed_rejections() {
    let production = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), SOURCE).unwrap();
    })
    .await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production signature server");
    warm_code_index_search(&server, "scale").await;

    let scale = signature_json(
        &server,
        json!({"qualified_name": "src/lib.rs::scale", "format": "json"}),
    )
    .await;
    let scale_id = record_id(&scale, "scale");
    assert_eq!(scale, json!([scale_record(&scale_id)]));

    let by_node_id = signature_json(&server, json!({"node_id": scale_id, "format": "json"})).await;
    assert_eq!(by_node_id, json!([scale_record(&scale_id)]));

    let by_search_anchor = signature_json(
        &server,
        json!({"node_id": format!("code-symbol:{scale_id}"), "format": "json"}),
    )
    .await;
    assert_eq!(by_search_anchor, json!([scale_record(&scale_id)]));

    let bump = signature_json(
        &server,
        json!({"qualified_name": "src/lib.rs::Counter::bump", "format": "json"}),
    )
    .await;
    let bump_id = record_id(&bump, "bump");
    assert_eq!(bump, json!([bump_record(&bump_id)]));
    assert_ne!(bump_id, scale_id);

    let impl_block = signature_json(
        &server,
        json!({"qualified_name": "src/lib.rs::Counter", "format": "json"}),
    )
    .await;
    let impl_id = record_id(&impl_block, "Counter");
    assert_eq!(impl_block, json!([impl_record(&impl_id)]));

    // `node_id` wins when both selectors are present, so a stale name cannot
    // hide the occurrence the caller already resolved.
    let node_id_wins = signature_json(
        &server,
        json!({
            "node_id": bump_id,
            "qualified_name": "src/lib.rs::scale",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(node_id_wins, json!([bump_record(&bump_id)]));

    let missing_name = signature_json(
        &server,
        json!({"qualified_name": "src/lib.rs::missing_symbol", "format": "json"}),
    )
    .await;
    assert_eq!(missing_name, json!([]));

    let missing_node = signature_json(
        &server,
        json!({"node_id": "absent-symbol-occurrence", "format": "json"}),
    )
    .await;
    assert_eq!(missing_node, json!([]));

    // Markdown is the production default. The suite helper inserts `json`
    // when `format` is omitted, so this call names the default explicitly.
    assert_eq!(
        signature_text(
            &server,
            json!({"qualified_name": "src/lib.rs::scale", "format": "markdown"}),
        )
        .await,
        scale_markdown(&scale_id)
    );
    assert_eq!(
        signature_text(
            &server,
            json!({"qualified_name": "src/lib.rs::missing_symbol", "format": "markdown"}),
        )
        .await,
        "_None._\n"
    );

    let missing_selector = signature_response(&server, json!({})).await;
    assert_eq!(missing_selector["error"]["code"], -32602);
    assert_eq!(
        missing_selector["error"]["message"],
        "missing required parameter: qualified_name or node_id"
    );
    assert_eq!(
        missing_selector["error"]["data"],
        json!({
            "tool": "tracedecay_signature",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: qualified_name or node_id",
        })
    );

    let empty_node = signature_response(&server, json!({"node_id": "   "})).await;
    assert_eq!(empty_node["error"]["code"], -32603);
    assert_eq!(
        empty_node["error"]["message"],
        "tool execution failed: config error: invalid parameter: node_id must not be empty"
    );
    assert_eq!(empty_node["error"]["data"]["tool"], "tracedecay_signature");

    let evidence_anchor =
        signature_response(&server, json!({"node_id": "code-graph:not-a-symbol"})).await;
    assert_eq!(evidence_anchor["error"]["code"], -32603);
    assert_eq!(
        evidence_anchor["error"]["message"],
        "tool execution failed: config error: invalid parameter: node_id `code-graph:not-a-symbol` is an evidence anchor, not a graph symbol occurrence"
    );
    assert_eq!(
        evidence_anchor["error"]["data"]["tool"],
        "tracedecay_signature"
    );

    production.harness.shutdown().await;
}

fn scale_record(node_id: &str) -> Value {
    json!({
        "node_id": node_id,
        "name": "scale",
        "qualified_name": "src/lib.rs::scale",
        "kind": "function",
        "visibility": "public",
        "signature": SCALE_SIGNATURE,
        "docstring": "Doubles a copyable value.",
        "is_async": true,
        "file": "src/lib.rs",
        "start_line": 2,
        "end_line": 7,
        "cost_to_expand": {"body": 120, "full_file": 55},
        "unavailable_fields": ["attrs_start_line"],
    })
}

fn bump_record(node_id: &str) -> Value {
    json!({
        "node_id": node_id,
        "name": "bump",
        "qualified_name": "src/lib.rs::Counter::bump",
        "kind": "method",
        "visibility": "pub_crate",
        "signature": BUMP_SIGNATURE,
        "docstring": null,
        "is_async": false,
        "file": "src/lib.rs",
        "start_line": 10,
        "end_line": 12,
        "cost_to_expand": {"body": 60, "full_file": 55},
        "unavailable_fields": ["attrs_start_line"],
    })
}

fn impl_record(node_id: &str) -> Value {
    json!({
        "node_id": node_id,
        "name": "Counter",
        "qualified_name": "src/lib.rs::Counter",
        "kind": "impl",
        "visibility": "private",
        "signature": "impl Counter",
        "docstring": null,
        "is_async": false,
        "file": "src/lib.rs",
        "start_line": 9,
        "end_line": 13,
        "cost_to_expand": {"body": 100, "full_file": 55},
        "unavailable_fields": ["attrs_start_line"],
    })
}

fn scale_markdown(node_id: &str) -> String {
    format!(
        "\
- **scale**
  **kind:** function
  **file:** src/lib.rs
  **signature:** `pub async fn scale<T>(value: T, factor: u32) -> T
    where
        T: Copy,`
  **cost_to_expand:** body=120, full_file=55
  **docstring:** Doubles a copyable value.
  **end_line:** 7
  **is_async:** true
  **node_id:** `{node_id}`
  **qualified_name:** `src/lib.rs::scale`
  **start_line:** 2
  **unavailable_fields:** attrs_start_line
  **visibility:** public
"
    )
}

async fn signature_response(server: &Arc<McpServer>, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_signature", arguments).await
}

async fn signature_json(server: &Arc<McpServer>, arguments: Value) -> Value {
    let response = signature_response(server, arguments).await;
    assert!(
        response["error"].is_null(),
        "tracedecay_signature failed: {response}"
    );
    let text = extract_text(&response["result"]);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("signature response was not JSON: {error}; text={text}"))
}

async fn signature_text(server: &Arc<McpServer>, arguments: Value) -> String {
    let response = signature_response(server, arguments).await;
    assert!(
        response["error"].is_null(),
        "tracedecay_signature failed: {response}"
    );
    extract_text(&response["result"]).to_owned()
}

fn record_id(payload: &Value, name: &str) -> String {
    payload
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["node_id"].as_str())
        .unwrap_or_else(|| panic!("{name} signature response omitted node_id: {payload}"))
        .to_owned()
}

#![cfg(feature = "test-transport")]

//! Observable `tracedecay_impact` behavior through the production MCP server.
//!
//! The fixture graph is fixed so line numbers are part of the contract:
//!
//! `src/lib.rs`
//! ```text
//!  1 | mod callers;
//!  3 | pub fn callee() -> i32
//!  7 | pub fn local_caller() -> i32   // calls callee
//! 11 | pub fn untouched() -> i32      // calls nothing
//! ```
//!
//! `src/callers.rs`
//! ```text
//!  3 | pub fn direct() -> i32         // calls callee
//!  7 | pub fn indirect() -> i32       // calls direct
//! ```
//!
//! Impact walks incoming dependents. `callee` is therefore reached by
//! `local_caller` and `direct` at depth 1, and by `indirect` at depth 2.
//! `untouched` has no dependents. Argument failures stay typed JSON-RPC
//! errors rather than an empty radius.

use std::collections::HashMap;
use std::fs;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    handle_real_server_tool_call_raw, production_composition_fixture_with_sources,
    warm_code_index_search,
};

const LIB_RS: &str = "\
mod callers;\n\
\n\
pub fn callee() -> i32 {\n\
    1\n\
}\n\
\n\
pub fn local_caller() -> i32 {\n\
    callee()\n\
}\n\
\n\
pub fn untouched() -> i32 {\n\
    2\n\
}\n\
";

const CALLERS_RS: &str = "\
use crate::callee;\n\
\n\
pub fn direct() -> i32 {\n\
    callee()\n\
}\n\
\n\
pub fn indirect() -> i32 {\n\
    direct()\n\
}\n\
";

const UNKNOWN_NODE_ID: &str = "function:0000000000000000000000000000ffff";

#[tokio::test]
async fn impact_reports_callers_by_depth_and_refuses_invalid_requests() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("src directory");
        fs::write(project.join("src/lib.rs"), LIB_RS).expect("lib.rs");
        fs::write(project.join("src/callers.rs"), CALLERS_RS).expect("callers.rs");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    warm_code_index_search(&server, "callee").await;

    let mut ids = HashMap::new();
    for name in ["callee", "local_caller", "direct", "indirect", "untouched"] {
        ids.insert(name.to_owned(), symbol_id(&server, name).await);
    }

    let full = impact(
        &server,
        json!({ "node_id": ids["callee"], "format": "json" }),
    )
    .await;
    assert_radius(
        &full,
        true,
        3,
        &[
            node(&ids["direct"], "direct", "src/callers.rs", 3, 1),
            node(&ids["local_caller"], "local_caller", "src/lib.rs", 7, 1),
            node(&ids["indirect"], "indirect", "src/callers.rs", 7, 2),
        ],
    );

    let depth_one = impact(
        &server,
        json!({
            "node_id": ids["callee"],
            "max_depth": 1,
            "format": "json",
        }),
    )
    .await;
    assert_radius(
        &depth_one,
        false,
        2,
        &[
            node(&ids["direct"], "direct", "src/callers.rs", 3, 1),
            node(&ids["local_caller"], "local_caller", "src/lib.rs", 7, 1),
        ],
    );

    let anchored = format!("code-symbol:{}", ids["callee"]);
    let from_anchor = impact(&server, json!({ "node_id": anchored, "format": "json" })).await;
    assert_radius(
        &from_anchor,
        true,
        3,
        &[
            node(&ids["direct"], "direct", "src/callers.rs", 3, 1),
            node(&ids["local_caller"], "local_caller", "src/lib.rs", 7, 1),
            node(&ids["indirect"], "indirect", "src/callers.rs", 7, 2),
        ],
    );

    let untouched = impact(
        &server,
        json!({ "node_id": ids["untouched"], "format": "json" }),
    )
    .await;
    assert_eq!(
        untouched,
        json!({
            "node_count": 0,
            "complete": true,
            "unavailable_fields": ["edge_count"],
            "nodes": [],
        }),
        "a symbol with no dependents is an empty radius, unlike callee: {full}"
    );

    let unknown = impact(
        &server,
        json!({ "node_id": UNKNOWN_NODE_ID, "format": "json" }),
    )
    .await;
    assert_eq!(
        unknown,
        json!({
            "node_count": 0,
            "complete": true,
            "unavailable_fields": ["edge_count"],
            "nodes": [],
        }),
        "an unknown occurrence is empty; callee is not: {full}"
    );

    assert_refused(
        &server,
        json!({ "node_id": "   ", "format": "json" }),
        "tool execution failed: config error: invalid parameter: node_id must not be empty",
    )
    .await;
    assert_refused(
        &server,
        json!({ "node_id": ids["callee"], "max_depth": 0, "format": "json" }),
        "tool execution failed: config error: invalid parameter: max_depth must be at least 1",
    )
    .await;
    assert_refused(
        &server,
        json!({ "format": "json" }),
        "tool execution failed: config error: invalid arguments for tracedecay_impact: missing field `node_id`",
    )
    .await;
    assert_refused(
        &server,
        json!({ "node_id": "bad\u{0001}id", "format": "json" }),
        "tool execution failed: config error: invalid graph symbol occurrence: SymbolOccurrenceId is not canonical",
    )
    .await;
    assert_refused(
        &server,
        json!({ "node_id": "code-chunk:not-a-symbol", "format": "json" }),
        "tool execution failed: config error: invalid parameter: node_id `code-chunk:not-a-symbol` is an evidence anchor, not a graph symbol occurrence",
    )
    .await;

    fixture.harness.shutdown().await;
}

fn node(id: &str, name: &str, file: &str, line: u64, depth: u64) -> Value {
    json!({
        "id": id,
        "name": name,
        "kind": "function",
        "file": file,
        "line": line,
        "depth": depth,
    })
}

fn assert_radius(payload: &Value, complete: bool, node_count: u64, expected: &[Value]) {
    assert_eq!(payload["complete"], json!(complete), "{payload}");
    assert_eq!(payload["node_count"], json!(node_count), "{payload}");
    assert_eq!(
        payload["unavailable_fields"],
        json!(["edge_count"]),
        "{payload}"
    );
    let mut nodes = payload["nodes"]
        .as_array()
        .unwrap_or_else(|| panic!("impact nodes must be an array: {payload}"))
        .clone();
    nodes.sort_by_key(node_sort_key);
    let mut expected = expected.to_vec();
    expected.sort_by_key(node_sort_key);
    assert_eq!(nodes, expected, "impact radius: {payload}");
}

fn node_sort_key(node: &Value) -> (u64, String, u64, String) {
    (
        node["depth"].as_u64().expect("depth"),
        node["file"].as_str().expect("file").to_owned(),
        node["line"].as_u64().expect("line"),
        node["name"].as_str().expect("name").to_owned(),
    )
}

async fn impact(server: &McpServer, arguments: Value) -> Value {
    let response = handle_real_server_tool_call_raw(server, "tracedecay_impact", arguments).await;
    assert!(
        response.get("error").is_none_or(Value::is_null),
        "impact call failed: {response}"
    );
    let text = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("impact response missing text: {response}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("impact JSON ({error}): {text}"))
}

async fn symbol_id(server: &McpServer, name: &str) -> String {
    let response = handle_real_server_tool_call_raw(
        server,
        "tracedecay_find_exact_symbol",
        json!({ "name": name, "limit": 20, "format": "json" }),
    )
    .await;
    let text = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("exact-symbol response missing text: {response}"));
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("exact-symbol JSON ({error}): {text}"));
    let matches = payload["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("exact-symbol matches missing: {payload}"))
        .iter()
        .filter(|item| item["name"] == name)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "expected one symbol named {name}: {payload}"
    );
    matches[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("exact-symbol id missing: {payload}"))
        .to_owned()
}

async fn assert_refused(server: &McpServer, arguments: Value, message: &str) {
    let response = handle_real_server_tool_call_raw(server, "tracedecay_impact", arguments).await;
    assert_eq!(
        response["error"]["code"],
        json!(-32603),
        "typed refusal code: {response}"
    );
    assert_eq!(
        response["error"]["message"], message,
        "typed refusal: {response}"
    );
    assert_eq!(
        response["error"]["data"]["tool"],
        json!("tracedecay_impact"),
        "typed refusal names the tool: {response}"
    );
}

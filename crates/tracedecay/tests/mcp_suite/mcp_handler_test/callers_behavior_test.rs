//! `tracedecay_callers` through the production MCP `tools/call` path.
//!
//! The fixture is a two-file Rust crate:
//!
//! ```text
//! src/main.rs
//!   mod worker;
//!   use crate::worker::prepare_order;
//!
//!   fn main() {          // line 4
//!       prepare_order();
//!   }
//!
//! src/worker.rs
//!   pub fn prepare_order() { // line 1
//!       settle();
//!   }
//!
//!   fn also() {          // line 5
//!       settle();
//!   }
//!
//!   fn settle() {}       // line 9
//! ```
//!
//! `settle` is called by `also` and `prepare_order`. `prepare_order` is called
//! by `main`. The callee is not named `run`: that bare name is withheld from
//! cross-file binding, so a depth-2 walk would never reach `main`. `main` and
//! an unknown occurrence have no callers. Call edges are ordered by caller
//! occurrence identity, which is what the verified graph returns.

#![cfg(feature = "test-transport")]

use std::cmp::Ordering;
use std::fs;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_mcp::McpTransport;

use crate::support::{
    handle_real_server_tool_call_raw, production_composition_fixture_with_sources,
    warm_code_index_search,
};

const MAIN_RS: &str = "\
mod worker;\n\
use crate::worker::prepare_order;\n\
\n\
fn main() {\n\
    prepare_order();\n\
}\n";

const WORKER_RS: &str = "\
pub fn prepare_order() {\n\
    settle();\n\
}\n\
\n\
fn also() {\n\
    settle();\n\
}\n\
\n\
fn settle() {}\n";

const UNKNOWN_OCCURRENCE: &str =
    "symbol.v1.sha256:4f4adb437af949d76698f841fde2eab2d2d4c62c56e24bdfa0f1de614219a34b";

struct LineTransport {
    incoming: Option<String>,
    output: String,
}

impl McpTransport for LineTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        Ok(self.incoming.take())
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.output.push_str(line);
        Ok(())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn call_callers(server: &McpServer, arguments: Value) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_callers",
            "arguments": arguments,
        }
    });
    let mut transport = LineTransport {
        incoming: Some(request.to_string()),
        output: String::new(),
    };
    Box::pin(server.run_connection(&mut transport))
        .await
        .expect("tracedecay_callers MCP call");
    serde_json::from_str(transport.output.trim()).expect("JSON-RPC response")
}

fn tool_text(response: &Value) -> &str {
    assert!(
        response["error"].is_null(),
        "tracedecay_callers failed: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tracedecay_callers result has no text: {response}"))
}

fn caller_record(node_id: &str, name: &str, file: &str, line: u64, depth: u64) -> Value {
    json!({
        "node_id": node_id,
        "name": name,
        "kind": "function",
        "file": file,
        "line": line,
        "edge_kind": "calls",
        "depth": depth,
    })
}

fn identity_order(left: &Value, right: &Value) -> Ordering {
    left["node_id"]
        .as_str()
        .unwrap_or("")
        .cmp(right["node_id"].as_str().unwrap_or(""))
}

/// Direct callers of `settle`: `also` at line 5 and `prepare_order` at line 1,
/// both in `src/worker.rs`, in occurrence-identity order.
fn direct_settle_callers(also_id: &str, prepare_order_id: &str) -> Value {
    let mut callers = vec![
        caller_record(also_id, "also", "src/worker.rs", 5, 1),
        caller_record(prepare_order_id, "prepare_order", "src/worker.rs", 1, 1),
    ];
    callers.sort_by(identity_order);
    Value::Array(callers)
}

/// Depth 2 continues from `prepare_order` to `main` on line 4 of `src/main.rs`.
fn transitive_settle_callers(also_id: &str, prepare_order_id: &str, main_id: &str) -> Value {
    let Value::Array(mut callers) = direct_settle_callers(also_id, prepare_order_id) else {
        unreachable!("direct callers are an array");
    };
    callers.push(caller_record(main_id, "main", "src/main.rs", 4, 2));
    Value::Array(callers)
}

fn direct_settle_markdown(callers: &Value) -> String {
    let rows = callers.as_array().expect("direct callers are an array");
    let mut body = String::from(
        "**kind:** function\n**file:** src/worker.rs\n**depth:** 1\n**edge_kind:** calls\n\n",
    );
    for row in rows {
        body.push_str(&format!(
            "- **{}**\n  **line:** {}\n  **node_id:** `{}`\n",
            row["name"].as_str().expect("caller name"),
            row["line"],
            row["node_id"].as_str().expect("caller node id"),
        ));
    }
    body
}

fn single_caller_markdown(row: &Value) -> String {
    format!(
        "- **{}**\n  **kind:** function\n  **file:** {}\n  **line:** {}\n  **depth:** {}\n  **edge_kind:** calls\n  **node_id:** `{}`\n",
        row["name"].as_str().expect("caller name"),
        row["file"].as_str().expect("caller file"),
        row["line"],
        row["depth"],
        row["node_id"].as_str().expect("caller node id"),
    )
}

fn internal_error(message: &str) -> Value {
    json!({
        "code": -32603,
        "message": message,
        "data": {
            "tool": "tracedecay_callers",
            "cli_fallback": "This tool is also available from the shell: `tracedecay tool callers ...` (`tracedecay tool callers --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
        }
    })
}

async fn function_id(server: &McpServer, name: &str) -> String {
    let response = handle_real_server_tool_call_raw(
        server,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20}),
    )
    .await;
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("exact-symbol response has no text: {response}"));
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("exact-symbol response is not JSON: {error}: {text}"));
    let matches = payload["matches"]
        .as_array()
        .unwrap_or_else(|| panic!("exact-symbol response has no matches: {payload}"));
    let ids = matches
        .iter()
        .filter(|item| item["name"] == name && item["kind"] == "function")
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids.len(),
        1,
        "expected one function named {name}, got {payload}"
    );
    ids[0].to_owned()
}

#[tokio::test]
async fn tracedecay_callers_reports_literal_call_sites_and_typed_rejections() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/main.rs"), MAIN_RS).unwrap();
        fs::write(project.join("src/worker.rs"), WORKER_RS).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, "settle").await;

    let settle_id = function_id(&server, "settle").await;
    let also_id = function_id(&server, "also").await;
    let prepare_order_id = function_id(&server, "prepare_order").await;
    let main_id = function_id(&server, "main").await;
    let direct = direct_settle_callers(&also_id, &prepare_order_id);
    let transitive = transitive_settle_callers(&also_id, &prepare_order_id, &main_id);
    let prepare_order_caller = json!([caller_record(&main_id, "main", "src/main.rs", 4, 1)]);

    let depth_one = call_callers(
        &server,
        json!({"node_id": settle_id, "max_depth": 1, "format": "json"}),
    )
    .await;
    let depth_one_payload: Value =
        serde_json::from_str(tool_text(&depth_one)).expect("depth-1 callers JSON");
    assert_eq!(
        depth_one_payload, direct,
        "max_depth 1 must list only the two functions that call settle"
    );

    let depth_two = call_callers(
        &server,
        json!({"node_id": settle_id, "max_depth": 2, "format": "json"}),
    )
    .await;
    let depth_two_payload: Value =
        serde_json::from_str(tool_text(&depth_two)).expect("depth-2 callers JSON");
    assert_eq!(
        depth_two_payload, transitive,
        "max_depth 2 must keep the direct callers and add main through prepare_order"
    );

    let default_depth =
        call_callers(&server, json!({"node_id": settle_id, "format": "json"})).await;
    let default_payload: Value =
        serde_json::from_str(tool_text(&default_depth)).expect("default-depth callers JSON");
    assert_eq!(
        default_payload, transitive,
        "omitted max_depth defaults to a walk that reaches main"
    );

    let by_alias = call_callers(
        &server,
        json!({"id": settle_id, "max_depth": 1, "format": "json"}),
    )
    .await;
    let alias_payload: Value =
        serde_json::from_str(tool_text(&by_alias)).expect("id-alias callers JSON");
    assert_eq!(
        alias_payload, direct,
        "the id alias must address the same symbol as node_id"
    );

    let prepare_order_callers = call_callers(
        &server,
        json!({"node_id": prepare_order_id, "max_depth": 1, "format": "json"}),
    )
    .await;
    let prepare_order_payload: Value = serde_json::from_str(tool_text(&prepare_order_callers))
        .expect("prepare_order callers JSON");
    assert_eq!(
        prepare_order_payload, prepare_order_caller,
        "prepare_order's only caller is main at src/main.rs:4"
    );

    let no_callers = call_callers(
        &server,
        json!({"node_id": main_id, "max_depth": 1, "format": "json"}),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(tool_text(&no_callers)).expect("main callers JSON"),
        json!([]),
        "main has no callers; settle in the same graph does"
    );

    let unknown = call_callers(
        &server,
        json!({"node_id": UNKNOWN_OCCURRENCE, "max_depth": 1, "format": "json"}),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(tool_text(&unknown)).expect("unknown callers JSON"),
        json!([]),
        "an unknown occurrence is an empty caller list, not a populated one"
    );

    let markdown = call_callers(&server, json!({"node_id": settle_id, "max_depth": 1})).await;
    assert_eq!(
        tool_text(&markdown),
        direct_settle_markdown(&direct),
        "agents that omit format receive markdown of the same caller rows"
    );

    let prepare_order_markdown = call_callers(
        &server,
        json!({"node_id": prepare_order_id, "max_depth": 1}),
    )
    .await;
    assert_eq!(
        tool_text(&prepare_order_markdown),
        single_caller_markdown(&prepare_order_caller[0]),
        "a single caller is rendered as one markdown record"
    );

    let empty_markdown = call_callers(&server, json!({"node_id": main_id, "max_depth": 1})).await;
    assert_eq!(
        tool_text(&empty_markdown),
        "_None._\n",
        "a symbol with no callers renders an explicit empty markdown note"
    );

    let missing = call_callers(&server, json!({})).await;
    assert_eq!(
        missing["error"],
        json!({
            "code": -32602,
            "message": "missing required parameter: node_id",
            "data": {
                "tool": "tracedecay_callers",
                "reason_code": "missing_required_parameter",
                "retryable": false,
                "detail": "missing required parameter: node_id"
            }
        }),
        "a call with no node_id must name the missing parameter: {missing}"
    );

    let blank = call_callers(&server, json!({"node_id": "   "})).await;
    assert_eq!(
        blank["error"],
        internal_error(
            "tool execution failed: config error: invalid parameter: node_id must not be empty"
        ),
        "a blank node_id is a typed rejection: {blank}"
    );

    let zero_depth = call_callers(&server, json!({"node_id": settle_id, "max_depth": 0})).await;
    assert_eq!(
        zero_depth["error"],
        internal_error(
            "tool execution failed: config error: invalid parameter: max_depth must be at least 1"
        ),
        "max_depth 0 must be rejected rather than returning callers: {zero_depth}"
    );

    let anchor = call_callers(
        &server,
        json!({"node_id": "code-graph:symbol.v1.not-a-symbol"}),
    )
    .await;
    assert_eq!(
        anchor["error"],
        internal_error(
            "tool execution failed: config error: invalid parameter: node_id `code-graph:symbol.v1.not-a-symbol` is an evidence anchor, not a graph symbol occurrence"
        ),
        "an evidence anchor must not be walked as a symbol: {anchor}"
    );

    fixture.harness.shutdown().await;
}

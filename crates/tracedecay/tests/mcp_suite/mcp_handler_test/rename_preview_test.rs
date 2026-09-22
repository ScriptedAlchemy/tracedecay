//! `tracedecay_rename_preview` as a host sees it: one JSON-RPC `tools/call`.
//!
//! The fixture is the production call graph, not a handler double. `checkout`
//! in `src/lib.rs` calls `reserve_stock` in `src/stock.rs` through an import.
//! The same caller file also names the symbol in a string and a comment.
//! Those two sites plus the import are not graph edges, so they must show up
//! as text-only matches, and neither preview may change the sources.

#![cfg(feature = "test-transport")]

use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    ProductionCompositionFixture, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, warm_code_index_search,
};

const STOCK_RS: &str = "\
pub fn reserve_stock(qty: u32) -> u32 {\n\
    qty\n\
}\n";

const LIB_RS: &str = "\
mod stock;\n\
\n\
use crate::stock::reserve_stock;\n\
\n\
pub fn checkout(qty: u32) -> u32 {\n\
    let _note = \"reserve_stock\";\n\
    // reserve_stock stays in this comment\n\
    reserve_stock(qty)\n\
}\n";

const PREVIEW_NOTE: &str = "\
Preview only. Nothing is edited. 'references' are graph reference sites \
(the declaration is reported separately in 'node'); 'text_only_matches' are \
literal name occurrences NOT backed by a graph edge (comments, strings, \
dynamic dispatch, unresolved refs) and must be reviewed by hand. Graph \
call-edge coverage improves as the resolver does.";

const TEXT_ONLY_NOTE: &str = "text-only matches, review manually";

async fn opened_fixture() -> (ProductionCompositionFixture, Arc<McpServer>) {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/stock.rs"), STOCK_RS).unwrap();
        fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    warm_code_index_search(&server, "reserve_stock").await;
    (fixture, server)
}

fn tool_json(response: &Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "tools/call failed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("rename preview text content: {response}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error} in {text}"))
}

async fn symbol_id(server: &McpServer, name: &str) -> String {
    let response = handle_real_server_tool_call_raw(
        server,
        "tracedecay_find_exact_symbol",
        json!({ "name": name, "limit": 20, "format": "json" }),
    )
    .await;
    let payload = tool_json(&response);
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches.iter().find_map(|item| {
                (item["name"] == name)
                    .then(|| item["id"].as_str().map(str::to_owned))
                    .flatten()
            })
        })
        .unwrap_or_else(|| panic!("exact symbol {name} missing: {payload}"))
}

fn expected_preview(reserve_id: &str, checkout_id: &str, new_name: Option<&str>) -> Value {
    json!({
        "read_only": true,
        "note": PREVIEW_NOTE,
        "symbol": "reserve_stock",
        "new_name": new_name,
        "node": {
            "id": reserve_id,
            "name": "reserve_stock",
            "qualified_name": "src/stock.rs::reserve_stock",
            "kind": "function",
            "file": "src/stock.rs",
            "line": 1,
            "snippet": "pub fn reserve_stock(qty: u32) -> u32 {"
        },
        "reference_count": 1,
        "references": [{
            "from_node_id": checkout_id,
            "from_name": "checkout",
            "from_kind": "function",
            "edge_kind": "calls",
            "file": "src/lib.rs",
            "line": 8,
            "snippet": "reserve_stock(qty)"
        }],
        "text_only_matches": [{
            "file": "src/lib.rs",
            "text_only_count": 3,
            "note": TEXT_ONLY_NOTE
        }]
    })
}

fn assert_sources_unchanged(fixture: &ProductionCompositionFixture) {
    let stock = fs::read_to_string(fixture.project_root.join("src/stock.rs")).unwrap();
    let lib = fs::read_to_string(fixture.project_root.join("src/lib.rs")).unwrap();
    assert_eq!(stock, STOCK_RS);
    assert_eq!(lib, LIB_RS);
}

#[tokio::test]
async fn rename_preview_reports_the_declaration_the_caller_and_text_only_names() {
    let (fixture, server) = opened_fixture().await;
    let reserve_id = symbol_id(&server, "reserve_stock").await;
    let checkout_id = symbol_id(&server, "checkout").await;

    let omitted = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({ "node_id": reserve_id, "format": "json" }),
    )
    .await;
    assert!(omitted["result"].get("isError").is_none(), "{omitted}");
    assert_eq!(
        tool_json(&omitted),
        expected_preview(&reserve_id, &checkout_id, None)
    );

    let named = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({
            "node_id": reserve_id,
            "new_name": "hold_inventory",
            "format": "json"
        }),
    )
    .await;
    assert!(named["result"].get("isError").is_none(), "{named}");
    assert_eq!(
        tool_json(&named),
        expected_preview(&reserve_id, &checkout_id, Some("hold_inventory"))
    );
    assert_sources_unchanged(&fixture);
}

fn assert_execution_refused(response: &Value, message: &str) {
    assert_eq!(
        response["error"],
        json!({
            "code": -32603,
            "message": message,
            "data": {
                "tool": "tracedecay_rename_preview",
                "cli_fallback": "This tool is also available from the shell: `tracedecay tool rename_preview ...` (`tracedecay tool rename_preview --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
            }
        }),
        "{response}"
    );
    assert!(response.get("result").is_none(), "{response}");
}

#[tokio::test]
async fn rename_preview_refuses_unknown_and_unusable_node_identity() {
    let (fixture, server) = opened_fixture().await;

    let missing = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({ "node_id": "nonexistent_id_12345", "format": "json" }),
    )
    .await;
    assert!(missing.get("error").is_none(), "{missing}");
    assert_eq!(missing["result"]["isError"], json!(true));
    assert_eq!(
        tool_json(&missing),
        json!({
            "status": "not_found",
            "reason_code": "node_not_found",
            "node_id": "nonexistent_id_12345",
            "message": "Node not found: nonexistent_id_12345"
        })
    );

    let omitted = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({ "format": "json" }),
    )
    .await;
    assert_execution_refused(
        &omitted,
        "tool execution failed: config error: invalid arguments for tracedecay_rename_preview: missing field `node_id`",
    );

    let empty = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({ "node_id": "", "format": "json" }),
    )
    .await;
    assert_execution_refused(
        &empty,
        "tool execution failed: config error: invalid parameter: node_id must not be empty",
    );

    let apply_shaped = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_rename_preview",
        json!({
            "node_id": "nonexistent_id_12345",
            "dry_run": false,
            "format": "json"
        }),
    )
    .await;
    assert_execution_refused(
        &apply_shaped,
        "tool execution failed: config error: invalid arguments for tracedecay_rename_preview: unknown field `dry_run`, expected `node_id` or `new_name`",
    );
    assert_sources_unchanged(&fixture);
}

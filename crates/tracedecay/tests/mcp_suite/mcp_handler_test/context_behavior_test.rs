//! `tracedecay_context` as an MCP client calls it.
//!
//! Each case sends JSON-RPC `tools/call` through the production server and
//! checks the text the client receives against a literal. `include_code` is
//! set when the body must be stable: without it the handler races search
//! against the verified graph and may omit symbols.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, warm_code_index_search,
};

fn write_billing_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).expect("billing src dir");
    fs::write(
        project.join("src/lib.rs"),
        "pub fn invoice_total(cents: u32) -> u32 {\n    cents\n}\n\npub trait TaxPolicy {\n    fn tax(&self, cents: u32) -> u32;\n}\n",
    )
    .expect("billing lib.rs");
}

async fn context_text(server: &McpServer, arguments: Value) -> String {
    let result = handle_real_server_tool_call(server, "tracedecay_context", arguments).await;
    assert_ne!(
        result["isError"],
        Value::Bool(true),
        "tracedecay_context failed: {result}"
    );
    extract_real_server_text(&result).to_owned()
}

async fn context_json(server: &McpServer, mut arguments: Value) -> Value {
    arguments
        .as_object_mut()
        .expect("context arguments")
        .insert("format".to_owned(), json!("json"));
    let text = context_text(server, arguments).await;
    serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("tracedecay_context JSON was not an object: {error}; text={text}")
    })
}

fn require_line(markdown: &str, exact: &str) {
    assert!(
        markdown.lines().any(|line| line == exact),
        "missing line {exact:?} in:\n{markdown}"
    );
}

fn search_identity(payload: &Value, name: &str) -> Value {
    let rendered = payload.to_string();
    let matches = payload
        .get("search_matches")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("search_matches missing in {rendered}"));
    let found = matches
        .iter()
        .find(|search_match| search_match["name"] == name)
        .unwrap_or_else(|| panic!("no search match named {name} in {rendered}"));
    json!({
        "name": found["name"],
        "qualified_name": found["qualified_name"],
        "kind": found["kind"],
        "file": found["file"],
        "exact_class": found["exact_class"],
        "rank": found["rank"],
    })
}

fn code_identity(payload: &Value) -> Value {
    let rendered = payload.to_string();
    let blocks = payload
        .get("code")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("code missing in {rendered}"));
    assert_eq!(blocks.len(), 1, "one code block: {rendered}");
    let block = &blocks[0];
    json!({
        "file": block["file"],
        "start_line": block["start_line"],
        "end_line": block["end_line"],
        "code": block["code"],
    })
}

fn symbol_identity(payload: &Value, name: &str) -> Value {
    let rendered = payload.to_string();
    let symbols = payload
        .get("symbols")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("symbols missing in {rendered}"));
    let symbol = symbols
        .iter()
        .find(|symbol| symbol["name"] == name)
        .unwrap_or_else(|| panic!("no symbol named {name} in {rendered}"));
    json!({
        "name": symbol["name"],
        "qualified_name": symbol["qualified_name"],
        "kind": symbol["kind"],
        "file": symbol["file"],
        "start_line": symbol["start_line"],
        "end_line": symbol["end_line"],
    })
}

#[tokio::test]
async fn tracedecay_context_returns_invoice_total_and_tax_policy() {
    let fixture = production_composition_fixture_with_sources(write_billing_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("billing context server");
    warm_code_index_search(&server, "invoice_total").await;

    let invoice = context_json(
        &server,
        json!({
            "task": "invoice_total",
            "max_nodes": 1,
            "include_code": true,
            "max_code_blocks": 1
        }),
    )
    .await;
    assert_eq!(invoice["task"], "invoice_total");
    assert_eq!(invoice["mode"], "explore");
    assert_eq!(invoice["freshness"], json!({"state": "fresh"}));
    assert_eq!(
        invoice["coverage"],
        json!({
            "exact": "complete",
            "lexical": "complete",
            "graph": "complete",
            "recall": "full"
        })
    );
    assert_eq!(invoice["memory_matches"], json!([]));
    assert_eq!(
        search_identity(&invoice, "invoice_total"),
        json!({
            "name": "invoice_total",
            "qualified_name": "src/lib.rs::invoice_total",
            "kind": "function",
            "file": "src/lib.rs",
            "exact_class": "exact_message",
            "rank": 1
        })
    );
    assert_eq!(
        symbol_identity(&invoice, "invoice_total"),
        json!({
            "name": "invoice_total",
            "qualified_name": "src/lib.rs::invoice_total",
            "kind": "function",
            "file": "src/lib.rs",
            "start_line": 1,
            "end_line": 3
        })
    );
    assert_eq!(
        code_identity(&invoice),
        json!({
            "file": "src/lib.rs",
            "start_line": 1,
            "end_line": 3,
            "code": "pub fn invoice_total(cents: u32) -> u32 {\n    cents\n}"
        })
    );

    let invoice_markdown = context_text(
        &server,
        json!({
            "task": "invoice_total",
            "max_nodes": 1,
            "include_code": true,
            "max_code_blocks": 1,
            "format": "markdown"
        }),
    )
    .await;
    let invoice_head = "freshness: fresh\n# Context for invoice_total\n\n### Code\n#### src/lib.rs:1\n```\npub fn invoice_total(cents: u32) -> u32 {\n    cents\n}\n```\n\n### Related Symbols\n";
    assert!(
        invoice_markdown.starts_with(invoice_head),
        "invoice markdown:\n{invoice_markdown}"
    );
    require_line(&invoice_markdown, "- `src/lib.rs::invoice_total`");

    let without_code = context_json(
        &server,
        json!({
            "task": "invoice_total",
            "max_nodes": 1,
            "include_code": false
        }),
    )
    .await;
    assert_eq!(without_code["code"], json!([]));
    assert_eq!(without_code["task"], "invoice_total");
    assert_eq!(
        search_identity(&without_code, "invoice_total"),
        json!({
            "name": "invoice_total",
            "qualified_name": "src/lib.rs::invoice_total",
            "kind": "function",
            "file": "src/lib.rs",
            "exact_class": "exact_message",
            "rank": 1
        })
    );

    let absent = context_json(
        &server,
        json!({
            "task": "zzz_not_in_this_crate",
            "max_nodes": 5,
            "include_code": true,
            "max_code_blocks": 1
        }),
    )
    .await;
    let absent_names = absent
        .get("search_matches")
        .and_then(Value::as_array)
        .map(|matches| {
            matches
                .iter()
                .filter_map(|search_match| search_match["name"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        absent_names.iter().all(|name| *name != "invoice_total"),
        "unrelated task must not return invoice_total: {absent}"
    );

    let plan = context_json(
        &server,
        json!({
            "task": "TaxPolicy",
            "mode": "plan",
            "max_nodes": 1,
            "include_code": true,
            "max_code_blocks": 1
        }),
    )
    .await;
    assert_eq!(plan["task"], "TaxPolicy");
    assert_eq!(plan["mode"], "plan");
    assert_eq!(
        symbol_identity(&plan, "TaxPolicy"),
        json!({
            "name": "TaxPolicy",
            "qualified_name": "src/lib.rs::TaxPolicy",
            "kind": "trait",
            "file": "src/lib.rs",
            "start_line": 5,
            "end_line": 7
        })
    );
    assert_eq!(
        code_identity(&plan),
        json!({
            "file": "src/lib.rs",
            "start_line": 5,
            "end_line": 7,
            "code": "pub trait TaxPolicy {\n    fn tax(&self, cents: u32) -> u32;\n}"
        })
    );

    let plan_markdown = context_text(
        &server,
        json!({
            "task": "TaxPolicy",
            "mode": "plan",
            "max_nodes": 1,
            "include_code": true,
            "max_code_blocks": 1,
            "format": "markdown"
        }),
    )
    .await;
    require_line(
        &plan_markdown,
        "- **TaxPolicy** (trait) - src/lib.rs:5 (0 implementors)",
    );
    require_line(
        &plan_markdown,
        "_No test files found covering these modules._",
    );
    require_line(&plan_markdown, "- `src/lib.rs::TaxPolicy`");

    fixture.harness.shutdown().await;
}

fn assert_rejected(response: &Value, message: &str) {
    assert!(
        response.get("result").is_none(),
        "rejected call must not return a result: {response}"
    );
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert_eq!(response["error"]["code"], json!(-32603));
    assert_eq!(response["error"]["message"], message);
    assert_eq!(response["error"]["data"]["tool"], "tracedecay_context");
}

#[tokio::test]
async fn tracedecay_context_rejects_malformed_requests() {
    let fixture = production_composition_fixture_with_sources(write_billing_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("billing context server");

    let missing = handle_real_server_tool_call_raw(&server, "tracedecay_context", json!({})).await;
    assert_rejected(
        &missing,
        "tool execution failed: config error: invalid arguments for tracedecay_context: missing field `task`",
    );

    let unknown = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_context",
        json!({"task": "invoice_total", "not_a_context_field": true}),
    )
    .await;
    assert_rejected(
        &unknown,
        "tool execution failed: config error: invalid arguments for tracedecay_context: unknown field `not_a_context_field`, expected one of `task`, `max_nodes`, `include_code`, `max_code_blocks`, `mode`, `include_memory`, `memory_limit`, `memory_min_trust`, `lexical_anchors`, `prefer_symbol`",
    );

    let mode = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_context",
        json!({"task": "invoice_total", "mode": "nope"}),
    )
    .await;
    assert_rejected(
        &mode,
        "tool execution failed: config error: invalid arguments for tracedecay_context: unknown variant `nope`, expected `explore` or `plan`",
    );

    let empty_anchor = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_context",
        json!({"task": "invoice_total", "lexical_anchors": [""]}),
    )
    .await;
    assert_rejected(
        &empty_anchor,
        "tool execution failed: config error: lexical anchor 0 is empty",
    );

    let spaced_anchor = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_context",
        json!({"task": "invoice_total", "lexical_anchors": ["invoice total"]}),
    )
    .await;
    assert_rejected(
        &spaced_anchor,
        "tool execution failed: config error: lexical anchor 0 must be one identifier or technical term: no surrounding whitespace, inner whitespace, or control characters",
    );

    fixture.harness.shutdown().await;
}

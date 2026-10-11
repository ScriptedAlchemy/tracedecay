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
    production_composition_fixture_with_sources, refusal_problem, warm_code_index_search,
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

    let budgeted = context_json(
        &server,
        json!({
            "task": "invoice_total", "max_nodes": 1, "include_code": true,
            "max_code_blocks": 1, "budget_tokens": 1
        }),
    )
    .await;
    assert_eq!(
        code_identity(&budgeted),
        json!({
            "file": "src/lib.rs", "start_line": 1, "end_line": 3,
            "code": "pub fn invoice_total(cents: u32) -> u32 {\n    cents\n}"
        })
    );
    assert_eq!(budgeted["token_budget"]["budget_tokens"], 1);
    assert_eq!(budgeted["token_budget"]["over_ceiling"], true);
    assert_eq!(
        budgeted["token_budget"]["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|section| section["section"] == "code"),
        Some(&json!({"section": "code", "total": 1, "shown": 1}))
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

#[tokio::test]
async fn tracedecay_context_serves_sanitized_code_when_catalog_is_ready() {
    let secret = ["sk", "-test-", "1234567890abcdef"].concat();
    let source = format!("pub fn billing_token() -> &'static str {{\n    \"{secret}\"\n}}\n");
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("token src dir");
        fs::write(project.join("src/lib.rs"), &source).expect("token lib.rs");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("token context server");
    warm_code_index_search(&server, "billing_token").await;

    let payload = context_json(
        &server,
        json!({
            "task": "billing_token",
            "max_nodes": 1,
            "include_code": true,
            "max_code_blocks": 1
        }),
    )
    .await;
    assert_eq!(payload["task"], "billing_token");
    assert_eq!(
        payload["coverage"],
        json!({
            "exact": "complete",
            "lexical": "complete",
            "graph": "complete",
            "recall": "full"
        }),
        "{payload}"
    );
    assert_eq!(
        code_identity(&payload),
        json!({
            "file": "src/lib.rs",
            "start_line": 1,
            "end_line": 3,
            "code": "pub fn billing_token() -> &'static str {\n    \"[TraceDecay redacted: exact credential]\"\n}"
        }),
        "{payload}"
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn tracedecay_context_withholds_code_that_drifted_after_indexing() {
    let fixture = production_composition_fixture_with_sources(write_billing_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("drift context server");
    warm_code_index_search(&server, "invoice_total").await;
    let request = json!({
        "task": "invoice_total",
        "max_nodes": 1,
        "include_code": true,
        "max_code_blocks": 1
    });
    let indexed = context_json(&server, request.clone()).await;
    assert_eq!(
        code_identity(&indexed),
        json!({
            "file": "src/lib.rs",
            "start_line": 1,
            "end_line": 3,
            "code": "pub fn invoice_total(cents: u32) -> u32 {\n    cents\n}"
        }),
        "{indexed}"
    );

    fs::write(
        fixture.project_root.join("src/lib.rs"),
        "pub fn invoice_total(cents: u32) -> u32 {\n    cents + 1\n}\n\npub trait TaxPolicy {\n    fn tax(&self, cents: u32) -> u32;\n}\n",
    )
    .expect("drift billing lib.rs");
    let drifted = context_json(&server, request).await;
    assert_eq!(
        search_identity(&drifted, "invoice_total"),
        json!({
            "name": "invoice_total",
            "qualified_name": "src/lib.rs::invoice_total",
            "kind": "function",
            "file": "src/lib.rs",
            "exact_class": "exact_message",
            "rank": 1
        }),
        "{drifted}"
    );
    assert_eq!(drifted["code"], json!([]), "{drifted}");

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn tracedecay_context_binds_same_name_declarations_to_their_own_code() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("ledger src dir");
        fs::write(
            project.join("src/lib.rs"),
            concat!(
                "pub struct Ledger;\n\n",
                "impl Ledger {\n    pub fn alpha() -> u32 {\n        1\n    }\n}\n\n",
                "impl Ledger {\n    pub fn beta() -> u32 {\n        2\n    }\n}\n\n",
                "impl Ledger {\n    pub fn gamma() -> u32 {\n        3\n    }\n}\n\n",
                "impl Ledger {\n    pub fn delta() -> u32 {\n        4\n    }\n}\n",
            ),
        )
        .expect("ledger lib.rs");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("ledger context server");
    warm_code_index_search(&server, "Ledger").await;

    let payload = context_json(
        &server,
        json!({
            "task": "Ledger",
            "include_code": true,
            "max_code_blocks": 10
        }),
    )
    .await;
    let blocks = payload["code"].as_array().expect("code blocks");
    let impl_blocks = blocks
        .iter()
        .filter(|block| {
            block["code"]
                .as_str()
                .is_some_and(|code| code.starts_with("impl Ledger"))
        })
        .count();
    assert_eq!(impl_blocks, 4, "{payload}");
    for block in blocks {
        let symbol = payload["symbols"]
            .as_array()
            .expect("symbols")
            .iter()
            .find(|symbol| symbol["node_id"] == block["node_id"])
            .unwrap_or_else(|| panic!("code block names no listed symbol: {payload}"));
        assert_eq!(
            (&block["start_line"], &block["end_line"]),
            (&symbol["start_line"], &symbol["end_line"]),
            "code block serves another declaration than its node: {payload}"
        );
    }

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn tracedecay_context_withholds_partial_windows_of_oversized_declarations() {
    let statements = 4_000;
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("oversized src dir");
        let body: String = (0..statements)
            .map(|index| format!("    let value_{index} = {index};\n"))
            .collect();
        fs::write(
            project.join("src/lib.rs"),
            format!("pub fn sprawling() -> u32 {{\n{body}    0\n}}\n"),
        )
        .expect("oversized lib.rs");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("oversized context server");
    warm_code_index_search(&server, "sprawling").await;
    let request = json!({
        "task": "sprawling",
        "max_nodes": 1,
        "include_code": true,
        "max_code_blocks": 1
    });

    let payload = context_json(&server, request.clone()).await;
    assert_eq!(payload["code"], json!([]), "{payload}");
    let end_line = statements + 3;
    assert_eq!(
        payload["oversized_symbols"]
            .as_array()
            .map(|symbols| symbols
                .iter()
                .map(|symbol| json!([symbol["name"], symbol["start_line"], symbol["end_line"]]))
                .collect::<Vec<_>>()),
        Some(vec![json!(["sprawling", 1, end_line])]),
        "{payload}"
    );

    let mut markdown_request = request;
    markdown_request["format"] = json!("markdown");
    let markdown = context_text(&server, markdown_request).await;
    require_line(
        &markdown,
        &format!(
            "- **sprawling** (function), src/lib.rs:1-{end_line} is larger than one code block; \
             read it with `tracedecay_source_body` or list its members with \
             `tracedecay_source_outline`."
        ),
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn context_preserves_single_name_and_multiword_prose_routing() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("routing source directory");
        fs::write(
            project.join("src/lib.rs"),
            concat!(
                "/// Numerical solver convergence.\n",
                "pub fn gmres() -> u32 { 1 }\n\n",
                "pub struct Type;\n",
                "impl Type {\n    pub fn method() -> u32 { 2 }\n}\n\n",
                "pub fn getUserById() -> u32 { 3 }\n\n",
                "pub fn parse_config() -> u32 { 4 }\n",
            ),
        )
        .expect("routing source");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("routing context server");
    warm_code_index_search(&server, "gmres").await;

    for (task, route, name, source) in [
        ("gmres", "name", "gmres", "pub fn gmres() -> u32 { 1 }"),
        (
            "Type.method",
            "prose",
            "method",
            "pub fn method() -> u32 { 2 }",
        ),
        (
            "numerical solver convergence",
            "prose",
            "gmres",
            "pub fn gmres() -> u32 { 1 }",
        ),
        (
            "getUserById handler",
            "prose",
            "getUserById",
            "pub fn getUserById() -> u32 { 3 }",
        ),
        (
            "where is parse_config called",
            "prose",
            "parse_config",
            "pub fn parse_config() -> u32 { 4 }",
        ),
    ] {
        let request = json!({
            "task": task,
            "max_nodes": 1,
            "include_code": true,
            "include_memory": false,
            "max_code_blocks": 1,
        });
        let automatic = context_json(&server, request.clone()).await;
        assert_eq!(automatic["route"], route, "{task}: {automatic}");
        assert_eq!(
            automatic["search_matches"][0]["name"], name,
            "{task}: {automatic}"
        );
        assert_eq!(automatic["search_matches"][0]["file"], "src/lib.rs");
        assert_eq!(automatic["search_matches"][0]["rank"], 1);
        assert!(
            automatic["code"][0]["code"]
                .as_str()
                .expect("source block")
                .contains(source),
            "{task}: {automatic}"
        );
        let mut explicit_request = request;
        explicit_request["prefer_symbol"] = json!(route == "name");
        let explicit = context_json(&server, explicit_request).await;
        assert_eq!(explicit["route"], route);
        assert_eq!(explicit["search_matches"], automatic["search_matches"]);
        assert_eq!(explicit["code"], automatic["code"]);
    }
    for (task, prefer_symbol, route) in [
        ("gmres", false, "prose"),
        ("getUserById handler", true, "name"),
        ("function", true, "prose"),
    ] {
        let result = context_json(
            &server,
            json!({"task": task, "prefer_symbol": prefer_symbol, "include_code": true}),
        )
        .await;
        assert_eq!(result["route"], route, "{result}");
    }
    fixture.harness.shutdown().await;
}

fn assert_rejected(response: &Value, message: &str) {
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "rejected call returned a JSON-RPC error: {response}"
    );
    let problem = refusal_problem(&response["result"]);
    assert_eq!(problem["kind"], "invalid_request");
    assert_eq!(problem["code"], "application.surface.invalid_request");
    assert_eq!(problem["message"], message, "{response}");
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
        "invalid arguments for tracedecay_context: missing field `task`",
    );

    let unknown = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_context",
        json!({"task": "invoice_total", "not_a_context_field": true}),
    )
    .await;
    assert_rejected(
        &unknown,
        "invalid arguments for tracedecay_context: unknown field `not_a_context_field`, expected one of `task`, `max_nodes`, `include_code`, `max_code_blocks`, `mode`, `include_memory`, `memory_limit`, `memory_min_trust`, `lexical_anchors`, `prefer_symbol`, `budget_tokens`",
    );

    let mode = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_context",
        json!({"task": "invoice_total", "mode": "nope"}),
    )
    .await;
    assert_rejected(
        &mode,
        "invalid arguments for tracedecay_context: unknown variant `nope`, expected `explore` or `plan`",
    );

    let empty_anchor = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_context",
        json!({"task": "invoice_total", "lexical_anchors": [""]}),
    )
    .await;
    assert_rejected(&empty_anchor, "lexical anchor 0 is empty");

    let spaced_anchor = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_context",
        json!({"task": "invoice_total", "lexical_anchors": ["invoice total"]}),
    )
    .await;
    assert_rejected(
        &spaced_anchor,
        "lexical anchor 0 must be one identifier or technical term: no surrounding whitespace, inner whitespace, or control characters",
    );

    fixture.harness.shutdown().await;
}

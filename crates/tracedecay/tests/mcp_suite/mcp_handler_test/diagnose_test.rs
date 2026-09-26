//! `tracedecay_diagnose` as a caller sees it: production MCP `tools/call`,
//! not the parser.
//!
//! Compiler stderr is the input. The observable result is the mapped
//! diagnostic, the publication report, and the default markdown rendering.
//! Occurrence ids are generation-minted, so they are checked against the live
//! exact-symbol read rather than pinned.

#![cfg(feature = "test-transport")]

use std::fs;

use serde_json::{Value, json};
use tracedecay_mcp::JsonRpcResponse;

use crate::support::{
    ProductionCompositionFixture, production_composition_fixture_with_sources,
    warm_code_index_search,
};

const SOURCE: &str = "pub fn target() {}\npub fn caller() { target(); }\n";

const RUSTC_ERROR: &str = "\
error[E0308]: mismatched types
  --> src/lib.rs:1:10
   |
1  | pub fn target() {}
   |          ^^^^^^ expected `u32`, found `()`

error: aborting due to 1 previous error
";

const RUSTC_ERROR_AND_WARNING: &str = "\
error[E0308]: mismatched types
  --> src/lib.rs:1:10
   |
warning: unused function
  --> src/lib.rs:2:1
   |
error: aborting due to 1 previous error
";

const COLORED_SHORT_ERROR: &str = "\
\u{1b}[1m\u{1b}[92m    Checking\u{1b}[0m diag-fixture v0.1.0\n\
src/lib.rs:1:10: \u{1b}[1m\u{1b}[91merror[E0308]\u{1b}[0m: mismatched types\n\
\u{1b}[1m\u{1b}[91merror\u{1b}[0m: could not compile `diag-fixture` (lib) due to 1 previous error\n";

const UNMAPPED_ERROR: &str = "\
error[E0425]: cannot find value `missing` in this scope
  --> src/missing.rs:4:5
   |
";

#[tokio::test]
async fn diagnose_reports_literal_mapping_filters_and_refusals() {
    let fixture = open_indexed_project().await;
    let target_id = exact_symbol_id(&fixture, "target").await;
    let caller_id = exact_symbol_id(&fixture, "caller").await;

    let mapped = diagnose_json(
        &fixture,
        json!({"cargo_output": RUSTC_ERROR, "format": "json"}),
    )
    .await;
    assert_eq!(mapped["diagnostics"][0]["node"]["node_id"], target_id);
    assert_eq!(mapped["diagnostics"][0]["callers"][0]["node_id"], caller_id);
    assert_eq!(
        without_minted_ids(&mapped),
        json!({
            "diagnostics_parsed": 1,
            "diagnostics_returned": 1,
            "mapped_to_node": 1,
            "unmapped": 0,
            "truncated": false,
            "published": {
                "status": "published",
                "publication_revision": 1,
                "inserted": 1,
                "cleared": 0,
                "unresolved": [],
                "rejected": []
            },
            "diagnostics": [{
                "severity": "error",
                "code": "E0308",
                "message": "mismatched types",
                "file": "src/lib.rs",
                "line": 1,
                "column": 10,
                "node": {
                    "name": "target",
                    "kind": "function",
                    "qualified_name": "src/lib.rs::target",
                    "file": "src/lib.rs",
                    "line": 1,
                    "start_line": 0,
                    "end_line": 0
                },
                "callers": [{
                    "name": "caller",
                    "kind": "function",
                    "qualified_name": "src/lib.rs::caller",
                    "file": "src/lib.rs",
                    "line": 2,
                    "start_line": 1,
                    "end_line": 1
                }]
            }]
        }),
        "mapped diagnose payload: {mapped}"
    );

    assert_eq!(
        diagnose_text(&fixture, json!({"cargo_output": RUSTC_ERROR})).await,
        "\
## Diagnostics
**Diagnostics parsed:** 1
**Diagnostics returned:** 1
**Mapped to node:** 1
**Unmapped:** 0
**Truncated:** false

### Findings
- **ERROR E0308 at src/lib.rs:1:10**
  **Message:** mismatched types
  **Node:** src/lib.rs::target
  **Callers:** caller (src/lib.rs:2)
"
    );

    let colored = diagnose_json(
        &fixture,
        json!({"cargo_output": COLORED_SHORT_ERROR, "format": "json"}),
    )
    .await;
    assert_eq!(
        without_minted_ids(&colored)["diagnostics"],
        json!([{
            "severity": "error",
            "code": "E0308",
            "message": "mismatched types",
            "file": "src/lib.rs",
            "line": 1,
            "column": 10,
            "node": {
                "name": "target",
                "kind": "function",
                "qualified_name": "src/lib.rs::target",
                "file": "src/lib.rs",
                "line": 1,
                "start_line": 0,
                "end_line": 0
            },
            "callers": [{
                "name": "caller",
                "kind": "function",
                "qualified_name": "src/lib.rs::caller",
                "file": "src/lib.rs",
                "line": 2,
                "start_line": 1,
                "end_line": 1
            }]
        }]),
        "colored short cargo output must map the same diagnostic: {colored}"
    );

    let errors_only = diagnose_json(
        &fixture,
        json!({
            "cargo_output": RUSTC_ERROR_AND_WARNING,
            "severity": "error",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(errors_only["diagnostics_parsed"], 1);
    assert_eq!(errors_only["diagnostics_returned"], 1);
    assert_eq!(errors_only["truncated"], false);
    assert_eq!(
        without_minted_ids(&errors_only)["diagnostics"],
        json!([{
            "severity": "error",
            "code": "E0308",
            "message": "mismatched types",
            "file": "src/lib.rs",
            "line": 1,
            "column": 10,
            "node": {
                "name": "target",
                "kind": "function",
                "qualified_name": "src/lib.rs::target",
                "file": "src/lib.rs",
                "line": 1,
                "start_line": 0,
                "end_line": 0
            },
            "callers": [{
                "name": "caller",
                "kind": "function",
                "qualified_name": "src/lib.rs::caller",
                "file": "src/lib.rs",
                "line": 2,
                "start_line": 1,
                "end_line": 1
            }]
        }]),
        "severity=error must keep only the error: {errors_only}"
    );

    let warnings_only = diagnose_json(
        &fixture,
        json!({
            "cargo_output": RUSTC_ERROR_AND_WARNING,
            "severity": "warning",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        warnings_only["diagnostics"][0]["node"]["node_id"],
        caller_id
    );
    assert_eq!(
        without_minted_ids(&warnings_only)["diagnostics"],
        json!([{
            "severity": "warning",
            "code": null,
            "message": "unused function",
            "file": "src/lib.rs",
            "line": 2,
            "column": 1,
            "node": {
                "name": "caller",
                "kind": "function",
                "qualified_name": "src/lib.rs::caller",
                "file": "src/lib.rs",
                "line": 2,
                "start_line": 1,
                "end_line": 1
            },
            "callers": []
        }]),
        "severity=warning must keep only the warning and no callers: {warnings_only}"
    );

    let truncated = diagnose_json(
        &fixture,
        json!({
            "cargo_output": RUSTC_ERROR_AND_WARNING,
            "max_diagnostics": 1,
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        (
            truncated["diagnostics_parsed"].as_u64(),
            truncated["diagnostics_returned"].as_u64(),
            truncated["truncated"].as_bool(),
            truncated["diagnostics"][0]["code"].as_str(),
            truncated["diagnostics"][0]["message"].as_str(),
        ),
        (
            Some(2),
            Some(1),
            Some(true),
            Some("E0308"),
            Some("mismatched types")
        ),
        "spanless summary is not a diagnostic, and the cap keeps the first spanned one: {truncated}"
    );

    let hidden_callers = diagnose_json(
        &fixture,
        json!({
            "cargo_output": RUSTC_ERROR,
            "include_callers": false,
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        hidden_callers["diagnostics"][0]["node"]["node_id"],
        target_id
    );
    assert_eq!(hidden_callers["diagnostics"][0]["callers"], Value::Null);
    assert_eq!(hidden_callers["mapped_to_node"], 1);

    let unmapped = diagnose_json(
        &fixture,
        json!({"cargo_output": UNMAPPED_ERROR, "format": "json"}),
    )
    .await;
    assert_eq!(
        without_minted_ids(&unmapped)["diagnostics"],
        json!([{
            "severity": "error",
            "code": "E0425",
            "message": "cannot find value `missing` in this scope",
            "file": "src/missing.rs",
            "line": 4,
            "column": 5,
            "node": null,
            "callers": []
        }]),
        "a span outside the graph stays in the result with a null node: {unmapped}"
    );
    assert_eq!(unmapped["diagnostics_parsed"], 1);
    assert_eq!(unmapped["mapped_to_node"], 0);
    assert_eq!(unmapped["unmapped"], 1);

    assert_eq!(
        diagnose_text(&fixture, json!({"cargo_output": ""})).await,
        "\
## Diagnostics
**Diagnostics parsed:** 0
**Diagnostics returned:** 0
**Mapped to node:** 0
**Unmapped:** 0
**Truncated:** false

_No diagnostics._
"
    );

    let refused = diagnose_rpc(&fixture, json!({})).await;
    let error = refused
        .error
        .as_ref()
        .expect("missing cargo_output is a JSON-RPC error");
    assert_eq!(error.code, -32603);
    assert_eq!(
        error.message,
        "tool execution failed: config error: invalid arguments for tracedecay_diagnose: missing field `cargo_output`"
    );
    assert_eq!(
        error.data.as_ref().map(|data| &data["tool"]),
        Some(&json!("tracedecay_diagnose"))
    );

    fixture.harness.shutdown().await;
}

async fn open_indexed_project() -> ProductionCompositionFixture {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), SOURCE).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("diagnose fixture server");
    warm_code_index_search(&server, "target").await;
    drop(server);
    fixture
}

async fn exact_symbol_id(fixture: &ProductionCompositionFixture, name: &str) -> String {
    let response = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_find_exact_symbol",
            json!({"name": name, "limit": 20, "format": "json"}),
        )
        .await
        .expect("exact symbol read");
    assert!(response.error.is_none(), "{:?}", response.error);
    let payload = json_text(&response);
    payload["matches"]
        .as_array()
        .and_then(|matches| matches.iter().find(|item| item["name"] == name))
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("symbol {name} missing from {payload}"))
        .to_owned()
}

async fn diagnose_json(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let response = diagnose_rpc(fixture, arguments).await;
    assert!(
        response.error.is_none(),
        "diagnose failed: {:?}",
        response.error
    );
    json_text(&response)
}

async fn diagnose_text(fixture: &ProductionCompositionFixture, arguments: Value) -> String {
    let response = diagnose_rpc(fixture, arguments).await;
    assert!(
        response.error.is_none(),
        "diagnose failed: {:?}",
        response.error
    );
    tool_text(&response)
}

async fn diagnose_rpc(fixture: &ProductionCompositionFixture, arguments: Value) -> JsonRpcResponse {
    fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_diagnose", arguments)
        .await
        .expect("production MCP tools/call for tracedecay_diagnose")
}

fn json_text(response: &JsonRpcResponse) -> Value {
    let text = tool_text(response);
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{error}\n{text}"))
}

fn tool_text(response: &JsonRpcResponse) -> String {
    response
        .result
        .as_ref()
        .and_then(|result| result["content"][0]["text"].as_str())
        .unwrap_or_else(|| panic!("diagnose returned no text: {response:?}"))
        .to_owned()
}

/// Drop generation-minted occurrence ids so the remaining object is the
/// literal caller-visible diagnostic.
fn without_minted_ids(value: &Value) -> Value {
    let mut value = value.clone();
    strip_minted_ids(&mut value);
    value
}

fn strip_minted_ids(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                strip_minted_ids(item);
            }
        }
        Value::Object(map) => {
            map.remove("node_id");
            map.remove("generation");
            for child in map.values_mut() {
                strip_minted_ids(child);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

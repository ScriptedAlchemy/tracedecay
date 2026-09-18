#![cfg(feature = "test-transport")]

//! `tracedecay_ast_grep_search` as an MCP client sees it: `tools/call` on the
//! production composition, with the literal hit, markdown, and refusal text.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{ProductionCompositionFixture, production_composition_fixture_with_sources};

const RUST_CHECKOUT: &str = "\
fn checkout(qty: u32, sku: &str) {
    reserve_stock(qty, sku);
    // reserve_stock(sku, 0)
    let note = \"reserve_stock(sku, 0)\";
    log_stock(sku, 0);
    reserve_stock(sku, 0);
    reserve_stock(0, sku);
}
";

const PYTHON_CHECKOUT: &str = "\
def checkout(qty, sku):
    reserve_stock(qty, sku)
    # reserve_stock(sku, 0)
    note = \"reserve_stock(sku, 0)\"
    log_stock(sku, 0)
    reserve_stock(sku, 0)
";

fn write_checkout_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/checkout.rs"), RUST_CHECKOUT).unwrap();
    fs::write(project.join("src/checkout.py"), PYTHON_CHECKOUT).unwrap();
}

fn write_capped_calls(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    let mut source = String::from("fn calls() {\n");
    for index in 0..=200 {
        source.push_str(&format!("    reserve_stock({index}, 0);\n"));
    }
    source.push_str("}\n");
    fs::write(project.join("src/calls.rs"), source).unwrap();
}

async fn open_fixture(write_sources: impl FnOnce(&Path)) -> ProductionCompositionFixture {
    production_composition_fixture_with_sources(write_sources).await
}

async fn call_ast_grep(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let response = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_ast_grep_search",
            arguments,
        )
        .await
        .expect("production MCP tools/call");
    serde_json::to_value(response).expect("JSON-RPC response")
}

fn tool_payload(response: &Value) -> Value {
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "tools/call failed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tools/call returned no text: {response}"));
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tools/call text was not the JSON payload ({error}): {text}")
    })
}

fn tool_text(response: &Value) -> &str {
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "tools/call failed: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tools/call returned no text: {response}"))
}

fn assert_invalid_params(response: &Value, message: &str, data: Value) {
    assert_eq!(response.get("result"), None);
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(response["error"]["message"], message);
    assert_eq!(response["error"]["data"], data);
}

fn assert_config_error(response: &Value, detail: &str) {
    assert_eq!(response.get("result"), None);
    assert_eq!(response["error"]["code"], -32603);
    assert_eq!(
        response["error"]["message"],
        format!("tool execution failed: config error: {detail}")
    );
    assert_eq!(
        response["error"]["data"]["tool"],
        "tracedecay_ast_grep_search"
    );
}

fn rust_hit(line: u64, matched: &str, line_text: &str) -> Value {
    json!({
        "file": "src/checkout.rs",
        "line": line,
        "column": 5,
        "lang": "rust",
        "match": matched,
        "line_text": line_text,
    })
}

#[tokio::test]
async fn ast_grep_search_returns_the_call_and_rejects_text_lookalikes() {
    let fixture = open_fixture(write_checkout_sources).await;
    let literal_zero = call_ast_grep(
        &fixture,
        json!({
            "pattern": "reserve_stock($Q, 0)",
            "lang": "rust",
            "path_glob": "src/checkout.rs",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        tool_payload(&literal_zero),
        json!({
            "results": [
                rust_hit(6, "reserve_stock(sku, 0)", "    reserve_stock(sku, 0);")
            ],
            "match_count": 1,
            "files_scanned": 1,
            "truncated": false
        })
    );

    let markdown = call_ast_grep(
        &fixture,
        json!({
            "pattern": "reserve_stock($Q, 0)",
            "lang": "rust",
            "path_glob": "src/checkout.rs"
        }),
    )
    .await;
    assert_eq!(
        tool_text(&markdown),
        "\
## Structural Search Results
- src/checkout.rs:6
  > reserve_stock(sku, 0)

_1 matches across 1 files._
"
    );

    let every_call = call_ast_grep(
        &fixture,
        json!({
            "pattern": "reserve_stock($Q, $S)",
            "lang": "rust",
            "path_glob": "src/checkout.rs",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        tool_payload(&every_call),
        json!({
            "results": [
                rust_hit(2, "reserve_stock(qty, sku)", "    reserve_stock(qty, sku);"),
                rust_hit(6, "reserve_stock(sku, 0)", "    reserve_stock(sku, 0);"),
                rust_hit(7, "reserve_stock(0, sku)", "    reserve_stock(0, sku);")
            ],
            "match_count": 3,
            "files_scanned": 1,
            "truncated": false
        })
    );

    let absent = call_ast_grep(
        &fixture,
        json!({
            "pattern": "missing_stock($Q, 0)",
            "lang": "rust",
            "path_glob": "src/checkout.rs",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        tool_payload(&absent),
        json!({
            "results": [],
            "match_count": 0,
            "files_scanned": 1,
            "truncated": false
        })
    );
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn ast_grep_search_path_glob_and_language_select_one_file() {
    let fixture = open_fixture(write_checkout_sources).await;
    let rust_only = call_ast_grep(
        &fixture,
        json!({
            "pattern": "reserve_stock($Q, 0)",
            "path_glob": "src/checkout.rs",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        tool_payload(&rust_only),
        json!({
            "results": [
                rust_hit(6, "reserve_stock(sku, 0)", "    reserve_stock(sku, 0);")
            ],
            "match_count": 1,
            "files_scanned": 1,
            "truncated": false
        })
    );

    let python_only = call_ast_grep(
        &fixture,
        json!({
            "pattern": "reserve_stock($Q, 0)",
            "path_glob": "src/checkout.py",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        tool_payload(&python_only),
        json!({
            "results": [
                {
                    "file": "src/checkout.py",
                    "line": 6,
                    "column": 5,
                    "lang": "python",
                    "match": "reserve_stock(sku, 0)",
                    "line_text": "    reserve_stock(sku, 0)"
                }
            ],
            "match_count": 1,
            "files_scanned": 1,
            "truncated": false
        })
    );
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn ast_grep_search_caps_results_and_refuses_invalid_arguments() {
    let fixture = open_fixture(write_capped_calls).await;

    let one = call_ast_grep(
        &fixture,
        json!({
            "pattern": "reserve_stock($N, 0)",
            "lang": "rust",
            "max_results": 1,
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        tool_payload(&one),
        json!({
            "results": [
                {
                    "file": "src/calls.rs",
                    "line": 2,
                    "column": 5,
                    "lang": "rust",
                    "match": "reserve_stock(0, 0)",
                    "line_text": "    reserve_stock(0, 0);"
                }
            ],
            "match_count": 1,
            "files_scanned": 1,
            "truncated": true
        })
    );

    let capped_markdown = call_ast_grep(
        &fixture,
        json!({
            "pattern": "reserve_stock($N, 0)",
            "lang": "rust",
            "max_results": 1
        }),
    )
    .await;
    assert_eq!(
        tool_text(&capped_markdown),
        "\
## Structural Search Results
- src/calls.rs:2
  > reserve_stock(0, 0)

_1 matches across 1 files._ Results capped. Narrow with `path_glob` or `max_results`.
"
    );

    let default_cap = call_ast_grep(
        &fixture,
        json!({
            "pattern": "reserve_stock($N, 0)",
            "lang": "rust",
            "format": "json"
        }),
    )
    .await;
    let default_cap = tool_payload(&default_cap);
    assert_eq!(default_cap["match_count"], 50);
    assert_eq!(default_cap["files_scanned"], 1);
    assert_eq!(default_cap["truncated"], true);
    assert_eq!(
        default_cap["results"][0],
        json!({
            "file": "src/calls.rs",
            "line": 2,
            "column": 5,
            "lang": "rust",
            "match": "reserve_stock(0, 0)",
            "line_text": "    reserve_stock(0, 0);"
        })
    );
    assert_eq!(
        default_cap["results"][49],
        json!({
            "file": "src/calls.rs",
            "line": 51,
            "column": 5,
            "lang": "rust",
            "match": "reserve_stock(49, 0)",
            "line_text": "    reserve_stock(49, 0);"
        })
    );

    let hard_cap = call_ast_grep(
        &fixture,
        json!({
            "pattern": "reserve_stock($N, 0)",
            "lang": "rust",
            "max_results": 500,
            "format": "json"
        }),
    )
    .await;
    let hard_cap = tool_payload(&hard_cap);
    assert_eq!(hard_cap["match_count"], 200);
    assert_eq!(hard_cap["files_scanned"], 1);
    assert_eq!(hard_cap["truncated"], true);
    assert_eq!(
        hard_cap["results"][199],
        json!({
            "file": "src/calls.rs",
            "line": 201,
            "column": 5,
            "lang": "rust",
            "match": "reserve_stock(199, 0)",
            "line_text": "    reserve_stock(199, 0);"
        })
    );

    let zero_is_one = call_ast_grep(
        &fixture,
        json!({
            "pattern": "reserve_stock($N, 0)",
            "lang": "rust",
            "max_results": 0,
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        tool_payload(&zero_is_one),
        json!({
            "results": [
                {
                    "file": "src/calls.rs",
                    "line": 2,
                    "column": 5,
                    "lang": "rust",
                    "match": "reserve_stock(0, 0)",
                    "line_text": "    reserve_stock(0, 0);"
                }
            ],
            "match_count": 1,
            "files_scanned": 1,
            "truncated": true
        })
    );

    let missing = call_ast_grep(&fixture, json!({})).await;
    assert_invalid_params(
        &missing,
        "missing required parameter: pattern",
        json!({
            "tool": "tracedecay_ast_grep_search",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: pattern"
        }),
    );

    let blank = call_ast_grep(&fixture, json!({"pattern": "   "})).await;
    assert_config_error(&blank, "pattern must not be empty");

    let unknown_lang = call_ast_grep(
        &fixture,
        json!({"pattern": "reserve_stock($N, 0)", "lang": "klingon"}),
    )
    .await;
    assert_config_error(
        &unknown_lang,
        "unknown or unbundled language 'klingon'. Omit `lang` to auto-detect per file, or pass a language compiled into this build.",
    );

    let invalid_glob = call_ast_grep(
        &fixture,
        json!({"pattern": "reserve_stock($N, 0)", "path_glob": "["}),
    )
    .await;
    assert_config_error(&invalid_glob, "invalid path_glob '['");

    fixture.harness.shutdown().await;
}

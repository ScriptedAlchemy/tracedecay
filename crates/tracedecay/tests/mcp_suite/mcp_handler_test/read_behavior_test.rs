#![cfg(feature = "test-transport")]

//! `tracedecay_read` as an agent calls it: a production `tools/call` on the
//! mounted MCP server, with the text the agent sees compared to a literal.

use serde_json::{Value, json};
use tracedecay_mcp::jsonrpc::{JsonRpcError, JsonRpcResponse};

use crate::support::{
    ProductionCompositionFixture, extract_text, production_composition_fixture,
    wait_for_current_graph,
};

const MAIN_RS: &str = "\nuse crate::utils::helper;\nmod utils;\n\nfn main() {\n    let result = helper();\n    println!(\"{}\", result);\n}\n";
const MAIN_LINES_6_7: &str = "    let result = helper();\n    println!(\"{}\", result);";
const MAIN_LINE_6: &str = "    let result = helper();";
const UTILS_LINE_8: &str = "    format!(\"Hello, {}!\", name)";
const MAIN_DIGEST: &str = "a6db604d39bdea264080a5828f9267b39158b93e1688472d97c3c144a9357133";
const LINES_6_7_DIGEST: &str = "01689f1c87b95ae5347c98e92da0fa369bccb2589a0eb701603aa0014e4ee442";
const LINE_6_DIGEST: &str = "f6af8978aaf70c86256566cabd67f8539050d49b973b1ce30d49fd401af9e39b";
const UTILS_LINE_8_DIGEST: &str =
    "101f680c4b5c90a3c6e1543e9c04fcb0052a03b990b60c25775f09e3c1600d81";
const EMPTY_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

const MAIN_LINE_6_MARKDOWN: &str = "\
## src/main.rs (lines)
**tokens:** 7

### Context
**symbols:** 1
- function main 5-8: `fn main()`

```rs
    let result = helper();
```
";

const UNCHANGED_FULL_MARKDOWN: &str = "\
## src/main.rs (full)
**unchanged:** true
**digest:** a6db604d39bdea264080a5828f9267b39158b93e1688472d97c3c144a9357133
**tokens:** 27
";

type SymbolRow = (&'static str, &'static str, i64, i64, &'static str);

const MAIN_SYMBOLS: &[SymbolRow] = &[
    (
        "use",
        "crate::utils::helper",
        2,
        2,
        "use crate::utils::helper;",
    ),
    ("module", "utils", 3, 3, "mod utils"),
    ("function", "main", 5, 8, "fn main()"),
];

const UTILS_SYMBOLS: &[SymbolRow] = &[
    ("function", "helper", 3, 5, "pub fn helper() -> String"),
    (
        "function",
        "format_greeting",
        7,
        9,
        "fn format_greeting(name: &str) -> String",
    ),
];

async fn serving_fixture() -> ProductionCompositionFixture {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production tracedecay_read server");
    wait_for_current_graph(&server).await;
    fixture
}

async fn call_read(fixture: &ProductionCompositionFixture, arguments: Value) -> JsonRpcResponse {
    fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_read", arguments)
        .await
        .expect("tracedecay_read production invocation")
}

fn payload_text(response: &JsonRpcResponse) -> &str {
    assert!(
        response.error.is_none(),
        "tracedecay_read returned an MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    extract_text(response.result.as_ref().expect("tracedecay_read result"))
}

fn payload_json(response: &JsonRpcResponse) -> Value {
    let text = payload_text(response);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("read payload was not JSON: {error}\n{text}"))
}

fn expect_source(
    payload: &Value,
    file: &str,
    mode: &str,
    body: &str,
    digest: &str,
    token_count: u64,
) {
    assert_eq!(payload["file"], file);
    assert_eq!(payload["mode"], mode);
    assert_eq!(payload["body"], body);
    assert_eq!(payload["digest"], digest);
    assert_eq!(payload["token_count"], token_count);
    assert!(payload.get("unchanged").is_none());
    assert!(payload.get("context").is_none());
}

fn symbol_rows(body: &str) -> Vec<(String, String, i64, i64, String)> {
    let value: Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("symbol body was not JSON ({error}): {body}"));
    let symbols = value["symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("symbol body has no symbols array: {value}"));
    symbols
        .iter()
        .map(|symbol| {
            (
                symbol["kind"]
                    .as_str()
                    .unwrap_or_else(|| panic!("symbol kind: {symbol}"))
                    .to_owned(),
                symbol["name"]
                    .as_str()
                    .unwrap_or_else(|| panic!("symbol name: {symbol}"))
                    .to_owned(),
                symbol["line"]
                    .as_i64()
                    .unwrap_or_else(|| panic!("symbol line: {symbol}")),
                symbol["end_line"]
                    .as_i64()
                    .unwrap_or_else(|| panic!("symbol end_line: {symbol}")),
                symbol["signature"]
                    .as_str()
                    .unwrap_or_else(|| panic!("symbol signature: {symbol}"))
                    .to_owned(),
            )
        })
        .collect()
}

fn expect_symbol_rows(body: &str, file: &str, expected: &[SymbolRow]) {
    let value: Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("symbol body was not JSON ({error}): {body}"));
    assert_eq!(value["file"], file);
    assert_eq!(value["symbol_count"], expected.len());
    let actual = symbol_rows(body);
    let expected = expected
        .iter()
        .map(|row| {
            (
                row.0.to_owned(),
                row.1.to_owned(),
                row.2,
                row.3,
                row.4.to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

fn expect_error(response: &JsonRpcResponse) -> &JsonRpcError {
    assert!(
        response.result.is_none(),
        "tracedecay_read succeeded: {:?}",
        response.result
    );
    response.error.as_ref().expect("tracedecay_read error")
}

#[tokio::test]
async fn read_returns_the_exact_source_slice_and_marks_a_repeat_unchanged() {
    let mut fixture = serving_fixture().await;

    let first = call_read(&fixture, json!({"file": "src/main.rs", "format": "json"})).await;
    let first = payload_json(&first);
    expect_source(&first, "src/main.rs", "full", MAIN_RS, MAIN_DIGEST, 27);

    let repeated = call_read(&fixture, json!({"file": "src/main.rs", "mode": "full"})).await;
    assert_eq!(payload_text(&repeated), UNCHANGED_FULL_MARKDOWN);

    let lines = call_read(
        &fixture,
        json!({
            "file": "src/main.rs",
            "mode": "lines",
            "lines": "6-7",
            "include_symbols": false,
            "format": "json"
        }),
    )
    .await;
    expect_source(
        &payload_json(&lines),
        "src/main.rs",
        "lines",
        MAIN_LINES_6_7,
        LINES_6_7_DIGEST,
        14,
    );

    let one = call_read(
        &fixture,
        json!({
            "file": "src/main.rs",
            "mode": "lines",
            "lines": "6",
            "include_symbols": false,
            "format": "json"
        }),
    )
    .await;
    expect_source(
        &payload_json(&one),
        "src/main.rs",
        "lines",
        MAIN_LINE_6,
        LINE_6_DIGEST,
        7,
    );

    let other_file = call_read(
        &fixture,
        json!({
            "file": "src/utils.rs",
            "mode": "lines",
            "lines": "8",
            "include_symbols": false,
            "format": "json"
        }),
    )
    .await;
    expect_source(
        &payload_json(&other_file),
        "src/utils.rs",
        "lines",
        UTILS_LINE_8,
        UTILS_LINE_8_DIGEST,
        8,
    );

    let past_end = call_read(
        &fixture,
        json!({
            "file": "src/main.rs",
            "mode": "lines",
            "lines": "100-101",
            "include_symbols": false,
            "format": "json"
        }),
    )
    .await;
    expect_source(
        &payload_json(&past_end),
        "src/main.rs",
        "lines",
        "",
        EMPTY_DIGEST,
        0,
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn read_lines_markdown_names_the_overlapping_symbol() {
    let mut fixture = serving_fixture().await;

    let markdown = call_read(
        &fixture,
        json!({"file": "src/main.rs", "mode": "lines", "lines": "6"}),
    )
    .await;
    assert_eq!(payload_text(&markdown), MAIN_LINE_6_MARKDOWN);

    let with_context = call_read(
        &fixture,
        json!({
            "file": "src/main.rs",
            "mode": "lines",
            "lines": "6-7",
            "format": "json"
        }),
    )
    .await;
    let with_context = payload_json(&with_context);
    assert_eq!(with_context["body"], MAIN_LINES_6_7);
    assert_eq!(with_context["context"]["file"], "src/main.rs");
    assert_eq!(
        with_context["context"]["range"],
        json!({"start": 6, "end": 7})
    );
    assert_eq!(with_context["context"]["symbol_count"], 1);
    assert_eq!(with_context["context"]["truncated"], false);
    assert_eq!(with_context["context"]["symbols"][0]["kind"], "function");
    assert_eq!(with_context["context"]["symbols"][0]["name"], "main");
    assert_eq!(
        with_context["context"]["symbols"][0]["signature"],
        "fn main()"
    );
    assert_eq!(with_context["context"]["symbols"][0]["line"], 5);
    assert_eq!(with_context["context"]["symbols"][0]["end_line"], 8);

    let without_symbols = call_read(
        &fixture,
        json!({
            "file": "src/utils.rs",
            "mode": "lines",
            "lines": "3",
            "include_symbols": false,
            "format": "json"
        }),
    )
    .await;
    let without_symbols = payload_json(&without_symbols);
    assert_eq!(without_symbols["body"], "pub fn helper() -> String {");
    assert!(without_symbols.get("context").is_none());

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn read_map_and_signatures_list_symbols_and_not_source_text() {
    let mut fixture = serving_fixture().await;

    let full = call_read(
        &fixture,
        json!({"file": "src/main.rs", "mode": "full", "format": "json"}),
    )
    .await;
    let full = payload_json(&full);
    assert_eq!(full["body"], MAIN_RS);

    let map = call_read(
        &fixture,
        json!({"file": "src/main.rs", "mode": "map", "format": "json"}),
    )
    .await;
    let map = payload_json(&map);
    assert_eq!(map["file"], "src/main.rs");
    assert_eq!(map["mode"], "map");
    let map_body = map["body"].as_str().expect("map body");
    assert!(!map_body.contains("println!(\"{}\", result)"));
    expect_symbol_rows(map_body, "src/main.rs", MAIN_SYMBOLS);

    let signatures = call_read(
        &fixture,
        json!({"file": "src/utils.rs", "mode": "signatures", "format": "json"}),
    )
    .await;
    let signatures = payload_json(&signatures);
    assert_eq!(signatures["mode"], "signatures");
    let signatures_body = signatures["body"].as_str().expect("signatures body");
    let parsed: Value = serde_json::from_str(signatures_body).expect("signatures JSON");
    assert_eq!(parsed["without_signature"], 0);
    expect_symbol_rows(signatures_body, "src/utils.rs", UTILS_SYMBOLS);
    assert!(!signatures_body.contains("format_greeting(\"world\")"));

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn read_rejects_bad_arguments_with_the_agent_visible_error() {
    let mut fixture = serving_fixture().await;

    let missing = call_read(&fixture, json!({"format": "json"})).await;
    let missing = expect_error(&missing);
    assert_eq!(missing.code, -32602);
    assert_eq!(missing.message, "missing required parameter: file");
    assert_eq!(
        missing.data.as_ref().expect("missing-file error data"),
        &json!({
            "tool": "tracedecay_read",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: file"
        })
    );

    let unknown = call_read(&fixture, json!({"file": "src/main.rs", "mode": "blobs"})).await;
    let unknown = expect_error(&unknown);
    assert_eq!(unknown.code, -32603);
    assert_eq!(
        unknown.message,
        "tool execution failed: config error: unknown mode 'blobs'; expected one of full, lines, map, signatures"
    );
    assert_eq!(
        unknown.data.as_ref().expect("unknown-mode error data"),
        &json!({
            "tool": "tracedecay_read",
            "cli_fallback": "This tool is also available from the shell: `tracedecay tool read ...` (`tracedecay tool read --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
        })
    );

    let missing_lines = call_read(&fixture, json!({"file": "src/main.rs", "mode": "lines"})).await;
    let missing_lines = expect_error(&missing_lines);
    assert_eq!(missing_lines.code, -32603);
    assert_eq!(
        missing_lines.message,
        "tool execution failed: config error: mode='lines' requires the 'lines' argument (e.g. '120-180')"
    );

    let invalid_lines = call_read(
        &fixture,
        json!({"file": "src/main.rs", "mode": "lines", "lines": "7-5"}),
    )
    .await;
    assert_eq!(
        expect_error(&invalid_lines).message,
        "tool execution failed: config error: invalid 'lines' value '7-5'; expected 'A' or 'A-B'"
    );

    let zero = call_read(
        &fixture,
        json!({"file": "src/main.rs", "mode": "lines", "lines": "0"}),
    )
    .await;
    assert_eq!(
        expect_error(&zero).message,
        "tool execution failed: config error: invalid 'lines' value '0'; expected 'A' or 'A-B'"
    );

    let traversal = call_read(&fixture, json!({"file": "../outside.rs", "mode": "full"})).await;
    assert_eq!(
        expect_error(&traversal).message,
        "tool execution failed: config error: path '../outside.rs' contains unsafe components"
    );

    let empty = call_read(&fixture, json!({"file": ""})).await;
    assert_eq!(
        expect_error(&empty).message,
        "tool execution failed: config error: path must name a project file"
    );

    let missing_file = call_read(&fixture, json!({"file": "src/missing.rs"})).await;
    let root = fixture.project_root.display();
    assert_eq!(
        expect_error(&missing_file).message,
        format!(
            "tool execution failed: config error: path 'src/missing.rs' escapes project root '{root}' and is not indexed"
        )
    );

    fixture.harness.shutdown().await;
}

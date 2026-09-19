#![cfg(feature = "test-transport")]

//! Caller-visible `tracedecay_read` behavior through the production MCP
//! `tools/call` path. Expected bodies, digests, and error strings are literals
//! the test owns; they are not read back from the fixture writer or the tool.
//! Context order is nearest-first. Map and signature order follows the graph
//! page, so those assertions compare the symbol records, not their sequence.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tracedecay_mcp::jsonrpc::JsonRpcResponse;

use crate::support::{
    ProductionCompositionFixture, extract_json, extract_text, production_composition_fixture,
};

const MAIN_RS: &str = r#"
use crate::utils::helper;
mod utils;

fn main() {
    let result = helper();
    println!("{}", result);
}
"#;

const UTILS_RS: &str = r#"
/// Returns a greeting string.
pub fn helper() -> String {
    format_greeting("world")
}

fn format_greeting(name: &str) -> String {
    format!("Hello, {}!", name)
}
"#;

const MAIN_LINE_6: &str = "    let result = helper();";
const HELPER_LINES: &str = "pub fn helper() -> String {\n    format_greeting(\"world\")\n}";
const GREETING_LINES: &str =
    "fn format_greeting(name: &str) -> String {\n    format!(\"Hello, {}!\", name)\n}";
const MAIN_LINES: &str = "fn main() {\n    let result = helper();\n    println!(\"{}\", result);";
const LATE_RS: &str = "late source line\n";
const RENAMED_RS: &str = "fn renamed() {\n    let answer = 7;\n}\n";

fn main_full_markdown() -> String {
    format!("## src/main.rs (full)\n**tokens:** 27\n\n```rs\n{MAIN_RS}```\n")
}

fn main_lines_markdown() -> String {
    format!(
        "## src/main.rs (lines)\n**tokens:** 17\n\n### Context\n**symbols:** 1\n- function main 5-8: `fn main()`\n\n```rs\n{MAIN_LINES}\n```\n"
    )
}

const MAIN_LINES_UNCHANGED_MARKDOWN: &str = r#"## src/main.rs (lines)
**unchanged:** true
**digest:** 4fb13b9ff432de832e374ea1cfcce7e13478ebabfc6e6c67d213c445100d979f
**tokens:** 17

### Context
**symbols:** 1
- function main 5-8: `fn main()`
"#;

const RENAMED_UNCHANGED_MARKDOWN: &str = r#"## src/main.rs (full)
**unchanged:** true
**digest:** a5b7e2cd21e13410849a555fca6b3468e3046ef1d79df14f22e0daf0bbbb5a6c
**tokens:** 10
"#;

const LATE_MAP_BODY: &str =
    "{\n  \"file\": \"src/late.txt\",\n  \"symbol_count\": 0,\n  \"symbols\": []\n}";
const LATE_SIGNATURES_BODY: &str = "{\n  \"file\": \"src/late.txt\",\n  \"symbol_count\": 0,\n  \"symbols\": [],\n  \"without_signature\": 0\n}";

const EMPTY_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MAIN_DIGEST: &str = "a6db604d39bdea264080a5828f9267b39158b93e1688472d97c3c144a9357133";
const LINES_DIGEST: &str = "4fb13b9ff432de832e374ea1cfcce7e13478ebabfc6e6c67d213c445100d979f";
const LINE_6_DIGEST: &str = "f6af8978aaf70c86256566cabd67f8539050d49b973b1ce30d49fd401af9e39b";
const HELPER_DIGEST: &str = "84f019232d2cb36dff20109787af4cab93f28f444cd66ec5556b06c0e56c2ffb";
const GREETING_DIGEST: &str = "ba3596f3b8ba08caf739a2e2ad430d34635d72ad5f5156b85602fdd87a8d3fe5";
const UTILS_DIGEST: &str = "c50fdd26497fbd62bbeac6d45afbdd2d6595e199569b03b1e79bcf81a8e5816c";
const LATE_DIGEST: &str = "4b1076400731abd215698ae660171248eb227b7e88bcd15246d38161ae0b9333";
const LATE_MAP_DIGEST: &str = "31095ff39d5dbfe1e6f2305660bd1ce1d361e24df5611828bafb78bdd7a68d89";
const LATE_SIGNATURES_DIGEST: &str =
    "144ce3069ee1c09138d7bcb61e822179556c2edb53007206240bf91e61e6233d";
const RENAMED_DIGEST: &str = "a5b7e2cd21e13410849a555fca6b3468e3046ef1d79df14f22e0daf0bbbb5a6c";

fn utils_symbol() -> Value {
    json!({
        "kind": "module",
        "name": "utils",
        "qualified_name": "src/main.rs::utils",
        "visibility": "private",
        "line": 3,
        "end_line": 3,
        "signature": "mod utils"
    })
}

fn main_symbol() -> Value {
    json!({
        "kind": "function",
        "name": "main",
        "qualified_name": "src/main.rs::main",
        "visibility": "private",
        "line": 5,
        "end_line": 8,
        "signature": "fn main()"
    })
}

/// Symbol context is nearest-first, so `utils` (line 3) precedes `main`.
fn main_context_symbols() -> Value {
    json!([utils_symbol(), main_symbol()])
}

/// Map and signatures publish the projection page. That walk follows graph
/// entity order, which is not stable across processes, so the caller-visible
/// contract asserted here is the symbol records, not their sequence.
fn main_page_symbols() -> Value {
    json!([main_symbol(), utils_symbol()])
}

fn main_function_symbol() -> Value {
    json!([main_symbol()])
}

fn helper_symbol() -> Value {
    json!([{
        "kind": "function",
        "name": "helper",
        "qualified_name": "src/utils.rs::helper",
        "visibility": "public",
        "line": 3,
        "end_line": 5,
        "signature": "pub fn helper() -> String"
    }])
}

async fn call_read(fixture: &ProductionCompositionFixture, arguments: Value) -> JsonRpcResponse {
    fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_read", arguments)
        .await
        .expect("tracedecay_read production MCP call")
}

fn read_text(response: &JsonRpcResponse) -> &str {
    assert!(
        response.error.is_none(),
        "tracedecay_read failed: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    extract_text(response.result.as_ref().expect("tracedecay_read result"))
}

fn read_json(response: &JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "tracedecay_read failed: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    extract_json(response.result.as_ref().expect("tracedecay_read result"))
}

/// Nanoseconds since the epoch, the freshness value a caller can recompute
/// from the file without asking the tool.
fn file_mtime_ns(path: &Path) -> i64 {
    let modified = fs::metadata(path)
        .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()))
        .modified()
        .expect("file mtime");
    let elapsed = modified
        .duration_since(UNIX_EPOCH)
        .expect("mtime before epoch");
    let nanos = i128::from(elapsed.as_secs()) * 1_000_000_000 + i128::from(elapsed.subsec_nanos());
    i64::try_from(nanos).expect("mtime fits i64")
}

fn drop_mtime(payload: &mut Value) -> i64 {
    let mtime = payload["mtime_ns"]
        .as_i64()
        .unwrap_or_else(|| panic!("mtime_ns missing from {payload}"));
    payload
        .as_object_mut()
        .expect("read payload")
        .remove("mtime_ns");
    mtime
}

fn symbol_facts(symbols: &Value) -> Vec<Value> {
    symbols
        .as_array()
        .unwrap_or_else(|| panic!("symbols array: {symbols}"))
        .iter()
        .map(|symbol| {
            json!({
                "kind": symbol["kind"],
                "name": symbol["name"],
                "qualified_name": symbol["qualified_name"],
                "visibility": symbol["visibility"],
                "line": symbol["line"],
                "end_line": symbol["end_line"],
                "signature": symbol["signature"],
            })
        })
        .collect()
}

fn assert_symbols(actual: &Value, expected: Value) {
    assert_eq!(
        symbol_facts(actual),
        expected.as_array().expect("expected symbols").clone(),
        "published symbols: {actual}"
    );
}

fn symbol_key(symbol: &Value) -> String {
    symbol["qualified_name"].as_str().unwrap_or("").to_owned()
}

fn assert_symbol_records(actual: &Value, expected: Value) {
    let mut actual_symbols = symbol_facts(actual);
    let mut expected_symbols = expected.as_array().expect("expected symbols").clone();
    actual_symbols.sort_by_key(symbol_key);
    expected_symbols.sort_by_key(symbol_key);
    assert_eq!(
        actual_symbols, expected_symbols,
        "published symbol records: {actual}"
    );
}

fn assert_source_payload(payload: &mut Value, file: &Path, expected: Value) {
    assert_eq!(drop_mtime(payload), file_mtime_ns(file), "{payload}");
    assert_eq!(payload, &expected, "{payload}");
}

#[tokio::test]
async fn read_serves_source_symbols_and_a_cache_stub() {
    let fixture = production_composition_fixture().await;
    let main_path = fixture.project_root.join("src/main.rs");
    let utils_path = fixture.project_root.join("src/utils.rs");

    let full = call_read(&fixture, json!({"file": "src/main.rs"})).await;
    assert_eq!(read_text(&full), main_full_markdown());

    let full_json = call_read(&fixture, json!({"file": "src/main.rs", "format": "json"})).await;
    let mut full_json = read_json(&full_json);
    let original_mtime = drop_mtime(&mut full_json);
    assert_eq!(original_mtime, file_mtime_ns(&main_path));
    assert_eq!(
        full_json,
        json!({
            "file": "src/main.rs",
            "mode": "full",
            "digest": MAIN_DIGEST,
            "token_count": 27,
            "unchanged": true
        })
    );

    let with_symbols = call_read(
        &fixture,
        json!({
            "file": "src/main.rs",
            "format": "json",
            "include_symbols": true
        }),
    )
    .await;
    let mut with_symbols = read_json(&with_symbols);
    assert_eq!(drop_mtime(&mut with_symbols), original_mtime);
    assert_eq!(with_symbols["unchanged"], true);
    assert!(with_symbols.get("body").is_none(), "{with_symbols}");
    assert_eq!(with_symbols["context"]["symbol_count"], 2);
    assert_eq!(with_symbols["context"]["range"], Value::Null);
    assert_eq!(with_symbols["context"]["truncated"], false);
    assert_symbols(&with_symbols["context"]["symbols"], main_context_symbols());

    let lines = call_read(
        &fixture,
        json!({"file": "src/main.rs", "mode": "lines", "lines": "5-7"}),
    )
    .await;
    assert_eq!(read_text(&lines), main_lines_markdown());

    let lines_json = call_read(
        &fixture,
        json!({
            "file": "src/main.rs",
            "mode": "lines",
            "lines": "5-7",
            "format": "json"
        }),
    )
    .await;
    let mut lines_json = read_json(&lines_json);
    assert_eq!(drop_mtime(&mut lines_json), file_mtime_ns(&main_path));
    assert_eq!(lines_json["file"], "src/main.rs");
    assert_eq!(lines_json["mode"], "lines");
    assert_eq!(lines_json["digest"], LINES_DIGEST);
    assert_eq!(lines_json["token_count"], 17);
    assert_eq!(lines_json["unchanged"], true);
    assert!(lines_json.get("body").is_none(), "{lines_json}");
    assert_eq!(
        lines_json["context"]["range"],
        json!({"start": 5, "end": 7})
    );
    assert_eq!(lines_json["context"]["symbol_count"], 1);
    assert_eq!(lines_json["context"]["truncated"], false);
    assert_symbols(&lines_json["context"]["symbols"], main_function_symbol());
    let lines_again = call_read(
        &fixture,
        json!({"file": "src/main.rs", "mode": "lines", "lines": "5-7"}),
    )
    .await;
    assert_eq!(read_text(&lines_again), MAIN_LINES_UNCHANGED_MARKDOWN);

    let line_six = call_read(
        &fixture,
        json!({
            "file": "src/main.rs",
            "mode": "lines",
            "lines": "6",
            "format": "json"
        }),
    )
    .await;
    let mut line_six = read_json(&line_six);
    assert_eq!(drop_mtime(&mut line_six), file_mtime_ns(&main_path));
    assert_eq!(line_six["mode"], "lines");
    assert_eq!(line_six["body"], MAIN_LINE_6);
    assert_eq!(line_six["digest"], LINE_6_DIGEST);
    assert_eq!(line_six["token_count"], 7);
    assert!(line_six.get("unchanged").is_none(), "{line_six}");
    assert_eq!(line_six["context"]["range"], json!({"start": 6, "end": 6}));
    assert_symbols(&line_six["context"]["symbols"], main_function_symbol());

    let helper = call_read(
        &fixture,
        json!({
            "file": "src/utils.rs",
            "mode": "lines",
            "lines": "3-5",
            "format": "json"
        }),
    )
    .await;
    let mut helper = read_json(&helper);
    assert_eq!(drop_mtime(&mut helper), file_mtime_ns(&utils_path));
    assert_eq!(helper["file"], "src/utils.rs");
    assert_eq!(helper["body"], HELPER_LINES);
    assert_eq!(helper["digest"], HELPER_DIGEST);
    assert_eq!(helper["token_count"], 15);
    assert_eq!(helper["context"]["range"], json!({"start": 3, "end": 5}));
    assert_eq!(helper["context"]["symbol_count"], 1);
    assert_symbols(&helper["context"]["symbols"], helper_symbol());

    let greeting = call_read(
        &fixture,
        json!({
            "file": "src/utils.rs",
            "mode": "lines",
            "lines": "7-9",
            "include_symbols": false,
            "format": "json"
        }),
    )
    .await;
    let mut greeting = read_json(&greeting);
    drop_mtime(&mut greeting);
    assert_eq!(
        greeting,
        json!({
            "file": "src/utils.rs",
            "mode": "lines",
            "digest": GREETING_DIGEST,
            "token_count": 19,
            "body": GREETING_LINES
        })
    );

    let absolute = utils_path.to_string_lossy().into_owned();
    let absolute_read = call_read(&fixture, json!({"file": absolute, "format": "json"})).await;
    let mut absolute_read = read_json(&absolute_read);
    assert_eq!(drop_mtime(&mut absolute_read), file_mtime_ns(&utils_path));
    assert_eq!(
        absolute_read,
        json!({
            "file": "src/utils.rs",
            "mode": "full",
            "digest": UTILS_DIGEST,
            "token_count": 43,
            "body": UTILS_RS
        })
    );

    let map = call_read(
        &fixture,
        json!({"file": "src/main.rs", "mode": "map", "format": "json"}),
    )
    .await;
    let map = read_json(&map);
    assert_eq!(map["file"], "src/main.rs");
    assert_eq!(map["mode"], "map");
    assert!(map.get("context").is_none(), "{map}");
    assert!(map.get("unchanged").is_none(), "{map}");
    let map_body = map["body"].as_str().expect("map body");
    assert!(
        !map_body.contains("println!"),
        "map mode returned source bytes: {map_body}"
    );
    let map_body: Value = serde_json::from_str(map_body).expect("map body json");
    assert_eq!(map_body["file"], "src/main.rs");
    assert_eq!(map_body["symbol_count"], 2);
    assert_symbol_records(&map_body["symbols"], main_page_symbols());

    let signatures = call_read(
        &fixture,
        json!({"file": "src/main.rs", "mode": "signatures", "format": "json"}),
    )
    .await;
    let signatures = read_json(&signatures);
    assert_eq!(signatures["mode"], "signatures");
    let signatures_body: Value =
        serde_json::from_str(signatures["body"].as_str().expect("signatures body"))
            .expect("signatures body json");
    assert_eq!(signatures_body["file"], "src/main.rs");
    assert_eq!(signatures_body["symbol_count"], 2);
    assert_eq!(signatures_body["without_signature"], 0);
    assert_symbol_records(&signatures_body["symbols"], main_page_symbols());

    let late_path = fixture.project_root.join("src/late.txt");
    fs::write(&late_path, LATE_RS).expect("write late file");
    let late = call_read(&fixture, json!({"file": "src/late.txt", "format": "json"})).await;
    let mut late = read_json(&late);
    assert_source_payload(
        &mut late,
        &late_path,
        json!({
            "file": "src/late.txt",
            "mode": "full",
            "digest": LATE_DIGEST,
            "token_count": 5,
            "body": LATE_RS
        }),
    );
    let late_map = call_read(
        &fixture,
        json!({"file": "src/late.txt", "mode": "map", "format": "json"}),
    )
    .await;
    let mut late_map = read_json(&late_map);
    assert_source_payload(
        &mut late_map,
        &late_path,
        json!({
            "file": "src/late.txt",
            "mode": "map",
            "digest": LATE_MAP_DIGEST,
            "token_count": 17,
            "body": LATE_MAP_BODY
        }),
    );
    let late_signatures = call_read(
        &fixture,
        json!({"file": "src/late.txt", "mode": "signatures", "format": "json"}),
    )
    .await;
    let mut late_signatures = read_json(&late_signatures);
    assert_source_payload(
        &mut late_signatures,
        &late_path,
        json!({
            "file": "src/late.txt",
            "mode": "signatures",
            "digest": LATE_SIGNATURES_DIGEST,
            "token_count": 23,
            "body": LATE_SIGNATURES_BODY
        }),
    );

    let past_eof = call_read(
        &fixture,
        json!({
            "file": "src/main.rs",
            "mode": "lines",
            "lines": "100-101",
            "format": "json"
        }),
    )
    .await;
    let mut past_eof = read_json(&past_eof);
    drop_mtime(&mut past_eof);
    assert_eq!(
        past_eof,
        json!({
            "file": "src/main.rs",
            "mode": "lines",
            "digest": EMPTY_DIGEST,
            "token_count": 0,
            "body": "",
            "context": {
                "file": "src/main.rs",
                "range": {"start": 100, "end": 101},
                "symbol_count": 0,
                "truncated": false,
                "symbols": []
            }
        })
    );

    fs::write(&main_path, RENAMED_RS).expect("rewrite main");
    fs::File::options()
        .write(true)
        .open(&main_path)
        .expect("open rewritten main")
        .set_modified(SystemTime::now() + Duration::from_secs(5))
        .expect("bump main mtime");
    let rewritten = call_read(&fixture, json!({"file": "src/main.rs", "format": "json"})).await;
    let mut rewritten = read_json(&rewritten);
    let rewritten_mtime = drop_mtime(&mut rewritten);
    assert_ne!(rewritten_mtime, original_mtime);
    assert_eq!(rewritten_mtime, file_mtime_ns(&main_path));
    assert_eq!(
        rewritten,
        json!({
            "file": "src/main.rs",
            "mode": "full",
            "digest": RENAMED_DIGEST,
            "token_count": 10,
            "body": RENAMED_RS
        })
    );
    let rewritten_stub = call_read(&fixture, json!({"file": "src/main.rs"})).await;
    assert_eq!(read_text(&rewritten_stub), RENAMED_UNCHANGED_MARKDOWN);

    fixture.harness.shutdown().await;
}

async fn assert_read_error(
    fixture: &ProductionCompositionFixture,
    arguments: Value,
    code: i32,
    message: &str,
) {
    let response = call_read(fixture, arguments.clone()).await;
    let error = response
        .error
        .unwrap_or_else(|| panic!("expected tracedecay_read error for {arguments}"));
    assert_eq!(
        (error.code, error.message.as_str()),
        (code, message),
        "{arguments} -> {error:?}"
    );
    assert!(response.result.is_none(), "{arguments}");
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data.get("tool"))
            .and_then(Value::as_str),
        Some("tracedecay_read"),
        "{arguments}"
    );
}

#[tokio::test]
async fn read_rejects_bad_input_with_the_caller_visible_error() {
    let fixture = production_composition_fixture().await;
    let root = fixture.project_root.display().to_string();

    assert_read_error(
        &fixture,
        json!({}),
        -32602,
        "missing required parameter: file",
    )
    .await;
    let missing = call_read(&fixture, json!({})).await;
    assert_eq!(
        missing.error.expect("missing file error").data,
        Some(json!({
            "tool": "tracedecay_read",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: file"
        }))
    );

    let invalid_params = -32602;
    let execution_failed = -32603;
    assert_read_error(
        &fixture,
        json!({"mode": "lines", "lines": "1-2"}),
        invalid_params,
        "missing required parameter: file",
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": "src/main.rs", "mode": "sideways"}),
        execution_failed,
        "tool execution failed: config error: unknown mode 'sideways'; expected one of full, lines, map, signatures",
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": "src/main.rs", "mode": "FULL"}),
        execution_failed,
        "tool execution failed: config error: unknown mode 'FULL'; expected one of full, lines, map, signatures",
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": "src/main.rs", "mode": "lines"}),
        execution_failed,
        "tool execution failed: config error: mode='lines' requires the 'lines' argument (e.g. '120-180')",
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": "src/main.rs", "mode": "lines", "lines": "0"}),
        execution_failed,
        "tool execution failed: config error: invalid 'lines' value '0'; expected 'A' or 'A-B'",
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": "src/main.rs", "mode": "lines", "lines": "7-5"}),
        execution_failed,
        "tool execution failed: config error: invalid 'lines' value '7-5'; expected 'A' or 'A-B'",
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": "src/main.rs", "mode": "lines", "lines": "abc"}),
        execution_failed,
        "tool execution failed: config error: invalid 'lines' value 'abc'; expected 'A' or 'A-B'",
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": ""}),
        execution_failed,
        "tool execution failed: config error: path must name a project file",
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": "../outside.rs"}),
        execution_failed,
        "tool execution failed: config error: path '../outside.rs' contains unsafe components",
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": "a\0b"}),
        execution_failed,
        "tool execution failed: config error: path contains NUL byte",
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": "src/absent.rs"}),
        execution_failed,
        &format!(
            "tool execution failed: config error: path 'src/absent.rs' escapes project root '{root}' and is not indexed"
        ),
    )
    .await;
    assert_read_error(
        &fixture,
        json!({"file": "/etc/passwd"}),
        execution_failed,
        &format!(
            "tool execution failed: config error: path '/etc/passwd' escapes project root '{root}'"
        ),
    )
    .await;

    fixture.harness.shutdown().await;
}

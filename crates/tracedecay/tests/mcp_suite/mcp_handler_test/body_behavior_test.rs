//! Literal `tracedecay_body` behavior through a production MCP `tools/call`.
//!
//! These calls omit the suite helper that rewrites a missing `format` to JSON,
//! so an omitted `format` is the markdown agents receive by default.

use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::fixture::write_indexed_fixture_sources;
use crate::support::{
    CaptureTransport, ProductionCompositionFixture, production_composition_fixture_with_sources,
    warm_code_index_search,
};

const FORMAT_GREETING_BODY: &str = "\
fn format_greeting(name: &str) -> String {
    format!(\"Hello, {}!\", name)
}";

const HELPER_BODY: &str = "\
pub fn helper() -> String {
    format_greeting(\"world\")
}";

const GMRES_FUNCTION_BODY: &str = "\
pub fn gmres(x: u32) -> u32 {
    x + 1
}";

const GMRES_FIELD_BODY: &str = "    pub gmres: u32,";

const FORMAT_GREETING_MARKDOWN: &str = "\
## Body matches (1)

### format_greeting (function)
**location:** src/utils.rs:7-9
**signature:** fn format_greeting(name: &str) -> String
**tokens:** 19

```rs
fn format_greeting(name: &str) -> String {
    format!(\"Hello, {}!\", name)
}
```
";

async fn open_indexed(
    write_sources: impl FnOnce(&std::path::Path),
    warm_query: &str,
) -> (ProductionCompositionFixture, Arc<McpServer>) {
    let fixture = production_composition_fixture_with_sources(write_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    warm_code_index_search(&server, warm_query).await;
    (fixture, server)
}

async fn call_body(server: &McpServer, arguments: Value) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_body",
            "arguments": arguments,
        }
    });
    let mut transport = CaptureTransport {
        incoming: Some(request.to_string()),
        output: String::new(),
    };
    Box::pin(server.run_connection(&mut transport))
        .await
        .expect("real MCP server tool call");
    serde_json::from_str(transport.output.trim()).expect("JSON-RPC response")
}

fn assert_success_texts(response: &Value, texts: &[&str]) {
    assert!(
        response.get("error").is_none_or(Value::is_null),
        "tools/call failed: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("tool result content: {response}"));
    let actual = content
        .iter()
        .map(|item| {
            assert_eq!(item["type"], "text", "{item}");
            item["text"]
                .as_str()
                .unwrap_or_else(|| panic!("text block: {item}"))
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, texts, "{response}");
}

/// Occurrence ids mix the fixture file identity into the hash, so the hex
/// changes with the temp project root. The wire shape does not.
fn assert_symbol_occurrence_id(id: &str) {
    let Some(hex) = id.strip_prefix("symbol.v1.sha256:") else {
        panic!("occurrence id {id}");
    };
    assert_eq!(hex.len(), 64, "{id}");
    assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit()), "{id}");
}

fn assert_json_payload(response: &Value, expected: Value) -> Vec<String> {
    assert!(
        response.get("error").is_none_or(Value::is_null),
        "tools/call failed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("json text: {response}"));
    let mut payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("body JSON was not the tool payload ({error}): {text}"));
    let Some(matches) = payload.get_mut("matches").and_then(Value::as_array_mut) else {
        panic!("body JSON has no matches array: {text}");
    };
    let mut ids = Vec::with_capacity(matches.len());
    for item in matches.iter_mut() {
        let Some(object) = item.as_object_mut() else {
            panic!("match was not an object: {text}");
        };
        let Some(id) = object
            .remove("id")
            .and_then(|id| id.as_str().map(str::to_owned))
        else {
            panic!("match has no occurrence id: {text}");
        };
        assert_symbol_occurrence_id(&id);
        ids.push(id);
    }
    assert_eq!(payload, expected, "{text}");
    ids
}

fn assert_invalid_params(response: &Value, message: &str, data: Value) {
    assert!(
        response.get("result").is_none_or(Value::is_null),
        "expected a JSON-RPC error, got {response}"
    );
    assert_eq!(response["error"]["code"], -32602, "{response}");
    assert_eq!(response["error"]["message"], message, "{response}");
    assert_eq!(response["error"]["data"], data, "{response}");
}

#[tokio::test]
async fn indexed_symbol_body_is_the_source_span() {
    let (fixture, server) = open_indexed(write_indexed_fixture_sources, "helper").await;

    let json_body = call_body(
        &server,
        json!({"symbol": "format_greeting", "format": "json"}),
    )
    .await;
    assert_json_payload(
        &json_body,
        json!({
            "match_count": 1,
            "matches": [{
                "name": "format_greeting",
                "qualified_name": "src/utils.rs::format_greeting",
                "kind": "function",
                "file": "src/utils.rs",
                "start_line": 7,
                "end_line": 9,
                "signature": "fn format_greeting(name: &str) -> String",
                "body": FORMAT_GREETING_BODY,
            }]
        }),
    );

    let qualified = call_body(
        &server,
        json!({"symbol": "src/utils.rs::helper", "format": "json"}),
    )
    .await;
    assert_json_payload(
        &qualified,
        json!({
            "match_count": 1,
            "matches": [{
                "name": "helper",
                "qualified_name": "src/utils.rs::helper",
                "kind": "function",
                "file": "src/utils.rs",
                "start_line": 3,
                "end_line": 5,
                "signature": "pub fn helper() -> String",
                "body": HELPER_BODY,
            }]
        }),
    );

    let markdown = call_body(&server, json!({"symbol": "format_greeting"})).await;
    assert_success_texts(
        &markdown,
        &[
            FORMAT_GREETING_MARKDOWN,
            "\ntracedecay_metrics: before=42 after=60",
        ],
    );

    let missing = call_body(&server, json!({"symbol": "no_such_symbol_anywhere"})).await;
    assert_success_texts(
        &missing,
        &["No symbol named 'no_such_symbol_anywhere' found."],
    );

    let omitted = call_body(&server, json!({})).await;
    assert_invalid_params(
        &omitted,
        "missing required parameter: symbol",
        json!({
            "tool": "tracedecay_body",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: symbol",
        }),
    );

    let lazy = call_body(
        &server,
        json!({
            "symbol": "format_greeting",
            "lazy_index_ignored_dependencies": true,
        }),
    )
    .await;
    assert_invalid_params(
        &lazy,
        "tool project route failed: reason_code=verified-body-lazy-indexing-unavailable retryable=false: lazy dependency indexing cannot mutate the generation pinned for this body request",
        json!({
            "tool": "tracedecay_body",
            "reason_code": "verified-body-lazy-indexing-unavailable",
            "retryable": false,
            "detail": "lazy dependency indexing cannot mutate the generation pinned for this body request",
        }),
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn same_named_function_is_returned_ahead_of_the_field() {
    let (fixture, server) = open_indexed(
        |project| {
            fs::create_dir_all(project.join("src")).unwrap();
            fs::write(
                project.join("src/lib.rs"),
                "pub struct Solvers {\n    pub gmres: u32,\n}\n\npub fn gmres(x: u32) -> u32 {\n    x + 1\n}\n",
            )
            .unwrap();
        },
        "gmres",
    )
    .await;

    let both = call_body(&server, json!({"symbol": "gmres", "format": "json"})).await;
    let both_ids = assert_json_payload(
        &both,
        json!({
            "match_count": 2,
            "matches": [
                {
                    "name": "gmres",
                    "qualified_name": "src/lib.rs::gmres",
                    "kind": "function",
                    "file": "src/lib.rs",
                    "start_line": 5,
                    "end_line": 7,
                    "signature": "pub fn gmres(x: u32) -> u32",
                    "body": GMRES_FUNCTION_BODY,
                },
                {
                    "name": "gmres",
                    "qualified_name": "src/lib.rs::Solvers::gmres",
                    "kind": "field",
                    "file": "src/lib.rs",
                    "start_line": 2,
                    "end_line": 2,
                    "signature": "pub gmres: u32",
                    "body": GMRES_FIELD_BODY,
                }
            ]
        }),
    );

    let limited = call_body(
        &server,
        json!({"symbol": "gmres", "limit": 1, "format": "json"}),
    )
    .await;
    let limited_ids = assert_json_payload(
        &limited,
        json!({
            "match_count": 1,
            "matches": [{
                "name": "gmres",
                "qualified_name": "src/lib.rs::gmres",
                "kind": "function",
                "file": "src/lib.rs",
                "start_line": 5,
                "end_line": 7,
                "signature": "pub fn gmres(x: u32) -> u32",
                "body": GMRES_FUNCTION_BODY,
            }]
        }),
    );

    let clamped = call_body(
        &server,
        json!({"symbol": "gmres", "limit": 0, "format": "json"}),
    )
    .await;
    let clamped_ids = assert_json_payload(
        &clamped,
        json!({
            "match_count": 1,
            "matches": [{
                "name": "gmres",
                "qualified_name": "src/lib.rs::gmres",
                "kind": "function",
                "file": "src/lib.rs",
                "start_line": 5,
                "end_line": 7,
                "signature": "pub fn gmres(x: u32) -> u32",
                "body": GMRES_FUNCTION_BODY,
            }]
        }),
    );
    assert_ne!(
        both_ids[0], both_ids[1],
        "function and field occurrences must stay distinct"
    );
    assert_eq!(limited_ids.as_slice(), &both_ids[..1]);
    assert_eq!(clamped_ids.as_slice(), &both_ids[..1]);

    fixture.harness.shutdown().await;
}

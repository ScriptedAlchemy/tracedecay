//! Literal `tracedecay_outline` behavior through a production MCP `tools/call`.
//!
//! These calls omit the suite helper that rewrites a missing `format` to JSON,
//! so an omitted `format` is the markdown agents receive by default.
//!
//! Occurrence ids hash the temp repository, so page order follows them and
//! neither is pinned. The comparison sorts symbols by line and name and
//! checks that each id is a distinct `symbol.v1.sha256` digest.

use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::fixture::write_indexed_fixture_sources;
use crate::support::{
    CaptureTransport, ProductionCompositionFixture, production_composition_fixture_with_sources,
    warm_code_index_search,
};

/// The byte layout these spans name. A leading newline is the shared fixture.
const UTILS_MARKDOWN: &str = "\
## Outline, src/utils.rs
**symbols:** 2

- **helper** (function) - lines 3-5 - public
  `pub fn helper() -> String`
- **format_greeting** (function) - lines 7-9 - private
  `fn format_greeting(name: &str) -> String`
";

const EMPTY_MARKDOWN: &str = "\
## Outline, src/empty.rs
**symbols:** 0

_No symbols._
";

const WIDGET_SOURCE: &str = "\
pub struct Widget {
    label: String,
}

pub fn build() -> Widget {
    Widget { label: String::new() }
}
";

async fn open_indexed(
    write_sources: impl FnOnce(&Path),
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

async fn call_outline(server: &McpServer, arguments: Value) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_outline",
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

fn content_texts(response: &Value) -> Vec<&str> {
    assert!(
        response.get("error").is_none_or(Value::is_null),
        "tools/call failed: {response}"
    );
    response["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("tool result content: {response}"))
        .iter()
        .map(|item| {
            assert_eq!(item["type"], "text", "{item}");
            item["text"]
                .as_str()
                .unwrap_or_else(|| panic!("text block: {item}"))
        })
        .collect()
}

/// ast-grep echoes the absolute path of the temp project. Rewrite that one
/// field to `PROJECT/...` so the rest of the payload can be a literal.
fn normalize_ast_grep_paths(payload: &mut Value, project_root: &Path) {
    let Some(files) = payload
        .get_mut("ast_grep_outline")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let mut prefixes = vec![trim_path_slash(project_root.to_string_lossy().as_ref())];
    if let Ok(canonical) = project_root.canonicalize() {
        let canonical = trim_path_slash(&canonical.to_string_lossy());
        if !prefixes.iter().any(|prefix| prefix == &canonical) {
            prefixes.push(canonical);
        }
    }
    for file in files {
        let path = file
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("ast-grep file has no path: {file}"))
            .to_owned();
        let relative = prefixes.iter().find_map(|prefix| {
            let rest = path.strip_prefix(prefix.as_str())?;
            Some(rest.trim_start_matches(['/', '\\']).replace('\\', "/"))
        });
        let Some(relative) = relative else {
            panic!(
                "ast-grep path {path} is not inside {}",
                project_root.display()
            );
        };
        let object = file
            .as_object_mut()
            .unwrap_or_else(|| panic!("ast-grep file was not an object"));
        object.insert("path".to_owned(), json!(format!("PROJECT/{relative}")));
    }
}

fn trim_path_slash(path: &str) -> String {
    path.trim_end_matches(['/', '\\']).to_owned()
}

fn assert_json_payload(response: &Value, project_root: &Path, mut expected: Value) {
    let text = content_texts(response)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("missing outline text: {response}"));
    let mut payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("outline JSON was not the tool payload ({error}): {text}"));
    normalize_ast_grep_paths(&mut payload, project_root);
    accept_occurrence_ids(&mut payload);
    sort_symbols_by_line(&mut expected);
    assert_eq!(payload, expected, "{text}");
}

/// Ids bind the repository of the temp project, so two runs of the same source
/// mint different digests and a different page order. The span assertion is
/// ordered by line, then name.
fn accept_occurrence_ids(payload: &mut Value) {
    let Some(symbols) = payload.get_mut("symbols").and_then(Value::as_array_mut) else {
        return;
    };
    let mut ids = Vec::new();
    for symbol in symbols.iter_mut() {
        let Some(object) = symbol.as_object_mut() else {
            panic!("symbol was not an object");
        };
        let id = object
            .remove("id")
            .and_then(|id| id.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("symbol has no occurrence id"));
        let Some(digest) = id.strip_prefix("symbol.v1.sha256:") else {
            panic!("occurrence id is not symbol.v1.sha256: {id}");
        };
        assert!(
            digest.len() == 64 && digest.chars().all(|ch| ch.is_ascii_hexdigit()),
            "occurrence id is not symbol.v1.sha256: {id}"
        );
        ids.push(digest.to_owned());
    }
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "duplicate occurrence ids: {ids:?}");
    symbols.sort_by(symbol_line_order);
}

fn sort_symbols_by_line(payload: &mut Value) {
    let Some(symbols) = payload.get_mut("symbols").and_then(Value::as_array_mut) else {
        return;
    };
    symbols.sort_by(symbol_line_order);
}

fn symbol_line_order(left: &Value, right: &Value) -> std::cmp::Ordering {
    let line = |value: &Value| value.get("line").and_then(Value::as_u64).unwrap_or(0);
    let name = |value: &Value| {
        value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    line(left)
        .cmp(&line(right))
        .then_with(|| name(left).cmp(&name(right)))
}

/// Markdown lists symbols in occurrence-id order, which hashes the temp
/// repository. The header is compared literally and the entries as a sorted
/// set, each entry being its bullet line plus the indented signature line.
fn outline_parts(markdown: &str) -> (String, Vec<String>) {
    let mut header = String::new();
    let mut entries: Vec<String> = Vec::new();
    for line in markdown.lines() {
        if line.starts_with("- **") {
            entries.push(line.to_owned());
        } else if let Some(entry) = entries.last_mut().filter(|_| line.starts_with("  ")) {
            entry.push('\n');
            entry.push_str(line);
        } else {
            header.push_str(line);
            header.push('\n');
        }
    }
    entries.sort();
    (header, entries)
}

fn assert_markdown_outline(response: &Value, expected: &str, footer: &[&str]) {
    let texts = content_texts(response);
    let (outline, rest) = texts
        .split_first()
        .unwrap_or_else(|| panic!("missing outline text: {response}"));
    assert_eq!(
        outline_parts(outline),
        outline_parts(expected),
        "{response}"
    );
    assert_eq!(rest, footer, "{response}");
}

fn assert_rpc_error(response: &Value, code: i64, message: &str) {
    assert!(
        response.get("result").is_none_or(Value::is_null),
        "expected a JSON-RPC error, got {response}"
    );
    assert_eq!(response["error"]["code"], code, "{response}");
    assert_eq!(response["error"]["message"], message, "{response}");
    assert_eq!(
        response["error"]["data"]["tool"], "tracedecay_outline",
        "{response}"
    );
}

fn helper_symbol() -> Value {
    json!({
        "kind": "function",
        "name": "helper",
        "qualified_name": "src/utils.rs::helper",
        "visibility": "public",
        "line": 3,
        "end_line": 5,
        "signature": "pub fn helper() -> String",
        "start_byte": 1,
        "end_byte": 92,
    })
}

fn format_greeting_symbol() -> Value {
    json!({
        "kind": "function",
        "name": "format_greeting",
        "qualified_name": "src/utils.rs::format_greeting",
        "visibility": "private",
        "line": 7,
        "end_line": 9,
        "signature": "fn format_greeting(name: &str) -> String",
        "start_byte": 92,
        "end_byte": 169,
    })
}

fn utils_ast_grep() -> Value {
    json!([{
        "path": "PROJECT/src/utils.rs",
        "language": "Rust",
        "items": [
            {
                "role": "item",
                "symbolType": "function",
                "name": "helper",
                "signature": "pub fn helper() -> String",
                "astKind": "function_item",
                "isImport": false,
                "isExported": true,
                "range": {
                    "byteOffset": { "start": 32, "end": 90 },
                    "start": { "line": 2, "column": 0 },
                    "end": { "line": 4, "column": 1 }
                }
            },
            {
                "role": "item",
                "symbolType": "function",
                "name": "format_greeting",
                "signature": "fn format_greeting(name: &str) -> String",
                "astKind": "function_item",
                "isImport": false,
                "isExported": false,
                "range": {
                    "byteOffset": { "start": 92, "end": 168 },
                    "start": { "line": 6, "column": 0 },
                    "end": { "line": 8, "column": 1 }
                }
            }
        ]
    }])
}

fn utils_outline(symbols: Vec<Value>) -> Value {
    json!({
        "file": "src/utils.rs",
        "symbol_count": symbols.len(),
        "symbols": symbols,
        "ast_grep_outline": utils_ast_grep(),
    })
}

fn label_symbol() -> Value {
    json!({
        "kind": "field",
        "name": "label",
        "qualified_name": "src/widget.rs::Widget::label",
        "visibility": "private",
        "line": 2,
        "end_line": 2,
        "signature": "label: String",
        "start_byte": 24,
        "end_byte": 37,
    })
}

fn build_symbol() -> Value {
    json!({
        "kind": "function",
        "name": "build",
        "qualified_name": "src/widget.rs::build",
        "visibility": "public",
        "line": 5,
        "end_line": 7,
        "signature": "pub fn build() -> Widget",
        "start_byte": 42,
        "end_byte": 107,
    })
}

fn widget_symbol() -> Value {
    json!({
        "kind": "struct",
        "name": "Widget",
        "qualified_name": "src/widget.rs::Widget",
        "visibility": "public",
        "line": 1,
        "end_line": 3,
        "signature": "pub struct Widget",
        "start_byte": 0,
        "end_byte": 42,
    })
}

fn widget_ast_grep() -> Value {
    json!([{
        "path": "PROJECT/src/widget.rs",
        "language": "Rust",
        "items": [
            {
                "role": "item",
                "symbolType": "struct",
                "name": "Widget",
                "signature": "pub struct Widget",
                "astKind": "struct_item",
                "isImport": false,
                "isExported": true,
                "range": {
                    "byteOffset": { "start": 0, "end": 40 },
                    "start": { "line": 0, "column": 0 },
                    "end": { "line": 2, "column": 1 }
                },
                "members": [{
                    "role": "member",
                    "symbolType": "field",
                    "name": "label",
                    "signature": "label: String",
                    "astKind": "field_declaration",
                    "isPublic": false,
                    "range": {
                        "byteOffset": { "start": 24, "end": 37 },
                        "start": { "line": 1, "column": 4 },
                        "end": { "line": 1, "column": 17 }
                    }
                }]
            },
            {
                "role": "item",
                "symbolType": "function",
                "name": "build",
                "signature": "pub fn build() -> Widget",
                "astKind": "function_item",
                "isImport": false,
                "isExported": true,
                "range": {
                    "byteOffset": { "start": 42, "end": 106 },
                    "start": { "line": 4, "column": 0 },
                    "end": { "line": 6, "column": 1 }
                }
            }
        ]
    }])
}

fn widget_outline(symbols: Vec<Value>) -> Value {
    json!({
        "file": "src/widget.rs",
        "symbol_count": symbols.len(),
        "symbols": symbols,
        "ast_grep_outline": widget_ast_grep(),
    })
}

#[tokio::test]
async fn indexed_file_outline_is_the_symbol_map() {
    let (fixture, server) = open_indexed(
        |project| {
            write_indexed_fixture_sources(project);
            std::fs::write(project.join("src/empty.rs"), "").expect("empty source");
        },
        "helper",
    )
    .await;
    let root = fixture.project_root.clone();
    let both = utils_outline(vec![helper_symbol(), format_greeting_symbol()]);

    let json_outline =
        call_outline(&server, json!({"file": "src/utils.rs", "format": "json"})).await;
    assert_json_payload(&json_outline, &root, both.clone());

    // An absolute path of the same indexed file keeps the project-relative key.
    let absolute = root.join("src/utils.rs");
    let absolute_outline = call_outline(
        &server,
        json!({"file": absolute.to_string_lossy(), "format": "json"}),
    )
    .await;
    assert_json_payload(&absolute_outline, &root, both.clone());

    let markdown = call_outline(&server, json!({"file": "src/utils.rs"})).await;
    assert_markdown_outline(
        &markdown,
        UTILS_MARKDOWN,
        &["\ntracedecay_metrics: before=42 after=54"],
    );

    // `kinds` filters the indexed map and leaves the ast-grep attachment whole.
    // An empty list is not a filter.
    let functions = call_outline(
        &server,
        json!({"file": "src/utils.rs", "kinds": ["function"], "format": "json"}),
    )
    .await;
    assert_json_payload(&functions, &root, both.clone());
    let functions_upper = call_outline(
        &server,
        json!({"file": "src/utils.rs", "kinds": ["FUNCTION"], "format": "json"}),
    )
    .await;
    assert_json_payload(&functions_upper, &root, both.clone());
    let kinds_empty = call_outline(
        &server,
        json!({"file": "src/utils.rs", "kinds": [], "format": "json"}),
    )
    .await;
    assert_json_payload(&kinds_empty, &root, both);
    let structs = call_outline(
        &server,
        json!({"file": "src/utils.rs", "kinds": ["struct"], "format": "json"}),
    )
    .await;
    assert_json_payload(&structs, &root, utils_outline(Vec::new()));

    // A zero-byte file has no symbols and no metrics footer: raw tokens are 0.
    let empty = call_outline(&server, json!({"file": "src/empty.rs", "format": "json"})).await;
    assert_json_payload(
        &empty,
        &root,
        json!({
            "file": "src/empty.rs",
            "symbol_count": 0,
            "symbols": [],
            "ast_grep_outline": [],
        }),
    );
    let empty_markdown = call_outline(&server, json!({"file": "src/empty.rs"})).await;
    assert_eq!(content_texts(&empty_markdown), vec![EMPTY_MARKDOWN]);

    let omitted = call_outline(&server, json!({})).await;
    assert_rpc_error(&omitted, -32602, "missing required parameter: file");
    assert_eq!(
        omitted["error"]["data"],
        json!({
            "tool": "tracedecay_outline",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "detail": "missing required parameter: file",
        })
    );

    let unnormalized = call_outline(&server, json!({"file": "src/../src/utils.rs"})).await;
    assert_rpc_error(
        &unnormalized,
        -32603,
        "tool execution failed: config error: path 'src/../src/utils.rs' contains unsafe components",
    );

    let missing = call_outline(&server, json!({"file": "src/missing.rs"})).await;
    assert_rpc_error(
        &missing,
        -32603,
        &format!(
            "tool execution failed: config error: path 'src/missing.rs' escapes project root '{}' and is not indexed",
            root.display()
        ),
    );

    fixture.harness.shutdown().await;
}

/// `kinds` selects the indexed map and leaves the ast-grep outline whole.
/// `STRUCT` matches `struct`; a function filter does not return the field.
#[tokio::test]
async fn kinds_filter_keeps_the_named_kind() {
    let (fixture, server) = open_indexed(
        |project| {
            std::fs::create_dir_all(project.join("src")).expect("src");
            std::fs::write(project.join("src/widget.rs"), WIDGET_SOURCE).expect("widget");
        },
        "build",
    )
    .await;
    let root = fixture.project_root.clone();

    let all = call_outline(&server, json!({"file": "src/widget.rs", "format": "json"})).await;
    assert_json_payload(
        &all,
        &root,
        widget_outline(vec![label_symbol(), build_symbol(), widget_symbol()]),
    );

    let only_struct = widget_outline(vec![widget_symbol()]);
    let structs = call_outline(
        &server,
        json!({"file": "src/widget.rs", "kinds": ["struct"], "format": "json"}),
    )
    .await;
    assert_json_payload(&structs, &root, only_struct.clone());
    let structs_upper = call_outline(
        &server,
        json!({"file": "src/widget.rs", "kinds": ["STRUCT"], "format": "json"}),
    )
    .await;
    assert_json_payload(&structs_upper, &root, only_struct);

    let functions = call_outline(
        &server,
        json!({"file": "src/widget.rs", "kinds": ["function"], "format": "json"}),
    )
    .await;
    assert_json_payload(&functions, &root, widget_outline(vec![build_symbol()]));

    fixture.harness.shutdown().await;
}

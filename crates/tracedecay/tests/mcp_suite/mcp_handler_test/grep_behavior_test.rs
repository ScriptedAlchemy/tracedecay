#![cfg(feature = "test-transport")]

//! `tracedecay_grep` as an MCP client sees it.
//!
//! Each case sends `tools/call` through the production server and compares the
//! JSON-RPC text with a literal. Occurrence ids embed the fixture generation,
//! so the graph-enrichment id is the exact-symbol id for that source rather
//! than a hash pasted from one run.

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, warm_code_index_search,
};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const FILES_SCANNED: u64 = 6;
const LINES_EXAMINED: u64 = 24;
const GREETING_LINE: &str = "    format!(\"Hello, {}!\", name)";
const NOTE_LINE: &str = "    let _ = \"MixedCaseToken\";";
const GREET_SIGNATURE: &str = "pub fn greet(name: &str) -> String {";
const LIB_RS: &str = concat!(
    "/// Greets by name.\n",
    "pub fn greet(name: &str) -> String {\n",
    "    format!(\"Hello, {}!\", name)\n",
    "}\n",
    "fn note() {\n",
    "    let _ = \"MixedCaseToken\";\n",
    "}\n",
);
const NOTE_TXT: &str = "# Notes\nALPHA_NOTE_TOKEN\n";
const CONTEXT_TXT: &str = "w\nx\ny\nz\nCONTEXT_TARGET\nd\ne\nf\ng\n";
const CLI_FALLBACK: &str = "This tool is also available from the shell: `tracedecay tool grep ...` \
(`tracedecay tool grep --help` for parameters). If MCP calls keep failing or timing out, fall \
back to that CLI instead of querying .tracedecay databases directly.";

fn write_grep_project(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("docs")).unwrap();
    fs::create_dir_all(project.join("secret_dir")).unwrap();
    fs::create_dir_all(project.join("dist")).unwrap();
    fs::write(project.join(".gitignore"), "secret_dir/\n").unwrap();
    fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    fs::write(project.join("docs/note.txt"), NOTE_TXT).unwrap();
    fs::write(project.join("ctx.txt"), CONTEXT_TXT).unwrap();
    fs::write(project.join("many.txt"), "CAP_TOKEN\n".repeat(4)).unwrap();
    fs::write(project.join("tracked.txt"), "VISIBLE_TOKEN\n").unwrap();
    fs::write(
        project.join("secret_dir/hidden.txt"),
        "VISIBLE_TOKEN\nALPHA_NOTE_TOKEN\n",
    )
    .unwrap();
    fs::write(project.join("dist/skip.js"), "ALPHA_NOTE_TOKEN\n").unwrap();
    fs::write(
        project.join("blob.bin"),
        b"ALPHA_NOTE_TOKEN\0ALPHA_NOTE_TOKEN",
    )
    .unwrap();
}

async fn grep(server: &tracedecay::mcp::McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_grep", arguments).await
}

async fn symbol_node_id(server: &tracedecay::mcp::McpServer, name: &str) -> String {
    let response = handle_real_server_tool_call(
        server,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20, "format": "json"}),
    )
    .await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&response))
        .unwrap_or_else(|error| panic!("exact-symbol JSON for {name}: {error}; {response}"));
    payload["matches"]
        .as_array()
        .and_then(|matches| matches.iter().find(|item| item["name"] == name))
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("exact-symbol response did not contain {name}: {payload}"))
        .to_owned()
}

fn source_coverage(eligible: Option<u64>, returned: u64, partial: bool) -> Value {
    let completeness = if partial { "partial" } else { "complete" };
    json!({
        "requested_domains": ["source"],
        "visited": LINES_EXAMINED,
        "eligible": eligible,
        "returned": returned,
        "completeness": completeness,
        "domains": [{"domain": "source", "completeness": completeness}],
    })
}

fn complete_payload(results: Value, enriched: u64) -> Value {
    let returned = results.as_array().expect("results").len() as u64;
    json!({
        "results": results,
        "match_count": returned,
        "files_scanned": FILES_SCANNED,
        "truncated": false,
        "coverage": source_coverage(Some(returned), returned, false),
        "omissions": [],
        "graph_enrichment": {
            "status": "complete",
            "enriched": enriched,
            "returned": returned,
        },
    })
}

fn assert_markdown(response: &Value, text: &str, touched_bytes: Option<u64>) {
    assert!(
        response["error"].is_null(),
        "grep markdown call failed: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("grep content: {response}"));
    assert_eq!(content[0]["type"], "text", "{response}");
    assert_eq!(content[0]["text"], text, "{response}");
    match touched_bytes {
        None => assert_eq!(content.len(), 1, "{response}"),
        Some(bytes) => {
            let footer = format!(
                "\ntracedecay_metrics: before={} after={}",
                bytes / 4,
                text.len() / 4
            );
            assert_eq!(
                content.get(1).and_then(|item| item["text"].as_str()),
                Some(footer.as_str()),
                "{response}"
            );
            assert_eq!(content.len(), 2, "{response}");
        }
    }
}

fn assert_json_payload(response: &Value, expected: Value, touched_bytes: Option<u64>) {
    assert!(
        response["error"].is_null(),
        "grep JSON call failed: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("grep content: {response}"));
    let text = content[0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("grep text: {response}"));
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("grep payload is not JSON: {error}\n{text}"));
    assert_eq!(payload, expected);
    match touched_bytes {
        None => assert_eq!(content.len(), 1, "{response}"),
        Some(bytes) => {
            let footer = format!(
                "\ntracedecay_metrics: before={} after={}",
                bytes / 4,
                text.len() / 4
            );
            assert_eq!(
                content.get(1).and_then(|item| item["text"].as_str()),
                Some(footer.as_str()),
                "{response}"
            );
            assert_eq!(content.len(), 2, "{response}");
        }
    }
}

fn execution_failed(message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {
            "code": -32603,
            "message": message,
            "data": {
                "tool": "tracedecay_grep",
                "cli_fallback": CLI_FALLBACK,
            }
        }
    })
}

fn greeting_markdown(node_id: &str) -> String {
    format!(
        "\
## Grep Results
- src/lib.rs:3
  > {GREETING_LINE}
  _Enclosing symbol: `greet` (`{node_id}`)_

_Use `tracedecay_source_body` with a result's `node_id` to read the verified enclosing symbol._

_1 matches across {FILES_SCANNED} files._
"
    )
}

#[tokio::test]
async fn tracedecay_grep_reports_literal_matches_and_typed_failures() {
    let fixture = production_composition_fixture_with_sources(write_grep_project).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production grep server");
    warm_code_index_search(&server, "greet").await;
    let greet_id = symbol_node_id(&server, "greet").await;
    let note_id = symbol_node_id(&server, "note").await;

    let missing = grep(&server, json!({"format": "json"})).await;
    assert_eq!(
        missing,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32602,
                "message": "missing required parameter: pattern",
                "data": {
                    "tool": "tracedecay_grep",
                    "reason_code": "missing_required_parameter",
                    "retryable": false,
                    "detail": "missing required parameter: pattern",
                }
            }
        })
    );

    let empty = grep(&server, json!({"pattern": "", "format": "json"})).await;
    assert_eq!(
        empty,
        execution_failed("tool execution failed: config error: pattern must not be empty")
    );

    let absent = grep(
        &server,
        json!({"pattern": "zzz_no_such_token_anywhere_zzz", "format": "json"}),
    )
    .await;
    assert_json_payload(&absent, complete_payload(json!([]), 0), None);
    let absent_markdown = grep(
        &server,
        json!({"pattern": "zzz_no_such_token_anywhere_zzz", "format": "markdown"}),
    )
    .await;
    assert_markdown(
        &absent_markdown,
        &format!(
            "\
## Grep Results
_No matching lines._
_Scanned {FILES_SCANNED} files._
"
        ),
        None,
    );

    let lib_bytes = u64::try_from(LIB_RS.len()).expect("lib.rs length");
    let greeting_json = grep(
        &server,
        json!({
            "pattern": "Hello, {}!",
            "fixed_strings": true,
            "format": "json"
        }),
    )
    .await;
    assert_json_payload(
        &greeting_json,
        complete_payload(
            json!([{
                "file": "src/lib.rs",
                "line": 3,
                "text": GREETING_LINE,
                "symbol": "greet",
                "node_id": greet_id.clone(),
            }]),
            1,
        ),
        Some(lib_bytes),
    );
    let greeting_markdown_response = grep(
        &server,
        json!({
            "pattern": "Hello, {}!",
            "fixed_strings": true,
            "format": "markdown"
        }),
    )
    .await;
    assert_markdown(
        &greeting_markdown_response,
        &greeting_markdown(&greet_id),
        Some(lib_bytes),
    );

    let signature = grep(
        &server,
        json!({"pattern": "pub fn greet", "format": "json"}),
    )
    .await;
    assert_json_payload(
        &signature,
        complete_payload(
            json!([{
                "file": "src/lib.rs",
                "line": 2,
                "text": GREET_SIGNATURE,
                "symbol": "greet",
                "node_id": greet_id.clone(),
            }]),
            1,
        ),
        Some(u64::try_from(LIB_RS.len()).expect("lib.rs length")),
    );

    let insensitive = grep(
        &server,
        json!({"pattern": "mixedcasetoken", "format": "json"}),
    )
    .await;
    assert_json_payload(
        &insensitive,
        complete_payload(
            json!([{
                "file": "src/lib.rs",
                "line": 6,
                "text": NOTE_LINE,
                "symbol": "note",
                "node_id": note_id.clone(),
            }]),
            1,
        ),
        Some(u64::try_from(LIB_RS.len()).expect("lib.rs length")),
    );
    let sensitive = grep(
        &server,
        json!({"pattern": "mixedcasetoken", "case_sensitive": true, "format": "json"}),
    )
    .await;
    assert_json_payload(&sensitive, complete_payload(json!([]), 0), None);

    let narrow_context = grep(
        &server,
        json!({"pattern": "CONTEXT_TARGET", "context_lines": 1, "format": "json"}),
    )
    .await;
    assert_json_payload(
        &narrow_context,
        complete_payload(
            json!([{
                "file": "ctx.txt",
                "line": 5,
                "text": "CONTEXT_TARGET",
                "before": ["z"],
                "after": ["d"],
            }]),
            0,
        ),
        Some(u64::try_from(CONTEXT_TXT.len()).expect("ctx.txt length")),
    );
    let capped_context = grep(
        &server,
        json!({"pattern": "CONTEXT_TARGET", "context_lines": 99, "format": "json"}),
    )
    .await;
    assert_json_payload(
        &capped_context,
        complete_payload(
            json!([{
                "file": "ctx.txt",
                "line": 5,
                "text": "CONTEXT_TARGET",
                "before": ["x", "y", "z"],
                "after": ["d", "e", "f"],
            }]),
            0,
        ),
        Some(u64::try_from(CONTEXT_TXT.len()).expect("ctx.txt length")),
    );

    let capped = grep(
        &server,
        json!({"pattern": "CAP_TOKEN", "max_results": 3, "format": "json"}),
    )
    .await;
    let capped_payload = json_text(&capped);
    assert_eq!(
        capped_payload["results"],
        json!([
            {"file": "many.txt", "line": 1, "text": "CAP_TOKEN"},
            {"file": "many.txt", "line": 2, "text": "CAP_TOKEN"},
            {"file": "many.txt", "line": 3, "text": "CAP_TOKEN"},
        ])
    );
    assert_eq!(capped_payload["match_count"], 3);
    assert_eq!(capped_payload["truncated"], true);
    assert_eq!(capped_payload["coverage"]["completeness"], "partial");
    assert_eq!(capped_payload["coverage"]["returned"], 3);
    assert_eq!(capped_payload["coverage"]["eligible"], Value::Null);
    assert_eq!(capped_payload["omissions"], json!([]));

    let clamped = grep(
        &server,
        json!({"pattern": "CAP_TOKEN", "max_results": 0, "format": "json"}),
    )
    .await;
    let clamped_payload = json_text(&clamped);
    assert_eq!(
        clamped_payload["results"],
        json!([{"file": "many.txt", "line": 1, "text": "CAP_TOKEN"}])
    );
    assert_eq!(clamped_payload["match_count"], 1);
    assert_eq!(clamped_payload["truncated"], true);

    let visible = grep(
        &server,
        json!({"pattern": "VISIBLE_TOKEN", "format": "json"}),
    )
    .await;
    assert_json_payload(
        &visible,
        complete_payload(
            json!([{
                "file": "tracked.txt",
                "line": 1,
                "text": "VISIBLE_TOKEN",
            }]),
            0,
        ),
        Some("VISIBLE_TOKEN\n".len() as u64),
    );

    let note = grep(
        &server,
        json!({"pattern": "ALPHA_NOTE_TOKEN", "format": "json"}),
    )
    .await;
    assert_json_payload(
        &note,
        complete_payload(
            json!([{
                "file": "docs/note.txt",
                "line": 2,
                "text": "ALPHA_NOTE_TOKEN",
            }]),
            0,
        ),
        Some(u64::try_from(NOTE_TXT.len()).expect("note.txt length")),
    );
    let generated = grep(
        &server,
        json!({
            "pattern": "ALPHA_NOTE_TOKEN",
            "path_glob": "dist/**",
            "format": "json"
        }),
    )
    .await;
    let generated_payload = json_text(&generated);
    assert_eq!(
        generated_payload["results"],
        json!([{
            "file": "dist/skip.js",
            "line": 1,
            "text": "ALPHA_NOTE_TOKEN",
        }])
    );
    assert_eq!(generated_payload["match_count"], 1);
    assert_eq!(generated_payload["files_scanned"], 1);
    assert_eq!(generated_payload["truncated"], false);
    assert_eq!(generated_payload["coverage"]["visited"], 1);
    assert_eq!(generated_payload["coverage"]["eligible"], 1);
    assert_eq!(generated_payload["coverage"]["returned"], 1);
    assert_eq!(generated_payload["coverage"]["completeness"], "complete");
    assert_eq!(generated_payload["graph_enrichment"]["enriched"], 0);
    assert_eq!(generated_payload["omissions"], json!([]));

    let invalid_group = grep(&server, json!({"pattern": "(", "format": "json"})).await;
    assert_eq!(
        invalid_group,
        execution_failed(
            "tool execution failed: config error: invalid regex pattern '(': regex parse error:\n    (\n    ^\nerror: unclosed group"
        )
    );
    let invalid_braces = grep(&server, json!({"pattern": "Hello, {}!", "format": "json"})).await;
    assert_eq!(
        invalid_braces,
        execution_failed(
            "tool execution failed: config error: invalid regex pattern 'Hello, {}!': regex parse error:\n    Hello, {}!\n            ^\nerror: repetition quantifier expects a valid decimal"
        )
    );
    let invalid_glob = grep(
        &server,
        json!({"pattern": "VISIBLE_TOKEN", "path_glob": "[", "format": "json"}),
    )
    .await;
    assert_eq!(
        invalid_glob,
        execution_failed(
            "tool execution failed: config error: invalid path_glob '[': error parsing glob '[': unclosed character class; missing ']'"
        )
    );
}

fn json_text(response: &Value) -> Value {
    assert!(response["error"].is_null(), "{response}");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("grep text: {response}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("grep JSON: {error}\n{text}"))
}

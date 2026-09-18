//! Literal `tracedecay_outline` behavior through a production MCP `tools/call`.
//!
//! Calls omit the suite helper that rewrites a missing `format` to JSON, so an
//! omitted `format` is the markdown agents receive by default. The first run
//! writes the observed payloads so symbol ids and the ast-grep attachment can
//! be pinned as the values the server returned.

use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::fixture::write_indexed_fixture_sources;
use crate::support::{
    CaptureTransport, ProductionCompositionFixture, production_composition_fixture_with_sources,
    warm_code_index_search,
};

async fn open_indexed() -> (ProductionCompositionFixture, Arc<McpServer>) {
    let fixture = production_composition_fixture_with_sources(|project| {
        write_indexed_fixture_sources(project);
        std::fs::write(project.join("src/empty.rs"), "").expect("empty source");
        std::fs::write(
            project.join("src/widget.rs"),
            "pub struct Widget {\n    label: String,\n}\n\npub fn build() -> Widget {\n    Widget { label: String::new() }\n}\n",
        )
        .expect("widget source");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    warm_code_index_search(&server, "helper").await;
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

#[tokio::test]
async fn capture_outline_payloads() {
    let (fixture, server) = open_indexed().await;
    let utils = fixture.project_root.join("src/utils.rs");
    let cases = [
        (
            "utils_json",
            json!({"file": "src/utils.rs", "format": "json"}),
        ),
        ("utils_markdown", json!({"file": "src/utils.rs"})),
        (
            "utils_absolute",
            json!({"file": utils.to_string_lossy(), "format": "json"}),
        ),
        (
            "kinds_function",
            json!({"file": "src/utils.rs", "kinds": ["function"], "format": "json"}),
        ),
        (
            "kinds_function_upper",
            json!({"file": "src/utils.rs", "kinds": ["FUNCTION"], "format": "json"}),
        ),
        (
            "kinds_struct",
            json!({"file": "src/utils.rs", "kinds": ["struct"], "format": "json"}),
        ),
        (
            "kinds_empty",
            json!({"file": "src/utils.rs", "kinds": [], "format": "json"}),
        ),
        ("missing_file_arg", json!({})),
        ("unnormalized", json!({"file": "src/../src/utils.rs"})),
        ("missing_path", json!({"file": "src/missing.rs"})),
        (
            "empty_json",
            json!({"file": "src/empty.rs", "format": "json"}),
        ),
        ("empty_markdown", json!({"file": "src/empty.rs"})),
        (
            "widget_json",
            json!({"file": "src/widget.rs", "format": "json"}),
        ),
        (
            "widget_struct",
            json!({"file": "src/widget.rs", "kinds": ["struct"], "format": "json"}),
        ),
    ];
    let mut dump = Vec::new();
    for (name, arguments) in cases {
        let response = call_outline(&server, arguments).await;
        dump.push(json!({"name": name, "response": response}));
    }
    let path = "/tmp/outline-proof.json";
    std::fs::write(
        path,
        serde_json::to_string_pretty(&json!({
            "project_root": fixture.project_root,
            "utils_len": std::fs::metadata(&utils).expect("utils metadata").len(),
            "cases": dump,
        }))
        .expect("dump json"),
    )
    .expect("write dump");
    fixture.harness.shutdown().await;
    panic!("captured outline payloads at {path}");
}

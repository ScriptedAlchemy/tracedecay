//! Host-visible `tools/call` for the retired unused-import scan.
//!
//! The scan was removed because a graph-only walk reported a clean tree on
//! real code. An empty success would look the same. This test calls the MCP
//! name a host still sends and asserts the refusal the server returns.

use std::fs;

use serde_json::json;
use tempfile::TempDir;
use tracedecay::mcp::McpServer;

use crate::mcp_server_test::support::call_tool;

#[tokio::test]
async fn unused_imports_tool_call_refuses_the_retired_scan() {
    let dir = TempDir::new().expect("temporary project");
    let project = dir.path();
    fs::create_dir_all(project.join("src")).expect("src directory");
    fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("fixture source");
    let graph = crate::fixture::init_project_from_template(project)
        .await
        .expect("project fixture");
    let server = Box::pin(McpServer::new(graph, None)).await;

    let response = call_tool(
        server,
        41,
        "tracedecay_unused_imports",
        json!({ "limit": 50 }),
    )
    .await;

    assert_eq!(
        response,
        json!({
            "jsonrpc": "2.0",
            "id": 41,
            "error": {
                "code": -32603,
                "message": "tool execution failed: config error: unknown tool: tracedecay_unused_imports",
                "data": {
                    "tool": "tracedecay_unused_imports",
                    "cli_fallback": "This tool is also available from the shell: `tracedecay tool unused_imports ...` (`tracedecay tool unused_imports --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
                }
            }
        })
    );
}

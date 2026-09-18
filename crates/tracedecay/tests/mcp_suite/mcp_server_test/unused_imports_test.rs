//! Host-visible `tools/call` for the retired unused-import scan.
//!
//! The scan was removed because a graph-only walk reported a clean tree on
//! real code. An empty success would look the same. This test calls the MCP
//! name a host still sends and asserts the refusal the server returns.

use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::mcp::McpServer;

use crate::mcp_server_test::support::call_tool;

/// Grouped `use` where `HashMap` is referenced and `HashSet` is not.
///
/// A restored scanner that reads source must disagree with the refusal below:
/// the used half stays quiet and the unused half is a finding.
const GROUPED_USE_WITH_ONE_UNUSED_NAME: &str = "\
use std::collections::{HashMap, HashSet};

fn main() {
    let mut counts = HashMap::new();
    counts.insert(\"used\", 1);
    let _ = counts;
}
";

async fn server_over_grouped_use() -> (Arc<McpServer>, TempDir) {
    let dir = TempDir::new().expect("temporary project");
    let project = dir.path();
    fs::create_dir_all(project.join("src")).expect("src directory");
    fs::write(
        project.join("src/main.rs"),
        GROUPED_USE_WITH_ONE_UNUSED_NAME,
    )
    .expect("fixture source");
    let graph = crate::fixture::init_project_from_template(project)
        .await
        .expect("project fixture");
    let server = Box::pin(McpServer::new(graph, None)).await;
    (server, dir)
}

fn refused_unused_imports_call(id: i64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32603,
            "message": "tool execution failed: config error: unknown tool: tracedecay_unused_imports",
            "data": {
                "tool": "tracedecay_unused_imports",
                "cli_fallback": "This tool is also available from the shell: `tracedecay tool unused_imports ...` (`tracedecay tool unused_imports --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
            }
        }
    })
}

#[tokio::test]
async fn unused_imports_tool_call_refuses_the_retired_scan() {
    let (first_page, _first_project) = server_over_grouped_use().await;
    let first_page = call_tool(
        first_page,
        41,
        "tracedecay_unused_imports",
        json!({ "limit": 50 }),
    )
    .await;
    assert_eq!(first_page, refused_unused_imports_call(41));

    let (resume, _resume_project) = server_over_grouped_use().await;
    let resume = call_tool(
        resume,
        42,
        "tracedecay_unused_imports",
        json!({ "limit": 1, "cursor": "page-2" }),
    )
    .await;
    assert_eq!(resume, refused_unused_imports_call(42));
}

//! Host-visible MCP behavior of `tracedecay_workflow_run`.
//!
//! No Workflow operation key is `run`, so `tools/call` refuses the name
//! before any run body is decoded. Empty arguments and a concrete run id
//! must produce the same refusal.

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_mcp::transport::JsonRpcRequest;

use super::dispatch_test_support::SelectorEnv;
use crate::config::lock_user_data_dir_test_env;
use crate::mcp::McpServer;
use crate::project::TraceDecay;
use crate::test_support::host_admission::ProjectScopedTestRuntimeV1;

const TOOL_NAME: &str = "tracedecay_workflow_run";

fn refusal(id: i64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32603,
            "message": "tool execution failed: config error: unknown tool: tracedecay_workflow_run",
            "data": {
                "tool": "tracedecay_workflow_run",
                "cli_fallback": "This tool is also available from the shell: `tracedecay tool workflow_run ...` (`tracedecay tool workflow_run --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
            }
        }
    })
}

async fn call(server: &McpServer, id: i64, arguments: Value) -> Value {
    let response = server
        .handle_request(&JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(json!(id)),
            method: "tools/call".to_owned(),
            params: Some(json!({
                "name": TOOL_NAME,
                "arguments": arguments,
            })),
        })
        .await
        .expect("tools/call returns a JSON-RPC response");
    serde_json::to_value(response).expect("JSON-RPC response serializes")
}

#[tokio::test]
async fn tracedecay_workflow_run_is_refused_before_its_arguments_are_read() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("temp project");
    let _selector_env = SelectorEnv::new(dir.path());
    let project = dir.path().join("workflow-run");
    std::fs::create_dir_all(project.join("src")).expect("source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn marker() {}\n").expect("source file");
    let (graph, runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.workflow-run-behavior",
    )
    .await
    .expect("registered workflow-run fixture");
    let server = McpServer::new_with_host_admission_test_runtime_for_test(
        graph,
        None,
        ProjectScopedTestRuntimeV1::new(runtime).expect("project-scoped runtime"),
    )
    .await
    .expect("MCP server");

    assert_eq!(
        call(&server, 2, json!({})).await,
        refusal(2),
        "an empty object must be refused as an unknown tool"
    );
    assert_eq!(
        call(&server, 3, json!({"run_id": "wf_not-a-real-run"})).await,
        refusal(3),
        "a concrete run id must be refused before the body is decoded"
    );
}

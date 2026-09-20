//! `tracedecay_workflow_delete` is not a Workflow operation.
//!
//! Retirement and rejection are the typed lifecycle transitions. A host that
//! calls the delete name must see the unknown-tool JSON-RPC error, while the
//! advertised retire tool stays in the same discovery payload.

use crate::mcp_server_test::support::{
    jsonrpc_request, response_with_id, run_server_with_messages, setup_server,
};
use serde_json::json;

#[tokio::test]
async fn workflow_delete_call_is_the_unknown_tool_error() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(1), "tools/list", json!({})),
            jsonrpc_request(
                json!(2),
                "tools/call",
                json!({
                    "name": "tracedecay_workflow_delete",
                    "arguments": {
                        "definition_id": "workflow.definition.proof-delete",
                        "definition_version": 1,
                        "expected_revision": 1
                    }
                }),
            ),
        ],
    )
    .await;

    let listed = response_with_id(&responses, json!(1));
    let tools = listed["result"]["tools"]
        .as_array()
        .expect("tools/list returns a tools array");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    let workflow_names = names
        .iter()
        .copied()
        .filter(|name| name.starts_with("tracedecay_workflow_"))
        .collect::<Vec<_>>();
    assert!(
        workflow_names.contains(&"tracedecay_workflow_retire_definition"),
        "retire stays the typed non-delete transition; workflow tools: {workflow_names:?}"
    );
    assert!(
        !names.contains(&"tracedecay_workflow_delete"),
        "delete must not be advertised"
    );

    let called = response_with_id(&responses, json!(2));
    assert_eq!(
        called,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32603,
                "message": "tool execution failed: config error: unknown tool: tracedecay_workflow_delete",
                "data": {
                    "tool": "tracedecay_workflow_delete",
                    "cli_fallback": "This tool is also available from the shell: `tracedecay tool workflow_delete ...` (`tracedecay tool workflow_delete --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
                }
            }
        })
    );
}

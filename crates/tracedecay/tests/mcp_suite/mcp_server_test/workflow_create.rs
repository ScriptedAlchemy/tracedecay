//! Host-visible behavior of `tracedecay_workflow_create`.
//!
//! That name is not a mounted Workflow operation. A caller still reaches it
//! through `tools/call`, and the name is rejected before the body is decoded,
//! so a definition-shaped body and an empty body must return the same
//! JSON-RPC error.

use serde_json::json;

use super::support::{jsonrpc_request, response_with_id, run_server_with_messages, setup_server};

fn refused_workflow_create(id: i64) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32603,
            "message": "tool execution failed: config error: unknown tool: tracedecay_workflow_create",
            "data": {
                "tool": "tracedecay_workflow_create",
                "cli_fallback": "This tool is also available from the shell: `tracedecay tool workflow_create ...` (`tracedecay tool workflow_create --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
            }
        }
    })
}

#[tokio::test]
async fn workflow_create_call_refuses_a_definition_body_and_an_empty_body() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(
                json!(2),
                "tools/call",
                json!({
                    "name": "tracedecay_workflow_create",
                    "arguments": {
                        "definition_id": "workflow.definition.release-review",
                        "name": "release-review"
                    }
                }),
            ),
            jsonrpc_request(
                json!(3),
                "tools/call",
                json!({
                    "name": "tracedecay_workflow_create",
                    "arguments": {}
                }),
            ),
        ],
    )
    .await;

    assert_eq!(
        response_with_id(&responses, json!(2)),
        refused_workflow_create(2)
    );
    assert_eq!(
        response_with_id(&responses, json!(3)),
        refused_workflow_create(3)
    );
}

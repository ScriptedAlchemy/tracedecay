//! Host-visible behavior of calling `tracedecay_workflow_update`.
//!
//! Workflow definitions are immutable: a new version is registered, never
//! patched in place. This name is outside that closed operation set, so the
//! host sees the catalog refusal rather than a Workflow problem envelope or
//! an empty success.

use serde_json::json;

use crate::mcp_server_test::support::{
    jsonrpc_request, response_with_id, run_server_with_messages, setup_server,
};

const TOOL_NAME: &str = "tracedecay_workflow_update";

#[tokio::test]
async fn calling_workflow_update_is_refused_as_an_unknown_tool() {
    let (server, _project) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(1), "tools/list", json!({})),
            jsonrpc_request(
                json!(2),
                "tools/call",
                json!({
                    "name": TOOL_NAME,
                    "arguments": {
                        "definition_id": "wfdef_checkout",
                        "expected_revision": 3,
                        "summary": "retry payment capture after the adapter timeout"
                    }
                }),
            ),
        ],
    )
    .await;

    let listed = response_with_id(&responses, json!(1));
    let advertised: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools/list returns the advertised catalog")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        advertised.contains(&"tracedecay_workflow_list_definitions"),
        "discovery must still advertise the closed Workflow family: {advertised:?}"
    );
    assert!(
        !advertised.contains(&TOOL_NAME),
        "{TOOL_NAME} must stay off the advertised catalog: {advertised:?}"
    );

    let called = response_with_id(&responses, json!(2));
    assert_eq!(
        called,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32603,
                "message": "tool execution failed: config error: unknown tool: tracedecay_workflow_update",
                "data": {
                    "cli_fallback": "This tool is also available from the shell: `tracedecay tool workflow_update ...` (`tracedecay tool workflow_update --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly.",
                    "tool": "tracedecay_workflow_update"
                }
            }
        })
    );
}

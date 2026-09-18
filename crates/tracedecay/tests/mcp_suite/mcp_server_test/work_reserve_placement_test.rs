//! Host-visible MCP behavior of `tracedecay_work_reserve_placement`.
//!
//! Placement admission, status, and release are the mounted Work operations.
//! This name is not one of them. A client that calls it must receive the
//! closed unknown-tool refusal, not an admitted placement or an empty success.

use crate::mcp_server_test::support::{
    call_tool, jsonrpc_request, response_with_id, run_server_with_messages, setup_server,
};
use serde_json::{Value, json};

#[tokio::test]
async fn calling_work_reserve_placement_is_refused_as_an_unknown_tool() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server.clone(),
        vec![jsonrpc_request(json!(70), "tools/list", json!({}))],
    )
    .await;
    let listed = response_with_id(&responses, json!(70));
    let names = tool_names(&listed);
    assert!(
        names
            .iter()
            .any(|name| *name == "tracedecay_work_admit_placement"),
        "tools/list must still advertise the mounted placement claim, got {listed}"
    );
    assert!(
        !names
            .iter()
            .any(|name| *name == "tracedecay_work_reserve_placement"),
        "tools/list must not advertise tracedecay_work_reserve_placement, got {listed}"
    );

    let called = call_tool(
        server,
        71,
        "tracedecay_work_reserve_placement",
        json!({
            "task_id": "task.reserve-placement",
            "run_id": "run.reserve-placement",
            "target": {
                "kind": "clean_in_place",
                "root": null,
                "network_free": true,
                "in_place_acknowledged": true
            },
            "occurred_at": 1
        }),
    )
    .await;

    assert_eq!(
        called,
        json!({
            "jsonrpc": "2.0",
            "id": 71,
            "error": {
                "code": -32603,
                "message": "tool execution failed: config error: unknown tool: tracedecay_work_reserve_placement",
                "data": {
                    "tool": "tracedecay_work_reserve_placement",
                    "cli_fallback": "This tool is also available from the shell: `tracedecay tool work_reserve_placement ...` (`tracedecay tool work_reserve_placement --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
                }
            }
        })
    );
}

fn tool_names(listed: &Value) -> Vec<&str> {
    listed["result"]["tools"]
        .as_array()
        .expect("tools/list result.tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tools/list entry name"))
        .collect()
}

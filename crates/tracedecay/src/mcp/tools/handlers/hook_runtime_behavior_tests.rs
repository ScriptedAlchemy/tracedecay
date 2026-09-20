//! `tracedecay_hook_runtime` the way an MCP client calls it.
//!
//! Each case is one `tools/call` on the project server. The local counter and
//! saved-token metadata are the durable effects; the JSON-RPC body is what
//! the client reads. Expected values are those observations, not a schema
//! restatement or a digest the subject computed.

use std::fs;
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_mcp::{JsonRpcRequest, JsonRpcResponse};

use super::dispatch_test_support::SelectorEnv;
use crate::config::lock_user_data_dir_test_env;
use crate::mcp::McpServer;
use crate::project::TraceDecay;

async fn open_server() -> (TempDir, SelectorEnv, Arc<McpServer>) {
    let dir = TempDir::new().expect("temp dir");
    let env = SelectorEnv::new(dir.path());
    let project = dir.path().join("hook-runtime");
    fs::create_dir_all(project.join("src")).expect("project src");
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").expect("probe source");
    let (cg, runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.hook-runtime.proof",
    )
    .await
    .expect("enrolled project");
    cg.add_local_counter(41).await.expect("seed local counter");
    cg.set_tokens_saved(12).await.expect("seed saved tokens");
    let scoped = crate::test_support::host_admission::ProjectScopedTestRuntimeV1::new(runtime)
        .expect("project-scoped runtime");
    let server = McpServer::new_with_host_admission_test_runtime_for_test(cg, None, scoped)
        .await
        .expect("project MCP server");
    (dir, env, server)
}

async fn counters(server: &McpServer) -> (u64, u64) {
    let cg = server.cg().await;
    (
        cg.get_local_counter().await.expect("local counter"),
        cg.get_tokens_saved().await.expect("tokens saved"),
    )
}

async fn call_hook(server: &McpServer, id: i64, arguments: Value) -> JsonRpcResponse {
    let line = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_hook_runtime",
            "arguments": arguments
        }
    })
    .to_string();
    let request = JsonRpcRequest::decode(&line).expect("tools/call line");
    server
        .handle_request(&request)
        .await
        .expect("tools/call response")
}

fn success_payload(response: &JsonRpcResponse, id: i64) -> Value {
    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, json!(id));
    assert!(
        response.error.is_none(),
        "tools/call failed: {:?}",
        response.error
    );
    let result = response.result.as_ref().expect("success result");
    let content = result["content"].as_array().expect("content");
    assert_eq!(content.len(), 1, "unexpected content: {result}");
    assert_eq!(content[0]["type"], "text");
    let text = content[0]["text"].as_str().expect("text content");
    serde_json::from_str(text).unwrap_or_else(|error| panic!("payload JSON ({error}): {text}"))
}

fn assert_missing_action(response: &JsonRpcResponse, id: i64) {
    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, json!(id));
    assert!(response.result.is_none());
    let error = response.error.as_ref().expect("refusal");
    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "missing required parameter `action`");
    assert_eq!(
        error.data.as_ref(),
        Some(&json!({
            "detail": "missing required parameter `action`",
            "reason_code": "missing_required_parameter",
            "retryable": false,
            "tool": "tracedecay_hook_runtime"
        }))
    );
}

fn assert_execution_refused(response: &JsonRpcResponse, id: i64, message: &str) {
    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, json!(id));
    assert!(response.result.is_none());
    let error = response.error.as_ref().expect("refusal");
    assert_eq!(error.code, -32603);
    assert_eq!(error.message, message);
    assert_eq!(
        error.data.as_ref().and_then(|data| data.get("tool")),
        Some(&json!("tracedecay_hook_runtime"))
    );
}

#[tokio::test]
async fn hook_runtime_resets_the_local_counter_and_refuses_the_other_inputs() {
    let _env_lock = lock_user_data_dir_test_env();
    let (_dir, _env, server) = open_server().await;

    let removed = call_hook(
        &server,
        1,
        json!({
            "action": "reset_counter",
            "format": "json",
            "hermes_home": "/tmp/not-a-profile"
        }),
    )
    .await;
    assert_execution_refused(
        &removed,
        1,
        "tool execution failed: config error: unknown parameter `hermes_home` for `tracedecay_hook_runtime`",
    );

    let missing = call_hook(&server, 2, json!({})).await;
    assert_missing_action(&missing, 2);
    let blank = call_hook(&server, 3, json!({"action": ""})).await;
    assert_missing_action(&blank, 3);
    let numeric = call_hook(&server, 4, json!({"action": 42})).await;
    assert_missing_action(&numeric, 4);

    let unknown = call_hook(&server, 5, json!({"action": "not-a-hook-action"})).await;
    assert_execution_refused(
        &unknown,
        5,
        "tool execution failed: config error: unknown hook runtime action: not-a-hook-action",
    );
    let user_scope = call_hook(
        &server,
        6,
        json!({"action": "ingest_transcript", "user_scope": true}),
    )
    .await;
    assert_execution_refused(
        &user_scope,
        6,
        "tool execution failed: config error: user transcript ingest requires projectless daemon routing",
    );
    let projectless = call_hook(&server, 7, json!({"action": "codex_stop"})).await;
    assert_execution_refused(
        &projectless,
        7,
        "tool execution failed: config error: hook action `codex_stop` requires projectless daemon routing",
    );

    assert_eq!(counters(&server).await, (41, 12));

    let reset = call_hook(
        &server,
        8,
        json!({"action": "reset_counter", "format": "json"}),
    )
    .await;
    assert_eq!(
        success_payload(&reset, 8),
        json!({"action": "reset_counter", "reset": true})
    );
    assert_eq!(counters(&server).await, (0, 12));

    let receipt = call_hook(
        &server,
        9,
        json!({"action": "accounting_receipt", "format": "json"}),
    )
    .await;
    let receipt = success_payload(&receipt, 9);
    assert_eq!(receipt["action"], "accounting_receipt");
    assert_eq!(receipt["coverage"], "unavailable");
    assert_eq!(receipt["pricing_status"], "unavailable");
    assert_eq!(receipt["provider_usage_events"], 0);
    assert_eq!(receipt["tokens_saved"], 12);
    assert_eq!(receipt["tokens_consumed"], Value::Null);
    assert_eq!(receipt["cost_usd"], Value::Null);
    assert_eq!(receipt["efficiency"], Value::Null);
    assert_eq!(receipt["watermark"], json!(0));
    assert_eq!(counters(&server).await, (0, 12));
}

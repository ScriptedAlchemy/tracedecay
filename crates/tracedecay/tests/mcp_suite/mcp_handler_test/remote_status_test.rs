//! Client-visible `tracedecay_remote_status` reads.
//!
//! A daemon-mounted Remote Brain with no listener and no registered node is
//! unconfigured. A server that never installed the reader is unavailable.
//! Both are successful tool results; neither is an empty object or a
//! JSON-RPC error.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_mcp::JsonRpcResponse;
use tracedecay_mcp::transport::ChannelTransport;

use crate::support::{production_composition_fixture, real_mcp_server, setup_empty_project};

const UNCONFIGURED_JSON: &str = r#"{"kind":"unconfigured"}"#;
const UNAVAILABLE_JSON: &str = r#"{"kind":"unavailable"}"#;
const UNCONFIGURED_MARKDOWN: &str = "**kind:** unconfigured\n";
const UNAVAILABLE_MARKDOWN: &str = "**kind:** unavailable\n";

fn assert_remote_status_response(response: &Value, id: i64, text: &str) {
    assert_eq!(
        response,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": text }]
            }
        }),
        "tracedecay_remote_status response"
    );
}

fn assert_harness_remote_status(response: &JsonRpcResponse, text: &str) {
    let response = serde_json::to_value(response).expect("harness response is JSON");
    assert_remote_status_response(&response, 1, text);
}

#[tokio::test]
async fn fresh_daemon_remote_status_reports_unconfigured() {
    let fixture = production_composition_fixture().await;

    let json_status = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_remote_status",
            json!({ "format": "json" }),
        )
        .await
        .expect("daemon-mounted remote status call");
    assert_harness_remote_status(&json_status, UNCONFIGURED_JSON);

    let markdown_status = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_remote_status",
            json!({ "format": "markdown" }),
        )
        .await
        .expect("daemon-mounted markdown remote status call");
    assert_harness_remote_status(&markdown_status, UNCONFIGURED_MARKDOWN);

    let default_status = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_remote_status", json!({}))
        .await
        .expect("daemon-mounted default remote status call");
    assert_harness_remote_status(&default_status, UNCONFIGURED_MARKDOWN);

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn unmounted_remote_status_reports_unavailable() {
    let (graph, _env, _dir) = setup_empty_project().await;
    let server = real_mcp_server(graph).await;
    let responses = call_remote_status(
        server,
        vec![
            (1, json!({ "format": "json" })),
            (2, json!({ "format": "markdown" })),
            (3, json!({})),
        ],
    )
    .await;

    assert_remote_status_response(&response_with_id(&responses, 1), 1, UNAVAILABLE_JSON);
    assert_remote_status_response(&response_with_id(&responses, 2), 2, UNAVAILABLE_MARKDOWN);
    assert_remote_status_response(&response_with_id(&responses, 3), 3, UNAVAILABLE_MARKDOWN);
}

async fn call_remote_status(server: Arc<McpServer>, calls: Vec<(i64, Value)>) -> Vec<Value> {
    let (mut transport, sender, mut receiver) = ChannelTransport::new();
    for (id, arguments) in calls {
        let request = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "tracedecay_remote_status",
                "arguments": arguments
            }
        }))
        .expect("remote status request");
        sender
            .send(request)
            .expect("remote status request reaches the server");
    }
    drop(sender);

    let handle = tokio::spawn(async move {
        server
            .run(&mut transport)
            .await
            .expect("unmounted MCP server serves remote status");
    });

    let lines = tokio::time::timeout(Duration::from_secs(15), async {
        let mut lines = Vec::new();
        while let Some(line) = receiver.recv().await {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                lines.push(trimmed.to_string());
            }
        }
        lines
    })
    .await
    .expect("tracedecay_remote_status tools/call should answer");
    handle.await.expect("unmounted MCP server task joins");

    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("JSON-RPC line: {line}")))
        .collect()
}

fn response_with_id(responses: &[Value], id: i64) -> Value {
    let matches: Vec<&Value> = responses
        .iter()
        .filter(|response| response.get("id") == Some(&json!(id)))
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "one tools/call response for id {id}, got {responses:?}"
    );
    matches[0].clone()
}

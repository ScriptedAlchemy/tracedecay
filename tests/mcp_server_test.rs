//! Integration tests for the MCP server (`McpServer`) exercising the full
//! JSON-RPC 2.0 protocol via `ChannelTransport`.
//!
//! Run with: `cargo test --features test-transport --test mcp_server_test`

#![cfg(feature = "test-transport")]

mod common;

use common::EnvVarGuard;
use serde_json::{json, Value};
use std::fs;
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;
use tracedecay::branch_meta::{save_branch_meta, BranchMeta};
use tracedecay::mcp::transport::{ChannelTransport, McpTransport};
use tracedecay::mcp::McpServer;
use tracedecay::tracedecay::TraceDecay;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Creates a temporary Rust project, indexes it, and returns a ready server.
async fn setup_server() -> (Arc<McpServer>, TempDir) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/main.rs"),
        "fn main() { let x = helper(); }\nfn helper() -> i32 { 42 }\n",
    )
    .unwrap();
    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let server = McpServer::new(cg, None).await;
    (server, dir)
}

/// Sends a sequence of JSON-RPC messages to a server, runs it to completion,
/// and returns all non-empty response lines.
async fn run_server_with_messages(server: Arc<McpServer>, messages: Vec<String>) -> Vec<String> {
    let (mut transport, sender, mut receiver) = ChannelTransport::new();

    for msg in messages {
        sender.send(msg).unwrap();
    }
    drop(sender);

    let handle = tokio::spawn(async move {
        server.run(&mut transport).await.unwrap();
    });

    let mut responses = Vec::new();
    while let Some(line) = receiver.recv().await {
        let trimmed = line.trim().to_string();
        if !trimmed.is_empty() {
            responses.push(trimmed);
        }
    }
    handle.await.unwrap();
    responses
}

/// Helper to build a JSON-RPC request string.
fn jsonrpc_request(id: Value, method: &str, params: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    }))
    .unwrap()
}

/// Helper to build a JSON-RPC notification string (no id).
fn jsonrpc_notification(method: &str) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": method
    }))
    .unwrap()
}

/// Parses a JSON-RPC response and returns it.
fn parse_response(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn response_with_id(responses: &[String], id: Value) -> Value {
    responses
        .iter()
        .map(|r| parse_response(r))
        .find(|resp| resp.get("id") == Some(&id))
        .unwrap_or_else(|| panic!("response with id {id}"))
}

struct ReadErrorTransport;

impl McpTransport for ReadErrorTransport {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "synthetic read failure",
        ))
    }

    async fn write_line(&mut self, _line: &str) -> std::io::Result<()> {
        Ok(())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 1. test_initialize
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_initialize() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(1), "initialize", json!({}))],
    )
    .await;

    assert!(!responses.is_empty(), "should have at least one response");
    let resp = parse_response(&responses[0]);
    assert_eq!(resp["id"], 1);
    assert!(resp["result"]["protocolVersion"].is_string());
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(resp["result"]["serverInfo"]["name"], "tracedecay");
    assert!(resp["result"]["serverInfo"]["version"].is_string());
}

// ---------------------------------------------------------------------------
// 2. test_initialized_notification
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_initialized_notification() {
    let (server, _dir) = setup_server().await;
    // Send "initialized" notification (no id), then a ping to verify server is alive.
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_notification("initialized"),
            jsonrpc_request(json!(2), "ping", json!({})),
        ],
    )
    .await;

    // The notification should produce no response; we should only get the ping response.
    // Filter to find the ping response.
    let ping_responses: Vec<&String> = responses
        .iter()
        .filter(|r| {
            let v = parse_response(r);
            v["id"] == 2
        })
        .collect();
    assert_eq!(
        ping_responses.len(),
        1,
        "should get exactly one ping response"
    );
    let resp = parse_response(ping_responses[0]);
    assert!(resp["error"].is_null(), "ping should succeed");
}

#[tokio::test]
async fn test_any_notification_without_id_produces_no_response() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_notification("ping"),
            jsonrpc_request(json!(901), "ping", json!({})),
        ],
    )
    .await;

    assert_eq!(
        responses.len(),
        1,
        "only the request with id=901 should produce a response, got {responses:?}"
    );
    let resp = parse_response(&responses[0]);
    assert_eq!(resp["id"], 901);
    assert!(resp["error"].is_null(), "ping request should succeed");
}

#[tokio::test]
async fn test_explicit_null_id_is_still_a_request() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(null), "ping", json!({}))],
    )
    .await;

    assert_eq!(
        responses.len(),
        1,
        "explicit id=null is a request id and should receive a response"
    );
    let resp = parse_response(&responses[0]);
    assert!(resp["id"].is_null(), "response should preserve null id");
    assert!(resp["error"].is_null(), "ping request should succeed");
}

#[tokio::test]
async fn test_tools_call_explicit_null_id_is_still_a_request() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(null),
            "tools/call",
            json!({
                "name": "tracedecay_status",
                "arguments": {}
            }),
        )],
    )
    .await;

    assert_eq!(
        responses.len(),
        1,
        "explicit id=null is a tools/call request id and should receive a response"
    );
    let resp = response_with_id(&responses, json!(null));
    assert!(resp["error"].is_null(), "tools/call request should succeed");
    assert!(
        resp["result"].is_object(),
        "tools/call should return a result"
    );
}

// ---------------------------------------------------------------------------
// 3. test_notifications_initialized
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_notifications_initialized() {
    let (server, _dir) = setup_server().await;
    // Send "notifications/initialized" notification, then ping.
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_notification("notifications/initialized"),
            jsonrpc_request(json!(3), "ping", json!({})),
        ],
    )
    .await;

    let ping_responses: Vec<&String> = responses
        .iter()
        .filter(|r| {
            let v = parse_response(r);
            v["id"] == 3
        })
        .collect();
    assert_eq!(
        ping_responses.len(),
        1,
        "should get exactly one ping response"
    );
}

// ---------------------------------------------------------------------------
// 4. test_ping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_ping() {
    let (server, _dir) = setup_server().await;
    let responses =
        run_server_with_messages(server, vec![jsonrpc_request(json!(10), "ping", json!({}))]).await;

    assert!(!responses.is_empty());
    let resp = parse_response(&responses[0]);
    assert_eq!(resp["id"], 10);
    assert!(
        resp["result"].is_object(),
        "ping result should be an object"
    );
    assert!(resp["error"].is_null(), "ping should not have an error");
}

// ---------------------------------------------------------------------------
// 5. test_tools_list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tools_list() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(20), "tools/list", json!({}))],
    )
    .await;

    assert!(!responses.is_empty());
    let resp = parse_response(&responses[0]);
    assert_eq!(resp["id"], 20);
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty(), "tools list should not be empty");
    // Verify at least some well-known tools are present.
    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        tool_names.contains(&"tracedecay_search"),
        "should have tracedecay_search"
    );
    assert!(
        tool_names.contains(&"tracedecay_status"),
        "should have tracedecay_status"
    );
    assert!(
        tool_names.contains(&"tracedecay_context"),
        "should have tracedecay_context"
    );
}

// ---------------------------------------------------------------------------
// 6. test_tools_call_search
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tools_call_search() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(30),
            "tools/call",
            json!({
                "name": "tracedecay_search",
                "arguments": { "query": "helper" }
            }),
        )],
    )
    .await;

    // Find the response with id=30 (skip any notifications).
    let resp_str = responses
        .iter()
        .find(|r| {
            let v = parse_response(r);
            v["id"] == 30
        })
        .expect("should have a response for id=30");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_null(), "search should not error");
    let content = resp["result"]["content"].as_array().unwrap();
    // At least one content item should contain "helper".
    let has_helper = content.iter().any(|c| {
        c["text"]
            .as_str()
            .map(|t| t.contains("helper"))
            .unwrap_or(false)
    });
    assert!(has_helper, "search results should contain 'helper'");
}

#[tokio::test]
async fn test_tools_call_semantic_failure_sets_is_error() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(33),
            "tools/call",
            json!({
                "name": "tracedecay_str_replace",
                "arguments": {
                    "path": "src/main.rs",
                    "old_str": "fn missing() {}",
                    "new_str": "fn replaced() {}"
                }
            }),
        )],
    )
    .await;

    let resp = response_with_id(&responses, json!(33));
    assert!(
        resp["error"].is_null(),
        "semantic tool failures should not become JSON-RPC errors"
    );
    assert_eq!(
        resp["result"]["isError"], true,
        "semantic tool failure should set MCP isError=true, got {resp}"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tool result text");
    let payload: Value = serde_json::from_str(text).expect("tool result JSON");
    assert_eq!(payload["success"], false);
}

#[tokio::test]
async fn test_tools_call_plain_text_failure_sets_is_error() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(34),
            "tools/call",
            json!({
                "name": "tracedecay_changelog",
                "arguments": {
                    "from_ref": "HEAD~1",
                    "to_ref": "HEAD"
                }
            }),
        )],
    )
    .await;

    let resp = response_with_id(&responses, json!(34));
    assert!(
        resp["error"].is_null(),
        "plain-text semantic failures should not become JSON-RPC errors"
    );
    assert_eq!(
        resp["result"]["isError"], true,
        "plain-text semantic failure should set MCP isError=true, got {resp}"
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tool result text");
    assert!(
        text.contains("git diff failed"),
        "expected changelog git failure text, got: {text}"
    );
}

// ---------------------------------------------------------------------------
// 6b. test_tools_call_timings_flag
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tools_call_timings_flag_off_by_default() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(31),
            "tools/call",
            json!({"name": "tracedecay_search", "arguments": {"query": "helper"}}),
        )],
    )
    .await;
    let resp = parse_response(
        responses
            .iter()
            .find(|r| parse_response(r)["id"] == 31)
            .expect("response with id 31"),
    );
    assert!(
        resp["result"]["_meta"]["duration_us"].is_null(),
        "duration_us must NOT be present when timings flag is off — got {}",
        resp["result"]["_meta"]
    );
}

#[tokio::test]
async fn test_tools_call_timings_flag_on_emits_duration_us() {
    let (server, _dir) = setup_server().await;
    server.set_timings_enabled(true);
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(32),
            "tools/call",
            json!({"name": "tracedecay_search", "arguments": {"query": "helper"}}),
        )],
    )
    .await;
    let resp = parse_response(
        responses
            .iter()
            .find(|r| parse_response(r)["id"] == 32)
            .expect("response with id 32"),
    );
    let dur = resp["result"]["_meta"]["duration_us"]
        .as_u64()
        .expect("duration_us must be a u64 when timings are enabled");
    // Lower-bound sanity: any real query takes at least a few microseconds.
    // Upper bound is generous so the test isn't flaky on slow CI runners.
    assert!(
        dur < 5_000_000,
        "duration_us should be well under 5 s, got {dur}"
    );
}

// ---------------------------------------------------------------------------
// 7. test_tools_call_status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tools_call_status() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(40),
            "tools/call",
            json!({
                "name": "tracedecay_status",
                "arguments": {}
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| {
            let v = parse_response(r);
            v["id"] == 40
        })
        .expect("should have a response for id=40");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_null(), "status should not error");
    let content = resp["result"]["content"].as_array().unwrap();
    let text = content
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("node_count") || text.contains("file_count"),
        "status response should contain node_count or file_count, got: {}",
        text
    );
}

// ---------------------------------------------------------------------------
// 8. test_tools_call_missing_params
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tools_call_missing_params() {
    let (server, _dir) = setup_server().await;
    // Send tools/call with no params at all.
    let responses = run_server_with_messages(
        server,
        vec![serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": 50,
            "method": "tools/call"
        }))
        .unwrap()],
    )
    .await;

    assert!(!responses.is_empty());
    let resp = parse_response(&responses[0]);
    assert_eq!(resp["id"], 50);
    assert!(resp["error"].is_object(), "should have an error");
    assert_eq!(
        resp["error"]["code"], -32602,
        "should be InvalidParams error"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("missing params"),
        "error message should mention missing params"
    );
}

// ---------------------------------------------------------------------------
// 9. test_tools_call_missing_name
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tools_call_missing_name() {
    let (server, _dir) = setup_server().await;
    // Send tools/call with params but no "name" key.
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(60),
            "tools/call",
            json!({
                "arguments": { "query": "test" }
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| {
            let v = parse_response(r);
            v["id"] == 60
        })
        .expect("should have a response for id=60");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_object(), "should have an error");
    assert_eq!(
        resp["error"]["code"], -32602,
        "should be InvalidParams error"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("missing 'name'"),
        "error message should mention missing name"
    );
}

// ---------------------------------------------------------------------------
// 10. test_unknown_method
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_unknown_method() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(70), "some/unknown/method", json!({}))],
    )
    .await;

    assert!(!responses.is_empty());
    let resp = parse_response(&responses[0]);
    assert_eq!(resp["id"], 70);
    assert!(resp["error"].is_object(), "should have an error");
    assert_eq!(
        resp["error"]["code"], -32601,
        "should be MethodNotFound error"
    );
}

// ---------------------------------------------------------------------------
// 11. test_malformed_json
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_malformed_json() {
    let (server, _dir) = setup_server().await;
    // Send invalid JSON, then a valid ping to verify server continues.
    let responses = run_server_with_messages(
        server,
        vec![
            "this is not json {{{".to_string(),
            jsonrpc_request(json!(80), "ping", json!({})),
        ],
    )
    .await;

    // Should have at least 2 responses: parse error + ping response.
    assert!(
        responses.len() >= 2,
        "should have at least 2 responses (parse error + ping), got {}",
        responses.len()
    );

    // First response should be a parse error.
    let error_resp = parse_response(&responses[0]);
    assert!(
        error_resp["error"].is_object(),
        "first response should be an error"
    );
    assert_eq!(
        error_resp["error"]["code"], -32700,
        "should be ParseError (-32700)"
    );

    // Second (or later) should be the ping response.
    let ping_resp = responses
        .iter()
        .find(|r| {
            let v = parse_response(r);
            v["id"] == 80
        })
        .expect("should have a ping response after malformed JSON");
    let ping = parse_response(ping_resp);
    assert!(
        ping["error"].is_null(),
        "ping after malformed JSON should succeed"
    );
}

// ---------------------------------------------------------------------------
// 12. test_blank_lines_skipped
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_blank_lines_skipped() {
    let (server, _dir) = setup_server().await;
    // Send blank/whitespace lines, then a ping.
    let responses = run_server_with_messages(
        server,
        vec![
            "".to_string(),
            "   ".to_string(),
            "\t".to_string(),
            jsonrpc_request(json!(90), "ping", json!({})),
        ],
    )
    .await;

    // Only the ping response should come through.
    let ping_responses: Vec<&String> = responses
        .iter()
        .filter(|r| {
            let v: Value = serde_json::from_str(r).unwrap_or(json!(null));
            v["id"] == 90
        })
        .collect();
    assert_eq!(
        ping_responses.len(),
        1,
        "should get exactly 1 response (ping only), got {}",
        responses.len()
    );
}

// ---------------------------------------------------------------------------
// 13. test_multiple_tool_calls
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_multiple_tool_calls() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(100), "initialize", json!({})),
            jsonrpc_request(json!(101), "ping", json!({})),
            jsonrpc_request(json!(102), "tools/list", json!({})),
            jsonrpc_request(
                json!(103),
                "tools/call",
                json!({
                    "name": "tracedecay_search",
                    "arguments": { "query": "main" }
                }),
            ),
        ],
    )
    .await;

    // Collect response IDs (filtering out notifications which have no "id" or null id).
    let response_ids: Vec<i64> = responses
        .iter()
        .filter_map(|r| {
            let v = parse_response(r);
            v["id"].as_i64()
        })
        .collect();

    assert!(
        response_ids.contains(&100),
        "should have response for id=100 (initialize)"
    );
    assert!(
        response_ids.contains(&101),
        "should have response for id=101 (ping)"
    );
    assert!(
        response_ids.contains(&102),
        "should have response for id=102 (tools/list)"
    );
    assert!(
        response_ids.contains(&103),
        "should have response for id=103 (tools/call)"
    );
}

// ---------------------------------------------------------------------------
// 14. test_server_stats_initial
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_server_stats_initial() {
    let (server, _dir) = setup_server().await;
    let stats = server.server_stats_json().await;
    assert!(stats["uptime_secs"].is_number(), "should have uptime_secs");
    assert_eq!(
        stats["total_requests"], 0,
        "initial total_requests should be 0"
    );
    assert_eq!(stats["tool_calls"], 0, "initial tool_calls should be 0");
    assert_eq!(stats["errors"], 0, "initial errors should be 0");
}

// ---------------------------------------------------------------------------
// 15. test_server_stats_after_run (indirect via tracedecay_status response)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_server_stats_after_run() {
    let (server, _dir) = setup_server().await;
    // Send several requests then a tracedecay_status to check stats are embedded.
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(200), "initialize", json!({})),
            jsonrpc_request(json!(201), "ping", json!({})),
            jsonrpc_request(
                json!(202),
                "tools/call",
                json!({
                    "name": "tracedecay_status",
                    "arguments": {}
                }),
            ),
        ],
    )
    .await;

    let status_resp_str = responses
        .iter()
        .find(|r| {
            let v = parse_response(r);
            v["id"] == 202
        })
        .expect("should have a response for id=202");
    let resp = parse_response(status_resp_str);
    assert!(resp["error"].is_null(), "status should not error");
    let content = resp["result"]["content"].as_array().unwrap();
    let text = content
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    // The server stats should be embedded in the status response and reflect
    // that requests have been processed.
    assert!(
        text.contains("server") || text.contains("total_requests") || text.contains("tool_calls"),
        "status response should contain server stats, got: {}",
        text
    );
}

// ---------------------------------------------------------------------------
// 16. test_error_tracking
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_error_tracking() {
    let (server, _dir) = setup_server().await;
    // Send an unknown method (which produces an error), then check status.
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(300), "unknown/method", json!({})),
            jsonrpc_request(
                json!(301),
                "tools/call",
                json!({
                    "name": "tracedecay_status",
                    "arguments": {}
                }),
            ),
        ],
    )
    .await;

    // Verify the unknown method produced an error.
    let error_resp_str = responses
        .iter()
        .find(|r| {
            let v = parse_response(r);
            v["id"] == 300
        })
        .expect("should have a response for id=300");
    let error_resp = parse_response(error_resp_str);
    assert!(
        error_resp["error"].is_object(),
        "unknown method should produce error"
    );

    // Check status to verify errors count increased.
    let status_resp_str = responses
        .iter()
        .find(|r| {
            let v = parse_response(r);
            v["id"] == 301
        })
        .expect("should have a response for id=301");
    let status_resp = parse_response(status_resp_str);
    assert!(status_resp["error"].is_null(), "status should not error");
    let content = status_resp["result"]["content"].as_array().unwrap();
    let text = content
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    // Parse the server stats from the status text to verify errors > 0.
    assert!(
        text.contains("\"errors\"") || text.contains("errors"),
        "status should contain errors field, got: {}",
        text
    );
    // The error count should be at least 1 (from the unknown method).
    // The server stats JSON is embedded in the text; try to find it.
    if let Some(server_start) = text.find("\"server\"") {
        let server_section = &text[server_start..];
        assert!(
            server_section.contains("\"errors\": 1") || server_section.contains("\"errors\":1"),
            "errors should be at least 1 after sending unknown method, section: {}",
            server_section
        );
    }
}

// ---------------------------------------------------------------------------
// 17. test_initialize_has_resources_capability
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_initialize_has_resources_capability() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(1), "initialize", json!({}))],
    )
    .await;

    let resp = parse_response(&responses[0]);
    assert!(
        resp["result"]["capabilities"]["resources"].is_object(),
        "initialize should advertise resources capability"
    );
}

// ---------------------------------------------------------------------------
// 18. test_initialize_has_instructions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_initialize_has_instructions() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(1), "initialize", json!({}))],
    )
    .await;

    let resp = parse_response(&responses[0]);
    let instructions = resp["result"]["instructions"]
        .as_str()
        .expect("initialize should have instructions string");
    assert!(
        instructions.contains("tracedecay_context"),
        "instructions should mention tracedecay_context"
    );
}

// ---------------------------------------------------------------------------
// 19. test_resources_list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_resources_list() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(400), "resources/list", json!({}))],
    )
    .await;

    let resp = parse_response(&responses[0]);
    assert_eq!(resp["id"], 400);
    assert!(resp["error"].is_null(), "resources/list should not error");
    let resources = resp["result"]["resources"]
        .as_array()
        .expect("should have resources array");
    assert_eq!(resources.len(), 5, "should expose 5 resources");

    let uris: Vec<&str> = resources.iter().filter_map(|r| r["uri"].as_str()).collect();
    assert!(
        uris.contains(&"tracedecay://status"),
        "should have status resource"
    );
    assert!(
        uris.contains(&"tracedecay://files"),
        "should have files resource"
    );
    assert!(
        uris.contains(&"tracedecay://overview"),
        "should have overview resource"
    );
    assert!(
        uris.contains(&"tracedecay://branches"),
        "should have branches resource"
    );
    assert!(
        uris.contains(&"tracedecay://schema"),
        "should have schema resource"
    );

    // All resources should have name, description, and mimeType.
    for resource in resources {
        assert!(resource["name"].is_string(), "resource should have name");
        assert!(
            resource["description"].is_string(),
            "resource should have description"
        );
        assert!(
            resource["mimeType"].is_string(),
            "resource should have mimeType"
        );
    }
}

// ---------------------------------------------------------------------------
// 20. test_resources_read_status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_resources_read_status() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(410),
            "resources/read",
            json!({
                "uri": "tracedecay://status"
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 410)
        .expect("should have response for id=410");
    let resp = parse_response(resp_str);
    assert!(
        resp["error"].is_null(),
        "resources/read status should not error"
    );

    let contents = resp["result"]["contents"]
        .as_array()
        .expect("should have contents array");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["uri"], "tracedecay://status");
    assert_eq!(contents[0]["mimeType"], "application/json");

    let text = contents[0]["text"].as_str().unwrap();
    assert!(
        text.contains("node_count"),
        "status resource should contain node_count"
    );
    assert!(
        text.contains("file_count"),
        "status resource should contain file_count"
    );
}

// ---------------------------------------------------------------------------
// 21. test_resources_read_files
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_resources_read_files() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(420),
            "resources/read",
            json!({
                "uri": "tracedecay://files"
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 420)
        .expect("should have response for id=420");
    let resp = parse_response(resp_str);
    assert!(
        resp["error"].is_null(),
        "resources/read files should not error"
    );

    let contents = resp["result"]["contents"]
        .as_array()
        .expect("should have contents array");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["uri"], "tracedecay://files");
    assert_eq!(contents[0]["mimeType"], "text/plain");

    let text = contents[0]["text"].as_str().unwrap();
    assert!(
        text.contains("indexed files"),
        "files resource should contain file count summary"
    );
    assert!(
        text.contains("main.rs"),
        "files resource should list main.rs"
    );
}

// ---------------------------------------------------------------------------
// 22. test_resources_read_overview
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_resources_read_overview() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(430),
            "resources/read",
            json!({
                "uri": "tracedecay://overview"
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 430)
        .expect("should have response for id=430");
    let resp = parse_response(resp_str);
    assert!(
        resp["error"].is_null(),
        "resources/read overview should not error"
    );

    let contents = resp["result"]["contents"]
        .as_array()
        .expect("should have contents array");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["uri"], "tracedecay://overview");
    assert_eq!(contents[0]["mimeType"], "text/plain");

    let text = contents[0]["text"].as_str().unwrap();
    assert!(
        text.contains("Project:"),
        "overview should start with Project:"
    );
    assert!(
        text.contains("Graph:"),
        "overview should contain Graph summary"
    );
    assert!(text.contains("nodes"), "overview should mention nodes");
}

// ---------------------------------------------------------------------------
// 23. test_resources_read_unknown_uri
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_resources_read_unknown_uri() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(440),
            "resources/read",
            json!({
                "uri": "tracedecay://nonexistent"
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 440)
        .expect("should have response for id=440");
    let resp = parse_response(resp_str);
    assert!(
        resp["error"].is_object(),
        "unknown URI should produce error"
    );
    assert_eq!(
        resp["error"]["code"], -32602,
        "should be InvalidParams error"
    );
}

// ---------------------------------------------------------------------------
// 24. test_resources_read_missing_uri
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_resources_read_missing_uri() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(450), "resources/read", json!({}))],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 450)
        .expect("should have response for id=450");
    let resp = parse_response(resp_str);
    assert!(
        resp["error"].is_object(),
        "missing URI should produce error"
    );
    assert_eq!(
        resp["error"]["code"], -32602,
        "should be InvalidParams error"
    );
}

// ---------------------------------------------------------------------------
// Regression: logging/setLevel must be handled (not return MethodNotFound)
// ---------------------------------------------------------------------------

/// The MCP client sends `logging/setLevel` immediately after initialisation
/// whenever the server advertises the `logging` capability. Before the fix the
/// server returned -32601 (MethodNotFound), which Claude Code logged as an
/// error on every session start.
#[tokio::test]
async fn test_logging_set_level_returns_success() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(500),
            "logging/setLevel",
            json!({"level": "info"}),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 500)
        .expect("should have response for id=500");
    let resp = parse_response(resp_str);
    assert!(
        resp["error"].is_null(),
        "logging/setLevel must not return an error, got: {resp}"
    );
    assert!(
        resp["result"].is_object(),
        "logging/setLevel must return an object result"
    );
}

/// Verify every log level accepted by RFC 5424 is handled without error.
#[tokio::test]
async fn test_logging_set_level_all_levels() {
    let levels = [
        "debug",
        "info",
        "notice",
        "warning",
        "error",
        "critical",
        "alert",
        "emergency",
    ];
    for (idx, level) in levels.iter().enumerate() {
        let id = json!(600 + idx as u64);
        let (server, _dir) = setup_server().await;
        let responses = run_server_with_messages(
            server,
            vec![jsonrpc_request(
                id.clone(),
                "logging/setLevel",
                json!({"level": level}),
            )],
        )
        .await;
        let resp_str = responses
            .iter()
            .find(|r| parse_response(r)["id"] == id)
            .unwrap_or_else(|| panic!("no response for level={level}"));
        let resp = parse_response(resp_str);
        assert!(
            resp["error"].is_null(),
            "logging/setLevel with level={level} must not error, got: {resp}"
        );
    }
}

/// `logging/setLevel` mid-session must not disrupt subsequent tool calls.
#[tokio::test]
async fn test_logging_set_level_does_not_break_session() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(700), "logging/setLevel", json!({"level": "warning"})),
            jsonrpc_request(json!(701), "ping", json!({})),
        ],
    )
    .await;

    let set_level = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 700)
        .expect("missing response for logging/setLevel");
    assert!(
        parse_response(set_level)["error"].is_null(),
        "logging/setLevel should succeed"
    );

    let ping = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 701)
        .expect("missing response for ping after logging/setLevel");
    assert!(
        parse_response(ping)["result"].is_object(),
        "ping after setLevel should succeed"
    );
}

/// The `initialize` response must advertise the `logging` capability so that
/// clients know they may send `logging/setLevel`.
#[tokio::test]
async fn test_initialize_advertises_logging_capability() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(800), "initialize", json!({}))],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 800)
        .expect("missing initialize response");
    let resp = parse_response(resp_str);
    assert!(
        resp["result"]["capabilities"]["logging"].is_object(),
        "initialize must advertise logging capability, got: {resp}"
    );
}

#[tokio::test]
async fn test_run_returns_transport_read_errors() {
    let (server, _dir) = setup_server().await;
    let mut transport = ReadErrorTransport;

    let err = server
        .run(&mut transport)
        .await
        .expect_err("transport read failure should be returned");
    assert!(
        err.to_string().contains("synthetic read failure"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// search_call_writes_savings_ledger_row
// ---------------------------------------------------------------------------

// Repeated serve-mode LCM calls must keep working while the project session
// DB schema is ensured at most once per process: after the first write-path
// call creates the store and runs the migrations, later write-path calls
// (even from a fresh `McpServer` in the same process) take the
// version-gate fast path and never re-run the LCM migrations — observable
// via the migration row's `applied_at`, which only a migration run rewrites.
//
// Pure-read tools (lcm_status) no longer create the store, so each session
// issues a write-path call (`lcm_session_boundary`, whose storage open is
// the migration-running path) before the status reads.
#[tokio::test]
async fn repeated_serve_lcm_calls_do_not_rerun_migrations() {
    let (server, dir) = setup_server().await;
    let lcm_status_call = |id: i64| {
        jsonrpc_request(
            json!(id),
            "tools/call",
            json!({ "name": "tracedecay_lcm_status", "arguments": {} }),
        )
    };
    // Write-path call: opens the session DB in write mode, creating it and
    // ensuring the schema. With only a session_id it records nothing
    // (`not_compression_boundary`) — its sole job here is to exercise the
    // migration-running open in each serve session.
    let lcm_boundary_call = |id: i64| {
        jsonrpc_request(
            json!(id),
            "tools/call",
            json!({
                "name": "tracedecay_lcm_session_boundary",
                "arguments": { "session_id": "migration-rerun-probe" }
            }),
        )
    };
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(1), "initialize", json!({})),
            jsonrpc_notification("notifications/initialized"),
            lcm_boundary_call(4),
            lcm_status_call(2),
            lcm_status_call(3),
        ],
    )
    .await;
    {
        let resp = responses
            .iter()
            .map(|r| parse_response(r))
            .find(|r| r["id"] == json!(4))
            .expect("missing response for boundary call");
        assert!(
            resp["error"].is_null(),
            "lcm_session_boundary should not error"
        );
    }
    for id in [2_i64, 3] {
        let resp = responses
            .iter()
            .map(|r| parse_response(r))
            .find(|r| r["id"] == json!(id))
            .unwrap_or_else(|| panic!("missing response for id={id}"));
        assert!(
            resp["error"].is_null(),
            "lcm_status id={id} should not error"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        assert_eq!(payload["status"], "ok", "lcm_status id={id} payload");
    }

    // Stamp a sentinel applied_at; only a re-run of the migrations would
    // rewrite it (the version-gate fast path and the per-process ensured
    // flag both leave the row untouched).
    let db_path = tracedecay::sessions::cursor::project_session_db_path(dir.path());
    let applied_at = |db_path: std::path::PathBuf| async move {
        let db = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let conn = db.connect().unwrap();
        let mut rows = conn
            .query(
                "SELECT applied_at FROM session_schema_migrations WHERE name = 'lcm'",
                (),
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    };
    {
        let db = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "UPDATE session_schema_migrations SET applied_at = 123 WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    }
    assert_eq!(applied_at(db_path.clone()).await, 123);

    // A second serve session over the same project in the same process.
    // The write-path boundary call is the one that would re-run migrations
    // if the per-process ensured cache failed; the status reads must also
    // keep working.
    let cg = TraceDecay::open(dir.path()).await.unwrap();
    let server = McpServer::new(cg, None).await;
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(1), "initialize", json!({})),
            lcm_boundary_call(4),
            lcm_status_call(2),
            lcm_status_call(3),
        ],
    )
    .await;
    for id in [4_i64, 2, 3] {
        let resp = responses
            .iter()
            .map(|r| parse_response(r))
            .find(|r| r["id"] == json!(id))
            .unwrap_or_else(|| panic!("missing response for id={id} in second session"));
        assert!(resp["error"].is_null(), "second-session lcm call id={id}");
    }
    assert_eq!(
        applied_at(db_path).await,
        123,
        "repeated serve-mode LCM calls must not re-run the LCM migrations"
    );
}

/// Serializes the savings-accounting tests below: they all mutate
/// process-wide env vars (`HOME`, `TRACEDECAY_GLOBAL_DB`,
/// `TRACEDECAY_ENABLE_GLOBAL_DB`). `#[tokio::test]` defaults to a
/// current-thread runtime, so holding the guard across `.await` is fine.
static SAVINGS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Redirects HOME at an isolated temp dir (so `~/.tracedecay/global.db`
/// lands there) and enables the global DB. Deliberately does NOT set
/// `TRACEDECAY_GLOBAL_DB`: that override also wins over project-local
/// LCM store discovery and would leak into concurrently running tests.
fn isolated_savings_env(tmp: &TempDir) -> Vec<EnvVarGuard> {
    let mut guards = vec![EnvVarGuard::set("HOME", tmp.path().as_os_str())];
    #[cfg(target_os = "windows")]
    guards.push(EnvVarGuard::set("USERPROFILE", tmp.path().as_os_str()));
    // `.cargo/config.toml` sets TRACEDECAY_DISABLE_GLOBAL_DB=1 to keep
    // cargo-launched processes hermetic; the explicit enable wins.
    guards.push(EnvVarGuard::set(
        "TRACEDECAY_ENABLE_GLOBAL_DB",
        std::ffi::OsStr::new("1"),
    ));
    guards
}

/// Awaits the server's ledger-write settlement signal (the rows are written
/// by spawned fire-and-forget tasks), then reads the ledger once and asserts
/// the expected row count for `project`. Deterministic where the old
/// 10-second DB poll was not:
///
/// - settlement replaces the wall-clock deadline race against the spawned
///   write task;
/// - the DB path is captured by the caller under `SAVINGS_ENV_LOCK`, so a
///   concurrent test's env mutation cannot redirect the read;
/// - the count is scoped to this test's unique project path, because tests
///   running in parallel *outside* the lock construct servers while `HOME`
///   is redirected here and append their own ledger rows to this same DB.
async fn settled_ledger_total(
    server: &McpServer,
    global_db_path: &std::path::Path,
    project: &std::path::Path,
    expected_calls: u64,
) -> tracedecay::global_db::SavingsTotal {
    server.ledger_writes_settled().await;
    let db = tracedecay::global_db::GlobalDb::open_at(global_db_path)
        .await
        .expect("global db opens at isolated path");
    let total = db.sum_savings(Some(&project.to_string_lossy()), 0).await;
    assert_eq!(
        total.calls, expected_calls,
        "every settled ledger write for this project must be visible (got {} calls)",
        total.calls
    );
    total
}

/// The global-DB path as resolved *right now* (callers hold
/// `SAVINGS_ENV_LOCK`, so this is the same path the server under test
/// resolves at construction).
fn locked_global_db_path() -> std::path::PathBuf {
    tracedecay::global_db::global_db_path().expect("global db path resolves under isolated HOME")
}

/// Creates a temp project whose `src/main.rs` is large enough that the
/// raw-file ("before") estimate is clearly nonzero, then indexes it.
async fn setup_savings_project() -> (Arc<McpServer>, TempDir) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    let mut source = String::from("fn main() { let x = helper(); }\nfn helper() -> i32 { 42 }\n");
    for i in 0..80 {
        source.push_str(&format!(
            "/// Filler documentation line {i} to inflate the raw-file estimate.\nfn filler_{i}() -> i32 {{ {i} }}\n"
        ));
    }
    fs::write(project.join("src/main.rs"), source).unwrap();
    let cg = TraceDecay::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    let server = McpServer::new(cg, None).await;
    (server, dir)
}

/// Extracts `(before, after)` from the `tracedecay_metrics:` line appended
/// to a tool response's content array.
fn parse_metrics_line(resp: &Value) -> Option<(u64, u64)> {
    let content = resp["result"]["content"].as_array()?;
    let line = content
        .iter()
        .filter_map(|item| item["text"].as_str())
        .find(|t| t.contains("tracedecay_metrics: before="))?;
    let tail = line.split("before=").nth(1)?;
    let (before, rest) = tail.split_once(' ')?;
    let after = rest.strip_prefix("after=")?;
    Some((before.trim().parse().ok()?, after.trim().parse().ok()?))
}

#[tokio::test]
// Intentional: serializes env-mutating savings tests; #[tokio::test]
// defaults to a current-thread runtime, so no executor thread blocks.
#[allow(clippy::await_holding_lock)]
async fn search_call_writes_savings_ledger_row() {
    let _env_guard = SAVINGS_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tmp_home = tempfile::tempdir().unwrap();
    let _env = isolated_savings_env(&tmp_home);

    let (server, proj_tmp) = setup_server().await;
    let server_handle = server.clone();
    let db_path = locked_global_db_path();

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(9001),
            "tools/call",
            json!({
                "name": "tracedecay_search",
                "arguments": { "query": "hello" }
            }),
        )],
    )
    .await;

    // Verify the request completed successfully.
    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 9001)
        .expect("should have a response for id=9001");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_null(), "search should not error");

    settled_ledger_total(&server_handle, &db_path, proj_tmp.path(), 1).await;
}

/// Regression test for the empty-ledger bug: the savings ledger must record
/// **by default**, with no env opt-in. The holographic-fact-store commit
/// made the global DB opt-in via `TRACEDECAY_ENABLE_GLOBAL_DB`, which
/// silently disabled ledger writes for every default MCP-server install
/// (dashboards showed "no events yet" while lifetime counters kept growing
/// through the ungated CLI paths).
#[tokio::test]
// Intentional: serializes env-mutating savings tests; #[tokio::test]
// defaults to a current-thread runtime, so no executor thread blocks.
#[allow(clippy::await_holding_lock)]
async fn ledger_records_by_default_without_env_opt_in() {
    let _env_guard = SAVINGS_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tmp_home = tempfile::tempdir().unwrap();
    let mut env = vec![EnvVarGuard::set("HOME", tmp_home.path().as_os_str())];
    #[cfg(target_os = "windows")]
    env.push(EnvVarGuard::set("USERPROFILE", tmp_home.path().as_os_str()));
    // Simulate a real (non-cargo) launch: neither the legacy opt-in nor the
    // cargo-test opt-out is present, so the default-on path is exercised.
    // Legacy `TOKENSAVE_*` spellings are still honored at runtime (and
    // `.cargo/config.toml` injects the legacy opt-out), so clear both.
    env.push(EnvVarGuard::unset("TRACEDECAY_ENABLE_GLOBAL_DB"));
    env.push(EnvVarGuard::unset("TRACEDECAY_DISABLE_GLOBAL_DB"));
    env.push(EnvVarGuard::unset("TOKENSAVE_ENABLE_GLOBAL_DB"));
    env.push(EnvVarGuard::unset("TOKENSAVE_DISABLE_GLOBAL_DB"));
    assert!(tracedecay::global_db::global_accounting_enabled());

    let (server, proj_tmp) = setup_server().await;
    let server_handle = server.clone();
    let db_path = locked_global_db_path();

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(9103),
            "tools/call",
            json!({
                "name": "tracedecay_search",
                "arguments": { "query": "hello" }
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 9103)
        .expect("should have a response for id=9103");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_null(), "search should not error");

    settled_ledger_total(&server_handle, &db_path, proj_tmp.path(), 1).await;
}

/// The explicit opt-outs must still work: a falsy
/// `TRACEDECAY_ENABLE_GLOBAL_DB` or a truthy `TRACEDECAY_DISABLE_GLOBAL_DB`
/// disables global accounting.
#[test]
fn global_accounting_env_overrides() {
    let _env_guard = SAVINGS_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    use tracedecay::global_db::{global_accounting_mode, AccountingMode};

    // Clear the legacy `TOKENSAVE_*` spellings too: they remain honored as a
    // runtime fallback, and `.cargo/config.toml` injects the legacy opt-out.
    let _clear_enable = EnvVarGuard::unset("TRACEDECAY_ENABLE_GLOBAL_DB");
    let _clear_disable = EnvVarGuard::unset("TRACEDECAY_DISABLE_GLOBAL_DB");
    let _clear_legacy_enable = EnvVarGuard::unset("TOKENSAVE_ENABLE_GLOBAL_DB");
    let _clear_legacy_disable = EnvVarGuard::unset("TOKENSAVE_DISABLE_GLOBAL_DB");
    assert_eq!(global_accounting_mode(), AccountingMode::Default);
    assert!(global_accounting_mode().enabled());

    {
        let _disable = EnvVarGuard::set("TRACEDECAY_DISABLE_GLOBAL_DB", std::ffi::OsStr::new("1"));
        assert_eq!(global_accounting_mode(), AccountingMode::DisabledByEnv);
        // An explicit enable wins over the opt-out (the cargo-test default).
        let _enable = EnvVarGuard::set("TRACEDECAY_ENABLE_GLOBAL_DB", std::ffi::OsStr::new("1"));
        assert_eq!(global_accounting_mode(), AccountingMode::EnabledByEnv);
    }

    let _enable_falsy = EnvVarGuard::set("TRACEDECAY_ENABLE_GLOBAL_DB", std::ffi::OsStr::new("0"));
    assert_eq!(global_accounting_mode(), AccountingMode::DisabledByEnv);
    assert!(!global_accounting_mode().enabled());
}

/// A full-file read returns the entire file to the agent, so it must not
/// credit the lifetime counters: net saving = before - after, clamped at
/// zero. Guards against the historical bug where the counters accumulated
/// the gross "before" estimate even when the response carried the whole file.
#[tokio::test]
// Intentional: serializes env-mutating savings tests; #[tokio::test]
// defaults to a current-thread runtime, so no executor thread blocks.
#[allow(clippy::await_holding_lock)]
async fn full_file_read_credits_zero_net_savings() {
    let _env_guard = SAVINGS_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tmp_home = tempfile::tempdir().unwrap();
    let _env = isolated_savings_env(&tmp_home);

    let (server, proj_tmp) = setup_savings_project().await;
    let server_handle = server.clone();
    let db_path = locked_global_db_path();
    let project_path = proj_tmp.path().to_path_buf();

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(9101),
            "tools/call",
            json!({
                "name": "tracedecay_read",
                "arguments": { "file": "src/main.rs", "mode": "full" }
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 9101)
        .expect("should have a response for id=9101");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_null(), "read should not error");

    // The metrics line must prove the raw estimate was real (before > 0)
    // and that a full read delivers at least as much as it "saves".
    let (before, after) = parse_metrics_line(&resp).expect("metrics line present");
    assert!(before > 0, "raw-file estimate should be nonzero");
    assert!(
        after >= before,
        "full-file response ({after}) should be at least the raw estimate ({before})"
    );

    let total = settled_ledger_total(&server_handle, &db_path, &project_path, 1).await;
    assert_eq!(
        total.saved_tokens, 0,
        "ledger must not count a full-file read as savings"
    );
    let db = tracedecay::global_db::GlobalDb::open_at(&db_path)
        .await
        .expect("global db opens at isolated path");
    assert_eq!(
        db.get_project_tokens(&project_path).await,
        0,
        "lifetime counter must not be credited with the gross before estimate"
    );
}

/// The lifetime counter and the ledger must agree: both credit the net
/// saving (before - after) per call, so after a single compressed call the
/// per-project counter equals the ledger total.
#[tokio::test]
// Intentional: serializes env-mutating savings tests; #[tokio::test]
// defaults to a current-thread runtime, so no executor thread blocks.
#[allow(clippy::await_holding_lock)]
async fn lifetime_counter_matches_ledger_net_savings() {
    let _env_guard = SAVINGS_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tmp_home = tempfile::tempdir().unwrap();
    let _env = isolated_savings_env(&tmp_home);

    let (server, proj_tmp) = setup_savings_project().await;
    let server_handle = server.clone();
    let db_path = locked_global_db_path();
    let project_path = proj_tmp.path().to_path_buf();

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(9102),
            "tools/call",
            json!({
                "name": "tracedecay_search",
                "arguments": { "query": "helper" }
            }),
        )],
    )
    .await;

    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == 9102)
        .expect("should have a response for id=9102");
    let resp = parse_response(resp_str);
    assert!(resp["error"].is_null(), "search should not error");

    let (before, after) = parse_metrics_line(&resp).expect("metrics line present");
    assert!(
        before > after,
        "compressed search should save tokens (before={before}, after={after})"
    );

    let total = settled_ledger_total(&server_handle, &db_path, &project_path, 1).await;
    assert_eq!(
        total.saved_tokens,
        before - after,
        "ledger net saving must match the metrics line"
    );
    let db = tracedecay::global_db::GlobalDb::open_at(&db_path)
        .await
        .expect("global db opens at isolated path");
    assert_eq!(
        db.get_project_tokens(&project_path).await,
        total.saved_tokens,
        "lifetime counter must equal the ledger's net saving, not the gross before"
    );
}

// ---------------------------------------------------------------------------
// Mid-session branch switch: tool calls must reopen onto the live branch's
// DB instead of serving the branch pinned at startup.
// ---------------------------------------------------------------------------

fn git(project: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("git failed to spawn");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Drives one `tools/call` of `tracedecay_search` through the JSON-RPC
/// transport and returns the full response text for the given id.
async fn search_via_transport(server: Arc<McpServer>, id: i64, query: &str) -> Value {
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(id),
            "tools/call",
            json!({ "name": "tracedecay_search", "arguments": { "query": query } }),
        )],
    )
    .await;
    let resp_str = responses
        .iter()
        .find(|r| parse_response(r)["id"] == id)
        .unwrap_or_else(|| panic!("no response for id={id}"));
    parse_response(resp_str)
}

#[tokio::test]
async fn tool_calls_reopen_branch_db_after_mid_session_checkout() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();

    // main: one committed source file, indexed into the default DB.
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn main_only() -> u32 { 1 }\n",
    )
    .unwrap();
    fs::write(project.join(".gitignore"), ".tracedecay/\n").unwrap();
    git(project, &["init"]);
    git(project, &["config", "user.email", "test@test.com"]);
    git(project, &["config", "user.name", "Test"]);
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "initial"]);
    git(project, &["branch", "-M", "main"]);

    {
        let cg = TraceDecay::init(project).await.unwrap();
        cg.index_all().await.unwrap();
        cg.checkpoint().await.unwrap();
    }

    // Track main + feature, seeding feature's DB from main's.
    let tracedecay_dir = project.join(".tracedecay");
    let mut meta = BranchMeta::new("main");
    meta.add_branch("feature", "branches/feature.db", "main");
    save_branch_meta(&tracedecay_dir, &meta).unwrap();
    fs::create_dir_all(tracedecay_dir.join("branches")).unwrap();
    fs::copy(
        tracedecay_dir.join("tracedecay.db"),
        tracedecay_dir.join("branches/feature.db"),
    )
    .unwrap();

    // feature: add a feature-only symbol and index it into feature's DB.
    git(project, &["checkout", "-b", "feature"]);
    fs::write(
        project.join("src/feat.rs"),
        "pub fn feature_only() -> u32 { 2 }\n",
    )
    .unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-m", "feature work"]);
    {
        let cg = TraceDecay::open(project).await.unwrap();
        assert_eq!(cg.serving_branch(), Some("feature"));
        cg.sync().await.unwrap();
        cg.checkpoint().await.unwrap();
    }

    // Back on main: start the server pinned to main's DB.
    git(project, &["checkout", "main"]);
    let cg = TraceDecay::open(project).await.unwrap();
    assert_eq!(cg.serving_branch(), Some("main"));
    let server = McpServer::new(cg, None).await;
    assert!(
        server
            .wait_for_startup_catch_up(std::time::Duration::from_secs(30))
            .await,
        "startup catch-up must settle before the mid-test checkout"
    );

    // While on main, the feature-only symbol must be invisible.
    let resp = search_via_transport(server.clone(), 1, "feature_only").await;
    assert!(resp["error"].is_null(), "search on main should not error");
    assert!(
        !resp["result"]["content"]
            .to_string()
            .contains("feature_only"),
        "main's DB must not contain the feature-only symbol"
    );

    // Mid-session checkout. The next tool call must detect the drift,
    // reopen onto feature's DB, and serve the feature-only symbol.
    git(project, &["checkout", "feature"]);
    let resp = search_via_transport(server.clone(), 2, "feature_only").await;
    assert!(
        resp["error"].is_null(),
        "search after checkout should not error: {resp}"
    );
    assert!(
        resp["result"]["content"]
            .to_string()
            .contains("feature_only"),
        "after the checkout, reads must serve the feature branch's DB: {resp}"
    );

    let cg_now = server.cg().await;
    assert_eq!(
        cg_now.serving_branch(),
        Some("feature"),
        "the served instance must have been swapped onto the live branch"
    );
    assert!(
        !cg_now.branch_drifted(),
        "drift must be cleared after the reopen"
    );
}

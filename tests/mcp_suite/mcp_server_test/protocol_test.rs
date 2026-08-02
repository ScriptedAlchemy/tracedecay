use crate::mcp_server_test::support::*;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::handle_tool_call;
use tracedecay::mcp::response_handles::{
    RESPONSE_HANDLE_TTL_SECS, cleanup_expired_response_handles, store_response_handle,
};
use tracedecay::storage::resolve_response_handle_root;
use tracedecay::tracedecay::{TraceDecay, current_timestamp};

fn initialize_protocol_fixture(project: &Path, module: &str) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(project.join("src/lib.rs"), format!("pub mod {module};\n")).unwrap();
    fs::write(
        project.join(format!("src/{module}.rs")),
        format!("pub fn {module}_marker() {{}}\n"),
    )
    .unwrap();
    for args in [
        &["init", "--quiet"][..],
        &["add", "."][..],
        &[
            "-c",
            "user.name=TraceDecay Tests",
            "-c",
            "user.email=tests@tracedecay.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(project)
                .status()
                .unwrap()
                .success()
        );
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
    assert_eq!(
        resp["result"]["capabilities"]["tools"]["listChanged"],
        json!(true)
    );
    assert_eq!(resp["result"]["serverInfo"]["name"], "tracedecay");
    assert!(resp["result"]["serverInfo"]["version"].is_string());
}

#[tokio::test]
async fn initialize_roots_route_registered_reader_tools_without_explicit_selector() {
    let isolation = TempDir::new().unwrap();
    let active_project = isolation.path().join("active-project");
    let target_project = isolation.path().join("target-project");
    initialize_protocol_fixture(&active_project, "active");
    initialize_protocol_fixture(&target_project, "target");
    let harness = ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [active_project.clone(), target_project.clone()],
    )
    .await
    .unwrap();
    let server = harness.server(&active_project).unwrap();
    let target_root_uri = url::Url::from_file_path(&target_project)
        .expect("target project has a portable file URI")
        .to_string();

    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(
                json!(1),
                "initialize",
                json!({
                    "clientInfo": {"name": "codex", "version": "test"},
                    "roots": [{"uri": target_root_uri, "name": "target-project"}]
                }),
            ),
            jsonrpc_request(
                json!(2),
                "tools/call",
                json!({
                    "name": "tracedecay_files",
                    "arguments": {"layout": "flat"}
                }),
            ),
        ],
    )
    .await;

    let files_response = response_with_id(&responses, json!(2));
    let text = files_response["result"]["content"][0]["text"]
        .as_str()
        .expect("files response text");
    assert!(
        text.contains("src/target.rs"),
        "initialize root should route reader tools to target project, got {text}"
    );
    assert!(
        !text.contains("src/active.rs"),
        "implicit initialize-root routing should not read the active project: {text}"
    );
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
    let has_helper = content
        .iter()
        .any(|c| c["text"].as_str().is_some_and(|t| t.contains("helper")));
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
                    "new_str": "fn replaced() {}",
                    "dry_run": true,
                    "format": "json"
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
        text.contains("## error") && text.contains("**kind:** git"),
        "expected rendered changelog git failure, got: {text}"
    );
}

// ---------------------------------------------------------------------------
// 6b. test_tools_call_timings_flag
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_tools_call_timings_enabled_by_default() {
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
    let dur = resp["result"]["_meta"]["duration_us"]
        .as_u64()
        .expect("duration_us must be present by default");
    assert!(
        dur < 5_000_000,
        "duration_us should be well under 5 s, got {dur}"
    );
}

#[tokio::test]
async fn test_tools_call_timings_can_be_disabled() {
    let (server, _dir) = setup_server().await;
    server.set_timings_enabled(false);
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
    assert!(
        resp["result"]["_meta"]["duration_us"].is_null(),
        "duration_us must NOT be present when timings are disabled — got {}",
        resp["result"]["_meta"]
    );
}

/// The CLI and the stdio proxy shut down their write half as soon as the
/// request is on the wire, so a live-cancellable tool must still be answered
/// after end-of-input rather than being cancelled with no response.
#[tokio::test]
async fn cancellable_tool_call_is_answered_after_client_half_close() {
    struct HalfClosedTransport {
        request: Option<String>,
        written: Vec<String>,
    }

    impl tracedecay::mcp::transport::McpTransport for HalfClosedTransport {
        async fn read_line(&mut self) -> std::io::Result<Option<String>> {
            Ok(self.request.take())
        }

        async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
            self.written.push(line.to_string());
            Ok(())
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let (server, _dir) = setup_server().await;
    let mut transport = HalfClosedTransport {
        request: Some(jsonrpc_request(
            json!(41),
            "tools/call",
            json!({"name": "tracedecay_search", "arguments": {"query": "helper"}}),
        )),
        written: Vec::new(),
    };

    server
        .run_connection(&mut transport)
        .await
        .expect("half-closed connection should end cleanly");

    let resp = transport
        .written
        .iter()
        .map(|line| parse_response(line.trim()))
        .find(|resp| resp["id"] == 41)
        .expect("half-closed client must still receive its response");
    assert!(
        extract_tool_text(&resp["result"]).contains("helper"),
        "expected search results, got: {resp}"
    );
}

/// A full peer close is distinct from the write-half close above. Once the
/// transport reports HUP, an in-flight handler is dropped without a response
/// so its daemon admission permit cannot remain pinned.
#[tokio::test]
async fn cancellable_tool_call_is_dropped_on_full_peer_close() {
    struct FullClosedTransport {
        requests: std::collections::VecDeque<String>,
        written: Vec<String>,
    }

    impl tracedecay::mcp::transport::McpTransport for FullClosedTransport {
        async fn read_line(&mut self) -> std::io::Result<Option<String>> {
            Ok(self.requests.pop_front())
        }

        async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
            self.written.push(line.to_string());
            Ok(())
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        async fn peer_fully_closed_after_eof(&self) {}
    }

    let (server, _dir) = setup_server().await;
    let mut requests = std::collections::VecDeque::from([jsonrpc_request(
        json!(44),
        "tools/call",
        json!({"name": "tracedecay_search", "arguments": {"query": "helper"}}),
    )]);
    // Keep the read side busy long enough for the in-flight search to reach
    // its cancellation point before EOF reports the full close.
    requests.extend((0..8).map(|_| jsonrpc_notification("notifications/progress")));
    let mut transport = FullClosedTransport {
        requests,
        written: Vec::new(),
    };

    server
        .run_connection(&mut transport)
        .await
        .expect("full peer close should end cleanly");
    assert!(
        transport.written.is_empty(),
        "full peer close must drop the in-flight handler, got {:?}",
        transport.written
    );
}

/// The same full-close path must release a handler that is not in the live
/// cancellation allow-list; this branch used to await it without observing
/// the transport at all.
#[tokio::test]
async fn non_cancellable_tool_call_is_dropped_on_full_peer_close() {
    struct FullClosedTransport {
        requests: std::collections::VecDeque<String>,
        written: Vec<String>,
    }

    impl tracedecay::mcp::transport::McpTransport for FullClosedTransport {
        async fn read_line(&mut self) -> std::io::Result<Option<String>> {
            Ok(self.requests.pop_front())
        }

        async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
            self.written.push(line.to_string());
            Ok(())
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        async fn peer_fully_closed_after_eof(&self) {}
    }

    let (server, _dir) = setup_server().await;
    let mut requests = std::collections::VecDeque::from([jsonrpc_request(
        json!(45),
        "tools/call",
        json!({"name": "tracedecay_status", "arguments": {}}),
    )]);
    requests.extend((0..8).map(|_| jsonrpc_notification("notifications/progress")));
    let mut transport = FullClosedTransport {
        requests,
        written: Vec::new(),
    };

    server
        .run_connection(&mut transport)
        .await
        .expect("full peer close should end cleanly");
    assert!(
        transport.written.is_empty(),
        "full peer close must drop the non-cancellable handler, got {:?}",
        transport.written
    );
}

/// A hard peer-loss read error during an in-flight cancellable `tools/call`
/// must cancel the request and fail the connection without writing a response.
#[tokio::test]
async fn cancellable_tool_call_is_cancelled_on_peer_read_failure() {
    struct PeerLossTransport {
        request: Option<String>,
        written: Vec<String>,
    }

    impl tracedecay::mcp::transport::McpTransport for PeerLossTransport {
        async fn read_line(&mut self) -> std::io::Result<Option<String>> {
            match self.request.take() {
                Some(line) => Ok(Some(line)),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "peer lost during tools/call",
                )),
            }
        }

        async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
            self.written.push(line.to_string());
            Ok(())
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let (server, _dir) = setup_server().await;
    let mut transport = PeerLossTransport {
        request: Some(jsonrpc_request(
            json!(42),
            "tools/call",
            json!({"name": "tracedecay_search", "arguments": {"query": "helper"}}),
        )),
        written: Vec::new(),
    };

    let err = server
        .run_connection(&mut transport)
        .await
        .expect_err("peer read failure should fail the connection");
    assert!(
        err.to_string().contains("peer lost during tools/call"),
        "unexpected error: {err}"
    );
    assert!(
        transport.written.is_empty(),
        "peer-loss cancellation must not write a tools/call response, got {:?}",
        transport.written
    );
}

/// A write-side peer loss after the request has been accepted must fail the
/// connection rather than pretending the response was delivered.
#[tokio::test]
async fn cancellable_tool_call_fails_connection_on_peer_write_failure() {
    struct WriteFailTransport {
        request: Option<String>,
    }

    impl tracedecay::mcp::transport::McpTransport for WriteFailTransport {
        async fn read_line(&mut self) -> std::io::Result<Option<String>> {
            Ok(self.request.take())
        }

        async fn write_line(&mut self, _line: &str) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "peer write half gone",
            ))
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let (server, _dir) = setup_server().await;
    let mut transport = WriteFailTransport {
        request: Some(jsonrpc_request(
            json!(43),
            "tools/call",
            json!({"name": "tracedecay_search", "arguments": {"query": "helper"}}),
        )),
    };

    let err = server
        .run_connection(&mut transport)
        .await
        .expect_err("peer write failure should fail the connection");
    assert!(
        err.to_string().contains("peer write half gone"),
        "unexpected error: {err}"
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
        vec![
            serde_json::to_string(&json!({
                "jsonrpc": "2.0",
                "id": 50,
                "method": "tools/call"
            }))
            .unwrap(),
        ],
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

#[tokio::test]
async fn test_tracedecay_retrieve_missing_handle_argument_is_invalid_params_with_reason_code() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(61),
            "tools/call",
            json!({
                "name": "tracedecay_retrieve",
                "arguments": {}
            }),
        )],
    )
    .await;

    let resp = response_with_id(&responses, json!(61));
    assert_eq!(resp["error"]["code"], -32602);
    assert_eq!(
        resp["error"]["data"]["reason_code"],
        "missing_handle_argument"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("requires the `handle` argument")
    );
}

#[tokio::test]
async fn test_tracedecay_retrieve_invalid_handle_is_invalid_params_with_reason_code() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(62),
            "tools/call",
            json!({
                "name": "tracedecay_retrieve",
                "arguments": { "handle": "bogus" }
            }),
        )],
    )
    .await;

    let resp = response_with_id(&responses, json!(62));
    assert_eq!(resp["error"]["code"], -32602);
    assert_eq!(resp["error"]["data"]["reason_code"], "invalid_handle");
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid response handle")
    );
}

#[tokio::test]
async fn test_tracedecay_retrieve_corrupt_handle_record_returns_actionable_internal_error() {
    let (server, _dir) = setup_server().await;
    let cg = server.cg().await;
    let stored =
        store_response_handle(cg.project_root(), "{\"items\":[1]}", current_timestamp()).unwrap();
    fs::write(
        response_handle_dir(&cg).join(format!("{}.json", stored.handle)),
        "{not-json",
    )
    .unwrap();

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(63),
            "tools/call",
            json!({
                "name": "tracedecay_retrieve",
                "arguments": { "handle": stored.handle }
            }),
        )],
    )
    .await;

    let resp = response_with_id(&responses, json!(63));
    assert_eq!(resp["error"]["code"], -32603);
    assert_eq!(
        resp["error"]["data"]["reason_code"],
        "corrupt_handle_record"
    );
    assert_eq!(resp["error"]["data"]["retryable"], true);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cached response handle record is unreadable")
    );
}

#[tokio::test]
async fn test_tracedecay_retrieve_handle_read_failure_returns_actionable_internal_error() {
    let (server, _dir) = setup_server().await;
    let cg = server.cg().await;
    let stored =
        store_response_handle(cg.project_root(), "{\"items\":[2]}", current_timestamp()).unwrap();
    let handle_path = response_handle_dir(&cg).join(format!("{}.json", stored.handle));
    fs::remove_file(&handle_path).unwrap();
    fs::create_dir(&handle_path).unwrap();

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(64),
            "tools/call",
            json!({
                "name": "tracedecay_retrieve",
                "arguments": { "handle": stored.handle }
            }),
        )],
    )
    .await;

    let resp = response_with_id(&responses, json!(64));
    assert_eq!(resp["error"]["code"], -32603);
    assert_eq!(resp["error"]["data"]["reason_code"], "handle_read_failed");
    assert_eq!(resp["error"]["data"]["retryable"], true);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("failed to read cached response handle")
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
    assert!(stats["method_call_counts"].is_object());
    assert!(stats["resource_read_counts"].is_object());
    assert_eq!(stats["ratios"]["tool_calls_per_jsonrpc_message"], 0.0);
}

#[tokio::test]
async fn test_server_stats_include_response_handle_metrics() {
    let (_env, _active_project) = crate::common::IsolatedEnv::acquire().await;
    let (server, _dir) = setup_server().await;
    let baseline = server.server_stats_json().await;
    let baseline_handles = &baseline["response_handles"];
    let baseline_counter = |key: &str| baseline_handles[key].as_u64().unwrap_or(0);

    let cg = server.cg().await;
    let mut last_fact_id = None;
    for i in 0..35 {
        let added = handle_tool_call(
            &cg,
            "tracedecay_fact_store",
            json!({
                "action": "add",
                "content": format!(
                    "SERVER_STATS_HANDLE_METRIC_{i:02}: {}",
                    "response handle telemetry should survive truncation ".repeat(80)
                ),
                "category": "project",
                "trust": 0.9,
                "format": "json"
            }),
            None,
            None,
        )
        .await
        .unwrap();
        if i == 34 {
            let added: Value = serde_json::from_str(extract_tool_text(&added.value)).unwrap();
            last_fact_id = added["fact"]["fact_id"].as_i64();
        }
    }

    let listed = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "list",
            "category": "project",
            "min_trust": 0.0,
            "limit": 200,
            "format": "json"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let envelope: Value = serde_json::from_str(extract_tool_text(&listed.value)).unwrap();
    let handle = envelope["handle"]
        .as_str()
        .expect("retrieve handle")
        .to_string();

    let retrieved = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": handle, "format": "json" }),
        None,
        None,
    )
    .await
    .unwrap();
    let retrieved_payload: Value =
        serde_json::from_str(extract_tool_text(&retrieved.value)).unwrap();
    assert_eq!(retrieved_payload["expired"], false);

    let missing = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({
            "handle": "rh_0123456789abcdef01234567",
            "format": "json"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let missing_payload: Value = serde_json::from_str(extract_tool_text(&missing.value)).unwrap();
    assert_eq!(missing_payload["reason_code"], "handle_not_found");

    let expired = store_response_handle(
        cg.project_root(),
        "{\"expired\":true}",
        current_timestamp() - RESPONSE_HANDLE_TTL_SECS - 5,
    )
    .unwrap();
    let expired_result = handle_tool_call(
        &cg,
        "tracedecay_retrieve",
        json!({ "handle": expired.handle, "format": "json" }),
        None,
        None,
    )
    .await
    .unwrap();
    let expired_payload: Value =
        serde_json::from_str(extract_tool_text(&expired_result.value)).unwrap();
    assert_eq!(expired_payload["reason_code"], "handle_expired");

    let broken =
        store_response_handle(cg.project_root(), "{\"broken\":true}", current_timestamp()).unwrap();
    let broken_path = response_handle_dir(&cg).join(format!("{}.json", broken.handle));
    fs::remove_file(&broken_path).unwrap();
    fs::create_dir(&broken_path).unwrap();
    assert!(
        handle_tool_call(
            &cg,
            "tracedecay_retrieve",
            json!({ "handle": broken.handle }),
            None,
            None,
        )
        .await
        .is_err(),
        "broken handle fixture should increment retrieve failure telemetry"
    );

    assert!(
        store_response_handle(cg.project_root(), "{\"expires\":true}", current_timestamp()).is_ok(),
        "direct store should succeed so cleanup has something to expire"
    );
    let expired_removed = cleanup_expired_response_handles(
        cg.project_root(),
        current_timestamp() + RESPONSE_HANDLE_TTL_SECS + 1,
    )
    .unwrap();
    assert!(
        expired_removed >= 1,
        "cleanup should remove at least one expired handle"
    );

    let failure_root = TempDir::new().unwrap();
    let failure_handle_root = resolve_response_handle_root(failure_root.path()).unwrap();
    fs::create_dir_all(failure_handle_root.parent().unwrap()).unwrap();
    fs::write(&failure_handle_root, "not-a-directory").unwrap();
    assert!(
        store_response_handle(
            failure_root.path(),
            "store failure telemetry",
            current_timestamp()
        )
        .is_err(),
        "store failure fixture should increment failure telemetry"
    );

    let after = server.server_stats_json().await;
    let handles = &after["response_handles"];
    assert!(
        handles.is_object(),
        "server stats should include response_handles section"
    );
    assert!(
        handles["truncation_total"].as_u64().unwrap_or(0) > baseline_counter("truncation_total")
    );
    assert!(
        handles["store_attempts"].as_u64().unwrap_or(0) >= baseline_counter("store_attempts") + 2
    );
    assert!(handles["store_success"].as_u64().unwrap_or(0) > baseline_counter("store_success"));
    assert!(handles["store_failures"].as_u64().unwrap_or(0) > baseline_counter("store_failures"));
    assert!(handles["retrieve_hits"].as_u64().unwrap_or(0) > baseline_counter("retrieve_hits"));
    assert!(handles["retrieve_misses"].as_u64().unwrap_or(0) > baseline_counter("retrieve_misses"));
    assert!(
        handles["retrieve_expired"].as_u64().unwrap_or(0) > baseline_counter("retrieve_expired")
    );
    assert!(
        handles["retrieve_failures"].as_u64().unwrap_or(0) > baseline_counter("retrieve_failures")
    );
    assert!(
        handles["cleanup_removed_expired_total"]
            .as_u64()
            .unwrap_or(0)
            >= baseline_counter("cleanup_removed_expired_total") + expired_removed as u64
    );
    assert!(
        handles["on_disk"]["file_count"].is_number(),
        "response handle cache stats should expose on-disk file counts"
    );

    if let Some(fact_id) = last_fact_id {
        let _ = handle_tool_call(
            &cg,
            "tracedecay_fact_store",
            json!({ "action": "remove", "fact_id": fact_id }),
            None,
            None,
        )
        .await
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// 15. test_server_stats_after_run (indirect via tracedecay_status response)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_server_stats_after_run() {
    let (server, _dir) = setup_server().await;
    let server_handle = server.clone();
    // Send several requests then a tracedecay_status to check stats are embedded.
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(200), "initialize", json!({})),
            jsonrpc_request(json!(201), "ping", json!({})),
            jsonrpc_request(
                json!(203),
                "resources/read",
                json!({
                    "uri": "tracedecay://status"
                }),
            ),
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

    let stats = server_handle.server_stats_json().await;
    assert_eq!(stats["jsonrpc_messages"], 4);
    assert_eq!(stats["method_call_counts"]["initialize"], 1);
    assert_eq!(stats["method_call_counts"]["ping"], 1);
    assert_eq!(stats["method_call_counts"]["resources/read"], 1);
    assert_eq!(stats["method_call_counts"]["tools/call"], 1);
    assert_eq!(stats["resource_read_counts"]["tracedecay://status"], 1);
    assert_eq!(stats["tool_call_counts"]["tracedecay_status"], 1);
    assert_eq!(stats["ratios"]["tool_calls_per_jsonrpc_message"], 0.25);
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
                    "arguments": { "format": "json" }
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
    let payload: Value = serde_json::from_str(&text).expect("status result JSON");
    assert!(
        payload["server"]["errors"].as_u64().unwrap_or(0) >= 1,
        "errors should be at least 1 after sending unknown method: {payload}"
    );
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
///
/// One server session carries all eight requests: level changes are a
/// mid-session operation, and building a fresh server per level only
/// multiplied fixture cost (8x setup dominated this test's runtime) without
/// adding coverage.
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
    let (server, _dir) = setup_server().await;
    let requests = levels
        .iter()
        .enumerate()
        .map(|(idx, level)| {
            jsonrpc_request(
                json!(600 + idx as u64),
                "logging/setLevel",
                json!({"level": level}),
            )
        })
        .collect();
    let responses = run_server_with_messages(server, requests).await;
    for (idx, level) in levels.iter().enumerate() {
        let id = json!(600 + idx as u64);
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
    let (_env, _active_project) = crate::common::IsolatedEnv::acquire().await;
    let (server, dir) = setup_server_with_session_authority().await;
    let lcm_status_call = |id: i64| {
        jsonrpc_request(
            json!(id),
            "tools/call",
            json!({
                "name": "tracedecay_lcm_status",
                "arguments": { "format": "json" }
            }),
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
                "arguments": {
                    "provider": "codex",
                    "session_id": "migration-rerun-probe"
                }
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
            "lcm_session_boundary should not error: {resp}"
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
    let project_id = project_id_of(&TraceDecay::open(dir.path()).await.unwrap());
    let profile_root = tracedecay::storage::default_profile_root().unwrap();
    let runtime = tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::project(
        &profile_root,
        dir.path(),
        project_id.clone(),
    )
    .await
    .unwrap();
    runtime
        .set_lcm_schema_migration_applied_at_for_test(
            tracedecay::application::host_admission::HostAdmissionScope::Project,
            123,
        )
        .await
        .unwrap();
    assert_eq!(
        runtime
            .lcm_schema_migration_applied_at_for_test(
                tracedecay::application::host_admission::HostAdmissionScope::Project,
            )
            .await
            .unwrap(),
        Some(123)
    );
    drop(runtime);

    // A second serve session over the same project in the same process.
    // The write-path boundary call is the one that would re-run migrations
    // if the per-process ensured cache failed; the status reads must also
    // keep working.
    let server = server_with_session_authority(dir.path()).await;
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
    for id in [2_i64, 3] {
        let resp = response_with_id(&responses, json!(id));
        let payload: Value =
            serde_json::from_str(successful_tool_text(&resp, "second-session lcm_status")).unwrap();
        assert_eq!(
            payload["status"], "ok",
            "second-session lcm_status id={id} payload"
        );
    }
    let runtime = tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::project(
        &profile_root,
        dir.path(),
        project_id,
    )
    .await
    .unwrap();
    assert_eq!(
        runtime
            .lcm_schema_migration_applied_at_for_test(
                tracedecay::application::host_admission::HostAdmissionScope::Project,
            )
            .await
            .unwrap(),
        Some(123),
        "repeated serve-mode LCM calls must not re-run the LCM migrations"
    );
}

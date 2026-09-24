use crate::mcp_server_test::support::*;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::McpServer;
use tracedecay_mcp::response_handles::{
    RESPONSE_HANDLE_TTL_SECS, cleanup_expired_response_handles, store_response_handle,
};
use tracedecay_runtime_core::storage::resolve_response_handle_root;
use tracedecay_runtime_core::tracedecay::current_timestamp;

/// Logging is deprecated by MCP SEP-2577 and the server emits no log
/// notifications, so the advertised capabilities are exactly tools and
/// resources: `initialize` must not invite `logging/setLevel`.
#[tokio::test]
async fn test_initialize() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(server, vec![spec_initialize_request(json!(1))]).await;

    assert_eq!(responses.len(), 1, "{responses:?}");
    let resp = parse_response(&responses[0]);
    assert_eq!(resp["id"], 1);
    assert_eq!(
        resp["result"]["protocolVersion"], INITIALIZE_PROTOCOL_VERSION,
        "rmcp keeps the client's supported protocol version: {resp}"
    );
    assert_eq!(
        resp["result"]["capabilities"],
        json!({"tools": {"listChanged": true}, "resources": {}}),
        "{resp}"
    );
    assert!(
        resp["result"]["capabilities"].get("logging").is_none(),
        "initialize must not advertise logging: {resp}"
    );
    assert_eq!(
        resp["result"]["serverInfo"],
        json!({
            "name": "tracedecay",
            "version": tracedecay_project::version::build_version().unwrap()
        })
    );
}

#[tokio::test]
async fn legacy_project_selectors_are_rejected_instead_of_rerouting() {
    assert_legacy_selectors_cannot_reroute_the_active_project().await;
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
    assert_eq!(
        parse_response(&responses[0]),
        json!({"jsonrpc": "2.0", "id": 901, "result": {}})
    );
}

/// MCP forbids a null request id. It is still answered, with a typed
/// `InvalidRequest` carrying the null id, never dropped as a notification.
fn assert_null_id_refused(responses: &[String], label: &str) {
    assert_eq!(responses.len(), 1, "{label}: {responses:?}");
    assert_eq!(
        parse_response(&responses[0]),
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {
                "code": -32600,
                "message": "invalid JSON-RPC request: MCP request id must be a string or number, not null"
            }
        }),
        "{label}"
    );
}

#[tokio::test]
async fn test_explicit_null_id_is_refused_as_invalid_request() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(null), "ping", json!({}))],
    )
    .await;
    assert_null_id_refused(&responses, "ping with id=null");
}

#[tokio::test]
async fn test_tools_call_explicit_null_id_is_refused_before_dispatch() {
    let (server, _dir) = setup_server().await;
    let stats_view = Arc::clone(&server);
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
    assert_null_id_refused(&responses, "tools/call with id=null");
    assert_eq!(
        stats_view.server_stats_json().await["tool_calls"],
        0,
        "a refused null-id tools/call must not execute"
    );
}

/// The advertised schema is the contract a host validates arguments against,
/// so the listed `tracedecay_retrieve` schema is pinned whole and then called
/// with arguments it admits, against a handle stored in the fixture project.
#[tokio::test]
async fn test_tools_list() {
    let (server, _dir) = setup_server().await;
    let now = current_timestamp();
    let stored =
        store_response_handle(server.cg().await.project_root(), "{\"items\":[1,2,3]}", now)
            .unwrap();
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(20), "tools/list", json!({})),
            jsonrpc_request(
                json!(21),
                "tools/call",
                json!({
                    "name": "tracedecay_retrieve",
                    "arguments": {
                        "handle": stored.handle,
                        "offset": 2,
                        "max_chars": 5,
                        "format": "json"
                    }
                }),
            ),
        ],
    )
    .await;

    let listed = response_with_id(&responses, json!(20));
    let retrieve = listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result: {listed}"))
        .iter()
        .find(|tool| tool["name"] == "tracedecay_retrieve")
        .unwrap_or_else(|| panic!("tracedecay_retrieve must be listed: {listed}"));
    assert_eq!(
        retrieve["inputSchema"],
        json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "string",
                    "description": "The required `handle` argument copied exactly from a truncated MCP response envelope."
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Character offset into the immutable stored response. Use the prior page's next_offset."
                },
                "max_chars": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 15000,
                    "description": "Maximum characters requested for this page. Values above the safe response-frame budget are clamped."
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "json"],
                    "description": "Output format. Default 'markdown' (compact, LLM-optimized; no tables). 'json' for machine-readable output."
                },
                "project_selector": {
                    "type": "object",
                    "description": "Optional registered project selector. Omit to use the active project.",
                    "properties": {
                        "project_id": {
                            "type": "string",
                            "description": "Registered project id to query."
                        }
                    },
                    "required": ["project_id"],
                    "additionalProperties": false
                }
            },
            "required": ["handle"],
            "additionalProperties": false
        })
    );

    let called = response_with_id(&responses, json!(21));
    let page: Value = serde_json::from_str(successful_tool_text(&called, "tracedecay_retrieve"))
        .expect("retrieve page JSON");
    assert_eq!(
        page,
        json!({
            "handle": stored.handle,
            "expired": false,
            "original_chars": 17,
            "total_chars": 17,
            "offset": 2,
            "next_offset": 7,
            "has_more": true,
            "created_at": now,
            "expires_at": now + RESPONSE_HANDLE_TTL_SECS,
            "content": "items"
        })
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn test_tools_call_search() {
    // Search lanes are served by the daemon-owned code-index authority; a
    // server outside the production composition has no executor and answers
    // typed-unavailable, so this protocol journey runs through the real
    // composition (the same cutover migration as `search_large_response`).
    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    crate::support::warm_code_index_search(&server, "helper").await;
    let responses = run_server_with_messages(
        std::sync::Arc::clone(&server),
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
    let has_helper = content
        .iter()
        .any(|c| c["text"].as_str().is_some_and(|t| t.contains("helper")));
    assert!(has_helper, "search results should contain 'helper'");
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn test_tools_call_semantic_failure_sets_is_error() {
    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let responses = run_server_with_messages(
        std::sync::Arc::clone(&server),
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
    fixture.harness.shutdown().await;
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

#[tokio::test]
async fn test_tools_call_timings_enabled_by_default() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(31),
            "tools/call",
            json!({"name": "tracedecay_status", "arguments": {"admission_only": true}}),
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
    let (server, dir) = setup_server().await;
    server.set_timings_enabled(false);
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(32),
            "tools/call",
            json!({
                "name": "tracedecay_status",
                "arguments": {"admission_only": true, "format": "json"}
            }),
        )],
    )
    .await;
    let resp = response_with_id(&responses, json!(32));
    let payload: Value =
        serde_json::from_str(successful_tool_text(&resp, "status")).expect("status result JSON");
    assert_eq!(payload["project_admitted"], true, "{payload}");
    assert_eq!(
        payload["project_root"],
        json!(dir.path().canonicalize().unwrap()),
        "{payload}"
    );
    assert!(
        resp["result"]["_meta"]["duration_us"].is_null(),
        "duration_us must NOT be present when timings are disabled, got {}",
        resp["result"]["_meta"]
    );
}

/// The CLI and the stdio proxy shut down their write half as soon as the
/// request is on the wire, so a live-cancellable tool must still be answered
/// after end-of-input rather than being cancelled with no response.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn cancellable_tool_call_is_answered_after_client_half_close() {
    struct HalfClosedTransport {
        request: Option<String>,
        written: Vec<String>,
    }

    impl tracedecay_mcp::transport::McpTransport for HalfClosedTransport {
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

    // Search lanes require the daemon-owned code-index authority, so the
    // half-close journey runs against the production composition's server.
    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    crate::support::warm_code_index_search(&server, "helper").await;
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
    fixture.harness.shutdown().await;
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

    impl tracedecay_mcp::transport::McpTransport for FullClosedTransport {
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

        fn peer_fully_closed_after_eof(
            &self,
        ) -> impl std::future::Future<Output = ()> + Send + 'static {
            std::future::ready(())
        }
    }

    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    crate::support::warm_code_index_search(&server, "helper").await;
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
    fixture.harness.shutdown().await;
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

    impl tracedecay_mcp::transport::McpTransport for FullClosedTransport {
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

        fn peer_fully_closed_after_eof(
            &self,
        ) -> impl std::future::Future<Output = ()> + Send + 'static {
            std::future::ready(())
        }
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

    impl tracedecay_mcp::transport::McpTransport for PeerLossTransport {
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

    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    crate::support::warm_code_index_search(&server, "helper").await;
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
    fixture.harness.shutdown().await;
}

/// A write-side peer loss after the request has been accepted must fail the
/// connection rather than pretending the response was delivered.
#[tokio::test]
async fn cancellable_tool_call_fails_connection_on_peer_write_failure() {
    struct WriteFailTransport {
        request: Option<String>,
    }

    impl tracedecay_mcp::transport::McpTransport for WriteFailTransport {
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

    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    crate::support::warm_code_index_search(&server, "helper").await;
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
    fixture.harness.shutdown().await;
}

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
                "arguments": { "format": "json" }
            }),
        )],
    )
    .await;

    let resp = response_with_id(&responses, json!(40));
    let payload: Value =
        serde_json::from_str(successful_tool_text(&resp, "status")).expect("status result JSON");
    assert_eq!(payload["graph_statistics"]["state"], "unavailable");
    assert_eq!(
        payload["graph_statistics"]["reason"],
        "authority_unavailable"
    );
    assert_eq!(
        payload["code_index_freshness"]["reason"], "code_index_scheduler_authority_not_attached",
        "a direct protocol server must report the missing scheduler authority truthfully: {payload}"
    );
}

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

    assert_eq!(responses.len(), 1, "{responses:?}");
    assert_eq!(
        parse_response(&responses[0]),
        json!({
            "jsonrpc": "2.0",
            "id": 50,
            "error": {"code": -32602, "message": "missing params for tools/call"}
        })
    );
}

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

    assert_eq!(
        response_with_id(&responses, json!(60))["error"],
        json!({"code": -32602, "message": "missing 'name' in tools/call params"})
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

    assert_eq!(
        response_with_id(&responses, json!(61))["error"],
        json!({
            "code": -32602,
            "message": "tracedecay_retrieve requires the `handle` argument copied from a truncated MCP response envelope.",
            "data": {
                "tool": "tracedecay_retrieve",
                "reason_code": "missing_handle_argument",
                "retryable": false,
                "retry_instruction": "Call `tracedecay_retrieve` again with the exact `handle` value emitted by the truncated response envelope."
            }
        })
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

    assert_eq!(
        response_with_id(&responses, json!(62))["error"],
        json!({
            "code": -32602,
            "message": "invalid response handle: expected `rh_` followed by 24 hex characters copied from a truncated MCP response envelope",
            "data": {
                "tool": "tracedecay_retrieve",
                "reason_code": "invalid_handle",
                "retryable": false,
                "retry_instruction": "Pass the exact `handle` string from a truncated MCP response envelope; do not shorten or edit it."
            }
        })
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

    assert_eq!(
        response_with_id(&responses, json!(63))["error"],
        json!({
            "code": -32603,
            "message": "tool execution failed: cached response handle record is unreadable.",
            "data": {
                "tool": "tracedecay_retrieve",
                "reason_code": "corrupt_handle_record",
                "retryable": true,
                "retry_instruction": "Re-run the original MCP tool in this project to regenerate the full response and a fresh handle."
            }
        })
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

    assert_eq!(
        response_with_id(&responses, json!(64))["error"],
        handle_read_failed_error()
    );
}

fn handle_read_failed_error() -> Value {
    json!({
        "code": -32603,
        "message": "tool execution failed: failed to read cached response handle.",
        "data": {
            "tool": "tracedecay_retrieve",
            "reason_code": "handle_read_failed",
            "retryable": true,
            "retry_instruction": "Fix the local project cache/filesystem issue, then re-run the original MCP tool to regenerate the full response and a fresh handle."
        }
    })
}

#[tokio::test]
async fn test_unknown_method() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(70), "some/unknown/method", json!({}))],
    )
    .await;

    assert_eq!(responses.len(), 1, "{responses:?}");
    assert_eq!(
        parse_response(&responses[0]),
        json!({
            "jsonrpc": "2.0",
            "id": 70,
            "error": {"code": -32601, "message": "method not found: some/unknown/method"}
        })
    );
}

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

    assert_eq!(responses.len(), 2, "parse error + ping: {responses:?}");
    assert_eq!(
        parse_response(&responses[0]),
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {
                "code": -32700,
                "message": "failed to parse JSON-RPC request: expected ident at line 1 column 2"
            }
        })
    );
    assert_eq!(
        parse_response(&responses[1]),
        json!({"jsonrpc": "2.0", "id": 80, "result": {}})
    );
}

/// The envelope rule is enforced once at the transport boundary: a frame
/// whose `jsonrpc` is not exactly `"2.0"` is answered with `InvalidRequest`
/// carrying its id and never reaches the handler, so it cannot execute a tool
/// or enter request accounting. Malformed JSON stays `ParseError`
/// (`test_malformed_json`).
#[tokio::test]
async fn test_foreign_protocol_version_is_rejected_before_dispatch() {
    let (server, _dir) = setup_server().await;
    let stats_view = std::sync::Arc::clone(&server);
    let status_call = |envelope: Value| {
        let mut frame = envelope;
        frame["method"] = json!("tools/call");
        frame["params"] = json!({"name": "tracedecay_status", "arguments": {}});
        serde_json::to_string(&frame).unwrap()
    };
    let responses = run_server_with_messages(
        server,
        vec![
            status_call(json!({"jsonrpc": "1.0", "id": 501})),
            status_call(json!({"jsonrpc": 2.0, "id": 502})),
            status_call(json!({"id": 503})),
            serde_json::to_string(&json!({
                "jsonrpc": "1.0",
                "method": "notifications/initialized"
            }))
            .unwrap(),
            jsonrpc_request(json!(504), "tools/list", json!({})),
        ],
    )
    .await;

    assert_eq!(responses.len(), 5, "{responses:?}");
    let invalid_request = |id: Value, reason: &str| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32600, "message": format!("invalid JSON-RPC request: {reason}")}
        })
    };
    for (id, reason) in [
        (json!(501), r#"jsonrpc must be "2.0", got "1.0""#),
        (json!(502), r#"jsonrpc must be "2.0", got 2.0"#),
        (json!(503), "missing jsonrpc member"),
        (Value::Null, r#"jsonrpc must be "2.0", got "1.0""#),
    ] {
        assert_eq!(
            response_with_id(&responses, id.clone()),
            invalid_request(id, reason)
        );
    }
    let tools_list = response_with_id(&responses, json!(504));
    assert!(
        tools_list["result"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "tracedecay_status")),
        "{tools_list}"
    );

    let stats = stats_view.server_stats_json().await;
    assert_eq!(
        stats["total_requests"], 1,
        "only the tools/list is work: {stats}"
    );
    assert_eq!(stats["tool_calls"], 0, "{stats}");
    assert_eq!(
        stats["method_call_counts"],
        json!({"tools/list": 1}),
        "{stats}"
    );
}

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

    assert_eq!(
        responses.len(),
        1,
        "blank lines are not frames: {responses:?}"
    );
    assert_eq!(
        parse_response(&responses[0]),
        json!({"jsonrpc": "2.0", "id": 90, "result": {}})
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn test_server_stats_include_response_handle_metrics() {
    let fixture = crate::support::production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production response-handle metrics server");
    let baseline = server.server_stats_json().await;
    let baseline_handles = &baseline["response_handles"];
    let baseline_counter = |key: &str| baseline_handles[key].as_u64().unwrap_or(0);

    let cg = server.cg().await;
    let mut last_fact = None;
    for i in 0..35 {
        let added = crate::support::handle_real_server_tool_call(
            &server,
            "tracedecay_fact_store_add",
            json!({
                "content": format!(
                    "SERVER_STATS_HANDLE_METRIC_{i:02}: {}",
                    "response handle telemetry should survive truncation ".repeat(80)
                ),
                "category": "project",
                "trust": 0.9
            }),
        )
        .await;
        if i == 34 {
            let payload: Value =
                serde_json::from_str(crate::support::extract_real_server_text(&added)).unwrap();
            assert_eq!(payload["outcome"], "committed");
            assert_eq!(payload["result"]["disposition"], "added");
            assert_eq!(payload["result"]["fact"]["kind"], "available");
            let fact_id = payload["result"]["fact"]["fact"]["fact_id"]
                .as_str()
                .expect("canonical string fact id")
                .to_owned();
            assert!(fact_id.starts_with("fact.v1."));
            let last_event_id = payload["result"]["commit"]["last_event_id"]
                .as_str()
                .expect("canonical fact commit generation")
                .to_owned();
            last_fact = Some((fact_id, last_event_id));
        }
    }

    let listed = crate::support::handle_real_server_tool_call_raw(
        &server,
        "tracedecay_fact_store_list",
        json!({
            "category": "project",
            "min_trust": 0.0,
            "limit": 200
        }),
    )
    .await;
    let envelope: Value =
        serde_json::from_str(crate::support::extract_real_server_text(&listed["result"])).unwrap();
    let handle = envelope["handle"]
        .as_str()
        .expect("retrieve handle")
        .to_string();
    let retrieved = crate::support::handle_real_server_tool_call(
        &server,
        "tracedecay_retrieve",
        json!({ "handle": handle, "format": "json" }),
    )
    .await;
    let retrieved_payload: Value =
        serde_json::from_str(crate::support::extract_real_server_text(&retrieved)).unwrap();
    assert_eq!(retrieved_payload["expired"], false);

    let missing = crate::support::handle_real_server_tool_call(
        &server,
        "tracedecay_retrieve",
        json!({"handle": "rh_0123456789abcdef01234567"}),
    )
    .await;
    let missing_payload: Value =
        serde_json::from_str(crate::support::extract_real_server_text(&missing)).unwrap();
    assert_eq!(missing_payload["reason_code"], "handle_not_found");

    let expired = store_response_handle(
        cg.project_root(),
        "{\"expired\":true}",
        current_timestamp() - RESPONSE_HANDLE_TTL_SECS - 5,
    )
    .unwrap();
    let expired_result = crate::support::handle_real_server_tool_call(
        &server,
        "tracedecay_retrieve",
        json!({ "handle": expired.handle, "format": "json" }),
    )
    .await;
    let expired_payload: Value =
        serde_json::from_str(crate::support::extract_real_server_text(&expired_result)).unwrap();
    assert_eq!(expired_payload["reason_code"], "handle_expired");

    let broken =
        store_response_handle(cg.project_root(), "{\"broken\":true}", current_timestamp()).unwrap();
    let broken_path = response_handle_dir(&cg).join(format!("{}.json", broken.handle));
    fs::remove_file(&broken_path).unwrap();
    fs::create_dir(&broken_path).unwrap();
    let broken_result = crate::support::handle_real_server_tool_call_raw(
        &server,
        "tracedecay_retrieve",
        json!({ "handle": broken.handle }),
    )
    .await;
    assert_eq!(
        broken_result["error"],
        handle_read_failed_error(),
        "broken handle fixture should increment retrieve failure telemetry"
    );
    fs::remove_dir(&broken_path).unwrap();

    store_response_handle(cg.project_root(), "{\"expires\":true}", current_timestamp())
        .expect("direct store should succeed so cleanup has something to expire");
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
    store_response_handle(
        failure_root.path(),
        "store failure telemetry",
        current_timestamp(),
    )
    .expect_err("store failure fixture should increment failure telemetry");

    let after = server.server_stats_json().await;
    let handles = &after["response_handles"];
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
    // The cleanup ran past every stored handle's expiry, so the project's
    // on-disk cache is empty.
    assert_eq!(
        handles["on_disk"],
        json!({
            "available": true,
            "file_count": 0,
            "total_bytes": 0,
            "oldest_expires_at": null,
            "newest_expires_at": null
        }),
        "{handles}"
    );

    if let Some((fact_id, expected_last_event_id)) = last_fact {
        let removed = crate::support::handle_real_server_tool_call(
            &server,
            "tracedecay_fact_store_remove",
            json!({
                "fact_id": fact_id.clone(),
                "expected_last_event_id": expected_last_event_id
            }),
        )
        .await;
        let payload: Value =
            serde_json::from_str(crate::support::extract_real_server_text(&removed)).unwrap();
        assert_eq!(payload["outcome"], "removed");
        assert_eq!(payload["fact"]["kind"], "unavailable");
        assert_eq!(payload["fact"]["status"]["fact_id"], fact_id);
        assert_eq!(payload["fact"]["status"]["payload_access"], "deleted");
        assert_eq!(payload["commit"]["fact_id"], fact_id);
    }
    fixture.harness.shutdown().await;
}
#[tokio::test]
async fn test_server_stats_after_run() {
    let (server, _dir) = setup_server().await;
    let server_handle = server.clone();
    // Send several requests then a tracedecay_status to check stats are embedded.
    let responses = run_server_with_messages(
        server,
        vec![
            spec_initialize_request(json!(200)),
            jsonrpc_request(json!(201), "tools/list", json!({})),
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
                    "arguments": {"format": "json"}
                }),
            ),
        ],
    )
    .await;

    // The status call is counted before it runs, so its embedded server stats
    // include itself whatever the order of the concurrent earlier requests.
    let resp = response_with_id(&responses, json!(202));
    let payload: Value =
        serde_json::from_str(successful_tool_text(&resp, "status")).expect("status result JSON");
    assert_eq!(payload["server"]["tool_calls"], 1, "{payload}");
    assert_eq!(
        payload["server"]["tool_call_counts"],
        json!({"tracedecay_status": 1}),
        "{payload}"
    );

    let stats = server_handle.server_stats_json().await;
    assert_eq!(stats["jsonrpc_messages"], 4);
    assert_eq!(
        stats["method_call_counts"],
        json!({"initialize": 1, "tools/list": 1, "resources/read": 1, "tools/call": 1})
    );
    assert_eq!(
        stats["resource_read_counts"],
        json!({"tracedecay://status": 1})
    );
    assert_eq!(stats["tool_call_counts"], json!({"tracedecay_status": 1}));
    assert_eq!(stats["ratios"]["tool_calls_per_jsonrpc_message"], 0.25);
}

#[tokio::test]
async fn test_error_tracking() {
    let (server, _dir) = setup_server().await;
    let stats_view = Arc::clone(&server);
    let responses = run_server_with_messages(
        server,
        vec![
            jsonrpc_request(json!(300), "unknown/method", json!({})),
            jsonrpc_request(
                json!(301),
                "tools/call",
                json!({
                    "name": "tracedecay_status",
                    "arguments": {"admission_only": true, "format": "json"}
                }),
            ),
        ],
    )
    .await;

    assert_eq!(
        response_with_id(&responses, json!(300))["error"],
        json!({"code": -32601, "message": "method not found: unknown/method"})
    );
    let status = response_with_id(&responses, json!(301));
    let payload: Value =
        serde_json::from_str(successful_tool_text(&status, "status")).expect("status result JSON");
    assert_eq!(payload["project_admitted"], true, "{payload}");
    let stats = stats_view.server_stats_json().await;
    assert_eq!(stats["errors"], 1, "only the unknown method errs: {stats}");
}

#[tokio::test]
async fn test_resources_list() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(400), "resources/list", json!({}))],
    )
    .await;

    assert_eq!(responses.len(), 1, "{responses:?}");
    assert_eq!(
        parse_response(&responses[0]),
        json!({
            "jsonrpc": "2.0",
            "id": 400,
            "result": {
                "resources": [
                    {
                        "uri": "tracedecay://status",
                        "name": "Graph Status",
                        "description": "Code graph statistics: node/edge/file counts, languages, DB size, and index freshness.",
                        "mimeType": "application/json"
                    },
                    {
                        "uri": "tracedecay://files",
                        "name": "File List",
                        "description": "All indexed project files grouped by directory with symbol counts.",
                        "mimeType": "text/plain"
                    },
                    {
                        "uri": "tracedecay://overview",
                        "name": "Project Overview",
                        "description": "High-level project summary: language distribution, largest modules, and top entry points.",
                        "mimeType": "text/plain"
                    },
                    {
                        "uri": "tracedecay://branches",
                        "name": "Tracked Branches",
                        "description": "List of tracked branches with DB sizes, parent branch, and last sync time. Empty if multi-branch is not active.",
                        "mimeType": "application/json"
                    },
                    {
                        "uri": "tracedecay://schema",
                        "name": "SQLite Schema",
                        "description": "Installed project-store DDL for this binary, generated from the fresh-store shape create_schema admits. Code topology is not in these tables.",
                        "mimeType": "text/markdown"
                    }
                ]
            }
        })
    );
}

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

    let resp = response_with_id(&responses, json!(410));
    let contents = resp["result"]["contents"]
        .as_array()
        .expect("should have contents array");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["uri"], "tracedecay://status");
    assert_eq!(contents[0]["mimeType"], "application/json");

    let text = contents[0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).expect("status resource JSON");
    assert_eq!(payload["graph_statistics"]["state"], "unavailable");
    assert_eq!(
        payload["graph_statistics"]["reason"],
        "authority_unavailable"
    );
}

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

    assert_eq!(
        response_with_id(&responses, json!(420))["result"],
        json!({
            "contents": [{
                "uri": "tracedecay://files",
                "mimeType": "text/plain",
                "text": "status: unavailable\nreason: verified_generation_file_inventory_not_admitted"
            }]
        })
    );
}

#[tokio::test]
async fn test_resources_read_overview() {
    let (server, dir) = setup_server().await;
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

    assert_eq!(
        response_with_id(&responses, json!(430))["result"],
        json!({
            "contents": [{
                "uri": "tracedecay://overview",
                "mimeType": "text/plain",
                "text": format!(
                    "Project: {}\nGraph statistics: unavailable (sealed generation statistics are not published)",
                    dir.path().canonicalize().unwrap().display()
                )
            }]
        })
    );
}

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

    assert_eq!(
        response_with_id(&responses, json!(440))["error"],
        json!({"code": -32602, "message": "unknown resource URI: tracedecay://nonexistent"})
    );
}

#[tokio::test]
async fn test_resources_read_missing_uri() {
    let (server, _dir) = setup_server().await;
    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(json!(450), "resources/read", json!({}))],
    )
    .await;

    assert_eq!(
        response_with_id(&responses, json!(450))["error"],
        json!({"code": -32602, "message": "missing 'uri' in resources/read params"})
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

// Repeated serve-mode LCM calls must keep working while the project session
// DB schema is ensured at most once per process: after the first write-path
// call creates the store and runs the migrations, later write-path calls
// (even from a fresh `McpServer` in the same process) take the
// version-gate fast path and never re-run the LCM migrations, observable
// via the migration row's `applied_at`, which only a migration run rewrites.
//
// Pure-read tools (lcm_status) no longer create the store, so each session
// issues a write-path call (`lcm_session_boundary`, whose storage open is
// the migration-running path) before the status reads.
#[tokio::test]
async fn repeated_serve_lcm_calls_do_not_rerun_migrations() {
    let profile = crate::common::fixture::TestProfile::acquire().await;
    let repository =
        crate::common::fixture::GitFixture::primary(profile.path("lcm-migration-project"));
    fs::create_dir_all(repository.root().join("src")).unwrap();
    fs::write(
        repository.root().join("src/main.rs"),
        "fn main() { let x = helper(); }\nfn helper() -> i32 { 42 }\n",
    )
    .unwrap();
    let project = profile.enroll(repository.root()).await;
    let project_root = project.root().to_path_buf();
    let server = project.mcp_server().await;
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
    let responses = run_server_with_messages(
        server,
        vec![
            spec_initialize_request(json!(1)),
            jsonrpc_notification("notifications/initialized"),
            lcm_status_call(2),
            lcm_status_call(3),
        ],
    )
    .await;
    for id in [2_i64, 3] {
        let resp = response_with_id(&responses, json!(id));
        let envelope: Value =
            serde_json::from_str(successful_tool_text(&resp, "lcm_status")).unwrap();
        assert_eq!(
            envelope.pointer("/outcome/outcome").and_then(Value::as_str),
            Some("evidence"),
            "lcm_status id={id} must return retained evidence: {envelope}"
        );
        let payload = envelope
            .pointer("/outcome/value/payload")
            .expect("lcm_status retained evidence payload");
        assert_eq!(payload["status"], "ok", "lcm_status id={id} payload");
    }

    // Stamp a sentinel applied_at; only a re-run of the migrations would
    // rewrite it (the version-gate fast path and the per-process ensured
    // flag both leave the row untouched).
    project
        .registry()
        .set_lcm_schema_migration_applied_at_for_test(
            tracedecay_sessions::admission::HostAdmissionScope::Project,
            123,
        )
        .await
        .unwrap();
    assert_eq!(
        project
            .registry()
            .lcm_schema_migration_applied_at_for_test(
                tracedecay_sessions::admission::HostAdmissionScope::Project,
            )
            .await
            .unwrap(),
        Some(123)
    );

    // Store-resolution evidence for the final assertion: a rewritten sentinel
    // means migrations re-ran, and the stats below distinguish "the same
    // store file was recreated" (created/length drift on one path) from "a
    // different store file answered" (the seeded file left untouched).
    let sessions_db =
        tracedecay_runtime_core::storage::resolve_project_session_db_path(&project_root)
            .expect("resolve the project's sessions db path");
    let stat_sessions_db = |label: &str| match std::fs::metadata(&sessions_db) {
        Ok(meta) => format!(
            "{label}: path={} len={} created={:?} modified={:?}",
            sessions_db.display(),
            meta.len(),
            meta.created().ok(),
            meta.modified().ok(),
        ),
        Err(error) => format!(
            "{label}: path={} unavailable: {error}",
            sessions_db.display()
        ),
    };
    let seeded_stat = stat_sessions_db("after-seed");

    // A second serve session over the same project in the same process. The
    // LCM mutation tools are daemon-internal now, so the status reads are the
    // remaining serve-mode calls that would re-run migrations if the
    // per-process ensured cache failed.
    let server = project.mcp_server().await;
    let responses = run_server_with_messages(
        server,
        vec![
            spec_initialize_request(json!(1)),
            lcm_status_call(2),
            lcm_status_call(3),
        ],
    )
    .await;
    for id in [2_i64, 3] {
        let resp = response_with_id(&responses, json!(id));
        let envelope: Value =
            serde_json::from_str(successful_tool_text(&resp, "second-session lcm_status")).unwrap();
        assert_eq!(
            envelope.pointer("/outcome/outcome").and_then(Value::as_str),
            Some("evidence"),
            "second-session lcm_status id={id} must return retained evidence: {envelope}"
        );
        let payload = envelope
            .pointer("/outcome/value/payload")
            .expect("second-session lcm_status retained evidence payload");
        assert_eq!(
            payload["status"], "ok",
            "second-session lcm_status id={id} payload"
        );
    }
    assert_eq!(
        project
            .registry()
            .lcm_schema_migration_applied_at_for_test(
                tracedecay_sessions::admission::HostAdmissionScope::Project,
            )
            .await
            .unwrap(),
        Some(123),
        "repeated serve-mode LCM calls must not re-run the LCM migrations\n  \
         {seeded_stat}\n  {}",
        stat_sessions_db("after-second-serve"),
    );
}

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

async fn fixture() -> (
    TempDir,
    ProductionProjectCompositionHarnessV1,
    Arc<McpServer>,
    PathBuf,
) {
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
    (isolation, harness, server, target_project)
}

fn files_request(id: u64, arguments: Value) -> String {
    jsonrpc_request(
        json!(id),
        "tools/call",
        json!({
            "name": "tracedecay_files",
            "arguments": arguments,
        }),
    )
}

async fn assert_legacy_selectors_cannot_reroute_the_active_project() {
    let (_isolation, harness, server, target_project) = fixture().await;
    let target_graph = harness
        .server(&target_project)
        .expect("target project server")
        .cg()
        .await;
    let target_root = target_graph.project_root().to_string_lossy().into_owned();
    let target_project_id = target_graph
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("target project has a registered identity");

    let spoof_cases = [
        (
            "top-level project_path",
            json!({"layout": "flat", "project_path": target_root.clone()}),
        ),
        (
            "top-level project_root",
            json!({"layout": "flat", "project_root": target_root.clone()}),
        ),
        (
            "nested selector path",
            json!({"layout": "flat", "project_selector": {"path": target_root.clone()}}),
        ),
        (
            "nested selector project_path",
            json!({"layout": "flat", "project_selector": {"project_path": target_root}}),
        ),
        (
            "top-level project_id alias",
            json!({"layout": "flat", "project_id": target_project_id}),
        ),
    ];
    let mut messages = Vec::new();
    for (offset, (_, arguments)) in spoof_cases.iter().enumerate() {
        messages.push(files_request(10 + offset as u64, arguments.clone()));
    }
    messages.push(files_request(100, json!({"layout": "flat"})));

    let responses = run_server_with_messages(server, messages).await;
    for (offset, (case, _)) in spoof_cases.iter().enumerate() {
        let response = response_with_id(&responses, json!(10 + offset as u64));
        assert_eq!(
            response["error"]["code"], -32602,
            "{case} must be rejected as invalid parameters instead of rerouting: {response}"
        );
        assert!(
            response["result"].is_null(),
            "{case} must not return a tool result after invalid-parameter rejection: {response}"
        );
        assert!(
            !response.to_string().contains("src/target.rs"),
            "{case} must not serve the selected project's data: {response}"
        );
    }

    let clean_response = response_with_id(&responses, json!(100));
    let clean_text = clean_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("clean files response text: {clean_response}"));
    assert!(
        clean_text.contains("src/active.rs") && !clean_text.contains("src/target.rs"),
        "rejected selectors must not disturb the active project route: {clean_text}"
    );
    harness.shutdown().await;
}

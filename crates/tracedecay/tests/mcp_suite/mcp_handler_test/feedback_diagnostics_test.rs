//! `tracedecay_feedback_diagnostics` through the production MCP `tools/call` path.
//!
//! A handle that fails the reviewed request contract is refused before the
//! daemon is asked. A handle the contract accepts but the daemon never minted
//! is a concealment problem, not an empty cycle and not a schema error.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};

use crate::support::{handle_real_server_tool_call_raw, production_composition_fixture};

const TOOL: &str = "tracedecay_feedback_diagnostics";

fn invalid_request(detail: &str) -> Value {
    json!({
        "code": -32602,
        "message": format!(
            "tool project route failed: reason_code=application_surface_invalid_request retryable=false: {detail}"
        ),
        "data": {
            "tool": TOOL,
            "reason_code": "application_surface_invalid_request",
            "retryable": false,
            "detail": detail,
            "kind": "invalid_request",
            "code": "application_surface_invalid_request"
        }
    })
}

fn denied_problem(request_id: &str) -> Value {
    json!({
        "revision": 1,
        "kind": "not_found_or_not_authorized",
        "code": "not_found_or_not_authorized",
        "message": "The requested resource was not found or is not authorized",
        "diagnostic": null,
        "committed_receipt": null,
        "owning_layer": "application",
        "terminality": "pre_admission",
        "retryable": false,
        "retry": "never",
        "retry_scope": null,
        "retry_after_millis": null,
        "cancellation_stage": null,
        "unavailable_classification": null,
        "execution_failure_classification": null,
        "request_id": request_id,
        "trace_id": request_id,
        "details": [],
        "legal_actions": [],
        "coverage": null
    })
}

async fn call(server: &tracedecay::mcp::McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, TOOL, arguments).await
}

fn assert_invalid_request(response: &Value, detail: &str) {
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(
        response.get("result").is_none(),
        "schema refusal must be a JSON-RPC error, not a tool result: {response}"
    );
    assert_eq!(response["error"], invalid_request(detail));
}

fn assert_unknown_handle(response: &Value) {
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(
        response.get("error").is_none(),
        "an accepted handle must not become a JSON-RPC error: {response}"
    );
    let result = &response["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][0]["type"], "text");

    let text = result["content"][0]["text"]
        .as_str()
        .expect("diagnostics text");
    let envelope: Value = serde_json::from_str(text).expect("diagnostics JSON");
    let request_id = envelope["request_id"]
        .as_str()
        .expect("diagnostics request id");
    assert!(
        request_id.starts_with("request.mcp."),
        "request id must be the MCP connection identity, got {request_id}"
    );
    let problem = denied_problem(request_id);
    assert_eq!(result["problem"], problem);
    assert_eq!(
        envelope,
        json!({
            "contract": {
                "schema_id": "schema.application.feedback.diagnostics.result",
                "schema_revision": 1
            },
            "request_id": request_id,
            "problem": problem
        })
    );
}

#[tokio::test]
async fn feedback_diagnostics_refuses_bad_arguments_and_denies_unknown_handles() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");

    assert_invalid_request(
        &call(&server, json!({})).await,
        "application surface request does not match its reviewed schema: missing field `request_handle`",
    );
    assert_invalid_request(
        &call(&server, json!({"request_handle": 7})).await,
        "application surface request does not match its reviewed schema: invalid type: integer `7`, expected a string",
    );
    assert_invalid_request(
        &call(
            &server,
            json!({"request_handle": "rh_0123456789abcdef01234567", "extra": true}),
        )
        .await,
        "application surface request does not match its reviewed schema: unknown field `extra`, expected `request_handle`",
    );
    let too_long = "a".repeat(257);
    for handle in ["", " rh_leading", "rh_trailing ", too_long.as_str()] {
        assert_invalid_request(
            &call(&server, json!({"request_handle": handle})).await,
            "application surface request handle is invalid",
        );
    }

    assert_unknown_handle(
        &call(
            &server,
            json!({"request_handle": "rh_0123456789abcdef01234567"}),
        )
        .await,
    );
    let accepted_but_unminted = "a".repeat(256);
    assert_unknown_handle(&call(&server, json!({"request_handle": accepted_but_unminted})).await);

    fixture.harness.shutdown().await;
}

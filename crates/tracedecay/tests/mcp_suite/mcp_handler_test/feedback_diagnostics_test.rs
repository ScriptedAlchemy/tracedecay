//! `tracedecay_feedback_diagnostics` through the production MCP `tools/call` path.
//!
//! A handle that fails the reviewed request contract is refused before the
//! daemon is asked. A handle the contract accepts but the daemon never minted,
//! or minted for a different read, is a concealment problem, not an empty
//! cycle and not a schema error. A handle the advisory cycle actually minted
//! returns that cycle's diagnostics, keyed to the fixture checkout.

#![cfg(feature = "test-transport")]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use url::Url;

use crate::support::{
    handle_real_server_tool_call_raw, production_composition_fixture, wait_for_current_graph,
};

const TOOL: &str = "tracedecay_feedback_diagnostics";
const ADVISORY_CYCLE: &str = "tracedecay_feedback_advisory_cycle";

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

async fn call_tool(server: &tracedecay::mcp::McpServer, tool: &str, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, tool, arguments).await
}

async fn call(server: &tracedecay::mcp::McpServer, arguments: Value) -> Value {
    call_tool(server, TOOL, arguments).await
}

fn fixture_checkout(project: &Path) -> (String, String) {
    let git = crate::common::git_program();
    let branch = Command::new(&git)
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(project)
        .output()
        .expect("read fixture branch");
    assert!(
        branch.status.success(),
        "fixture branch: {}",
        String::from_utf8_lossy(&branch.stderr)
    );
    let head = Command::new(&git)
        .args(["rev-parse", "HEAD"])
        .current_dir(project)
        .output()
        .expect("read fixture HEAD");
    assert!(
        head.status.success(),
        "fixture HEAD: {}",
        String::from_utf8_lossy(&head.stderr)
    );
    (
        String::from_utf8(branch.stdout)
            .expect("fixture branch")
            .trim()
            .to_owned(),
        String::from_utf8(head.stdout)
            .expect("fixture HEAD")
            .trim()
            .to_owned(),
    )
}

fn successful_envelope(response: &Value, tool: &str) -> Value {
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(
        response.get("error").is_none(),
        "{tool} must not be a JSON-RPC error: {response}"
    );
    let result = &response["result"];
    assert_ne!(result["isError"], true, "{tool} must succeed: {response}");
    assert_eq!(result["content"][0]["type"], "text");
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} text: {response}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{tool} JSON ({error}): {text}"))
}

fn retryable_advisory_unavailable(response: &Value) -> bool {
    let problem = &response["result"]["problem"];
    response["result"]["isError"] == true
        && problem["retryable"] == true
        && problem["code"] == "feedback.advisory-cycle.unavailable"
}

/// The advisory cycle is the production mint of a diagnostics handle. This
/// waits out the deferred owner registration, then returns that handle, the
/// sibling list handle, and the cycle body the diagnostics read must return.
async fn minted_diagnostics_cycle(
    server: &tracedecay::mcp::McpServer,
    document_uri: &str,
) -> (String, String, Value) {
    wait_for_current_graph(server).await;
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let response = call_tool(
            server,
            ADVISORY_CYCLE,
            json!({ "document_uri": document_uri }),
        )
        .await;
        if retryable_advisory_unavailable(&response) {
            assert!(
                Instant::now() < deadline,
                "advisory cycle stayed unavailable: {response}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        let envelope = successful_envelope(&response, ADVISORY_CYCLE);
        assert_eq!(
            envelope["contract"]["schema_id"],
            "schema.application.feedback.advisory-cycle.result"
        );
        assert_eq!(envelope["contract"]["schema_revision"], 1);
        let payload = &envelope["outcome"]["value"]["payload"];
        let diagnostics_handle = payload["read_handles"]["diagnostics_handle"]
            .as_str()
            .unwrap_or_else(|| panic!("published cycle minted no diagnostics handle: {envelope}"))
            .to_owned();
        let list_handle = payload["read_handles"]["list_handle"]
            .as_str()
            .unwrap_or_else(|| panic!("published cycle minted no list handle: {envelope}"))
            .to_owned();
        assert_ne!(
            diagnostics_handle, list_handle,
            "diagnostics and list handles must be distinct: {payload}"
        );
        let mut cycle = payload["cycle"].clone();
        cycle
            .as_object_mut()
            .expect("advisory cycle object")
            .remove("published");
        return (diagnostics_handle, list_handle, cycle);
    }
}

fn assert_published_cycle(envelope: &Value, expected_cycle: &Value, branch: &str, head: &str) {
    assert_eq!(
        envelope["contract"]["schema_id"],
        "schema.application.feedback.diagnostics.result"
    );
    assert_eq!(envelope["contract"]["schema_revision"], 1);
    assert_eq!(envelope["outcome"]["outcome"], "evidence");
    assert!(envelope.get("problem").is_none());
    let evidence = &envelope["outcome"]["value"];
    let payload = &evidence["payload"];
    assert_eq!(
        payload.as_object().map(|object| object.len()),
        Some(1),
        "diagnostics payload is the cycle only: {payload}"
    );
    let cycle = &payload["cycle"];
    assert_eq!(cycle, expected_cycle);
    assert_eq!(cycle["durability"], "durable");
    assert_eq!(cycle["scope"]["branch_ref"], format!("refs/heads/{branch}"));
    assert_eq!(cycle["scope"]["head_commit_id"], head);
    let returned = cycle["returned_findings"].as_u64().expect("returned");
    let omitted = cycle["omitted_findings"].as_u64().expect("omitted");
    let total = cycle["total_findings"].as_u64().expect("total");
    assert_eq!(total, returned + omitted);
    assert_eq!(
        returned,
        cycle["findings"].as_array().expect("findings").len() as u64
    );
    let expected_termination = match cycle["termination"].as_str() {
        Some("clean" | "duplicate_noop") => "completed",
        Some("budget_exceeded") => "timed_out",
        Some("cancelled") => "cancelled",
        Some("daemon_unavailable") => "unavailable",
        Some("blocked" | "incomplete_coverage" | "stale_replan_required" | "user_stop") => {
            "partial"
        }
        other => panic!("diagnostics cycle termination is not a closed state: {other:?}"),
    };
    assert_eq!(
        evidence["execution"]["termination"], expected_termination,
        "execution termination must follow the cycle state: {cycle}"
    );
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

#[tokio::test]
async fn feedback_diagnostics_returns_the_published_cycle_for_its_minted_handle() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    let (branch, head) = fixture_checkout(&fixture.project_root);
    let document_uri = Url::from_file_path(fixture.project_root.join("src/utils.rs"))
        .expect("fixture document URI")
        .to_string();
    let (diagnostics_handle, list_handle, expected_cycle) =
        minted_diagnostics_cycle(&server, &document_uri).await;

    let first = successful_envelope(
        &call(&server, json!({ "request_handle": &diagnostics_handle })).await,
        TOOL,
    );
    assert_published_cycle(&first, &expected_cycle, &branch, &head);

    let second = successful_envelope(
        &call(&server, json!({ "request_handle": &diagnostics_handle })).await,
        TOOL,
    );
    assert_published_cycle(&second, &expected_cycle, &branch, &head);
    assert_ne!(
        first["request_id"], second["request_id"],
        "each tools/call must mint its own request identity"
    );

    assert_unknown_handle(&call(&server, json!({ "request_handle": list_handle })).await);

    fixture.harness.shutdown().await;
}

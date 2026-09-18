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
    handle_real_server_tool_call_raw, production_composition_fixture,
    production_composition_fixture_with_sources, wait_for_current_graph,
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

fn write_warned_fixture_sources(project: &Path) {
    crate::fixture::write_indexed_fixture_sources(project);
    let path = project.join("src/utils.rs");
    let source = std::fs::read_to_string(&path).expect("fixture utils source");
    let updated = source.replacen(
        "pub fn helper() -> String {\n    format_greeting(\"world\")\n}",
        "pub fn helper() -> String {\n    let unused_anchor = 1;\n    format_greeting(\"world\")\n}",
        1,
    );
    assert_ne!(
        source, updated,
        "fixture helper body was not the expected source"
    );
    std::fs::write(&path, updated).expect("write warned fixture source");
}

fn compiler_warning(project: &Path) -> String {
    let out_dir = project.join("rustc-out");
    std::fs::create_dir_all(&out_dir).expect("rustc out dir");
    let compiled = Command::new("rustc")
        .current_dir(project)
        .args([
            "--edition=2021",
            "--crate-type=bin",
            "--emit=metadata",
            "--color=never",
            "src/main.rs",
            "--out-dir",
        ])
        .arg(&out_dir)
        .output()
        .expect("run rustc");
    let stderr = String::from_utf8(compiled.stderr).expect("rustc stderr");
    assert!(
        compiled.status.success(),
        "rustc failed\nstdout:\n{}\nstderr:\n{stderr}",
        String::from_utf8_lossy(&compiled.stdout)
    );
    assert!(
        stderr.contains("unused variable: `unused_anchor`"),
        "rustc must warn on the fixture anchor: {stderr}"
    );
    stderr
}

/// Publish one real compiler warning into the same diagnostic store the
/// feedback cycle reads. A fixture with no diagnostics never records a
/// publication, so the daemon never mints a handle.
async fn publish_compiler_warning(server: &tracedecay::mcp::McpServer, project: &Path) {
    let response = call_tool(
        server,
        "tracedecay_diagnose",
        json!({
            "cargo_output": compiler_warning(project),
            "include_callers": false,
        }),
    )
    .await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert!(
        response.get("error").is_none(),
        "diagnose must publish, not refuse: {response}"
    );
    assert_ne!(response["result"]["isError"], true, "{response}");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("diagnose text: {response}"));
    let body: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("diagnose JSON ({error}): {text}"));
    assert_eq!(body["published"]["status"], "published", "{body}");
    assert!(
        body["published"]["inserted"]
            .as_u64()
            .is_some_and(|inserted| inserted > 0),
        "the compiler warning must land in the diagnostic store: {body}"
    );
}

/// The advisory cycle is the production mint of a diagnostics handle. This
/// waits out owner registration and a recorded publication, then returns
/// that handle, the sibling list handle, and the cycle body the diagnostics
/// read must return.
async fn minted_diagnostics_cycle(
    server: &tracedecay::mcp::McpServer,
    project: &Path,
    document_uri: &str,
) -> (String, String, Value) {
    wait_for_current_graph(server).await;
    publish_compiler_warning(server, project).await;
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = Value::Null;
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
        last = envelope["outcome"]["value"]["payload"].clone();
        if let Some(minted) = split_minted_cycle(&last) {
            return minted;
        }
        assert!(
            Instant::now() < deadline,
            "advisory cycle never published a diagnostics handle: {last}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn split_minted_cycle(payload: &Value) -> Option<(String, String, Value)> {
    let diagnostics_handle = payload["read_handles"]["diagnostics_handle"]
        .as_str()?
        .to_owned();
    let list_handle = payload["read_handles"]["list_handle"].as_str()?.to_owned();
    if diagnostics_handle.is_empty() || list_handle.is_empty() || diagnostics_handle == list_handle
    {
        return None;
    }
    let mut cycle = payload["cycle"].clone();
    cycle.as_object_mut()?.remove("published");
    Some((diagnostics_handle, list_handle, cycle))
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
    // The read itself completed. The cycle it returns is incomplete because
    // GitHub, CI, and proximity have nothing to contribute; the compiler
    // warning is still the one finding.
    assert_eq!(evidence["execution"]["termination"], "completed");
    assert_eq!(cycle["termination"], "incomplete_coverage");
    assert_eq!(cycle["advisory_only"], true);
    assert_eq!(cycle["returned_findings"], 1);
    assert_eq!(cycle["omitted_findings"], 0);
    assert_eq!(cycle["total_findings"], 1);
    assert_eq!(
        cycle["findings"].as_array().map(Vec::len),
        Some(1),
        "the compiler warning is the only finding: {cycle}"
    );
    let finding = &cycle["findings"][0];
    assert_eq!(finding["classification"], "new");
    assert_eq!(finding["lifecycle"], "active");
    assert_eq!(finding["provider_state"], "supported_completed_complete");
    assert_eq!(
        finding["safe_bounded_preview"],
        "unused variable: `unused_anchor`"
    );
    assert_eq!(
        finding["diagnostic_projection"]["safe_bounded_message"],
        "unused variable: `unused_anchor`"
    );
    assert_eq!(finding["diagnostic_projection"]["severity"], "warning");
    assert_eq!(finding["diagnostic_projection"]["code"], "warning");
    assert_eq!(
        finding["diagnostic_projection"]["producer"],
        "code_diagnostic"
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
    let fixture = production_composition_fixture_with_sources(write_warned_fixture_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    let (branch, head) = fixture_checkout(&fixture.project_root);
    let document_uri = Url::from_file_path(fixture.project_root.join("src/utils.rs"))
        .expect("fixture document URI")
        .to_string();
    let (diagnostics_handle, list_handle, expected_cycle) =
        minted_diagnostics_cycle(&server, &fixture.project_root, &document_uri).await;

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

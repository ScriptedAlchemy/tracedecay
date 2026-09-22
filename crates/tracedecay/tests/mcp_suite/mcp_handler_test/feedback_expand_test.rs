#![cfg(feature = "test-transport")]

//! `tracedecay_feedback_expand` is the handle-gated read that hydrates one
//! published finding's retained diagnostic anchor. Callers never send the
//! finding or the anchor: the daemon minted the handle, and the tool either
//! returns that finding plus the anchor, or a typed refusal.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, wait_for_current_graph,
};

const TOOL: &str = "tracedecay_feedback_expand";
const PUBLISHED_DIAGNOSTIC: &str = "unused variable: `feedback_expand_unused_probe`";
const PROBE_SOURCE: &str = "\
pub fn feedback_expand_probe() -> u32 {
    let feedback_expand_unused_probe = 7_u32;
    1
}
";

#[tokio::test]
async fn feedback_expand_returns_the_published_diagnostic_and_denies_other_handles() {
    let fixture = production_composition_fixture_with_sources(write_probe_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let missing = call_tool(&server, TOOL, json!({})).await;
    assert_invalid_request(
        &missing,
        "application surface request does not match its reviewed schema: missing field `request_handle`",
    );

    let blank = call_tool(&server, TOOL, json!({ "request_handle": " rh_leading" })).await;
    assert_invalid_request(&blank, "application surface request handle is invalid");

    let denied = poll_until(
        &server,
        TOOL,
        json!({ "request_handle": "rh_feedback_expand_unknown" }),
        tool_problem_kind_is("not_found_or_not_authorized"),
        "feedback owner never admitted the unknown-handle refusal",
    )
    .await;
    assert_not_found(&denied);

    let diagnostic_text = compile_probe_warning(&fixture.project_root);
    let diagnosed = call_tool(
        &server,
        "tracedecay_diagnose",
        json!({
            "cargo_output": diagnostic_text,
            "include_callers": false,
        }),
    )
    .await;
    let diagnosed = tool_json(&diagnosed);
    assert_eq!(
        diagnosed["published"]["status"], "published",
        "the compiler warning must reach the diagnostic store before expand can hydrate it: {diagnosed}"
    );
    assert!(
        diagnosed["published"]["inserted"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "publication must insert the unused-variable diagnostic: {diagnosed}"
    );

    let document = fixture.project_root.join("src/lib.rs");
    let document_uri = url::Url::from_file_path(&document)
        .expect("probe document URI")
        .to_string();
    let cycle = poll_until(
        &server,
        "tracedecay_feedback_advisory_cycle",
        json!({ "document_uri": document_uri }),
        cycle_published_the_probe,
        "advisory cycle never published the unused-variable finding",
    )
    .await;
    let payload = &cycle["outcome"]["value"]["payload"];
    let finding = payload["cycle"]["findings"]
        .as_array()
        .expect("cycle findings")
        .iter()
        .find(|finding| finding["safe_bounded_preview"] == PUBLISHED_DIAGNOSTIC)
        .expect("cycle must carry the published diagnostic preview");
    let finding_id = finding["finding_id"]
        .as_str()
        .expect("finding id")
        .to_owned();
    let anchor = finding["retrieval_anchor_id"]
        .as_str()
        .expect("published finding retains its diagnostic anchor")
        .to_owned();
    let expansion_handle = payload["finding_handles"]
        .as_array()
        .expect("finding handles")
        .iter()
        .find(|handle| handle["finding_id"] == finding_id)
        .and_then(|handle| handle["expansion_handle"].as_str())
        .expect("anchored finding mints an expansion handle")
        .to_owned();
    let diagnostics_handle = payload["read_handles"]["diagnostics_handle"]
        .as_str()
        .expect("cycle diagnostics handle")
        .to_owned();
    assert_ne!(
        expansion_handle, diagnostics_handle,
        "expand and diagnostics must not share a handle"
    );

    let expanded = call_tool(&server, TOOL, json!({ "request_handle": expansion_handle })).await;
    assert!(
        expanded["error"].is_null(),
        "expand must not fail the JSON-RPC call: {expanded}"
    );
    assert_ne!(expanded["result"]["isError"], json!(true), "{expanded}");
    let expanded = tool_json(&expanded);
    assert_eq!(
        expanded["contract"]["schema_id"],
        "schema.application.feedback.expand.result"
    );
    assert_eq!(expanded["contract"]["schema_revision"], 1);
    assert_eq!(expanded["outcome"]["outcome"], "evidence");
    assert_eq!(
        expanded["outcome"]["value"]["execution"]["termination"],
        "completed"
    );
    assert_eq!(
        expanded["outcome"]["value"]["coverage"]["requested_domains"],
        json!(["anchor"])
    );
    assert_eq!(
        expanded["outcome"]["value"]["coverage"]["completeness"],
        "complete"
    );
    assert_eq!(expanded["outcome"]["value"]["coverage"]["returned"], 1);
    assert_eq!(expanded["outcome"]["value"]["omissions"], json!([]));
    let expanded_finding = &expanded["outcome"]["value"]["payload"]["finding"];
    assert_eq!(expanded_finding["finding"]["finding_id"], finding_id);
    assert_eq!(
        expanded_finding["finding"]["safe_bounded_preview"],
        PUBLISHED_DIAGNOSTIC
    );
    assert_eq!(expanded_finding["finding"]["classification"], "new");
    assert_eq!(expanded_finding["finding"]["lifecycle"], "active");
    assert_eq!(
        expanded["outcome"]["value"]["payload"]["expansion"]["anchors"],
        json!([anchor])
    );

    let wrong_operation = call_tool(
        &server,
        TOOL,
        json!({ "request_handle": diagnostics_handle }),
    )
    .await;
    assert_not_found(&tool_json(&wrong_operation));

    fixture.harness.shutdown().await;
}

fn write_probe_sources(project: &Path) {
    fs::create_dir_all(project.join("src")).expect("probe source directory");
    fs::write(project.join("src/lib.rs"), PROBE_SOURCE).expect("probe source");
}

fn compile_probe_warning(project: &Path) -> String {
    let output_directory = tempfile::tempdir().expect("compiler output directory");
    let compiled = Command::new("rustc")
        .current_dir(project)
        .args([
            "--crate-type=lib",
            "--edition=2024",
            "--emit=metadata",
            "--color=never",
            "src/lib.rs",
            "--out-dir",
        ])
        .arg(output_directory.path())
        .output()
        .expect("compile the probe");
    assert!(
        compiled.status.success(),
        "probe must compile with a warning only\nstderr:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let stderr = String::from_utf8(compiled.stderr).expect("compiler stderr is utf-8");
    assert!(
        stderr.contains(PUBLISHED_DIAGNOSTIC),
        "compiler did not emit the probe warning: {stderr}"
    );
    stderr
}

fn assert_invalid_request(response: &Value, detail: &str) {
    assert!(
        response["result"].is_null(),
        "an invalid expand request must be a JSON-RPC error, not a tool result: {response}"
    );
    assert_eq!(response["error"]["code"], -32602, "{response}");
    assert_eq!(response["error"]["data"]["tool"], TOOL);
    assert_eq!(
        response["error"]["data"]["reason_code"],
        "application_surface_invalid_request"
    );
    assert_eq!(response["error"]["data"]["retryable"], false);
    assert_eq!(response["error"]["data"]["kind"], "invalid_request");
    assert_eq!(
        response["error"]["data"]["code"],
        "application_surface_invalid_request"
    );
    assert_eq!(response["error"]["data"]["detail"], detail);
    assert_eq!(
        response["error"]["message"],
        format!(
            "tool project route failed: reason_code=application_surface_invalid_request retryable=false: {detail}"
        )
    );
}

fn assert_not_found(body: &Value) {
    let problem = &body["problem"];
    assert_eq!(problem["kind"], "not_found_or_not_authorized");
    assert_eq!(problem["code"], "not_found_or_not_authorized");
    assert_eq!(
        problem["message"],
        "The requested resource was not found or is not authorized"
    );
    assert_eq!(problem["retry"], "never");
    assert_eq!(problem["retryable"], false);
    assert_eq!(problem["legal_actions"], json!([]));
    assert_eq!(problem["diagnostic"], Value::Null);
    assert_eq!(problem["committed_receipt"], Value::Null);
}

fn tool_problem_kind_is(kind: &'static str) -> impl Fn(&Value) -> bool {
    move |body| body["problem"]["kind"] == kind
}

fn cycle_published_the_probe(body: &Value) -> bool {
    body["outcome"]["outcome"] == "evidence"
        && body["outcome"]["value"]["payload"]["cycle"]["published"] == true
        && body["outcome"]["value"]["payload"]["cycle"]["findings"]
            .as_array()
            .is_some_and(|findings| {
                findings
                    .iter()
                    .any(|finding| finding["safe_bounded_preview"] == PUBLISHED_DIAGNOSTIC)
            })
}

async fn poll_until(
    server: &McpServer,
    tool: &str,
    arguments: Value,
    ready: impl Fn(&Value) -> bool,
    failure: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let response = call_tool(server, tool, arguments.clone()).await;
        if !response["error"].is_null() {
            last = response;
        } else {
            last = tool_json(&response);
            if ready(&last) {
                return last;
            }
            // A completed cycle that has not yet seen the diagnostic can be
            // retried; a typed non-retryable refusal cannot.
            if last.get("problem").is_some() && last["problem"]["retryable"] != true {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("{failure}: {last}");
}

async fn call_tool(server: &McpServer, tool: &str, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, tool, arguments).await
}

fn tool_json(response: &Value) -> Value {
    serde_json::from_str(extract_real_server_text(&response["result"])).expect("MCP tool JSON")
}

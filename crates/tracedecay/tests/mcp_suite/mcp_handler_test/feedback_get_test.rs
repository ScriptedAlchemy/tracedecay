//! `tracedecay_feedback_get` as a host calls it: one daemon-minted request handle
//! in, one finding or a typed denial out.
//!
//! The finding text is the compiler diagnostic that was admitted, not a string
//! the tool invents. An unknown handle and a handle minted for a different
//! read both deny without returning that finding.

#![cfg(feature = "test-transport")]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, wait_for_current_graph,
};

const LIB_RS: &str = "pub fn entry() { feedback_get_missing_symbol(); }\n";
/// The `error[E0425]` header `rustc` emits for [`LIB_RS`]. The published
/// finding keeps this text; the span label is not appended.
const PREVIEW: &str = "cannot find function `feedback_get_missing_symbol` in this scope";
const UNKNOWN_HANDLE: &str = "rh_unknown_feedback_get";

fn write_missing_symbol_crate(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("fixture src");
    std::fs::write(project.join("src/lib.rs"), LIB_RS).expect("fixture lib.rs");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"feedback-get-proof\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest");
}

fn compiler_diagnostic(project: &Path) -> String {
    let out_dir = project
        .parent()
        .expect("fixture isolation root")
        .join("rustc-out");
    std::fs::create_dir_all(&out_dir).expect("rustc output directory");
    let compiled = Command::new("rustc")
        .current_dir(project)
        .args([
            "--crate-type=lib",
            "--edition=2024",
            "--color=never",
            "--out-dir",
        ])
        .arg(&out_dir)
        .arg("src/lib.rs")
        .output()
        .expect("run rustc");
    let stderr = String::from_utf8(compiled.stderr).expect("rustc stderr is utf-8");
    assert!(
        !compiled.status.success(),
        "the missing symbol must fail compilation: {stderr}"
    );
    assert!(
        stderr.contains(PREVIEW),
        "rustc must report the missing function by name: {stderr}"
    );
    stderr
}

fn tool_body(response: &Value) -> Value {
    assert!(response["error"].is_null(), "MCP call failed: {response}");
    serde_json::from_str(extract_real_server_text(&response["result"]))
        .unwrap_or_else(|error| panic!("tool text was not JSON ({error}): {response}"))
}

fn retryable(response: &Value) -> bool {
    if response["error"]["data"]["retryable"] == true {
        return true;
    }
    let Some(text) = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
    else {
        return false;
    };
    let Ok(body) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    if body.pointer("/problem/retryable") == Some(&Value::Bool(true)) {
        return true;
    }
    matches!(
        body.pointer("/published/reason").and_then(Value::as_str),
        Some("code-index-identity-unavailable" | "code-index-generation-unavailable")
    )
}

async fn call_until_ready(
    server: &tracedecay::mcp::McpServer,
    tool: &str,
    arguments: Value,
    ready: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let response = handle_real_server_tool_call_raw(server, tool, arguments.clone()).await;
        if response["error"].is_null() {
            let body = tool_body(&response);
            if ready(&body) {
                return body;
            }
            assert!(
                retryable(&response),
                "{tool} stopped retrying before it was ready: {body}"
            );
        } else {
            assert!(
                retryable(&response),
                "{tool} failed before it was ready: {response}"
            );
        }
        assert!(
            Instant::now() < deadline,
            "{tool} stayed unavailable past the publication budget: {response}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn call_tool(server: &tracedecay::mcp::McpServer, tool: &str, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, tool, arguments).await;
    serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {result}"))
}

fn assert_unknown_handle_denied(body: &Value) {
    assert_eq!(
        body["contract"]["schema_id"], "schema.application.feedback.get.result",
        "{body}"
    );
    assert_eq!(body["contract"]["schema_revision"], 1_i64, "{body}");
    assert_eq!(body["problem"]["revision"], 1_i64, "{body}");
    assert_eq!(
        body["problem"]["kind"], "not_found_or_not_authorized",
        "{body}"
    );
    assert_eq!(
        body["problem"]["code"], "not_found_or_not_authorized",
        "{body}"
    );
    assert_eq!(
        body["problem"]["message"], "The requested resource was not found or is not authorized",
        "{body}"
    );
    assert_eq!(body["problem"]["retryable"], false, "{body}");
    assert_eq!(body["problem"]["retry"], "never", "{body}");
    assert_eq!(body["problem"]["terminality"], "pre_admission", "{body}");
    assert_eq!(body["problem"]["owning_layer"], "application", "{body}");
    assert_eq!(body["problem"]["legal_actions"], json!([]), "{body}");
    assert!(body.get("outcome").is_none(), "{body}");
}

fn assert_published_finding(body: &Value, finding_id: &str, cycle_id: &str) {
    assert_eq!(
        body["contract"]["schema_id"], "schema.application.feedback.get.result",
        "{body}"
    );
    assert_eq!(body["contract"]["schema_revision"], 1_i64, "{body}");
    assert_eq!(body["outcome"]["outcome"], "evidence", "{body}");
    assert_eq!(
        body["outcome"]["value"]["execution"]["termination"], "completed",
        "{body}"
    );
    assert_eq!(
        body["outcome"]["value"]["coverage"]["returned"], 1_i64,
        "{body}"
    );
    assert_eq!(
        body["outcome"]["value"]["coverage"]["completeness"], "complete",
        "{body}"
    );
    let finding = &body["outcome"]["value"]["payload"]["finding"];
    assert_eq!(finding["cycle_id"], cycle_id, "{body}");
    assert_eq!(finding["finding"]["finding_id"], finding_id, "{body}");
    assert_eq!(finding["finding"]["classification"], "new", "{body}");
    assert_eq!(finding["finding"]["lifecycle"], "active", "{body}");
    assert_eq!(
        finding["finding"]["provider_state"], "supported_completed_complete",
        "{body}"
    );
    assert_eq!(
        finding["finding"]["safe_bounded_preview"], PREVIEW,
        "{body}"
    );
    let projection = &finding["finding"]["diagnostic_projection"];
    assert_eq!(projection["code"], "E0425", "{body}");
    assert_eq!(projection["severity"], "error", "{body}");
    assert_eq!(projection["safe_bounded_message"], PREVIEW, "{body}");
    assert_eq!(projection["producer"], "code_diagnostic", "{body}");
    assert_eq!(projection["span"]["start_byte"], 17_i64, "{body}");
    assert_eq!(projection["span"]["end_byte"], 49_i64, "{body}");
    assert!(body.get("problem").is_none(), "{body}");
}

/// Hosts receive the compiler finding when they spend the cycle's get handle,
/// and a typed denial when the handle was never minted for this read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_get_returns_published_finding_and_denies_unknown_handle() {
    let production = production_composition_fixture_with_sources(write_missing_symbol_crate).await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let cargo_output = compiler_diagnostic(&production.project_root);
    let published = call_until_ready(
        &server,
        "tracedecay_diagnose",
        json!({
            "cargo_output": cargo_output,
            "include_callers": false
        }),
        |body| {
            body["published"]["status"] == "published"
                && body["published"]["inserted"] == 1_i64
                && body["diagnostics"][0]["message"] == PREVIEW
                && body["diagnostics"][0]["code"] == "E0425"
        },
    )
    .await;
    assert_eq!(published["diagnostics_parsed"], 1_i64, "{published}");
    assert_eq!(
        published["diagnostics"][0]["message"], PREVIEW,
        "{published}"
    );
    assert_eq!(published["diagnostics"][0]["code"], "E0425", "{published}");
    assert_eq!(published["published"]["inserted"], 1_i64, "{published}");

    let document_uri = url::Url::from_file_path(production.project_root.join("src/lib.rs"))
        .expect("advisory document URI")
        .to_string();
    let cycle = call_until_ready(
        &server,
        "tracedecay_feedback_advisory_cycle",
        json!({ "document_uri": document_uri }),
        |body| {
            body["outcome"]["outcome"] == "evidence"
                && body["outcome"]["value"]["payload"]["cycle"]["published"] == true
                && body["outcome"]["value"]["payload"]["finding_handles"]
                    .as_array()
                    .is_some_and(|handles| {
                        handles.iter().any(|handle| {
                            body["outcome"]["value"]["payload"]["cycle"]["findings"]
                                .as_array()
                                .is_some_and(|findings| {
                                    findings.iter().any(|finding| {
                                        finding["safe_bounded_preview"] == PREVIEW
                                            && finding["finding_id"] == handle["finding_id"]
                                    })
                                })
                        })
                    })
        },
    )
    .await;
    let payload = &cycle["outcome"]["value"]["payload"];
    let finding = payload["cycle"]["findings"]
        .as_array()
        .expect("cycle findings")
        .iter()
        .find(|finding| finding["safe_bounded_preview"] == PREVIEW)
        .expect("published compiler finding");
    let finding_id = finding["finding_id"]
        .as_str()
        .expect("finding id")
        .to_owned();
    let cycle_id = payload["cycle"]["cycle_id"]
        .as_str()
        .expect("cycle id")
        .to_owned();
    let get_handle = payload["finding_handles"]
        .as_array()
        .expect("finding handles")
        .iter()
        .find(|handle| handle["finding_id"] == finding_id)
        .and_then(|handle| handle["get_handle"].as_str())
        .expect("get handle")
        .to_owned();
    let diagnostics_handle = payload["read_handles"]["diagnostics_handle"]
        .as_str()
        .expect("diagnostics handle")
        .to_owned();

    let fetched = call_tool(
        &server,
        "tracedecay_feedback_get",
        json!({ "request_handle": get_handle }),
    )
    .await;
    assert_published_finding(&fetched, &finding_id, &cycle_id);

    let continued = fetched["outcome"]["value"]["payload"]["finding"]["get_handle"]
        .as_str()
        .expect("reminted get handle")
        .to_owned();
    let again = call_tool(
        &server,
        "tracedecay_feedback_get",
        json!({ "request_handle": continued }),
    )
    .await;
    assert_published_finding(&again, &finding_id, &cycle_id);

    let unknown = call_tool(
        &server,
        "tracedecay_feedback_get",
        json!({ "request_handle": UNKNOWN_HANDLE }),
    )
    .await;
    assert_unknown_handle_denied(&unknown);

    let wrong_operation = call_tool(
        &server,
        "tracedecay_feedback_get",
        json!({ "request_handle": diagnostics_handle }),
    )
    .await;
    assert_unknown_handle_denied(&wrong_operation);

    assert_eq!(
        std::fs::read_to_string(production.project_root.join("src/lib.rs")).expect("lib.rs"),
        LIB_RS
    );
    drop(server);
    production.harness.shutdown().await;
}

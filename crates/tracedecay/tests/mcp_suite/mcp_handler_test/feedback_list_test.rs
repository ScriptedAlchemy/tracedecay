#![cfg(feature = "test-transport")]

//! `tracedecay_feedback_list` through the production MCP `tools/call` path.
//!
//! A host cannot list findings until a completed cycle mints a list handle.
//! This test publishes one real compiler diagnostic, then reads it back and
//! checks that other handles are refused rather than returned as an empty page.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support::{production_composition_fixture_with_sources, wait_for_current_graph};

const SYMBOL: &str = "missing_feedback_list_symbol";
const SOURCE: &str = "pub fn entry() { missing_feedback_list_symbol(); }\n";

#[tokio::test]
async fn feedback_list_returns_the_published_compiler_finding_and_denies_other_handles() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).expect("source directory");
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"feedback-list-behavior\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("cargo manifest");
        fs::write(project.join("src/lib.rs"), SOURCE).expect("source");
    })
    .await;

    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;
    drop(server);

    let compiler = compiler_failure(&fixture.project_root);
    let diagnosed = tool_json(
        &mcp_call(
            &fixture,
            "tracedecay_diagnose",
            json!({
                "cargo_output": compiler,
                "include_callers": false,
                "format": "json",
            }),
        )
        .await,
    );
    assert_eq!(
        diagnosed["published"]["status"], "published",
        "compiler output must reach the diagnostic store before list can see it: {diagnosed}"
    );
    assert_eq!(diagnosed["published"]["inserted"], 1, "{diagnosed}");

    let cycle = published_cycle(&fixture).await;
    let list_handle = cycle["read_handles"]["list_handle"]
        .as_str()
        .expect("minted list handle")
        .to_owned();
    let cycle_finding = &cycle["finding_handles"][0];

    let listed_response = mcp_call(
        &fixture,
        "tracedecay_feedback_list",
        json!({
            "request_handle": list_handle,
            "format": "json",
        }),
    )
    .await;
    assert_ne!(
        listed_response["result"]["isError"], true,
        "a minted list handle must not be a tool error: {listed_response}"
    );
    let listed = tool_json(&listed_response);
    let (packet, payload) = evidence(&listed);
    let findings = payload["findings"].as_array().expect("findings array");
    assert_eq!(findings.len(), 1, "{payload}");
    assert_eq!(packet["page"]["total"], 1, "{packet}");
    assert_eq!(packet["page"]["returned"], 1, "{packet}");
    assert!(
        packet["page"]["cursor"].is_null(),
        "one finding is the whole page: {packet}"
    );
    assert_eq!(packet["page"]["returned"], findings.len() as u64);
    assert_eq!(packet["coverage"]["returned"], 1, "{packet}");
    assert_eq!(packet["coverage"]["completeness"], "complete", "{packet}");
    assert_eq!(packet["execution"]["termination"], "completed", "{packet}");

    let finding = &findings[0];
    let projection = &finding["finding"]["diagnostic_projection"];
    let line = SOURCE.trim_end_matches(['\n', '\r']);
    let symbol_start = u64::try_from(line.find(SYMBOL).expect("symbol in source")).unwrap();
    assert_eq!(
        finding["finding"]["finding_id"], cycle_finding["finding_id"],
        "{finding}"
    );
    assert_eq!(finding["cycle_id"], cycle["cycle"]["cycle_id"], "{finding}");
    assert_eq!(
        finding["get_handle"], cycle_finding["get_handle"],
        "{finding}"
    );
    assert_eq!(
        finding["expand_handle"], cycle_finding["expansion_handle"],
        "{finding}"
    );
    assert_ne!(finding["get_handle"], list_handle);
    assert_eq!(finding["finding"]["lifecycle"], "active", "{finding}");
    assert_eq!(finding["finding"]["classification"], "new", "{finding}");
    assert_eq!(
        finding["finding"]["provider_state"], "supported_completed_complete",
        "{finding}"
    );
    let message = format!("cannot find function `{SYMBOL}` in this scope");
    assert_eq!(projection["code"], "E0425", "{projection}");
    assert_eq!(projection["severity"], "error", "{projection}");
    assert_eq!(projection["producer"], "code_diagnostic", "{projection}");
    assert_eq!(
        projection["span"]["start_byte"], symbol_start,
        "{projection}"
    );
    assert_eq!(
        projection["span"]["end_byte"],
        u64::try_from(line.len()).unwrap(),
        "{projection}"
    );
    assert_eq!(projection["safe_bounded_message"], message, "{projection}");
    assert_eq!(
        finding["finding"]["safe_bounded_preview"], message,
        "{finding}"
    );

    let unknown_response = mcp_call(
        &fixture,
        "tracedecay_feedback_list",
        json!({
            "request_handle": "rh_unknown_feedback_list",
            "format": "json",
        }),
    )
    .await;
    let unknown = problem_record(&unknown_response);
    assert_eq!(unknown["kind"], "not_found_or_not_authorized", "{unknown}");
    assert_eq!(unknown["code"], "not_found_or_not_authorized", "{unknown}");
    assert_eq!(unknown["retryable"], false, "{unknown}");

    let get_handle = cycle_finding["get_handle"].as_str().expect("get handle");
    let wrong_operation_response = mcp_call(
        &fixture,
        "tracedecay_feedback_list",
        json!({
            "request_handle": get_handle,
            "format": "json",
        }),
    )
    .await;
    let wrong_operation = problem_record(&wrong_operation_response);
    assert_eq!(
        wrong_operation["kind"], "not_found_or_not_authorized",
        "a get handle must not list findings: {wrong_operation}"
    );
    assert_eq!(
        wrong_operation["code"], "not_found_or_not_authorized",
        "{wrong_operation}"
    );
    assert_eq!(wrong_operation["retryable"], false, "{wrong_operation}");

    let malformed = mcp_call(
        &fixture,
        "tracedecay_feedback_list",
        json!({
            "request_handle": " rh_leading_space",
            "format": "json",
        }),
    )
    .await;
    assert!(
        malformed["result"].is_null(),
        "a malformed handle is a protocol error, not an empty page: {malformed}"
    );
    assert_eq!(malformed["error"]["code"], -32602, "{malformed}");
    assert_eq!(
        malformed["error"]["data"]["kind"], "invalid_request",
        "{malformed}"
    );
    assert_eq!(
        malformed["error"]["data"]["reason_code"], "application_surface_invalid_request",
        "{malformed}"
    );
    assert_eq!(
        malformed["error"]["data"]["retryable"], false,
        "{malformed}"
    );

    fixture.harness.shutdown().await;
}

fn compiler_failure(project: &Path) -> String {
    let output_dir = project
        .parent()
        .expect("project parent")
        .join("compiler-out");
    fs::create_dir_all(&output_dir).expect("compiler output directory");
    let output = Command::new("rustc")
        .current_dir(project)
        .args([
            "--crate-type=lib",
            "--edition=2024",
            "--error-format=short",
            "--color=never",
            "src/lib.rs",
            "--out-dir",
        ])
        .arg(&output_dir)
        .output()
        .expect("run rustc");
    let stderr = String::from_utf8(output.stderr).expect("rustc stderr");
    assert!(
        !output.status.success(),
        "the unresolved call must fail compilation: {stderr}"
    );
    assert!(
        stderr.contains("error[E0425]") && stderr.contains(SYMBOL),
        "{stderr}"
    );
    stderr
}

async fn published_cycle(fixture: &crate::support::ProductionCompositionFixture) -> Value {
    let document_uri = url::Url::from_file_path(fixture.project_root.join("src/lib.rs"))
        .expect("document file URI")
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let response = mcp_call(
            fixture,
            "tracedecay_feedback_advisory_cycle",
            json!({
                "document_uri": document_uri,
                "format": "json",
            }),
        )
        .await;
        if retryable_mount(&response) {
            assert!(
                Instant::now() < deadline,
                "advisory cycle stayed unavailable: {response}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        let body = tool_json(&response);
        let (_packet, payload) = evidence(&body);
        assert_eq!(payload["cycle"]["published"], true, "{payload}");
        assert_eq!(payload["cycle"]["durability"], "durable", "{payload}");
        assert_eq!(
            payload["finding_handles"].as_array().map(Vec::len),
            Some(1),
            "{payload}"
        );
        return payload.clone();
    }
}

fn retryable_mount(response: &Value) -> bool {
    let reason = response["error"]["data"]["reason_code"].as_str();
    let code = response["result"]["problem"]["code"].as_str();
    let retryable = response["error"]["data"]["retryable"] == true
        || response["result"]["problem"]["retryable"] == true;
    retryable
        && matches!(
            reason.or(code),
            Some(
                "feedback.advisory-cycle.unavailable"
                    | "feedback.owner_unavailable"
                    | "code-graph-unavailable"
                    | "project_warming"
            )
        )
}

async fn mcp_call(
    fixture: &crate::support::ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} MCP call failed: {error}"));
    serde_json::to_value(&response)
        .unwrap_or_else(|error| panic!("{tool_name} MCP response was not JSON: {error}"))
}

fn tool_json(response: &Value) -> Value {
    assert!(
        response["error"].is_null(),
        "MCP tools/call failed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("MCP tool returned no text: {response}"));
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("MCP tool text was not JSON ({error}): {text}"))
}

fn evidence(envelope: &Value) -> (&Value, &Value) {
    assert_eq!(envelope["outcome"]["outcome"], "evidence", "{envelope}");
    let packet = &envelope["outcome"]["value"];
    let payload = packet
        .get("payload")
        .unwrap_or_else(|| panic!("evidence omitted its payload: {envelope}"));
    (packet, payload)
}

fn problem_record(response: &Value) -> &Value {
    assert_eq!(
        response["result"]["isError"], true,
        "a refused list must be a tool error, not an empty success: {response}"
    );
    let record = &response["result"]["problem"];
    assert!(
        record.is_object(),
        "refused list omitted its problem: {response}"
    );
    record
}

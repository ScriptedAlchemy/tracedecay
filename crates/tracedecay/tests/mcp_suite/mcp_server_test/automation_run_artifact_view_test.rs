//! `tracedecay_automation_run_artifact_view` over the MCP `tools/call` transport.
//!
//! The tool reads one artifact from the newest committed ledger row for that
//! run id, hash-checks the bytes on disk, and returns that payload. An earlier
//! row for the same id, a sibling artifact, and every other run must not
//! appear. A missing run or kind is a typed not-found. A rewritten artifact
//! file is rejected instead of returned.

use super::support::{jsonrpc_request, response_with_id, run_server_with_messages};
use crate::support::init_test_project;
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use tracedecay::mcp::McpServer;
use tracedecay_automation_runtime::automation::backend::AgentTaskKind;
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifactKind, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    append_run_record, run_artifact_path, run_ledger_path, write_run_artifact,
};

const EXACT_RUN_ID: &str = "run-artifact-exact";
const OTHER_RUN_ID: &str = "run-artifact-other";
const TAMPERED_RUN_ID: &str = "run-artifact-tampered";
const TOOL: &str = "tracedecay_automation_run_artifact_view";

/// Pretty bytes `write_run_artifact` stores. The `sha256:` digests in the
/// expected tool text are SHA-256 of these bytes, not of the compact response.
const HANDOFF_PRETTY: &str = "{\n  \"marker\": \"handoff-only\",\n  \"next_actions\": [\n    \"inspect the exact handoff payload\"\n  ],\n  \"status\": \"ready_for_review\"\n}";
const TRACES_PRETTY: &str = "{\n  \"status\": \"captured\",\n  \"trace_id\": \"trace-exact-1\"\n}";
const OTHER_PRETTY: &str = "{\n  \"marker\": \"other-run-only\",\n  \"status\": \"other-run\"\n}";
const TAMPERED_BYTES: &str = "{\"status\":\"tampered\"}";

const HANDOFF_JSON: &str = r#"{"artifact":{"created_at":"1782283200","kind":"codex_handoff","path":"automation_artifacts/run-artifact-exact/codex_handoff.json","schema_version":1,"sha256":"sha256:82be844c6577a43eef813735ae4df816584317daf0d92456853326983d406635","summary":"handoff ready"},"payload":{"marker":"handoff-only","next_actions":["inspect the exact handoff payload"],"status":"ready_for_review"},"run_id":"run-artifact-exact","status":"ok"}"#;

const TRACES_JSON: &str = r#"{"artifact":{"created_at":"1782283200","kind":"traces","path":"automation_artifacts/run-artifact-exact/traces.json","schema_version":1,"sha256":"sha256:7c1ae0a3b7ab5692b9d91d06839e176bf9baf590cb7992ab8face9578dbd1a87","summary":"traces captured"},"payload":{"status":"captured","trace_id":"trace-exact-1"},"run_id":"run-artifact-exact","status":"ok"}"#;

const OTHER_JSON: &str = r#"{"artifact":{"created_at":"1782283300","kind":"codex_handoff","path":"automation_artifacts/run-artifact-other/codex_handoff.json","schema_version":1,"sha256":"sha256:3e3b7769c8c9571db592f60cef6bdb985268b95501f175ab3467584fbd8c294d","summary":"other ready"},"payload":{"marker":"other-run-only","status":"other-run"},"run_id":"run-artifact-other","status":"ok"}"#;

const HANDOFF_MARKDOWN: &str = "\
## Automation Run Artifact
**status:** ok
**run_id:** run-artifact-exact
**kind:** codex_handoff
**path:** automation_artifacts/run-artifact-exact/codex_handoff.json
**sha256:** sha256:82be844c6577a43eef813735ae4df816584317daf0d92456853326983d406635

### Payload
**marker:** handoff-only
**status:** ready_for_review

## next_actions
- inspect the exact handoff payload
";

const TRACES_MARKDOWN: &str = "\
## Automation Run Artifact
**status:** ok
**run_id:** run-artifact-exact
**kind:** traces
**path:** automation_artifacts/run-artifact-exact/traces.json
**sha256:** sha256:7c1ae0a3b7ab5692b9d91d06839e176bf9baf590cb7992ab8face9578dbd1a87

### Payload
**status:** captured
**trace_id:** `trace-exact-1`
";

fn base_record(
    run_id: &str,
    status: AutomationRunStatus,
    completed_at: &str,
) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_owned(),
        trigger: AutomationTrigger::Scheduler,
        task: AgentTaskKind::SessionReflector,
        task_key: Some("session_reflector".to_owned()),
        backend: "codex_app_server".to_owned(),
        backend_identity: None,
        host_mode: Some("standalone".to_owned()),
        prompt_version: Some("session_reflector:v1".to_owned()),
        response_schema: None,
        strict_json: Some(true),
        model: Some("test-model".to_owned()),
        status,
        evidence_hash: None,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        skipped_count: 0,
        error: None,
        error_classification: None,
        error_retryable: None,
        backend_attempt_count: 0,
        backend_attempts: Vec::new(),
        fallback_status: None,
        report_ref: None,
        artifacts: Vec::new(),
        started_at: "1782283199".to_owned(),
        completed_at: completed_at.to_owned(),
        completed_at_micros: None,
    }
}

fn view_request(id: i64, arguments: Value) -> String {
    jsonrpc_request(
        json!(id),
        "tools/call",
        json!({
            "name": TOOL,
            "arguments": arguments,
        }),
    )
}

fn assert_tool_text(response: &Value, id: i64, expected: &str) {
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], id);
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "tools/call must succeed: {response}"
    );
    assert_eq!(
        response["result"]["content"],
        json!([{ "type": "text", "text": expected }]),
        "tool text must be the exact artifact rendering"
    );
}

fn invalid_params(id: i64, message: &str, reason_code: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32602,
            "message": message,
            "data": {
                "tool": TOOL,
                "reason_code": reason_code,
                "retryable": false,
                "detail": message
            }
        }
    })
}

fn internal_error(id: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32603,
            "message": message,
            "data": {
                "tool": TOOL,
                "cli_fallback": "This tool is also available from the shell: `tracedecay tool automation_run_artifact_view ...` (`tracedecay tool automation_run_artifact_view --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
            }
        }
    })
}

#[tokio::test]
async fn automation_run_artifact_view_returns_the_exact_hash_checked_payload() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let (cg, _env) = init_test_project(&project).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();

    let queued_artifact = write_run_artifact(
        &dashboard_root,
        EXACT_RUN_ID,
        AutomationRunArtifactKind::CodexHandoff,
        &json!({
            "status": "queued-stale",
            "marker": "queued-stale-must-not-appear",
        }),
        Some("queued stale".to_owned()),
        "1782283100",
    )
    .await
    .unwrap();
    let mut queued = base_record(EXACT_RUN_ID, AutomationRunStatus::Queued, "1782283100");
    queued.artifacts = vec![queued_artifact];
    append_run_record(&dashboard_root, &queued).await.unwrap();

    let handoff = write_run_artifact(
        &dashboard_root,
        EXACT_RUN_ID,
        AutomationRunArtifactKind::CodexHandoff,
        &json!({
            "status": "ready_for_review",
            "next_actions": ["inspect the exact handoff payload"],
            "marker": "handoff-only",
        }),
        Some("handoff ready".to_owned()),
        "1782283200",
    )
    .await
    .unwrap();
    let traces = write_run_artifact(
        &dashboard_root,
        EXACT_RUN_ID,
        AutomationRunArtifactKind::Traces,
        &json!({
            "status": "captured",
            "trace_id": "trace-exact-1",
        }),
        Some("traces captured".to_owned()),
        "1782283200",
    )
    .await
    .unwrap();
    let mut succeeded = base_record(EXACT_RUN_ID, AutomationRunStatus::Succeeded, "1782283200");
    succeeded.reviewed_count = 3;
    succeeded.accepted_count = 2;
    succeeded.artifacts = vec![handoff, traces];
    append_run_record(&dashboard_root, &succeeded)
        .await
        .unwrap();

    let other = write_run_artifact(
        &dashboard_root,
        OTHER_RUN_ID,
        AutomationRunArtifactKind::CodexHandoff,
        &json!({
            "marker": "other-run-only",
            "status": "other-run",
        }),
        Some("other ready".to_owned()),
        "1782283300",
    )
    .await
    .unwrap();
    let mut other_record = base_record(OTHER_RUN_ID, AutomationRunStatus::Succeeded, "1782283300");
    other_record.artifacts = vec![other];
    append_run_record(&dashboard_root, &other_record)
        .await
        .unwrap();

    let tampered = write_run_artifact(
        &dashboard_root,
        TAMPERED_RUN_ID,
        AutomationRunArtifactKind::CodexHandoff,
        &json!({ "status": "original" }),
        Some("will not match".to_owned()),
        "1782283400",
    )
    .await
    .unwrap();
    let mut tampered_record = base_record(
        TAMPERED_RUN_ID,
        AutomationRunStatus::Succeeded,
        "1782283400",
    );
    tampered_record.artifacts = vec![tampered];
    append_run_record(&dashboard_root, &tampered_record)
        .await
        .unwrap();
    let tampered_path = run_artifact_path(
        &dashboard_root,
        TAMPERED_RUN_ID,
        AutomationRunArtifactKind::CodexHandoff,
    )
    .unwrap();
    fs::write(&tampered_path, TAMPERED_BYTES).unwrap();

    let ledger_path = run_ledger_path(&dashboard_root);
    let ledger_before = fs::read(&ledger_path).unwrap();
    let handoff_path = run_artifact_path(
        &dashboard_root,
        EXACT_RUN_ID,
        AutomationRunArtifactKind::CodexHandoff,
    )
    .unwrap();
    let traces_path = run_artifact_path(
        &dashboard_root,
        EXACT_RUN_ID,
        AutomationRunArtifactKind::Traces,
    )
    .unwrap();
    let other_path = run_artifact_path(
        &dashboard_root,
        OTHER_RUN_ID,
        AutomationRunArtifactKind::CodexHandoff,
    )
    .unwrap();

    let server = Box::pin(McpServer::new(cg.into_inner(), None)).await;
    let responses = run_server_with_messages(
        server,
        vec![
            view_request(
                1,
                json!({ "run_id": EXACT_RUN_ID, "kind": "codex_handoff", "format": "json" }),
            ),
            view_request(
                2,
                json!({ "run_id": EXACT_RUN_ID, "kind": "codex_handoff" }),
            ),
            view_request(
                3,
                json!({ "run_id": EXACT_RUN_ID, "kind": "traces", "format": "json" }),
            ),
            view_request(4, json!({ "run_id": EXACT_RUN_ID, "kind": "traces" })),
            view_request(
                5,
                json!({ "run_id": OTHER_RUN_ID, "kind": "codex_handoff", "format": "json" }),
            ),
            view_request(
                6,
                json!({ "run_id": EXACT_RUN_ID, "kind": "generated_evals", "format": "json" }),
            ),
            view_request(
                7,
                json!({ "run_id": "run-missing", "kind": "codex_handoff" }),
            ),
            view_request(8, json!({})),
            view_request(9, json!({ "run_id": EXACT_RUN_ID })),
            view_request(10, json!({ "run_id": "", "kind": "codex_handoff" })),
            view_request(11, json!({ "run_id": EXACT_RUN_ID, "kind": "" })),
            view_request(
                12,
                json!({ "run_id": TAMPERED_RUN_ID, "kind": "codex_handoff", "format": "json" }),
            ),
        ],
    )
    .await;

    assert_tool_text(&response_with_id(&responses, json!(1)), 1, HANDOFF_JSON);
    assert_tool_text(&response_with_id(&responses, json!(2)), 2, HANDOFF_MARKDOWN);
    assert_tool_text(&response_with_id(&responses, json!(3)), 3, TRACES_JSON);
    assert_tool_text(&response_with_id(&responses, json!(4)), 4, TRACES_MARKDOWN);
    assert_tool_text(&response_with_id(&responses, json!(5)), 5, OTHER_JSON);

    assert_eq!(
        response_with_id(&responses, json!(6)),
        invalid_params(
            6,
            "automation run artifact not found: run-artifact-exact/generated_evals",
            "not_found"
        )
    );
    assert_eq!(
        response_with_id(&responses, json!(7)),
        invalid_params(7, "automation run not found: run-missing", "not_found")
    );
    assert_eq!(
        response_with_id(&responses, json!(8)),
        invalid_params(
            8,
            "missing required parameter: run_id",
            "missing_required_parameter"
        )
    );
    assert_eq!(
        response_with_id(&responses, json!(9)),
        invalid_params(
            9,
            "missing required parameter: kind",
            "missing_required_parameter"
        )
    );
    assert_eq!(
        response_with_id(&responses, json!(10)),
        internal_error(
            10,
            "tool execution failed: config error: automation run_id '' is not safe for artifact paths"
        )
    );
    assert_eq!(
        response_with_id(&responses, json!(11)),
        invalid_params(
            11,
            "automation run artifact not found: run-artifact-exact/",
            "not_found"
        )
    );
    assert_eq!(
        response_with_id(&responses, json!(12)),
        internal_error(
            12,
            "tool execution failed: config error: automation run artifact 'automation_artifacts/run-artifact-tampered/codex_handoff.json' hash mismatch"
        )
    );

    assert_eq!(
        fs::read(&ledger_path).unwrap(),
        ledger_before,
        "artifact view must not rewrite the automation ledger"
    );
    assert_eq!(fs::read(&handoff_path).unwrap(), HANDOFF_PRETTY.as_bytes());
    assert_eq!(fs::read(&traces_path).unwrap(), TRACES_PRETTY.as_bytes());
    assert_eq!(fs::read(&other_path).unwrap(), OTHER_PRETTY.as_bytes());
    assert_eq!(fs::read(&tampered_path).unwrap(), TAMPERED_BYTES.as_bytes());
}

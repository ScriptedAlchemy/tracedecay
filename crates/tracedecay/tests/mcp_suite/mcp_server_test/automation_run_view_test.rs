//! `tracedecay_automation_run_view` over the MCP `tools/call` transport.
//!
//! The tool returns the newest committed lifecycle row for one run id from
//! the active project's ledger. An earlier row for that id, and every other
//! run, must not appear. A missing id is a typed not-found and does not
//! enumerate the ledger.

use super::support::{jsonrpc_request, response_with_id, run_server_with_messages};
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;
use tracedecay::mcp::McpServer;
use tracedecay_automation_runtime::automation::backend::{AgentTaskFailureClass, AgentTaskKind};
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifact, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    append_run_record, run_ledger_path,
};

const EXACT_RUN_ID: &str = "run-view-exact";
const FAILED_RUN_ID: &str = "run-view-failed";
const QUEUED_MODEL: &str = "queued-model";

const EXACT_JSON: &str = r#"{"run":{"accepted_count":2,"artifacts":[{"created_at":"1782283200","kind":"codex_handoff","path":"automation_artifacts/run-view-exact/codex_handoff.json","schema_version":1,"sha256":"sha256:handoff","summary":"handoff ready"}],"backend":"codex_app_server","backend_attempt_count":1,"completed_at":"1782283200","completed_at_micros":1782283200000000,"host_mode":"standalone","model":"test-model","prompt_version":"session_reflector:v1","rejected_count":1,"reviewed_count":3,"run_id":"run-view-exact","schema_version":2,"skipped_count":0,"started_at":"1782283199","status":"succeeded","strict_json":true,"task":"session_reflector","task_key":"session_reflector","trigger":"scheduler","validation_report":{"passed":true}},"scope":"active_project","status":"ok"}"#;

const FAILED_JSON: &str = r#"{"run":{"accepted_count":0,"backend":"codex_app_server","backend_attempt_count":2,"completed_at":"1782283400","error":"backend_disabled","error_classification":"permanent","error_retryable":false,"host_mode":"standalone","model":"test-model","prompt_version":"memory_curator:v1","rejected_count":4,"reviewed_count":4,"run_id":"run-view-failed","schema_version":2,"skipped_count":1,"started_at":"1782283100","status":"failed","strict_json":true,"task":"memory_curator","task_key":"memory_curator","trigger":"manual_cli"},"scope":"active_project","status":"ok"}"#;

const EXACT_MARKDOWN: &str = "\
## Automation Run: run-view-exact
**status:** ok
**task:** session_reflector
**trigger:** scheduler
**backend:** codex_app_server
**model:** test-model
**status:** succeeded
**started_at:** 1782283199
**completed_at:** 1782283200
**reviewed_count:** 3
**accepted_count:** 2
**rejected_count:** 1
**skipped_count:** 0

### Artifacts
- **codex_handoff** - sha256: sha256:handoff
";

fn base_record(
    run_id: &str,
    trigger: AutomationTrigger,
    task: AgentTaskKind,
    task_key: &str,
    status: AutomationRunStatus,
    completed_at: &str,
) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_owned(),
        trigger,
        task,
        task_key: Some(task_key.to_owned()),
        backend: "codex_app_server".to_owned(),
        backend_identity: None,
        host_mode: Some("standalone".to_owned()),
        prompt_version: Some(format!("{task_key}:v1")),
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
            "name": "tracedecay_automation_run_view",
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
        "tool text must be the exact ledger rendering"
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
                "tool": "tracedecay_automation_run_view",
                "reason_code": reason_code,
                "retryable": false,
                "detail": message
            }
        }
    })
}

#[tokio::test]
async fn automation_run_view_returns_the_exact_active_project_record() {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let cg = crate::fixture::init_project_from_template(project)
        .await
        .unwrap();
    let dashboard_root = cg.store_layout().dashboard_root.clone();

    let mut queued = base_record(
        EXACT_RUN_ID,
        AutomationTrigger::Scheduler,
        AgentTaskKind::SessionReflector,
        "session_reflector",
        AutomationRunStatus::Queued,
        "1782283190",
    );
    queued.model = Some(QUEUED_MODEL.to_owned());
    queued.reviewed_count = 1;
    append_run_record(&dashboard_root, &queued).await.unwrap();

    let mut succeeded = base_record(
        EXACT_RUN_ID,
        AutomationTrigger::Scheduler,
        AgentTaskKind::SessionReflector,
        "session_reflector",
        AutomationRunStatus::Succeeded,
        "1782283200",
    );
    succeeded.reviewed_count = 3;
    succeeded.accepted_count = 2;
    succeeded.rejected_count = 1;
    succeeded.backend_attempt_count = 1;
    succeeded.completed_at_micros = Some(1_782_283_200_000_000);
    succeeded.validation_report = Some(json!({ "passed": true }));
    succeeded.artifacts = vec![AutomationRunArtifact {
        schema_version: 1,
        kind: "codex_handoff".to_owned(),
        path: "automation_artifacts/run-view-exact/codex_handoff.json".to_owned(),
        sha256: "sha256:handoff".to_owned(),
        summary: Some("handoff ready".to_owned()),
        created_at: "1782283200".to_owned(),
    }];
    append_run_record(&dashboard_root, &succeeded)
        .await
        .unwrap();

    let mut failed = base_record(
        FAILED_RUN_ID,
        AutomationTrigger::ManualCli,
        AgentTaskKind::MemoryCurator,
        "memory_curator",
        AutomationRunStatus::Failed,
        "1782283400",
    );
    failed.started_at = "1782283100".to_owned();
    failed.reviewed_count = 4;
    failed.rejected_count = 4;
    failed.skipped_count = 1;
    failed.backend_attempt_count = 2;
    failed.error = Some("backend_disabled".to_owned());
    failed.error_classification = Some(AgentTaskFailureClass::Permanent);
    failed.error_retryable = Some(false);
    append_run_record(&dashboard_root, &failed).await.unwrap();

    let ledger_path = run_ledger_path(&dashboard_root);
    let ledger_before = fs::read(&ledger_path).unwrap();
    let server = Box::pin(McpServer::new(cg, None)).await;
    let responses = run_server_with_messages(
        server,
        vec![
            view_request(1, json!({ "run_id": EXACT_RUN_ID, "format": "json" })),
            view_request(2, json!({ "run_id": EXACT_RUN_ID })),
            view_request(3, json!({ "run_id": FAILED_RUN_ID, "format": "json" })),
            view_request(4, json!({ "run_id": "run-missing", "format": "json" })),
            view_request(5, json!({ "run_id": "", "format": "json" })),
            view_request(6, json!({})),
        ],
    )
    .await;

    assert_tool_text(&response_with_id(&responses, json!(1)), 1, EXACT_JSON);
    assert_tool_text(&response_with_id(&responses, json!(2)), 2, EXACT_MARKDOWN);
    assert_tool_text(&response_with_id(&responses, json!(3)), 3, FAILED_JSON);

    assert_eq!(
        response_with_id(&responses, json!(4)),
        invalid_params(4, "automation run not found: run-missing", "not_found")
    );
    assert_eq!(
        response_with_id(&responses, json!(5)),
        invalid_params(
            5,
            "missing required parameter: run_id",
            "missing_required_parameter"
        )
    );
    assert_eq!(
        response_with_id(&responses, json!(6)),
        invalid_params(
            6,
            "missing required parameter: run_id",
            "missing_required_parameter"
        )
    );

    assert_eq!(
        fs::read(&ledger_path).unwrap(),
        ledger_before,
        "view must not rewrite the automation ledger"
    );
}

use crate::support::*;
use serde_json::json;
use std::fs;
use tempfile::TempDir;
use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger, append_run_record,
};

fn run_record(run_id: &str, completed_at: &str) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_owned(),
        trigger: AutomationTrigger::Scheduler,
        task: tracedecay_agent_hosts::automation::backend::AgentTaskKind::SessionReflector,
        task_key: Some("session_reflector".to_owned()),
        backend: "codex_app_server".to_owned(),
        host_mode: Some("standalone".to_owned()),
        prompt_version: Some("session_reflector:v1".to_owned()),
        response_schema: None,
        strict_json: Some(true),
        model: Some("test-model".to_owned()),
        status: AutomationRunStatus::Succeeded,
        evidence_hash: None,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 3,
        accepted_count: 2,
        rejected_count: 1,
        skipped_count: 0,
        error: None,
        error_classification: None,
        error_retryable: None,
        backend_attempt_count: 1,
        backend_attempts: Vec::new(),
        fallback_status: None,
        report_ref: None,
        artifacts: Vec::new(),
        started_at: "1782283199".to_owned(),
        completed_at: completed_at.to_owned(),
        completed_at_micros: None,
    }
}

#[test]
fn automation_run_read_definitions_are_bounded_and_read_only() {
    let tools = tracedecay::mcp::get_tool_definitions().expect("tool definitions");
    let list = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_automation_run_list")
        .expect("automation run list definition");
    let view = tools
        .iter()
        .find(|tool| tool.name == "tracedecay_automation_run_view")
        .expect("automation run view definition");

    assert_eq!(list.annotations.as_ref().unwrap()["readOnlyHint"], true);
    assert_eq!(list.input_schema["properties"]["limit"]["minimum"], 1);
    assert_eq!(list.input_schema["properties"]["limit"]["maximum"], 200);
    assert_eq!(view.annotations.as_ref().unwrap()["readOnlyHint"], true);
    assert_eq!(view.input_schema["required"], json!(["run_id"]));
}

#[tokio::test]
async fn automation_run_list_and_view_use_the_active_project_ledger() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let (cg, _env) = init_test_project(&project).await;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    append_run_record(&dashboard_root, &run_record("run-older", "1782283200"))
        .await
        .unwrap();
    append_run_record(&dashboard_root, &run_record("run-newer", "1782283300"))
        .await
        .unwrap();

    let listed = handle_tool_call(
        &cg,
        "tracedecay_automation_run_list",
        json!({"limit": 1, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let list_payload = extract_json(&listed.value);
    assert_eq!(list_payload["scope"], "active_project");
    assert_eq!(list_payload["count"], 1);
    assert_eq!(list_payload["limit"], 1);
    assert_eq!(list_payload["has_more"], true);
    assert_eq!(list_payload["malformed_row_count"], 0);
    assert_eq!(list_payload["completeness"], "partial");
    assert_eq!(list_payload["runs"][0]["run_id"], "run-newer");

    let viewed = handle_tool_call(
        &cg,
        "tracedecay_automation_run_view",
        json!({"run_id": "run-older", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let view_payload = extract_json(&viewed.value);
    assert_eq!(view_payload["scope"], "active_project");
    assert_eq!(view_payload["run"]["run_id"], "run-older");
    assert_eq!(view_payload["run"]["reviewed_count"], 3);

    let missing = handle_tool_call(
        &cg,
        "tracedecay_automation_run_view",
        json!({"run_id": "run-missing"}),
        None,
        None,
    )
    .await
    .unwrap_err();
    let message = missing.to_string();
    assert!(message.contains("automation run not found: run-missing"));
    assert!(!message.contains("run-older"));
    assert!(!message.contains("run-newer"));

    close_test_graph(cg).await;
}

use tempfile::tempdir;

use tracedecay::automation::backend::AgentTaskKind;
use tracedecay::automation::run_ledger::{
    append_run_record, load_run_records, run_ledger_path, AutomationRunLedgerRecord,
    AutomationRunStatus, AutomationTrigger,
};

fn record(run_id: &str, status: AutomationRunStatus) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 1,
        run_id: run_id.to_string(),
        trigger: AutomationTrigger::ManualCli,
        task: AgentTaskKind::MemoryCurator,
        backend: "fake".to_string(),
        model: Some("test-model".to_string()),
        status,
        evidence_hash: Some("sha256:abc".to_string()),
        proposed_ops: None,
        accepted_count: 1,
        rejected_count: 0,
        error: None,
        started_at: "2026-06-24T05:00:00Z".to_string(),
        completed_at: "2026-06-24T05:00:01Z".to_string(),
    }
}

#[tokio::test]
async fn run_ledger_appends_jsonl_under_dashboard_root() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");

    append_run_record(
        &dashboard_root,
        &record("run-1", AutomationRunStatus::Succeeded),
    )
    .await
    .unwrap();
    append_run_record(
        &dashboard_root,
        &record("run-2", AutomationRunStatus::Failed),
    )
    .await
    .unwrap();

    let path = run_ledger_path(&dashboard_root);
    let contents = tokio::fs::read_to_string(&path).await.unwrap();
    assert_eq!(contents.lines().count(), 2);
    assert!(contents.contains("\"run_id\":\"run-1\""));
    assert!(contents.contains("\"run_id\":\"run-2\""));

    let loaded = load_run_records(&dashboard_root, 10).await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].run_id, "run-2");
    assert_eq!(loaded[1].run_id, "run-1");
    assert_eq!(loaded[0].status, AutomationRunStatus::Failed);
}

#[tokio::test]
async fn run_ledger_limit_and_malformed_lines_are_handled() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");

    append_run_record(
        &dashboard_root,
        &record("run-1", AutomationRunStatus::Succeeded),
    )
    .await
    .unwrap();
    tokio::fs::write(
        run_ledger_path(&dashboard_root),
        "{\"run_id\":\"older\",\"schema_version\":1}\nnot json\n",
    )
    .await
    .unwrap();
    append_run_record(
        &dashboard_root,
        &record("run-2", AutomationRunStatus::Succeeded),
    )
    .await
    .unwrap();

    let loaded = load_run_records(&dashboard_root, 1).await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].run_id, "run-2");
}

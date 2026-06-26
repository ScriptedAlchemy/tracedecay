use tempfile::tempdir;

use tracedecay::automation::backend::AgentTaskKind;
use tracedecay::automation::run_ledger::{
    append_run_record, find_run_record, load_run_records, read_run_artifact_payload,
    run_artifact_path, run_ledger_path, write_run_artifact, AutomationRunArtifactKind,
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
};

fn record(run_id: &str, status: AutomationRunStatus) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_string(),
        trigger: AutomationTrigger::ManualCli,
        task: AgentTaskKind::MemoryCurator,
        task_key: Some("memory_curator".to_string()),
        backend: "fake".to_string(),
        host_mode: Some("standalone".to_string()),
        prompt_version: Some("memory_curator:v1".to_string()),
        response_schema: None,
        strict_json: None,
        model: Some("test-model".to_string()),
        status,
        evidence_hash: Some("sha256:abc".to_string()),
        input_hash: Some("sha256:input".to_string()),
        output_hash: Some("sha256:output".to_string()),
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 1,
        accepted_count: 1,
        rejected_count: 0,
        skipped_count: 0,
        error: None,
        error_classification: None,
        error_retryable: None,
        fallback_status: None,
        report_ref: None,
        artifacts: Vec::new(),
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
    assert_eq!(loaded[0].host_mode.as_deref(), Some("standalone"));
    assert_eq!(loaded[0].input_hash.as_deref(), Some("sha256:input"));
    assert_eq!(loaded[0].output_hash.as_deref(), Some("sha256:output"));
}

#[tokio::test]
async fn run_ledger_coalesces_lifecycle_records_by_latest_run_status() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");

    append_run_record(
        &dashboard_root,
        &record("run-1", AutomationRunStatus::Queued),
    )
    .await
    .unwrap();
    append_run_record(
        &dashboard_root,
        &record("run-1", AutomationRunStatus::Running),
    )
    .await
    .unwrap();
    append_run_record(
        &dashboard_root,
        &record("run-1", AutomationRunStatus::Succeeded),
    )
    .await
    .unwrap();
    append_run_record(
        &dashboard_root,
        &record("run-2", AutomationRunStatus::Queued),
    )
    .await
    .unwrap();

    let raw = tokio::fs::read_to_string(run_ledger_path(&dashboard_root))
        .await
        .unwrap();
    assert_eq!(raw.lines().count(), 4);
    assert!(raw.contains("\"status\":\"queued\""));
    assert!(raw.contains("\"status\":\"running\""));

    let loaded = load_run_records(&dashboard_root, 10).await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].run_id, "run-2");
    assert_eq!(loaded[0].status, AutomationRunStatus::Queued);
    assert_eq!(loaded[1].run_id, "run-1");
    assert_eq!(loaded[1].status, AutomationRunStatus::Succeeded);
    assert!(!loaded[0].status.is_terminal());
    assert!(loaded[1].status.is_terminal());
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

#[tokio::test]
async fn run_ledger_loads_legacy_records_without_new_optional_fields() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let legacy = serde_json::json!({
        "schema_version": 1,
        "run_id": "legacy-run",
        "trigger": "manual_cli",
        "task": "memory_curator",
        "backend": "codex_app_server",
        "model": "legacy-model",
        "status": "succeeded",
        "evidence_hash": "sha256:evidence",
        "proposed_ops": null,
        "accepted_count": 1,
        "rejected_count": 0,
        "error": null,
        "started_at": "2026-06-24T05:00:00Z",
        "completed_at": "2026-06-24T05:00:01Z"
    });
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    tokio::fs::write(run_ledger_path(&dashboard_root), format!("{legacy}\n"))
        .await
        .unwrap();

    let loaded = load_run_records(&dashboard_root, 10).await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].run_id, "legacy-run");
    assert_eq!(loaded[0].host_mode, None);
    assert_eq!(loaded[0].input_hash, None);
    assert_eq!(loaded[0].applied_ops, None);
    assert_eq!(loaded[0].fallback_status, None);
    assert!(loaded[0].artifacts.is_empty());
}

#[tokio::test]
async fn run_artifacts_write_sidecar_metadata_without_embedding_payloads() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let payload = serde_json::json!({
        "status": "blocked_pending_feedback_or_evals",
        "large_payload": "not embedded in ledger",
    });
    let artifact = write_run_artifact(
        &dashboard_root,
        "run_artifact_1",
        AutomationRunArtifactKind::ValidationGate,
        &payload,
        Some("validation gate".to_string()),
        "2026-06-24T05:00:02Z",
    )
    .await
    .unwrap();
    let mut record = record("run_artifact_1", AutomationRunStatus::Succeeded);
    record.artifacts = vec![artifact.clone()];
    append_run_record(&dashboard_root, &record).await.unwrap();

    let artifact_path = run_artifact_path(
        &dashboard_root,
        "run_artifact_1",
        AutomationRunArtifactKind::ValidationGate,
    )
    .unwrap();
    let artifact_contents = tokio::fs::read_to_string(artifact_path).await.unwrap();
    assert!(artifact_contents.contains("blocked_pending_feedback_or_evals"));

    let ledger_contents = tokio::fs::read_to_string(run_ledger_path(&dashboard_root))
        .await
        .unwrap();
    assert!(!ledger_contents.contains("large_payload"));

    let loaded = load_run_records(&dashboard_root, 10).await.unwrap();
    assert_eq!(loaded[0].artifacts, vec![artifact]);
    assert_eq!(loaded[0].artifacts[0].kind, "validation_gate");
    assert!(loaded[0].artifacts[0].sha256.starts_with("sha256:"));
}

#[tokio::test]
async fn run_artifacts_read_only_from_matching_run_directory() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let payload = serde_json::json!({
        "status": "ready_for_review",
        "run_id": "artifact_run_1",
    });
    let artifact = write_run_artifact(
        &dashboard_root,
        "artifact_run_1",
        AutomationRunArtifactKind::CodexHandoff,
        &payload,
        Some("handoff".to_string()),
        "2026-06-24T05:00:02Z",
    )
    .await
    .unwrap();

    let loaded = read_run_artifact_payload(&dashboard_root, "artifact_run_1", &artifact)
        .await
        .unwrap();
    assert_eq!(loaded, payload);

    let mut wrong_run_artifact = artifact;
    wrong_run_artifact.path = "automation_artifacts/other_run/codex_handoff.json".to_string();
    let err = read_run_artifact_payload(&dashboard_root, "artifact_run_1", &wrong_run_artifact)
        .await
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("does not match run 'artifact_run_1'"));
}

#[tokio::test]
async fn run_ledger_finds_requested_run_beyond_listing_limit() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");

    append_run_record(
        &dashboard_root,
        &record("old-run", AutomationRunStatus::Succeeded),
    )
    .await
    .unwrap();
    append_run_record(
        &dashboard_root,
        &record("new-run", AutomationRunStatus::Failed),
    )
    .await
    .unwrap();

    let listed = load_run_records(&dashboard_root, 1).await.unwrap();
    assert_eq!(listed[0].run_id, "new-run");

    let found = find_run_record(&dashboard_root, "old-run").await.unwrap();
    assert_eq!(found.unwrap().run_id, "old-run");
}

use std::sync::Arc;

use tempfile::tempdir;
use tokio::sync::Barrier;
use tracedecay_runtime_core::storage::PrivateStoreIo;

use tracedecay_automation_runtime::automation::backend::AgentTaskKind;
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifactKind, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    append_run_record, find_run_record, load_run_ledger_task_summary, load_run_records,
    read_run_artifact_payload, run_artifact_path, run_ledger_path, write_run_artifact,
};

fn record(run_id: &str, status: AutomationRunStatus) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_string(),
        trigger: AutomationTrigger::ManualCli,
        task: AgentTaskKind::MemoryCurator,
        task_key: Some("memory_curator".to_string()),
        backend: "fake".to_string(),
        backend_identity: None,
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
        backend_attempt_count: 0,
        backend_attempts: Vec::new(),
        fallback_status: None,
        session_evidence_budget_stage: None,
        report_ref: None,
        artifacts: Vec::new(),
        started_at: "1782277200".to_string(),
        completed_at: "1782277201".to_string(),
        completed_at_micros: Some(1_782_277_201_000_000),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_ledger_keeps_concurrent_jsonl_writes_intact() {
    let temp = tempdir().unwrap();
    let dashboard_root = Arc::new(temp.path().join("dashboard"));
    PrivateStoreIo::create_dir_all_durable(&dashboard_root).unwrap();
    let writers = 8;
    let lines_per_writer = 50;
    let barrier = Arc::new(Barrier::new(writers));
    let mut handles = Vec::new();

    for writer in 0..writers {
        let dashboard_root = Arc::clone(&dashboard_root);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for line in 0..lines_per_writer {
                let run_id = format!("run-{writer}-{line}");
                append_run_record(
                    &dashboard_root,
                    &record(&run_id, AutomationRunStatus::Succeeded),
                )
                .await
                .unwrap();
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let raw = tokio::fs::read_to_string(run_ledger_path(&dashboard_root))
        .await
        .unwrap();
    let lines = raw.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), writers * lines_per_writer);
    for line in lines {
        serde_json::from_str::<AutomationRunLedgerRecord>(line).unwrap();
    }
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
async fn run_ledger_loads_records_without_optional_fields() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let minimal = serde_json::json!({
        "schema_version": 2,
        "run_id": "minimal-run",
        "trigger": "manual_cli",
        "task": "memory_curator",
        "backend": "codex_app_server",
        "model": "minimal-model",
        "status": "succeeded",
        "evidence_hash": "sha256:evidence",
        "proposed_ops": null,
        "accepted_count": 1,
        "rejected_count": 0,
        "error": null,
        "started_at": "1782277200",
        "completed_at": "1782277201"
    });
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    tokio::fs::write(run_ledger_path(&dashboard_root), format!("{minimal}\n"))
        .await
        .unwrap();

    let loaded = load_run_records(&dashboard_root, 10).await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].run_id, "minimal-run");
    assert_eq!(loaded[0].host_mode, None);
    assert_eq!(loaded[0].input_hash, None);
    assert_eq!(loaded[0].applied_ops, None);
    assert_eq!(loaded[0].completed_at_micros, None);
    assert_eq!(loaded[0].fallback_status, None);
    assert!(loaded[0].artifacts.is_empty());
}

#[tokio::test]
async fn run_ledger_locked_read_resets_schema_v1_rfc3339_rows() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let current = serde_json::json!({
        "schema_version": 2,
        "run_id": "schema-v2-row",
        "trigger": "manual_cli",
        "task": "memory_curator",
        "backend": "codex_app_server",
        "status": "succeeded",
        "accepted_count": 1,
        "rejected_count": 0,
        "started_at": "1782277200",
        "completed_at": "1782277201",
        "completed_at_micros": 1_782_277_201_000_000_i64
    });
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    tokio::fs::write(run_ledger_path(&dashboard_root), format!("{current}\n"))
        .await
        .unwrap();
    let loaded = load_run_records(&dashboard_root, 10).await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].run_id, "schema-v2-row");
    assert_eq!(loaded[0].completed_at_micros, Some(1_782_277_201_000_000));

    let schema_v1 = serde_json::json!({
        "schema_version": 1,
        "run_id": "schema-v1-row",
        "trigger": "manual_cli",
        "task": "memory_curator",
        "backend": "codex_app_server",
        "status": "succeeded",
        "accepted_count": 1,
        "rejected_count": 0,
        "started_at": "2026-06-24T05:00:00Z",
        "completed_at": "2026-06-24T05:00:01Z"
    });
    tokio::fs::write(
        run_ledger_path(&dashboard_root),
        format!("{current}\n{schema_v1}\n"),
    )
    .await
    .unwrap();

    let refusal = find_run_record(&dashboard_root, "schema-v1-row")
        .await
        .unwrap_err();
    assert_eq!(
        refusal.reset_required_context(),
        Some((
            "automation run ledger",
            "a row predates schema v2 (the released v1 wrote RFC3339 timestamps)"
        ))
    );

    assert_eq!(
        load_run_records(&dashboard_root, 10).await.unwrap(),
        Vec::<AutomationRunLedgerRecord>::new()
    );
    assert!(
        !run_ledger_path(&dashboard_root).exists(),
        "the locked read deletes the retired ledger"
    );
    let after_reset = record("after-reset", AutomationRunStatus::Succeeded);
    append_run_record(&dashboard_root, &after_reset)
        .await
        .unwrap();
    assert_eq!(
        load_run_records(&dashboard_root, 10).await.unwrap(),
        [after_reset]
    );
}

#[tokio::test]
async fn run_ledger_resets_only_the_row_with_an_unregistered_skip_reason() {
    for alias in [
        "task_disabled",
        "no_skill_writer_evidence",
        "session_cursor_manifest_participants_limit_exceeded",
        "session_evidence_budget_exhausted_candidates",
        "shipped_fact_proposal_history_retired",
    ] {
        let temp = tempdir().unwrap();
        let dashboard_root = temp.path().join("dashboard");
        let mut known = record("registered-skip", AutomationRunStatus::Skipped);
        known.trigger = AutomationTrigger::Scheduler;
        known.task = AgentTaskKind::SkillWriter;
        known.task_key = Some("skill_writer".to_owned());
        known.error = Some("skill_writer_disabled".to_owned());
        let mut retired = known.clone();
        retired.run_id = "retired-skip".to_owned();
        retired.error = Some(alias.to_owned());
        retired.started_at = "1782277300".to_owned();
        retired.completed_at = "1782277301".to_owned();
        retired.completed_at_micros = Some(1_782_277_301_000_000);
        let known_line = serde_json::to_string(&known).unwrap();
        tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
        tokio::fs::write(
            run_ledger_path(&dashboard_root),
            format!(
                "{known_line}\n{}\n",
                serde_json::to_string(&retired).unwrap()
            ),
        )
        .await
        .unwrap();

        let summary = load_run_ledger_task_summary(
            &dashboard_root,
            AgentTaskKind::SkillWriter,
            "skill_writer",
        )
        .await
        .unwrap();
        assert_eq!(
            summary
                .latest_logical_activity()
                .map(|record| (record.run_id.as_str(), record.error.as_deref())),
            Some(("registered-skip", Some("skill_writer_disabled"))),
            "{alias}"
        );
        assert_eq!(
            tokio::fs::read_to_string(run_ledger_path(&dashboard_root))
                .await
                .unwrap(),
            format!("{known_line}\n"),
            "only the {alias} row is deleted"
        );
    }
}

#[tokio::test]
async fn run_ledger_task_summary_reads_registered_skip_reasons() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let mut skipped = record("registered-skip", AutomationRunStatus::Skipped);
    skipped.trigger = AutomationTrigger::Scheduler;
    skipped.task = AgentTaskKind::SkillWriter;
    skipped.task_key = Some("skill_writer".to_owned());
    skipped.error = Some("skill_writer_disabled".to_owned());
    append_run_record(&dashboard_root, &skipped).await.unwrap();

    let summary =
        load_run_ledger_task_summary(&dashboard_root, AgentTaskKind::SkillWriter, "skill_writer")
            .await
            .unwrap();
    assert_eq!(
        summary
            .latest_logical_activity()
            .map(|record| record.run_id.as_str()),
        Some("registered-skip")
    );
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
    assert!(err.to_string().contains("canonical path"));
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

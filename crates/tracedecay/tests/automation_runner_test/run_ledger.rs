use std::sync::Arc;

use tempfile::tempdir;
use tokio::sync::Barrier;

use tracedecay_automation_runtime::automation::backend::AgentTaskKind;
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifactKind, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    read_run_artifact_payload, run_artifact_path, write_run_artifact,
};

use crate::support::RunStoreFixture;

fn record(run_id: &str, status: AutomationRunStatus) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_string(),
        trigger: AutomationTrigger::ManualCli,
        task: AgentTaskKind::MemoryCurator,
        task_key: "memory_curator".to_string(),
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
async fn run_ledger_publishes_runs_newest_first_with_every_field() {
    let temp = tempdir().unwrap();
    let fixture = RunStoreFixture::open(temp.path()).await;
    let store = &fixture.runs;

    store
        .append_run_record(&record("run-1", AutomationRunStatus::Succeeded))
        .await
        .unwrap();
    store
        .append_run_record(&record("run-2", AutomationRunStatus::Failed))
        .await
        .unwrap();

    let loaded = store.load_run_records(10).await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].run_id, "run-2");
    assert_eq!(loaded[1].run_id, "run-1");
    assert_eq!(loaded[0], record("run-2", AutomationRunStatus::Failed));
    assert_eq!(loaded[1], record("run-1", AutomationRunStatus::Succeeded));
    assert_eq!(loaded[0].host_mode.as_deref(), Some("standalone"));
    assert_eq!(loaded[0].input_hash.as_deref(), Some("sha256:input"));
    assert_eq!(loaded[0].output_hash.as_deref(), Some("sha256:output"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_ledger_keeps_concurrent_publishes_intact() {
    let temp = tempdir().unwrap();
    let fixture = RunStoreFixture::open(temp.path()).await;
    let writers = 8;
    let runs_per_writer = 25;
    let barrier = Arc::new(Barrier::new(writers));
    let mut handles = Vec::new();

    for writer in 0..writers {
        let store = fixture.runs.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for index in 0..runs_per_writer {
                let run_id = format!("run-{writer}-{index}");
                store
                    .append_run_record(&record(&run_id, AutomationRunStatus::Succeeded))
                    .await
                    .unwrap();
            }
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    let page = fixture
        .runs
        .load_run_records_page(writers * runs_per_writer + 1)
        .await
        .unwrap();
    assert!(!page.has_more);
    assert_eq!(page.records.len(), writers * runs_per_writer);
    let mut run_ids = page
        .records
        .iter()
        .map(|record| record.run_id.clone())
        .collect::<Vec<_>>();
    run_ids.sort();
    run_ids.dedup();
    assert_eq!(
        run_ids.len(),
        writers * runs_per_writer,
        "every concurrent publish must land exactly once"
    );
}

#[tokio::test]
async fn run_ledger_coalesces_lifecycle_records_by_latest_run_status() {
    let temp = tempdir().unwrap();
    let fixture = RunStoreFixture::open(temp.path()).await;
    let store = &fixture.runs;

    for (run_id, status) in [
        ("run-1", AutomationRunStatus::Queued),
        ("run-1", AutomationRunStatus::Running),
        ("run-1", AutomationRunStatus::Succeeded),
        ("run-2", AutomationRunStatus::Queued),
    ] {
        store
            .append_run_record(&record(run_id, status))
            .await
            .unwrap();
    }

    let loaded = store.load_run_records(10).await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].run_id, "run-2");
    assert_eq!(loaded[0].status, AutomationRunStatus::Queued);
    assert_eq!(loaded[1].run_id, "run-1");
    assert_eq!(loaded[1].status, AutomationRunStatus::Succeeded);
    assert!(!loaded[0].status.is_terminal());
    assert!(loaded[1].status.is_terminal());
    assert_eq!(
        store
            .find_run_record("run-1")
            .await
            .unwrap()
            .unwrap()
            .status,
        AutomationRunStatus::Succeeded
    );
}

#[tokio::test]
async fn run_ledger_replays_identical_records_and_refuses_conflicting_replays() {
    let temp = tempdir().unwrap();
    let fixture = RunStoreFixture::open(temp.path()).await;
    let store = &fixture.runs;
    let original = record("run-1", AutomationRunStatus::Succeeded);

    store.append_run_record(&original).await.unwrap();
    store.append_run_record(&original).await.unwrap();
    assert_eq!(
        store.load_run_records(10).await.unwrap(),
        vec![original.clone()],
        "an identical replay must not duplicate or alter the run"
    );

    let mut conflicting = original.clone();
    conflicting.output_hash = Some("sha256:other-output".to_string());
    let error = store.append_run_record(&conflicting).await.unwrap_err();
    assert!(
        error.to_string().contains("conflicting lifecycle state"),
        "{error}"
    );
    assert_eq!(
        store.find_run_record("run-1").await.unwrap(),
        Some(original),
        "a refused conflicting replay must leave the stored run untouched"
    );
}

#[tokio::test]
async fn run_ledger_limit_bounds_the_newest_first_page() {
    let temp = tempdir().unwrap();
    let fixture = RunStoreFixture::open(temp.path()).await;
    let store = &fixture.runs;

    for run_id in ["run-1", "run-2"] {
        store
            .append_run_record(&record(run_id, AutomationRunStatus::Succeeded))
            .await
            .unwrap();
    }

    let loaded = store.load_run_records(1).await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].run_id, "run-2");
    let page = store.load_run_records_page(1).await.unwrap();
    assert!(page.has_more, "a bounded page must report the older run");
    let full = store.load_run_records_page(2).await.unwrap();
    assert!(!full.has_more);
    assert_eq!(full.records.len(), 2);
}

#[tokio::test]
async fn run_ledger_loads_records_without_optional_fields() {
    let temp = tempdir().unwrap();
    let fixture = RunStoreFixture::open(temp.path()).await;
    let store = &fixture.runs;
    let minimal: AutomationRunLedgerRecord = serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "run_id": "minimal-run",
        "trigger": "manual_cli",
        "task": "memory_curator",
        "task_key": "memory_curator",
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
    }))
    .unwrap();
    store.append_run_record(&minimal).await.unwrap();

    let loaded = store.load_run_records(10).await.unwrap();
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
async fn run_ledger_task_summary_reads_registered_skip_reasons() {
    let temp = tempdir().unwrap();
    let fixture = RunStoreFixture::open(temp.path()).await;
    let store = &fixture.runs;
    let mut skipped = record("registered-skip", AutomationRunStatus::Skipped);
    skipped.trigger = AutomationTrigger::Scheduler;
    skipped.task = AgentTaskKind::SkillWriter;
    skipped.task_key = "skill_writer".to_owned();
    skipped.error = Some("skill_writer_disabled".to_owned());
    store.append_run_record(&skipped).await.unwrap();

    let summary = store
        .load_run_ledger_task_summary(AgentTaskKind::SkillWriter, "skill_writer")
        .await
        .unwrap();
    assert_eq!(
        summary
            .latest_logical_activity()
            .map(|record| record.run_id.as_str()),
        Some("registered-skip")
    );
    assert_eq!(
        summary
            .latest_logical_activity()
            .and_then(|record| record.error.as_deref()),
        Some("skill_writer_disabled")
    );
    assert!(
        summary.latest_successful().is_none(),
        "a skipped run is never the latest success"
    );
}

#[tokio::test]
async fn run_artifacts_write_sidecar_metadata_without_embedding_payloads() {
    let temp = tempdir().unwrap();
    let fixture = RunStoreFixture::open(temp.path()).await;
    let store = &fixture.runs;
    let dashboard_root = fixture.dashboard_root();
    let payload = serde_json::json!({
        "status": "blocked_pending_feedback_or_evals",
        "large_payload": "not embedded in ledger",
    });
    let artifact = write_run_artifact(
        dashboard_root,
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
    store.append_run_record(&record).await.unwrap();

    let artifact_path = run_artifact_path(
        dashboard_root,
        "run_artifact_1",
        AutomationRunArtifactKind::ValidationGate,
    )
    .unwrap();
    let artifact_contents = tokio::fs::read_to_string(artifact_path).await.unwrap();
    assert!(artifact_contents.contains("blocked_pending_feedback_or_evals"));

    let loaded = store.load_run_records(10).await.unwrap();
    assert_eq!(loaded[0].artifacts, vec![artifact]);
    assert_eq!(loaded[0].artifacts[0].kind, "validation_gate");
    assert!(loaded[0].artifacts[0].sha256.starts_with("sha256:"));
    assert!(
        !serde_json::to_string(&loaded[0])
            .unwrap()
            .contains("large_payload"),
        "the registered run must reference its artifact, not embed the payload"
    );
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
    let fixture = RunStoreFixture::open(temp.path()).await;
    let store = &fixture.runs;

    store
        .append_run_record(&record("old-run", AutomationRunStatus::Succeeded))
        .await
        .unwrap();
    store
        .append_run_record(&record("new-run", AutomationRunStatus::Failed))
        .await
        .unwrap();

    let listed = store.load_run_records(1).await.unwrap();
    assert_eq!(listed[0].run_id, "new-run");

    let found = store.find_run_record("old-run").await.unwrap();
    assert_eq!(found.unwrap().run_id, "old-run");
    assert!(store.find_run_record("absent-run").await.unwrap().is_none());
}

use crate::support::{
    AgentTaskFailureClass, AgentTaskKind, AtomicBool, AutomationBackend, AutomationConfig,
    AutomationHostMode, AutomationRunStatus, AutomationTaskConfig, AutomationTaskSet,
    AutomationTrigger, FailingBackend, MalformedTextBackend, MemoryCuratorAutomationOptions,
    assert_noop_fallback_record, fact_exists, init_project, json, load_run_records,
    run_memory_curator_with_backend, seed_duplicate_facts, tempdir, test_automation_run_control,
};
use std::sync::Arc;

#[tokio::test]
async fn memory_curator_runner_ledgers_malformed_backend_output() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let facts = seed_duplicate_facts(&cg).await;
    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let backend = MalformedTextBackend::new(AgentTaskKind::MemoryCurator, "not json");
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let err = run_memory_curator_with_backend(
        &cg,
        &config,
        &run_control,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap_err();

    assert_eq!(backend.calls(), 1);
    assert!(
        err.to_string().contains("expected ident") || err.to_string().contains("expected value"),
        "unexpected error: {err}"
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, AgentTaskKind::MemoryCurator);
    assert_eq!(records[0].task_key.as_deref(), Some("memory_curator"));
    assert_eq!(records[0].status, AutomationRunStatus::Failed);
    assert_eq!(records[0].model.as_deref(), Some("fixture-model"));
    assert!(records[0].evidence_hash.is_some());
    assert!(records[0].input_hash.is_some());
    assert!(records[0].proposed_ops.is_none());
    assert!(records[0].error.as_deref().is_some_and(|error| {
        error.contains("expected ident") || error.contains("expected value")
    }));
    assert_eq!(
        records[0].error_classification,
        Some(AgentTaskFailureClass::MalformedOutput)
    );
    assert_eq!(records[0].error_retryable, Some(false));
    assert!(fact_exists(&cg, &facts.winner_id, run_control.read_control()).await);
    assert!(fact_exists(&cg, &facts.loser_id, run_control.read_control()).await);
}

#[tokio::test]
async fn memory_curator_runner_records_noop_fallback_when_backend_run_task_fails() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let facts = seed_duplicate_facts(&cg).await;
    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let backend = FailingBackend::new(AgentTaskKind::MemoryCurator);
    let config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        timeout_secs: 1,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("manual".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &run_control,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            fact_review_limit: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    // The backend failure is transient, but this test pins the noop-fallback
    // record, not retry semantics (covered by backend.rs retry tests) —
    // timeout_secs: 1 short-circuits the backoff so the test stays fast.
    assert_eq!(backend.calls(), 1);
    assert!(run.backend_response.is_none());
    assert!(run.committed_receipt.is_none());
    assert_noop_fallback_record(
        &run.ledger_record,
        AgentTaskKind::MemoryCurator,
        "memory_curator",
        json!({ "ops": [] }),
    );
    assert!(
        run.ledger_record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("executable"))
    );
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_noop_fallback_record(
        &records[0],
        AgentTaskKind::MemoryCurator,
        "memory_curator",
        json!({ "ops": [] }),
    );
    assert!(fact_exists(&cg, &facts.winner_id, run_control.read_control()).await);
    assert!(fact_exists(&cg, &facts.loser_id, run_control.read_control()).await);
}

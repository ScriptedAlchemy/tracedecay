use super::*;

#[tokio::test]
async fn manual_memory_curator_runs_when_scheduling_and_task_are_disabled() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = AutomationSettingsV1 {
        enabled: false,
        backend: AutomationBackendV1::CodexAppServer,
        host_mode: AutomationHostModeV1::Standalone,
        tasks: AutomationTaskSetV1 {
            memory_curator: AutomationTaskSettingsV1 {
                enabled: false,
                ..AutomationTaskSettingsV1::default()
            },
            ..AutomationTaskSetV1::default()
        },
        ..AutomationSettingsV1::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = settle_memory_curator(
        &automation_project_context(&cg),
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
        &run_control,
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.trigger, AutomationTrigger::ManualCli);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert_eq!(run.ledger_record.error, None);
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, run.run_id);
    assert_eq!(records[0].status, AutomationRunStatus::Succeeded);
    assert_eq!(records[0].error, None);
}

#[tokio::test]
async fn manual_memory_curator_skips_when_backend_is_disabled() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = AutomationSettingsV1 {
        enabled: false,
        backend: AutomationBackendV1::Disabled,
        host_mode: AutomationHostModeV1::Standalone,
        model_id: None,
        ..AutomationSettingsV1::default()
    };

    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let run = settle_memory_curator(
        &automation_project_context(&cg),
        &config,
        &test_configuration_revision(),
        &backend,
        MemoryCuratorAutomationOptions::default(),
        &run_control,
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.trigger, AutomationTrigger::ManualCli);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(run.ledger_record.error.as_deref(), Some("backend_disabled"));
    assert_eq!(run.report["reason"], json!("backend_disabled"));
}

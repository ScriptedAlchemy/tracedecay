use crate::support::*;

#[tokio::test]
async fn scheduler_memory_curator_respects_failure_cooldown() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let config = scheduler_config(Some(3600), Some(3600));
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record(
            "previous_failed_run",
            AutomationRunStatus::Failed,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_cooldown_active")
    );
}

#[tokio::test]
async fn scheduler_memory_curator_respects_interval_gate() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record(
            "previous_successful_run",
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_interval_not_elapsed")
    );
}

// The scheduler gate tests below deliberately skip session-evidence seeding:
// the runners evaluate the scheduler gate before opening any session store,
// and each test pins the exact gate skip reason, so a regression that
// reordered gating behind evidence gathering would fail with a different
// error. Skipping the seed avoids paying a full session-DB schema creation
// per test, which dominates these fixtures on Windows.
#[tokio::test]
async fn scheduler_session_reflector_respects_interval_gate() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record_for(
            "previous_session_reflector_run",
            AgentTaskKind::SessionReflector,
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = SessionJsonBackend::new(json!({"facts": []}));

    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[tokio::test]
async fn scheduler_skill_writer_respects_interval_gate() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record_for(
            "previous_skill_writer_run",
            AgentTaskKind::SkillWriter,
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = SkillJsonBackend::new(json!({"skills": []}));

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[tokio::test]
async fn scheduler_skill_writer_respects_idle_window_after_recent_session_activity() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let mut config = scheduler_config(Some(1), None);
    config.tasks.skill_writer.min_idle_secs = Some(3600);
    // A session message landed 60s ago: the project is not idle yet.
    seed_session_activity(&cg, current_timestamp() - 60).await;
    let backend = SkillJsonBackend::new(json!({"skills": []}));

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_idle_window_active")
    );
}

#[tokio::test]
async fn scheduler_skill_writer_skips_without_new_session_activity_since_last_success() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let config = scheduler_config(Some(1), None);
    let now = current_timestamp();
    // Activity landed, then a successful run consumed it.
    seed_session_activity(&cg, now - 120).await;
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record_for(
            "previous_skill_writer_run",
            AgentTaskKind::SkillWriter,
            AutomationRunStatus::Succeeded,
            now - 60,
        ),
    )
    .await
    .unwrap();
    let backend = SkillJsonBackend::new(json!({"skills": []}));

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("no_new_session_activity")
    );

    // New activity after the run: the very next tick is due again.
    seed_session_activity(&cg, now - 30).await;
    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_ne!(run.ledger_record.status, AutomationRunStatus::Skipped);
}

#[tokio::test]
async fn memory_curator_runner_cleans_up_lock_file() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = scheduler_config(None, None);

    run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert!(!cg
        .store_layout()
        .dashboard_root
        .join("automation_locks")
        .join("memory_curator.lock")
        .exists());
}

#[tokio::test]
async fn memory_curator_runner_recovers_stale_scheduler_lock_file() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let lock_dir = cg.store_layout().dashboard_root.join("automation_locks");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    fs::write(&lock_path, "pid=999999\ncreated_at=100\n").unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));
    let mut config = scheduler_config(None, None);
    config.tasks.memory_curator.stale_lock_secs = Some(1);

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_schedule_manual")
    );
    assert!(!lock_path.exists());
}

#[tokio::test]
async fn scheduler_memory_curator_ledgers_active_lock_skip() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let lock_dir = cg.store_layout().dashboard_root.join("automation_locks");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    fs::write(
        &lock_path,
        format!(
            "pid={}\ncreated_at={}\n",
            std::process::id(),
            current_timestamp()
        ),
    )
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = scheduler_config(None, None);

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_lock_active")
    );
    assert!(lock_path.exists());
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].error.as_deref(), Some("scheduler_lock_active"));
}

#[tokio::test]
async fn manual_memory_curator_run_ignores_scheduler_lock() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let lock_dir = cg.store_layout().dashboard_root.join("automation_locks");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    fs::write(
        &lock_path,
        format!(
            "pid={}\ncreated_at={}\n",
            std::process::id(),
            current_timestamp()
        ),
    )
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = scheduler_config(None, None);

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert!(lock_path.exists());
}

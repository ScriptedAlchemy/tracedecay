use tempfile::tempdir;
use tracedecay::automation::backend::{AgentTaskFailureClass, AgentTaskKind};
use tracedecay::automation::config::{
    effective_config, AutomationBackend, AutomationConfig, AutomationConfigPatch,
    AutomationTaskConfig, AutomationTaskPatch, AutomationTaskSet,
};
use tracedecay::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
};
use tracedecay::automation::scheduler::{
    load_scheduler_control, parse_schedule, save_scheduler_control, schedule_decision,
    scheduler_control_path, AutomationSchedule, AutomationSchedulerControl, AutomationTaskLock,
};

fn automation_config(schedule: Option<&str>, interval_secs: Option<u64>) -> AutomationConfig {
    AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: schedule.map(str::to_string),
                interval_secs,
                cooldown_secs: Some(300),
                ..AutomationTaskConfig::default()
            },
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: schedule.map(str::to_string),
                interval_secs,
                cooldown_secs: Some(300),
                ..AutomationTaskConfig::default()
            },
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: schedule.map(str::to_string),
                interval_secs,
                cooldown_secs: Some(300),
                ..AutomationTaskConfig::default()
            },
        },
        ..AutomationConfig::default()
    }
}

fn record(
    run_id: &str,
    task: AgentTaskKind,
    status: AutomationRunStatus,
    completed_at: i64,
) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_string(),
        trigger: AutomationTrigger::Scheduler,
        task,
        task_key: Some(test_task_key(task).to_string()),
        backend: "codex_app_server".to_string(),
        host_mode: Some("standalone".to_string()),
        prompt_version: Some(test_prompt_version(task).to_string()),
        response_schema: None,
        strict_json: None,
        model: Some("test-model".to_string()),
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
        skipped_count: usize::from(status == AutomationRunStatus::Skipped),
        error: None,
        error_classification: None,
        error_retryable: None,
        fallback_status: None,
        report_ref: None,
        artifacts: Vec::new(),
        started_at: (completed_at - 1).to_string(),
        completed_at: completed_at.to_string(),
    }
}

fn test_task_key(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator",
        AgentTaskKind::SessionReflector => "session_reflector",
        AgentTaskKind::SkillWriter => "skill_writer",
    }
}

fn test_prompt_version(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator:v1",
        AgentTaskKind::SessionReflector => "session_reflector:v1",
        AgentTaskKind::SkillWriter => "skill_writer:v1",
    }
}

#[test]
fn scheduler_parses_manual_aliases_and_intervals() {
    assert_eq!(parse_schedule(None).unwrap(), AutomationSchedule::Manual);
    assert_eq!(
        parse_schedule(Some("manual")).unwrap(),
        AutomationSchedule::Manual
    );
    assert_eq!(
        parse_schedule(Some("interval")).unwrap(),
        AutomationSchedule::ConfiguredInterval
    );
    assert_eq!(
        parse_schedule(Some("weekly")).unwrap(),
        AutomationSchedule::Interval {
            every_secs: 7 * 24 * 60 * 60
        }
    );
    assert_eq!(
        parse_schedule(Some("every 15m")).unwrap(),
        AutomationSchedule::Interval { every_secs: 900 }
    );
    assert_eq!(
        parse_schedule(Some("interval:2h")).unwrap(),
        AutomationSchedule::Interval { every_secs: 7200 }
    );
    assert!(parse_schedule(Some("after lunch")).is_err());
}

#[tokio::test]
async fn scheduler_control_sidecar_round_trips_pause_state() {
    let tmp = tempdir().unwrap();
    let dashboard_root = tmp.path().join("dashboard");

    let default_control = load_scheduler_control(&dashboard_root).await.unwrap();
    assert_eq!(default_control, AutomationSchedulerControl::default());

    save_scheduler_control(
        &dashboard_root,
        &AutomationSchedulerControl { paused: true },
    )
    .await
    .unwrap();
    assert!(scheduler_control_path(&dashboard_root).is_file());
    let paused = load_scheduler_control(&dashboard_root).await.unwrap();
    assert!(paused.paused);

    save_scheduler_control(
        &dashboard_root,
        &AutomationSchedulerControl { paused: false },
    )
    .await
    .unwrap();
    let resumed = load_scheduler_control(&dashboard_root).await.unwrap();
    assert!(!resumed.paused);
}

#[test]
fn scheduler_skips_disabled_and_manual_only_tasks() {
    let mut config = automation_config(Some("every 10m"), None);
    config.enabled = false;
    assert_eq!(
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &[], 1_000).skip_reason(),
        Some("automation_disabled")
    );

    let config = automation_config(Some("manual"), None);
    assert_eq!(
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &[], 1_000).skip_reason(),
        Some("scheduler_schedule_manual")
    );
}

#[test]
fn scheduler_uses_interval_and_latest_successful_ledger_record() {
    let config = automation_config(Some("every 10m"), None);
    let records = vec![record(
        "run-1",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Succeeded,
        1_000,
    )];

    assert_eq!(
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, 1_500).skip_reason(),
        Some("scheduler_interval_not_elapsed")
    );
    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, 1_700).is_due());
}

#[test]
fn scheduler_ignores_non_terminal_lifecycle_records_for_interval_decisions() {
    let config = automation_config(Some("every 10m"), None);
    let records = vec![
        record(
            "queued-run",
            AgentTaskKind::MemoryCurator,
            AutomationRunStatus::Queued,
            1_500,
        ),
        record(
            "running-run",
            AgentTaskKind::MemoryCurator,
            AutomationRunStatus::Running,
            1_600,
        ),
    ];

    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, 1_700).is_due());
}

#[test]
fn scheduler_respects_configured_interval_field() {
    let config = automation_config(Some("interval"), Some(600));
    let records = vec![record(
        "run-1",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Succeeded,
        1_000,
    )];

    assert_eq!(
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, 1_100).skip_reason(),
        Some("scheduler_interval_not_elapsed")
    );
    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, 1_700).is_due());
}

#[test]
fn scheduler_retries_failures_after_cooldown_instead_of_full_interval() {
    let config = automation_config(Some("daily"), None);
    let records = vec![record(
        "run-1",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Failed,
        1_000,
    )];

    assert_eq!(
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, 1_100).skip_reason(),
        Some("scheduler_cooldown_active")
    );
    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, 1_400).is_due());
}

#[test]
fn scheduler_does_not_retry_explicit_non_retryable_failures() {
    let config = automation_config(Some("daily"), None);
    let mut failed = record(
        "run-1",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Failed,
        1_000,
    );
    failed.error = Some("backend output must include a JSON object".to_string());
    failed.error_classification = Some(AgentTaskFailureClass::MalformedOutput);
    failed.error_retryable = Some(false);
    let records = vec![failed];

    assert_eq!(
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, 1_400).skip_reason(),
        Some("scheduler_non_retryable_failure")
    );
}

#[test]
fn scheduler_retries_explicit_retryable_failures_after_cooldown() {
    let config = automation_config(Some("daily"), None);
    let mut failed = record(
        "run-1",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Failed,
        1_000,
    );
    failed.error = Some("timed out waiting for codex app-server".to_string());
    failed.error_classification = Some(AgentTaskFailureClass::Timeout);
    failed.error_retryable = Some(true);
    let records = vec![failed];

    assert_eq!(
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, 1_100).skip_reason(),
        Some("scheduler_cooldown_active")
    );
    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, 1_400).is_due());
}

#[test]
fn scheduler_supports_all_self_improvement_tasks() {
    let config = automation_config(Some("hourly"), None);

    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &[], 1_000).is_due());
    assert!(schedule_decision(&config, AgentTaskKind::SessionReflector, &[], 1_000).is_due());
    assert!(schedule_decision(&config, AgentTaskKind::SkillWriter, &[], 1_000).is_due());
}

#[test]
fn scheduler_uses_latest_record_status_before_failure_cooldown() {
    let config = automation_config(Some("daily"), None);
    let records = vec![
        record(
            "failed-old",
            AgentTaskKind::SkillWriter,
            AutomationRunStatus::Failed,
            1_000,
        ),
        record(
            "success-new",
            AgentTaskKind::SkillWriter,
            AutomationRunStatus::Succeeded,
            1_200,
        ),
    ];

    assert_eq!(
        schedule_decision(&config, AgentTaskKind::SkillWriter, &records, 1_500).skip_reason(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[test]
fn scheduler_respects_task_idle_window_across_manual_runs() {
    let mut config = automation_config(Some("every 10m"), None);
    config.tasks.skill_writer.min_idle_secs = Some(600);
    let mut manual_record = record(
        "manual-skill-writer",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    manual_record.trigger = AutomationTrigger::ManualCli;
    let records = vec![manual_record];

    assert_eq!(
        schedule_decision(&config, AgentTaskKind::SkillWriter, &records, 1_500).skip_reason(),
        Some("scheduler_idle_window_active")
    );
    assert!(schedule_decision(&config, AgentTaskKind::SkillWriter, &records, 1_600).is_due());
}

#[test]
fn config_requires_interval_secs_for_configured_interval_schedule() {
    let patch = AutomationConfigPatch {
        enabled: Some(true),
        backend: Some(AutomationBackend::CodexAppServer),
        memory_curator: AutomationTaskPatch {
            enabled: Some(true),
            schedule: Some(Some("interval".to_string())),
            interval_secs: Some(None),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };

    let err = effective_config(&AutomationConfig::default(), Some(&patch)).unwrap_err();
    assert!(
        err.to_string()
            .contains("memory_curator interval_secs is required"),
        "unexpected error: {err}"
    );
}

#[test]
fn config_validates_scheduler_idle_and_lock_bounds() {
    let patch = AutomationConfigPatch {
        memory_curator: AutomationTaskPatch {
            min_idle_secs: Some(Some(0)),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };
    assert!(effective_config(&AutomationConfig::default(), Some(&patch))
        .unwrap_err()
        .to_string()
        .contains("min_idle_secs"));

    let patch = AutomationConfigPatch {
        memory_curator: AutomationTaskPatch {
            stale_lock_secs: Some(Some(0)),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };
    assert!(effective_config(&AutomationConfig::default(), Some(&patch))
        .unwrap_err()
        .to_string()
        .contains("stale_lock_secs"));
}

#[tokio::test]
async fn task_lock_reclaims_stale_dead_pid_lock_file() {
    let temp = tempdir().unwrap();
    let lock_dir = temp.path().join("automation_locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    std::fs::write(&lock_path, "pid=999999\ncreated_at=100\n").unwrap();

    let lock =
        AutomationTaskLock::try_acquire(temp.path(), AgentTaskKind::MemoryCurator, Some(10), 200)
            .await
            .unwrap();

    assert!(lock.is_some());
    drop(lock);
    assert!(!lock_path.exists());
}

#[tokio::test]
async fn task_lock_keeps_live_pid_lock_file() {
    let temp = tempdir().unwrap();
    let lock_dir = temp.path().join("automation_locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("skill_writer.lock");
    std::fs::write(
        &lock_path,
        format!("pid={}\ncreated_at=100\n", std::process::id()),
    )
    .unwrap();

    let lock =
        AutomationTaskLock::try_acquire(temp.path(), AgentTaskKind::SkillWriter, Some(10), 200)
            .await
            .unwrap();

    assert!(lock.is_none());
    assert!(lock_path.exists());
}

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
    load_scheduler_control, load_session_activity, parse_schedule, save_scheduler_control,
    schedule_decision, scheduler_control_path, AutomationSchedule, AutomationSchedulerControl,
    AutomationTaskLock, SessionActivity,
};
use tracedecay::global_db::GlobalDb;

use crate::support::{scheduler_record_for, seed_session_message_in_db, SeedSessionMessage};

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
        model: Some("test-model".to_string()),
        ..scheduler_record_for(run_id, task, status, completed_at)
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
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &[], SessionActivity::none(), 1_000).skip_reason(),
        Some("automation_disabled")
    );

    let config = automation_config(Some("manual"), None);
    assert_eq!(
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &[], SessionActivity::none(), 1_000).skip_reason(),
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
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_500).skip_reason(),
        Some("scheduler_interval_not_elapsed")
    );
    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_700).is_due());
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

    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_700).is_due());
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
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_100).skip_reason(),
        Some("scheduler_interval_not_elapsed")
    );
    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_700).is_due());
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
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_100).skip_reason(),
        Some("scheduler_cooldown_active")
    );
    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_400).is_due());
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
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_400).skip_reason(),
        Some("scheduler_non_retryable_failure")
    );
}

#[test]
fn scheduler_rechecks_stale_non_retryable_backend_transport_failures() {
    let config = automation_config(Some("daily"), None);
    let mut failed = record(
        "run-1",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Failed,
        1_000,
    );
    failed.error =
        Some("config error: codex app-server closed stdout before completing".to_string());
    failed.error_classification = Some(AgentTaskFailureClass::Permanent);
    failed.error_retryable = Some(false);
    let records = vec![failed];

    assert_eq!(
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_100).skip_reason(),
        Some("scheduler_cooldown_active")
    );
    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_400).is_due());
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
        schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_100).skip_reason(),
        Some("scheduler_cooldown_active")
    );
    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &records, SessionActivity::none(), 1_400).is_due());
}

#[test]
fn scheduler_supports_all_self_improvement_tasks() {
    let config = automation_config(Some("hourly"), None);

    assert!(schedule_decision(&config, AgentTaskKind::MemoryCurator, &[], SessionActivity::none(), 1_000).is_due());
    assert!(schedule_decision(&config, AgentTaskKind::SessionReflector, &[], SessionActivity::none(), 1_000).is_due());
    assert!(schedule_decision(&config, AgentTaskKind::SkillWriter, &[], SessionActivity::none(), 1_000).is_due());
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
        schedule_decision(&config, AgentTaskKind::SkillWriter, &records, SessionActivity::none(), 1_500).skip_reason(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[test]
fn scheduler_idle_window_measures_time_since_session_activity() {
    let mut config = automation_config(Some("every 10m"), None);
    config.tasks.skill_writer.min_idle_secs = Some(600);
    let activity = SessionActivity::at(1_000);

    // Activity landed 500s ago: still inside the 600s idle window.
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &[],
            activity,
            1_500
        )
        .skip_reason(),
        Some("scheduler_idle_window_active")
    );
    // 600s of quiet have elapsed: the project is idle, the task is due.
    assert!(
        schedule_decision(&config, AgentTaskKind::SkillWriter, &[], activity, 1_600).is_due()
    );
    // Unknown activity (no session store yet) counts as idle.
    assert!(schedule_decision(
        &config,
        AgentTaskKind::SkillWriter,
        &[],
        SessionActivity::none(),
        1_100
    )
    .is_due());
}

#[test]
fn scheduler_idle_window_ignores_task_run_history() {
    // The idle window used to measure time since the task's own last run;
    // it must now only observe session activity.
    let mut config = automation_config(Some("every 10m"), None);
    config.tasks.memory_curator.min_idle_secs = Some(600);
    let mut manual_record = record(
        "manual-memory-curator",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Succeeded,
        1_400,
    );
    manual_record.trigger = AutomationTrigger::ManualCli;
    let records = vec![manual_record];

    // A manual run 100s ago no longer arms the idle window when the last
    // session activity is old.
    assert!(schedule_decision(
        &config,
        AgentTaskKind::MemoryCurator,
        &records,
        SessionActivity::at(100),
        1_500
    )
    .is_due());
}

#[test]
fn scheduler_skips_session_evidence_tasks_without_new_activity() {
    let config = automation_config(Some("every 10m"), None);

    for task in [AgentTaskKind::SessionReflector, AgentTaskKind::SkillWriter] {
        // Last successful run: started_at 999, completed_at 1_000.
        let records = vec![record("run-1", task, AutomationRunStatus::Succeeded, 1_000)];

        // Interval elapsed but no session activity has ever been observed.
        assert_eq!(
            schedule_decision(&config, task, &records, SessionActivity::none(), 1_700)
                .skip_reason(),
            Some("no_new_session_activity")
        );
        // Interval elapsed but the newest activity predates the run.
        assert_eq!(
            schedule_decision(&config, task, &records, SessionActivity::at(900), 1_700)
                .skip_reason(),
            Some("no_new_session_activity")
        );
        // Activity landed after the run started: due on the next tick.
        assert!(
            schedule_decision(&config, task, &records, SessionActivity::at(1_650), 1_700)
                .is_due()
        );
        // The interval gate still wins while it has not elapsed.
        assert_eq!(
            schedule_decision(&config, task, &records, SessionActivity::at(1_050), 1_100)
                .skip_reason(),
            Some("scheduler_interval_not_elapsed")
        );
    }
}

#[test]
fn scheduler_first_session_evidence_run_is_not_gated_on_activity() {
    // With no prior successful run there is nothing to deduplicate against;
    // the runner's own evidence checks handle an empty session store.
    let config = automation_config(Some("every 10m"), None);

    assert!(schedule_decision(
        &config,
        AgentTaskKind::SessionReflector,
        &[],
        SessionActivity::none(),
        1_000
    )
    .is_due());
}

#[test]
fn scheduler_memory_curator_is_not_gated_on_session_activity() {
    // The memory curator reviews the fact store, not session transcripts.
    let config = automation_config(Some("every 10m"), None);
    let records = vec![record(
        "run-1",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Succeeded,
        1_000,
    )];

    assert!(schedule_decision(
        &config,
        AgentTaskKind::MemoryCurator,
        &records,
        SessionActivity::none(),
        1_700
    )
    .is_due());
}

#[test]
fn scheduler_retries_failed_session_evidence_runs_without_new_activity() {
    // The evidence gate keys off the last successful run; a failed run is
    // retried after its cooldown with the same evidence.
    let config = automation_config(Some("every 10m"), None);
    let records = vec![record(
        "run-1",
        AgentTaskKind::SessionReflector,
        AutomationRunStatus::Failed,
        1_000,
    )];

    assert!(schedule_decision(
        &config,
        AgentTaskKind::SessionReflector,
        &records,
        SessionActivity::none(),
        1_400
    )
    .is_due());
}

#[tokio::test]
async fn load_session_activity_reads_newest_message_timestamp() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("sessions.db");

    // Missing store: no activity signal.
    assert_eq!(
        load_session_activity(&db_path).await,
        SessionActivity::none()
    );

    let db = GlobalDb::open_at(&db_path).await.expect("session db open");
    seed_session_message_in_db(
        &db,
        temp.path(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "activity-1",
            message_id: "activity-1-message-001",
            role: "user",
            timestamp: 1_715_000_100,
            text: "older message",
            source: None,
        },
    )
    .await;
    seed_session_message_in_db(
        &db,
        temp.path(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "activity-2",
            message_id: "activity-2-message-001",
            role: "user",
            timestamp: 1_715_000_200,
            text: "newest message",
            source: None,
        },
    )
    .await;
    drop(db);

    assert_eq!(
        load_session_activity(&db_path).await,
        SessionActivity::at(1_715_000_200)
    );
}

#[tokio::test]
async fn load_session_activity_normalizes_millisecond_timestamps() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("sessions.db");
    let db = GlobalDb::open_at(&db_path).await.expect("session db open");
    seed_session_message_in_db(
        &db,
        temp.path(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "activity-ms",
            message_id: "activity-ms-message-001",
            role: "user",
            timestamp: 1_715_000_300_000,
            text: "millisecond provider timestamp",
            source: None,
        },
    )
    .await;
    drop(db);

    assert_eq!(
        load_session_activity(&db_path).await,
        SessionActivity::at(1_715_000_300)
    );
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

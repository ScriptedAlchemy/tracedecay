use tempfile::tempdir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_agent_hosts::automation::backend::{AgentTaskFailureClass, AgentTaskKind};
use tracedecay_agent_hosts::automation::config::{
    AutomationBackend, AutomationConfig, AutomationConfigPatch, AutomationTaskConfig,
    AutomationTaskPatch, AutomationTaskSet, effective_config,
};
use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
};
use tracedecay_agent_hosts::automation::scheduler::{
    AutomationSchedule, AutomationSchedulerControl, AutomationTaskLock, SessionActivity,
    host_receipt_decision, load_scheduler_control, parse_schedule, save_scheduler_control,
    schedule_decision, scheduler_control_path,
};
use tracedecay_domain::ProjectId;
use tracedecay_usecases::host_admission::HostAdmissionScope;

use crate::support::{SeedSessionMessage, scheduler_record_for, seed_session_message_in_db};

async fn scheduler_session_runtime(
    root: &std::path::Path,
    project_id: &str,
) -> HostAdmissionTestRuntimeV1 {
    let project_root = root.join("project");
    std::fs::create_dir_all(&project_root).unwrap();
    HostAdmissionTestRuntimeV1::project(
        root.join(".tracedecay"),
        project_root,
        ProjectId::new(project_id).unwrap(),
    )
    .await
    .expect("registered scheduler session runtime")
}

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

#[test]
fn host_receipt_bypasses_schedule_but_preserves_enablement_and_idle_gates() {
    let mut config = automation_config(None, None);
    assert!(
        host_receipt_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &[],
            SessionActivity::at(100),
            200,
        )
        .is_due()
    );
    config.tasks.session_reflector.min_idle_secs = Some(120);
    assert_eq!(
        host_receipt_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &[],
            SessionActivity::at(100),
            200,
        )
        .skip_reason(),
        Some("scheduler_idle_window_active")
    );
    config.enabled = false;
    assert_eq!(
        host_receipt_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &[],
            SessionActivity::none(),
            200,
        )
        .skip_reason(),
        Some("automation_disabled")
    );
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
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &[],
            SessionActivity::none(),
            1_000
        )
        .skip_reason(),
        Some("automation_disabled")
    );

    let config = automation_config(Some("manual"), None);
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &[],
            SessionActivity::none(),
            1_000
        )
        .skip_reason(),
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
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_500
        )
        .skip_reason(),
        Some("scheduler_interval_not_elapsed")
    );
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_700
        )
        .is_due()
    );
}

#[test]
fn fresh_session_activity_bypasses_interval_for_all_host_evidence_tasks() {
    let config = automation_config(Some("every 10m"), None);
    for task in [AgentTaskKind::SessionReflector, AgentTaskKind::SkillWriter] {
        let records = vec![record(
            "previous-success",
            task,
            AutomationRunStatus::Succeeded,
            1_000,
        )];
        assert!(
            schedule_decision(
                &config,
                task,
                &records,
                SessionActivity {
                    last_activity_secs: Some(1_100),
                },
                1_101,
            )
            .is_due(),
            "fresh completed-turn evidence should wake {task:?} without waiting for its repair interval"
        );
    }
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

    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_700
        )
        .is_due()
    );
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
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_100
        )
        .skip_reason(),
        Some("scheduler_interval_not_elapsed")
    );
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_700
        )
        .is_due()
    );
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
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_100
        )
        .skip_reason(),
        Some("scheduler_cooldown_active")
    );
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_400
        )
        .is_due()
    );
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
    failed.error = Some("model refused the request because policy rejected the prompt".to_string());
    failed.error_classification = Some(AgentTaskFailureClass::Permanent);
    failed.error_retryable = Some(false);
    let records = vec![failed];

    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_400
        )
        .skip_reason(),
        Some("scheduler_non_retryable_failure")
    );
}

#[test]
fn scheduler_retries_malformed_backend_output_after_cooldown() {
    let config = automation_config(Some("daily"), None);
    let mut failed = record(
        "run-1",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Failed,
        1_000,
    );
    failed.error =
        Some("config error: automation backend output must include a ops array".to_string());
    failed.error_classification = Some(AgentTaskFailureClass::MalformedOutput);
    failed.error_retryable = Some(false);
    let records = vec![failed];

    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_100
        )
        .skip_reason(),
        Some("scheduler_cooldown_active")
    );
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_400
        )
        .is_due()
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
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_100
        )
        .skip_reason(),
        Some("scheduler_cooldown_active")
    );
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_400
        )
        .is_due()
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
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_100
        )
        .skip_reason(),
        Some("scheduler_cooldown_active")
    );
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_400
        )
        .is_due()
    );
}

#[test]
fn scheduler_supports_all_self_improvement_tasks() {
    let config = automation_config(Some("hourly"), None);

    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &[],
            SessionActivity::none(),
            1_000
        )
        .is_due()
    );
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &[],
            SessionActivity::none(),
            1_000
        )
        .is_due()
    );
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &[],
            SessionActivity::none(),
            1_000
        )
        .is_due()
    );
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
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &records,
            SessionActivity::none(),
            1_500
        )
        .skip_reason(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[test]
fn scheduler_ranks_same_second_terminal_records_by_micros_then_run_id() {
    let config = automation_config(Some("daily"), None);
    let mut later_failure = record(
        "z-failure",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Failed,
        1_000,
    );
    later_failure.completed_at_micros = Some(1_000_000_900);
    later_failure.error = Some("the request is permanently invalid".to_string());
    later_failure.error_classification = Some(AgentTaskFailureClass::Permanent);
    later_failure.error_retryable = Some(false);
    let mut older_success = record(
        "a-success",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    older_success.completed_at_micros = None;
    let mut records = vec![later_failure, older_success];

    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &records,
            SessionActivity::none(),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_non_retryable_failure")
    );

    records.reverse();
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &records,
            SessionActivity::none(),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_non_retryable_failure")
    );

    records
        .iter_mut()
        .for_each(|record| record.completed_at_micros = Some(1_000_000_100));
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &records,
            SessionActivity::none(),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_non_retryable_failure")
    );
    records.reverse();
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &records,
            SessionActivity::none(),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_non_retryable_failure")
    );
}

#[test]
fn scheduler_ranks_legacy_fractional_completions_before_run_id() {
    let config = automation_config(Some("daily"), None);
    let mut later_failure = record(
        "a-failure",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Failed,
        1_000,
    );
    later_failure.schema_version = 1;
    later_failure.started_at = "1970-01-01T00:16:39Z".to_string();
    later_failure.completed_at = "1970-01-01T00:16:40.9Z".to_string();
    later_failure.completed_at_micros = None;
    later_failure.error = Some("the request is permanently invalid".to_string());
    later_failure.error_classification = Some(AgentTaskFailureClass::Permanent);
    later_failure.error_retryable = Some(false);
    let mut older_success = record(
        "z-success",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    older_success.schema_version = 1;
    older_success.started_at = "1970-01-01T00:16:39Z".to_string();
    older_success.completed_at = "1970-01-01T00:16:40.1Z".to_string();
    older_success.completed_at_micros = None;
    let mut records = vec![later_failure, older_success];

    for _ in 0..2 {
        assert_eq!(
            schedule_decision(
                &config,
                AgentTaskKind::SkillWriter,
                &records,
                SessionActivity::none(),
                1_500,
            )
            .skip_reason(),
            Some("scheduler_non_retryable_failure")
        );
        records.reverse();
    }
}

#[test]
fn scheduler_ranks_latest_success_by_canonical_completion() {
    let config = automation_config(Some("every 10m"), None);
    let mut later_success = record(
        "z-later-success",
        AgentTaskKind::SessionReflector,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    later_success.started_at = "999".to_string();
    later_success.completed_at_micros = Some(1_000_000_900);
    let mut older_success = record(
        "a-older-success",
        AgentTaskKind::SessionReflector,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    older_success.started_at = "998".to_string();
    older_success.completed_at_micros = Some(1_000_000_100);
    let mut records = vec![later_success, older_success];

    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &records,
            SessionActivity::at(999),
            1_700,
        )
        .skip_reason(),
        Some("no_new_session_activity")
    );

    records[0].completed_at_micros = Some(1_000_000_100);
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &records,
            SessionActivity::at(999),
            1_700,
        )
        .skip_reason(),
        Some("no_new_session_activity")
    );
}

#[test]
fn scheduler_fails_closed_on_invalid_completion_history() {
    let config = automation_config(Some("daily"), None);
    let mut inconsistent = record(
        "inconsistent",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    inconsistent.completed_at_micros = Some(1_001_000_000);
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &[inconsistent],
            SessionActivity::none(),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_history_invalid")
    );

    let mut malformed = record(
        "malformed",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    malformed.completed_at = "not-a-timestamp".to_string();
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &[malformed],
            SessionActivity::none(),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_history_invalid")
    );

    let mut overflow = record(
        "overflow",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    overflow.completed_at = i64::MAX.to_string();
    overflow.completed_at_micros = None;
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &[overflow],
            SessionActivity::none(),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_history_invalid")
    );

    let pre_epoch = record(
        "pre-epoch",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        -1,
    );
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &[pre_epoch],
            SessionActivity::none(),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_history_invalid")
    );
}

#[test]
fn scheduler_fails_closed_on_pre_epoch_session_start() {
    let config = automation_config(Some("daily"), None);
    let mut record = record(
        "pre-epoch-start",
        AgentTaskKind::SessionReflector,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    record.started_at = "-1".to_string();

    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &[record],
            SessionActivity::at(0),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_history_invalid")
    );
}

#[test]
fn scheduler_parses_started_at_by_record_schema() {
    let config = automation_config(Some("daily"), None);
    let mut schema_v2 = record(
        "schema-v2-rfc3339-start",
        AgentTaskKind::SessionReflector,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    schema_v2.started_at = "1970-01-01T00:16:39Z".to_string();
    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &[schema_v2],
            SessionActivity::at(1_001),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_history_invalid")
    );

    let mut schema_v1 = record(
        "schema-v1-rfc3339-start",
        AgentTaskKind::SessionReflector,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    schema_v1.schema_version = 1;
    schema_v1.started_at = "1970-01-01T00:16:39Z".to_string();
    schema_v1.completed_at = "1970-01-01T00:16:40Z".to_string();
    schema_v1.completed_at_micros = None;
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &[schema_v1],
            SessionActivity::at(1_000),
            1_500,
        )
        .is_due()
    );
}

#[test]
fn scheduler_rejects_conflicting_duplicate_canonical_identity_in_either_order() {
    let config = automation_config(Some("daily"), None);
    let success = record(
        "same-run",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    let mut failure = success.clone();
    failure.status = AutomationRunStatus::Failed;
    failure.error = Some("the request is permanently invalid".to_string());
    failure.error_classification = Some(AgentTaskFailureClass::Permanent);
    failure.error_retryable = Some(false);

    for records in [
        vec![success.clone(), failure.clone()],
        vec![failure.clone(), success.clone()],
    ] {
        assert_eq!(
            schedule_decision(
                &config,
                AgentTaskKind::SkillWriter,
                &records,
                SessionActivity::none(),
                1_500,
            )
            .skip_reason(),
            Some("scheduler_history_invalid")
        );
    }

    assert_eq!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &[success.clone(), success.clone()],
            SessionActivity::none(),
            1_500,
        )
        .skip_reason(),
        Some("scheduler_interval_not_elapsed")
    );

    let later = record(
        "later-run",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        1_200,
    );
    for records in [
        vec![success.clone(), failure.clone(), later.clone()],
        vec![later.clone(), success.clone(), failure.clone()],
    ] {
        assert_eq!(
            schedule_decision(
                &config,
                AgentTaskKind::SkillWriter,
                &records,
                SessionActivity::none(),
                1_500,
            )
            .skip_reason(),
            Some("scheduler_interval_not_elapsed")
        );
    }
}

#[test]
fn scheduler_idle_window_measures_time_since_session_activity() {
    let mut config = automation_config(Some("every 10m"), None);
    config.tasks.skill_writer.min_idle_secs = Some(600);
    let activity = SessionActivity::at(1_000);

    // Activity landed 500s ago: still inside the 600s idle window.
    assert_eq!(
        schedule_decision(&config, AgentTaskKind::SkillWriter, &[], activity, 1_500).skip_reason(),
        Some("scheduler_idle_window_active")
    );
    // 600s of quiet have elapsed: the project is idle, the task is due.
    assert!(schedule_decision(&config, AgentTaskKind::SkillWriter, &[], activity, 1_600).is_due());
    // Unknown activity (no session store yet) counts as idle.
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::SkillWriter,
            &[],
            SessionActivity::none(),
            1_100
        )
        .is_due()
    );
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
    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::at(100),
            1_500
        )
        .is_due()
    );
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
            schedule_decision(&config, task, &records, SessionActivity::at(1_650), 1_700).is_due()
        );
        // Fresh completed-turn evidence bypasses the periodic repair interval.
        assert!(
            schedule_decision(&config, task, &records, SessionActivity::at(1_050), 1_100).is_due()
        );
    }
}

#[test]
fn scheduler_first_session_evidence_run_is_not_gated_on_activity() {
    // With no prior successful run there is nothing to deduplicate against;
    // the runner's own evidence checks handle an empty session store.
    let config = automation_config(Some("every 10m"), None);

    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &[],
            SessionActivity::none(),
            1_000
        )
        .is_due()
    );
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

    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_700
        )
        .is_due()
    );
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

    assert!(
        schedule_decision(
            &config,
            AgentTaskKind::SessionReflector,
            &records,
            SessionActivity::none(),
            1_400
        )
        .is_due()
    );
}

#[tokio::test]
async fn load_session_activity_reads_newest_message_timestamp() {
    let temp = tempdir().unwrap();
    let db = scheduler_session_runtime(temp.path(), "project.scheduler-activity").await;
    assert_eq!(
        db.session_activity_for_test(HostAdmissionScope::Project)
            .await
            .unwrap(),
        SessionActivity::none()
    );
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
    assert_eq!(
        db.session_activity_for_test(HostAdmissionScope::Project)
            .await
            .unwrap(),
        SessionActivity::at(1_715_000_200)
    );
}

#[tokio::test]
async fn load_session_activity_normalizes_millisecond_timestamps() {
    let temp = tempdir().unwrap();
    let db = scheduler_session_runtime(temp.path(), "project.scheduler-millisecond").await;
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
    assert_eq!(
        db.session_activity_for_test(HostAdmissionScope::Project)
            .await
            .unwrap(),
        SessionActivity::at(1_715_000_300)
    );
}

#[tokio::test]
async fn load_session_activity_selects_newest_after_normalizing_mixed_timestamp_units() {
    let temp = tempdir().unwrap();
    let db = scheduler_session_runtime(temp.path(), "project.scheduler-mixed-units").await;
    seed_session_message_in_db(
        &db,
        temp.path(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "activity-ms",
            message_id: "activity-ms-message-001",
            role: "user",
            timestamp: 1_715_000_100_000,
            text: "older millisecond provider timestamp",
            source: None,
        },
    )
    .await;
    seed_session_message_in_db(
        &db,
        temp.path(),
        SeedSessionMessage {
            provider: "codex",
            session_id: "activity-secs",
            message_id: "activity-secs-message-001",
            role: "user",
            timestamp: 1_715_000_200,
            text: "newer second provider timestamp",
            source: None,
        },
    )
    .await;
    assert_eq!(
        db.session_activity_for_test(HostAdmissionScope::Project)
            .await
            .unwrap(),
        SessionActivity::at(1_715_000_200)
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
    let error = effective_config(&AutomationConfig::default(), Some(&patch))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("memory_curator min_idle_secs must be greater than zero"),
        "zero min_idle_secs must retain its field-specific rejection: {error}"
    );

    let patch = AutomationConfigPatch {
        memory_curator: AutomationTaskPatch {
            stale_lock_secs: Some(Some(0)),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };
    let error = effective_config(&AutomationConfig::default(), Some(&patch))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("memory_curator stale_lock_secs must be greater than zero"),
        "zero stale_lock_secs must retain its field-specific rejection: {error}"
    );

    // Nonzero bounds on the same fields remain accepted, so the rejection
    // above is about the zero value rather than the fields themselves.
    let patch = AutomationConfigPatch {
        memory_curator: AutomationTaskPatch {
            min_idle_secs: Some(Some(600)),
            stale_lock_secs: Some(Some(3_600)),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };
    let config =
        effective_config(&AutomationConfig::default(), Some(&patch)).expect("nonzero bounds");
    assert_eq!(config.tasks.memory_curator.min_idle_secs, Some(600));
    assert_eq!(config.tasks.memory_curator.stale_lock_secs, Some(3_600));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn task_lock_reclaims_stale_dead_pid_lock_file() {
    let temp = tempdir().unwrap();
    let lock_dir = temp.path().join("automation_locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    let dead_pid = reaped_task_lock_child_pid();
    std::fs::write(&lock_path, format!("pid={dead_pid}\ncreated_at=100\n")).unwrap();

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

#[cfg(any(unix, windows))]
#[tokio::test]
async fn task_lock_keeps_foreign_live_pid_past_stale_threshold() {
    let temp = tempdir().unwrap();
    let lock_dir = temp.path().join("automation_locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("session_reflector.lock");
    let ready_path = temp.path().join("live-child-ready");
    let mut child = spawn_task_lock_liveness_child(&ready_path);
    let child_pid = child.id();
    assert!(child.try_wait().unwrap().is_none());
    std::fs::write(
        &lock_path,
        format!(
            "pid={child_pid}\ncreated_at=100\ntoken={}\n",
            "1".repeat(64)
        ),
    )
    .unwrap();

    let lock = AutomationTaskLock::try_acquire(
        temp.path(),
        AgentTaskKind::SessionReflector,
        Some(10),
        200,
    )
    .await
    .unwrap();

    assert!(lock.is_none());
    assert!(lock_path.exists());
    child.kill().unwrap();
    child.wait().unwrap();
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn old_task_lock_guard_cannot_remove_replacement_token() {
    let temp = tempdir().unwrap();
    let lock_path = temp
        .path()
        .join("automation_locks")
        .join("memory_curator.lock");
    let first =
        AutomationTaskLock::try_acquire(temp.path(), AgentTaskKind::MemoryCurator, Some(10), 100)
            .await
            .unwrap()
            .unwrap();
    let first_record = std::fs::read_to_string(&lock_path).unwrap();
    let first_token = task_lock_token(&first_record);
    let dead_pid = reaped_task_lock_child_pid();
    std::fs::write(
        &lock_path,
        format!("pid={dead_pid}\ncreated_at=100\ntoken={first_token}\n"),
    )
    .unwrap();

    let replacement =
        AutomationTaskLock::try_acquire(temp.path(), AgentTaskKind::MemoryCurator, Some(10), 200)
            .await
            .unwrap()
            .unwrap();
    let replacement_record = std::fs::read_to_string(&lock_path).unwrap();
    assert_ne!(task_lock_token(&replacement_record), first_token);

    drop(first);
    assert_eq!(
        std::fs::read_to_string(&lock_path).unwrap(),
        replacement_record,
        "the prior guard must not unlink a newer owner's exact token"
    );
    let third =
        AutomationTaskLock::try_acquire(temp.path(), AgentTaskKind::MemoryCurator, Some(10), 300)
            .await
            .unwrap();
    assert!(third.is_none());

    drop(replacement);
    assert!(!lock_path.exists());
}

#[tokio::test]
async fn cancelled_task_lock_acquisition_cleans_detached_owner() {
    use std::future::Future;

    let temp = tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let lock_dir = root.join("automation_locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("skill_writer.lock");
    let coordination_path = tracedecay_runtime_core::storage::append_lock_path(&lock_path);
    #[cfg(windows)]
    let coordination = {
        let file = tracedecay_runtime_core::windows_security::open_or_create_private_lock_file(
            &coordination_path,
        )
        .unwrap();
        fs2::FileExt::lock_exclusive(&file).unwrap();
        file
    };
    #[cfg(not(windows))]
    let coordination =
        tracedecay_runtime_core::storage::acquire_sidecar_lock_blocking(&coordination_path)
            .unwrap();
    let (submitted_tx, submitted_rx) = tokio::sync::oneshot::channel();
    let mut submitted_tx = Some(submitted_tx);
    let acquire = tokio::spawn(async move {
        let mut future = Box::pin(AutomationTaskLock::try_acquire(
            &root,
            AgentTaskKind::SkillWriter,
            Some(10),
            200,
        ));
        std::future::poll_fn(move |context| {
            let poll = future.as_mut().poll(context);
            if poll.is_pending()
                && let Some(submitted_tx) = submitted_tx.take()
            {
                let _ = submitted_tx.send(());
            }
            poll
        })
        .await
    });
    submitted_rx.await.unwrap();
    acquire.abort();
    assert!(acquire.await.unwrap_err().is_cancelled());
    fs2::FileExt::unlock(&coordination).unwrap();
    drop(coordination);

    let acquired = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(lock) = AutomationTaskLock::try_acquire(
                temp.path(),
                AgentTaskKind::SkillWriter,
                Some(10),
                200,
            )
            .await
            .unwrap()
            {
                break lock;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("detached acquisition must release its exact owner token");
    drop(acquired);
    assert!(!lock_path.exists());
}

#[cfg(any(unix, windows))]
const TASK_LOCK_LIVENESS_CHILD_ENV: &str = "TRACEDECAY_TASK_LOCK_LIVENESS_CHILD";
#[cfg(any(unix, windows))]
const TASK_LOCK_LIVENESS_READY_ENV: &str = "TRACEDECAY_TASK_LOCK_LIVENESS_READY";

#[cfg(any(unix, windows))]
fn spawn_task_lock_liveness_child(ready_path: &std::path::Path) -> std::process::Child {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "scheduler::task_lock_liveness_child",
            "--nocapture",
        ])
        .env(TASK_LOCK_LIVENESS_CHILD_ENV, "1")
        .env(TASK_LOCK_LIVENESS_READY_ENV, ready_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..400 {
        if ready_path.exists() {
            return child;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("task-lock liveness child exited before readiness: {status}");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("task-lock liveness child did not publish readiness")
}

#[cfg(any(unix, windows))]
fn reaped_task_lock_child_pid() -> u32 {
    let temp = tempdir().unwrap();
    let mut child = spawn_task_lock_liveness_child(&temp.path().join("child-ready"));
    let pid = child.id();
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    child.wait().unwrap();
    pid
}

#[cfg(any(unix, windows))]
fn task_lock_token(record: &str) -> &str {
    record
        .lines()
        .find_map(|line| line.strip_prefix("token="))
        .unwrap()
}

#[cfg(any(unix, windows))]
#[test]
fn task_lock_liveness_child() {
    if std::env::var_os(TASK_LOCK_LIVENESS_CHILD_ENV).is_some() {
        let ready_path = std::env::var_os(TASK_LOCK_LIVENESS_READY_ENV).unwrap();
        std::fs::write(ready_path, b"ready").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

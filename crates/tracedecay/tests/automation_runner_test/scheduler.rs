use tempfile::tempdir;
use tracedecay_automation_runtime::automation::backend::{AgentTaskFailureClass, AgentTaskKind};
use tracedecay_automation_runtime::automation::config::{
    AutomationBackend, AutomationConfig, AutomationConfigPatch, AutomationTaskConfig,
    AutomationTaskPatch, AutomationTaskSet, effective_config,
};
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
};
use tracedecay_automation_runtime::automation::scheduler::{
    AutomationSchedule, AutomationScheduleDecision, AutomationSchedulerControl, AutomationTaskLock,
    SessionActivity, host_receipt_decision as admitted_host_receipt_decision,
    load_scheduler_control, resolve_schedule, save_scheduler_control,
    schedule_decision as admitted_schedule_decision, scheduler_control_path,
};
use tracedecay_contracts::retained_surfaces::AutomationSkipReasonV1;
use tracedecay_domain::ProjectId;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::admission::HostAdmissionScope;

use crate::support::{SeedSessionMessage, scheduler_record_for, seed_session_message_in_db};

fn schedule_decision(
    config: &AutomationConfig,
    executable: Option<&std::path::Path>,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
) -> AutomationScheduleDecision {
    admitted_schedule_decision(
        &tracedecay_automation::config::admit_config(config).unwrap(),
        executable,
        task,
        records,
        activity,
        now_secs,
    )
}

fn host_receipt_decision(
    config: &AutomationConfig,
    executable: Option<&std::path::Path>,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
) -> AutomationScheduleDecision {
    admitted_host_receipt_decision(
        &tracedecay_automation::config::admit_config(config).unwrap(),
        executable,
        task,
        records,
        activity,
        now_secs,
    )
}

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
fn schedules_resolve_once_into_one_timing_policy() {
    let every = |every_secs| AutomationSchedule::Interval { every_secs };
    for (schedule, interval_secs, expected) in [
        (None, None, AutomationSchedule::Manual),
        (None, Some(60), AutomationSchedule::Manual),
        (Some("manual"), Some(60), AutomationSchedule::Manual),
        (Some("off"), None, AutomationSchedule::Manual),
        (Some("interval"), Some(900), every(900)),
        (Some("weekly"), None, every(7 * 24 * 60 * 60)),
        (Some("weekly"), Some(5), every(7 * 24 * 60 * 60)),
        (Some("every 15m"), None, every(900)),
        (Some("interval:2h"), Some(5), every(7200)),
    ] {
        assert_eq!(
            resolve_schedule(schedule, interval_secs).unwrap(),
            expected,
            "{schedule:?} with {interval_secs:?}"
        );
    }
    for (schedule, interval_secs) in [
        (Some("interval"), None),
        (Some("interval"), Some(0)),
        (Some("after lunch"), Some(60)),
    ] {
        assert!(
            resolve_schedule(schedule, interval_secs).is_err(),
            "{schedule:?} with {interval_secs:?} must be refused, not fall through to manual"
        );
    }
}

#[test]
fn interval_schedule_without_a_period_is_invalid_not_manual() {
    let config = automation_config(Some("interval"), None);
    assert!(tracedecay_automation::config::admit_config(&config).is_err());
}

#[test]
fn admitted_schedules_preserve_due_boundaries_and_do_not_follow_input_mutation() {
    for (schedule, interval, boundary) in [
        ("interval", Some(600), 700),
        ("every 10m", Some(5), 700),
        ("*/15 * * * *", None, 900),
    ] {
        let mut input = automation_config(Some(schedule), interval);
        let admitted = tracedecay_automation::config::admit_config(&input).unwrap();
        let records = [record(
            "previous",
            AgentTaskKind::MemoryCurator,
            AutomationRunStatus::Succeeded,
            100,
        )];
        input.tasks.memory_curator.schedule = Some("after lunch".to_owned());
        input.tasks.memory_curator.interval_secs = None;
        assert!(tracedecay_automation::config::admit_config(&input).is_err());
        for now in [boundary - 1, boundary, boundary + 1, boundary] {
            let decision = admitted_schedule_decision(
                &admitted,
                None,
                AgentTaskKind::MemoryCurator,
                &records,
                SessionActivity::none(),
                now,
            );
            assert_eq!(decision.is_due(), now >= boundary, "{schedule} at {now}");
        }
    }
}

#[test]
fn host_receipt_bypasses_schedule_but_preserves_enablement_and_idle_gates() {
    let mut config = automation_config(None, None);
    assert!(
        host_receipt_decision(
            &config,
            None,
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
            None,
            AgentTaskKind::SessionReflector,
            &[],
            SessionActivity::at(100),
            200,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_idle_window_active")
    );
    config.enabled = false;
    assert_eq!(
        host_receipt_decision(
            &config,
            None,
            AgentTaskKind::SessionReflector,
            &[],
            SessionActivity::none(),
            200,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
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
            None,
            AgentTaskKind::MemoryCurator,
            &[],
            SessionActivity::none(),
            1_000
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("automation_disabled")
    );

    let config = automation_config(Some("manual"), None);
    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::MemoryCurator,
            &[],
            SessionActivity::none(),
            1_000
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
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
            None,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_500
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_interval_not_elapsed")
    );
    assert!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_700
        )
        .is_due()
    );
}

#[test]
fn scheduler_terminal_skips_advance_the_configured_interval() {
    let config = automation_config(Some("every 15m"), None);
    let mut skipped = record(
        "run-skipped",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Skipped,
        1_000,
    );
    skipped.error = Some("nothing_to_review".to_string());
    let mut failed = record(
        "run-failed",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Failed,
        900,
    );
    failed.error = Some("the request is permanently invalid".to_string());
    failed.error_classification = Some(AgentTaskFailureClass::Permanent);
    failed.error_retryable = Some(false);
    let records = vec![failed, skipped];

    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_250,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_interval_not_elapsed")
    );
    assert!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_900,
        )
        .is_due()
    );
}

#[test]
fn scheduler_diagnostic_skips_do_not_advance_the_interval() {
    let config = automation_config(Some("every 15m"), None);
    let mut diagnostic = record(
        "run-diagnostic",
        AgentTaskKind::MemoryCurator,
        AutomationRunStatus::Skipped,
        1_000,
    );
    diagnostic.error = Some("scheduler_interval_not_elapsed".to_string());

    assert!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::MemoryCurator,
            &[diagnostic],
            SessionActivity::none(),
            1_001,
        )
        .is_due()
    );
}

#[test]
fn fresh_session_activity_is_relative_to_the_latest_cadence_terminal() {
    let config = automation_config(Some("every 10m"), None);
    let success = record(
        "success",
        AgentTaskKind::SessionReflector,
        AutomationRunStatus::Succeeded,
        800,
    );
    let mut skipped = record(
        "skip",
        AgentTaskKind::SessionReflector,
        AutomationRunStatus::Skipped,
        1_000,
    );
    skipped.error = Some("nothing_to_review".to_string());

    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SessionReflector,
            &[success, skipped.clone()],
            SessionActivity::at(900),
            1_001,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_interval_not_elapsed")
    );
    assert!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SessionReflector,
            &[skipped],
            SessionActivity::at(1_100),
            1_101,
        )
        .is_due()
    );
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
            None,
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
            None,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_100
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_interval_not_elapsed")
    );
    assert!(
        schedule_decision(
            &config,
            None,
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
            None,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_100
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_cooldown_active")
    );
    assert!(
        schedule_decision(
            &config,
            None,
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
            None,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_400
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
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
            None,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_100
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_cooldown_active")
    );
    assert!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::MemoryCurator,
            &records,
            SessionActivity::none(),
            1_400
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
            None,
            AgentTaskKind::SkillWriter,
            &records,
            SessionActivity::none(),
            1_500
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
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
            None,
            AgentTaskKind::SkillWriter,
            &records,
            SessionActivity::none(),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_non_retryable_failure")
    );

    records.reverse();
    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SkillWriter,
            &records,
            SessionActivity::none(),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_non_retryable_failure")
    );

    records
        .iter_mut()
        .for_each(|record| record.completed_at_micros = Some(1_000_000_100));
    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SkillWriter,
            &records,
            SessionActivity::none(),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_non_retryable_failure")
    );
    records.reverse();
    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SkillWriter,
            &records,
            SessionActivity::none(),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_non_retryable_failure")
    );
}

#[test]
fn scheduler_ranks_fractional_completions_before_run_id() {
    let config = automation_config(Some("daily"), None);
    let mut later_failure = record(
        "a-failure",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Failed,
        1_000,
    );
    later_failure.started_at = "999".to_string();
    later_failure.completed_at_micros = Some(1_000_900_000);
    later_failure.error = Some("the request is permanently invalid".to_string());
    later_failure.error_classification = Some(AgentTaskFailureClass::Permanent);
    later_failure.error_retryable = Some(false);
    let mut older_success = record(
        "z-success",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    older_success.started_at = "999".to_string();
    older_success.completed_at_micros = Some(1_000_100_000);
    let mut records = vec![later_failure, older_success];

    for _ in 0..2 {
        assert_eq!(
            schedule_decision(
                &config,
                None,
                AgentTaskKind::SkillWriter,
                &records,
                SessionActivity::none(),
                1_500,
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
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
            None,
            AgentTaskKind::SessionReflector,
            &records,
            SessionActivity::at(999),
            1_700,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("no_new_session_activity")
    );

    records[0].completed_at_micros = Some(1_000_000_100);
    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SessionReflector,
            &records,
            SessionActivity::at(999),
            1_700,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
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
            None,
            AgentTaskKind::SkillWriter,
            &[inconsistent],
            SessionActivity::none(),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
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
            None,
            AgentTaskKind::SkillWriter,
            &[malformed],
            SessionActivity::none(),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
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
            None,
            AgentTaskKind::SkillWriter,
            &[overflow],
            SessionActivity::none(),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
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
            None,
            AgentTaskKind::SkillWriter,
            &[pre_epoch],
            SessionActivity::none(),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
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
            None,
            AgentTaskKind::SessionReflector,
            &[record],
            SessionActivity::at(0),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_history_invalid")
    );
}

#[test]
fn scheduler_rejects_rfc3339_started_at() {
    let config = automation_config(Some("daily"), None);
    let mut schema_v2 = record(
        "schema-v2-rfc3339-start",
        AgentTaskKind::SessionReflector,
        AutomationRunStatus::Succeeded,
        1_000,
    );
    assert_eq!(schema_v2.started_at, "999");
    assert!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SessionReflector,
            &[schema_v2.clone()],
            SessionActivity::at(1_001),
            1_500,
        )
        .is_due()
    );

    schema_v2.started_at = "1970-01-01T00:16:39Z".to_string();
    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SessionReflector,
            &[schema_v2],
            SessionActivity::at(1_001),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_history_invalid")
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
                None,
                AgentTaskKind::SkillWriter,
                &records,
                SessionActivity::none(),
                1_500,
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
            Some("scheduler_history_invalid")
        );
    }

    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SkillWriter,
            &[success.clone(), success.clone()],
            SessionActivity::none(),
            1_500,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
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
                None,
                AgentTaskKind::SkillWriter,
                &records,
                SessionActivity::none(),
                1_500,
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
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
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SkillWriter,
            &[],
            activity,
            1_500
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("scheduler_idle_window_active")
    );
    // 600s of quiet have elapsed: the project is idle, the task is due.
    assert!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SkillWriter,
            &[],
            activity,
            1_600
        )
        .is_due()
    );
    // Unknown activity satisfies the idle duration but cannot authorize a
    // session-evidence task.
    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SkillWriter,
            &[],
            SessionActivity::none(),
            1_100
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("no_new_session_activity")
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
            None,
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
            schedule_decision(
                &config,
                None,
                task,
                &records,
                SessionActivity::none(),
                1_700
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
            Some("no_new_session_activity")
        );
        // Interval elapsed but the newest activity predates the run.
        assert_eq!(
            schedule_decision(
                &config,
                None,
                task,
                &records,
                SessionActivity::at(900),
                1_700
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
            Some("no_new_session_activity")
        );
        // Activity landed after the run started: due on the next tick.
        assert!(
            schedule_decision(
                &config,
                None,
                task,
                &records,
                SessionActivity::at(1_650),
                1_700
            )
            .is_due()
        );
        // Fresh completed-turn evidence bypasses the periodic repair interval.
        assert!(
            schedule_decision(
                &config,
                None,
                task,
                &records,
                SessionActivity::at(1_050),
                1_100
            )
            .is_due()
        );
    }
}

#[test]
fn scheduler_retries_failed_session_evidence_runs_with_existing_activity() {
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
            None,
            AgentTaskKind::SessionReflector,
            &records,
            SessionActivity::at(900),
            1_400
        )
        .is_due()
    );
    assert_eq!(
        schedule_decision(
            &config,
            None,
            AgentTaskKind::SessionReflector,
            &records,
            SessionActivity::none(),
            1_400,
        )
        .skip_reason()
        .map(AutomationSkipReasonV1::as_str),
        Some("no_new_session_activity")
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
fn config_validates_scheduler_idle_bound_and_rejects_retired_lock_timeout() {
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

    for value in [
        serde_json::json!(0),
        serde_json::json!(3600),
        serde_json::Value::Null,
    ] {
        let error = serde_json::from_value::<AutomationConfigPatch>(serde_json::json!({
            "memory_curator": { "stale_lock_secs": value }
        }))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown field `stale_lock_secs`")
        );
    }

    // The active idle bound still accepts a nonzero value.
    let patch = AutomationConfigPatch {
        memory_curator: AutomationTaskPatch {
            min_idle_secs: Some(Some(600)),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };
    let config =
        effective_config(&AutomationConfig::default(), Some(&patch)).expect("nonzero bounds");
    assert_eq!(config.tasks.memory_curator.min_idle_secs, Some(600));
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

    let lock = AutomationTaskLock::try_acquire(temp.path(), AgentTaskKind::MemoryCurator)
        .await
        .unwrap();

    assert!(lock.is_some());
    assert!(!lock_path.exists(), "a dead owner's lock file is retired");
    drop(lock);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn task_lock_reclaims_a_fresh_dead_pid_without_waiting_out_the_age_gate() {
    let temp = tempdir().unwrap();
    let lock_dir = temp.path().join("automation_locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    let dead_pid = reaped_task_lock_child_pid();
    std::fs::write(
        &lock_path,
        format!("pid={dead_pid}\ncreated_at=200\ntoken={}\n", "a".repeat(64)),
    )
    .unwrap();

    let lock = AutomationTaskLock::try_acquire(temp.path(), AgentTaskKind::MemoryCurator)
        .await
        .unwrap();

    assert!(
        lock.is_some(),
        "a confirmed-dead owner must not block the next scheduler tick for stale_lock_secs"
    );
    assert!(!lock_path.exists(), "a dead owner's lock file is retired");
    drop(lock);
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

    let lock = AutomationTaskLock::try_acquire(temp.path(), AgentTaskKind::SkillWriter)
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

    let lock = AutomationTaskLock::try_acquire(temp.path(), AgentTaskKind::SessionReflector)
        .await
        .unwrap();

    assert!(lock.is_none());
    assert!(lock_path.exists());
    child.kill().unwrap();
    child.wait().unwrap();
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
#[test]
fn task_lock_liveness_child() {
    if std::env::var_os(TASK_LOCK_LIVENESS_CHILD_ENV).is_some() {
        let ready_path = std::env::var_os(TASK_LOCK_LIVENESS_READY_ENV).unwrap();
        std::fs::write(ready_path, b"ready").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

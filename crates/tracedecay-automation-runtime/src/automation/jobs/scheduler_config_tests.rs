use serde_json::json;
use tracedecay_contracts::retained_surfaces::AutomationSkipReasonV1;
use tracedecay_domain::errors::TraceDecayError;

use super::{
    AutomationJob, JOBS_SCHEMA_VERSION, JobDelivery, ScheduledJobDefinitions,
    job_schedule_decision as admitted_job_schedule_decision, jobs_path, validate_job,
};
use crate::automation::backend::{AgentTaskFailureClass, AgentTaskKind};
use crate::automation::config::AutomationSettingsV1;
use crate::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger, append_run_record,
    find_run_record_exact_bounded, load_run_records_for_task_key,
};

async fn jobs_configured_for_scheduler(
    root: &std::path::Path,
) -> tracedecay_domain::errors::Result<bool> {
    Ok(ScheduledJobDefinitions::new(root)
        .read()
        .await?
        .iter()
        .any(super::job_is_schedulable))
}

fn job_schedule_decision(
    job: &AutomationJob,
    records: &[AutomationRunLedgerRecord],
    now_secs: i64,
) -> Option<AutomationSkipReasonV1> {
    admitted_job_schedule_decision(&super::admit_job(job).unwrap(), records, now_secs)
}

#[tokio::test]
async fn corrupt_jobs_file_surfaces_error_instead_of_no_work() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    let path = jobs_path(root);
    let original = b"{ this is not valid json";
    std::fs::write(&path, original).unwrap();
    let err = jobs_configured_for_scheduler(root)
        .await
        .expect_err("corrupt jobs file must surface an error");
    let TraceDecayError::Config { message } = err else {
        panic!("corrupt jobs must fail as a typed configuration error");
    };
    assert_eq!(
        message,
        format!("invalid automation jobs envelope '{}'", path.display())
    );
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[tokio::test]
async fn missing_jobs_file_reports_no_work_without_error() {
    let temp = tempfile::TempDir::new().unwrap();
    assert!(!jobs_configured_for_scheduler(temp.path()).await.unwrap());
}

#[tokio::test]
async fn invalid_persisted_schedule_refuses_admission_without_rewriting_history() {
    let root = tempfile::tempdir().unwrap();
    for schedule in ["interval", "after lunch"] {
        let document = json!({
            "schema_version": JOBS_SCHEMA_VERSION,
            "jobs": [{
                "id": "broken",
                "name": "Broken schedule",
                "prompt": "summarize",
                "enabled": true,
                "schedule": schedule,
                "delivery": { "mode": "file" }
            }]
        });
        let bytes = serde_json::to_vec(&document).unwrap();
        std::fs::write(jobs_path(root.path()), &bytes).unwrap();
        assert!(jobs_configured_for_scheduler(root.path()).await.is_err());
        assert_eq!(std::fs::read(jobs_path(root.path())).unwrap(), bytes);
    }
}

#[test]
fn admitted_job_keeps_its_timing_until_a_new_revision_is_admitted() {
    let mut input = interval_job();
    input.interval_secs = Some(60);
    let admitted = super::admit_job(&input).unwrap();
    let records = [ledger_record(
        "prior",
        AutomationRunStatus::Succeeded,
        None,
        100,
    )];
    input.schedule = Some("manual".to_owned());
    input.interval_secs = None;
    for now in [159, 160, 161, 160] {
        assert_eq!(
            admitted_job_schedule_decision(&admitted, &records, now),
            if now < 160 {
                Some(AutomationSkipReasonV1::SchedulerIntervalNotElapsed)
            } else {
                None
            },
        );
    }
    let changed = super::admit_job(&input).unwrap();
    assert_eq!(
        admitted_job_schedule_decision(&changed, &records, 160),
        Some(AutomationSkipReasonV1::SchedulerScheduleManual)
    );
}

#[tokio::test]
async fn inactive_or_manual_invalid_job_does_not_block_schedulable_siblings() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    let hourly = json!({
        "id": "nightly",
        "name": "Nightly summary",
        "enabled": true,
        "schedule": "hourly",
        "prompt": "summarize",
        "delivery": { "mode": "file" }
    });
    for inactive in [
        json!({
            "id": "retired",
            "name": "Retired",
            "enabled": false,
            "schedule": "hourly",
            "prompt": "",
            "delivery": { "mode": "file" }
        }),
        json!({
            "id": "manual",
            "name": "Manual",
            "enabled": true,
            "schedule": "manual",
            "prompt": "",
            "delivery": { "mode": "file" }
        }),
    ] {
        let document = json!({
            "schema_version": JOBS_SCHEMA_VERSION,
            "jobs": [inactive, hourly]
        });
        std::fs::write(jobs_path(root), serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(
            jobs_configured_for_scheduler(root).await.unwrap(),
            "inactive or manual invalid siblings must not fail-close scheduled work"
        );
        assert_eq!(
            ScheduledJobDefinitions::new(root)
                .read()
                .await
                .unwrap()
                .iter()
                .map(|job| job.id.clone())
                .collect::<Vec<_>>(),
            ["nightly"]
        );
    }

    let disabled_only = json!({
        "schema_version": JOBS_SCHEMA_VERSION,
        "jobs": [{
            "id": "retired",
            "name": "Retired",
            "enabled": false,
            "schedule": "hourly",
            "prompt": "",
            "delivery": { "mode": "file" }
        }]
    });
    std::fs::write(jobs_path(root), serde_json::to_vec(&disabled_only).unwrap()).unwrap();
    assert!(!jobs_configured_for_scheduler(root).await.unwrap());
}

#[tokio::test]
async fn valid_schedulable_job_reports_work_and_recovers_after_corruption() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    let job = json!({
        "schema_version": JOBS_SCHEMA_VERSION,
        "jobs": [{
            "id": "nightly",
            "name": "Nightly summary",
            "enabled": true,
            "schedule": "hourly",
            "prompt": "summarize",
            "delivery": { "mode": "file" }
        }]
    });
    std::fs::write(jobs_path(root), serde_json::to_vec(&job).unwrap()).unwrap();
    assert!(jobs_configured_for_scheduler(root).await.unwrap());
    std::fs::write(jobs_path(root), b"nonsense").unwrap();
    assert!(jobs_configured_for_scheduler(root).await.is_err());
    std::fs::write(jobs_path(root), serde_json::to_vec(&job).unwrap()).unwrap();
    assert!(jobs_configured_for_scheduler(root).await.unwrap());
}

#[test]
fn large_cron_steps_do_not_make_user_jobs_due_on_unselected_days() {
    let mut job = interval_job();
    job.schedule = Some("0 0 31/4294967295 * *".to_owned());
    job.interval_secs = None;
    validate_job(&job).unwrap();
    let january_31 = 30 * 86_400;
    let records = [ledger_record(
        "january",
        AutomationRunStatus::Succeeded,
        None,
        january_31,
    )];
    for day in 31..89 {
        assert_eq!(
            job_schedule_decision(&job, &records, day * 86_400),
            Some(AutomationSkipReasonV1::SchedulerCronNotDue),
            "day {day} since 1970-01-01"
        );
    }
    assert_eq!(job_schedule_decision(&job, &records, 89 * 86_400), None);
    job.schedule = Some("0 0 31/4294967296 * *".to_owned());
    assert!(validate_job(&job).is_err());
    assert!(super::admit_job(&job).is_err());
}

#[test]
fn job_scheduler_ranks_same_second_terminal_records_by_micros_then_run_id() {
    let job = interval_job();
    let mut later_failure = ledger_record(
        "z-failure",
        AutomationRunStatus::Failed,
        Some("the request is permanently invalid"),
        100,
    );
    later_failure.completed_at_micros = Some(100_000_900);
    later_failure.error_classification = Some(AgentTaskFailureClass::Permanent);
    later_failure.error_retryable = Some(false);
    let mut older_success = ledger_record("a-success", AutomationRunStatus::Succeeded, None, 100);
    older_success.completed_at_micros = None;
    let mut records = vec![later_failure, older_success];

    assert_eq!(
        job_schedule_decision(&job, &records, 150),
        Some(AutomationSkipReasonV1::SchedulerNonRetryableFailure)
    );

    records.reverse();
    assert_eq!(
        job_schedule_decision(&job, &records, 150),
        Some(AutomationSkipReasonV1::SchedulerNonRetryableFailure)
    );

    for record in &mut records {
        record.completed_at_micros = Some(100_000_100);
    }
    assert_eq!(
        job_schedule_decision(&job, &records, 150),
        Some(AutomationSkipReasonV1::SchedulerNonRetryableFailure)
    );
    records.reverse();
    assert_eq!(
        job_schedule_decision(&job, &records, 150),
        Some(AutomationSkipReasonV1::SchedulerNonRetryableFailure)
    );
}

#[test]
fn job_scheduler_fails_closed_on_invalid_completion_history() {
    let job = interval_job();
    let mut inconsistent = ledger_record("inconsistent", AutomationRunStatus::Succeeded, None, 100);
    inconsistent.completed_at_micros = Some(101_000_000);
    assert_eq!(
        job_schedule_decision(&job, &[inconsistent], 150),
        Some(AutomationSkipReasonV1::SchedulerHistoryInvalid)
    );

    let mut malformed = ledger_record("malformed", AutomationRunStatus::Succeeded, None, 100);
    malformed.completed_at = "not-a-timestamp".to_string();
    assert_eq!(
        job_schedule_decision(&job, &[malformed], 150),
        Some(AutomationSkipReasonV1::SchedulerHistoryInvalid)
    );

    let mut overflow = ledger_record("overflow", AutomationRunStatus::Succeeded, None, 100);
    overflow.completed_at = i64::MAX.to_string();
    overflow.completed_at_micros = None;
    assert_eq!(
        job_schedule_decision(&job, &[overflow], 150),
        Some(AutomationSkipReasonV1::SchedulerHistoryInvalid)
    );

    let pre_epoch = ledger_record("pre-epoch", AutomationRunStatus::Succeeded, None, -1);
    assert_eq!(
        job_schedule_decision(&job, &[pre_epoch], 150),
        Some(AutomationSkipReasonV1::SchedulerHistoryInvalid)
    );
}

#[tokio::test]
async fn persisted_and_deduplicated_skips_do_not_block_later_due_execution() {
    let temp = tempfile::TempDir::new().unwrap();
    let job = interval_job();
    append_run_record(
        temp.path(),
        &ledger_record("success-a", AutomationRunStatus::Succeeded, None, 100),
    )
    .await
    .unwrap();
    let config = AutomationSettingsV1::default();
    let first = super::scheduler_gate::evaluate_and_record_scheduler_skip_at(
        temp.path(),
        &config,
        None,
        &job,
        "next-occurrence",
        101,
        // Mirrors production: the anchor comes from the same snapshot that
        // minted the occurrence identity, so the diagnostic scan window
        // always covers every row that can carry that identity.
        Some("success-a"),
    )
    .await
    .unwrap();
    assert!(first.is_some());
    let first = first.unwrap();
    assert_ne!(first.run_id, "next-occurrence");
    let repeated = super::scheduler_gate::evaluate_and_record_scheduler_skip_at(
        temp.path(),
        &config,
        None,
        &job,
        "next-occurrence",
        102,
        Some("success-a"),
    )
    .await
    .unwrap();
    assert!(repeated.is_some());
    let repeated = repeated.unwrap();
    assert_ne!(repeated.run_id, "next-occurrence");
    assert_eq!(repeated.run_id, first.run_id);
    assert_eq!(repeated.ledger_record, first.ledger_record);
    let exact_skip = find_run_record_exact_bounded(temp.path(), &repeated.run_id)
        .await
        .unwrap()
        .expect("repeated skip returns the exact persisted diagnostic");
    assert_eq!(exact_skip, first.ledger_record);
    let records = load_run_records_for_task_key(temp.path(), "user_job:nightly", 10)
        .await
        .unwrap();
    assert_eq!(
        records.len(),
        2,
        "the repeated skip must be ledger-deduplicated"
    );
    let due = super::scheduler_gate::evaluate_and_record_scheduler_skip_at(
        temp.path(),
        &config,
        None,
        &job,
        "next-occurrence",
        160,
        Some("success-a"),
    )
    .await
    .unwrap();
    assert!(
        due.is_none(),
        "the same next occurrence must become executable"
    );
    append_run_record(
        temp.path(),
        &ledger_record("next-occurrence", AutomationRunStatus::Succeeded, None, 160),
    )
    .await
    .unwrap();
    let exact = find_run_record_exact_bounded(temp.path(), "next-occurrence")
        .await
        .unwrap()
        .expect("due success remains exact after diagnostic skips");
    assert_eq!(exact.status, AutomationRunStatus::Succeeded);

    let next_skip = super::scheduler_gate::evaluate_and_record_scheduler_skip_at(
        temp.path(),
        &config,
        None,
        &job,
        "following-occurrence",
        161,
        Some("next-occurrence"),
    )
    .await
    .unwrap()
    .expect("the following occurrence is not due");
    assert_ne!(next_skip.run_id, first.run_id);
    let repeated_next = super::scheduler_gate::evaluate_and_record_scheduler_skip_at(
        temp.path(),
        &config,
        None,
        &job,
        "following-occurrence",
        162,
        Some("next-occurrence"),
    )
    .await
    .unwrap()
    .expect("the following occurrence remains not due");
    assert_eq!(repeated_next.run_id, next_skip.run_id);
    assert_eq!(repeated_next.ledger_record, next_skip.ledger_record);
    let exact_next = find_run_record_exact_bounded(temp.path(), &repeated_next.run_id)
        .await
        .unwrap()
        .expect("the following occurrence returns its own exact diagnostic");
    assert_eq!(exact_next, next_skip.ledger_record);

    for index in 0..300 {
        let mut unrelated = ledger_record(
            &format!("dashboard-{index}"),
            AutomationRunStatus::Succeeded,
            None,
            200 + index,
        );
        unrelated.trigger = AutomationTrigger::Dashboard;
        append_run_record(temp.path(), &unrelated).await.unwrap();
    }
    let after_noise = super::scheduler_gate::evaluate_and_record_scheduler_skip_at(
        temp.path(),
        &config,
        None,
        &job,
        "following-occurrence",
        163,
        Some("next-occurrence"),
    )
    .await
    .unwrap()
    .expect("exact diagnostic survives more than the generic lookback");
    assert_eq!(after_noise.run_id, next_skip.run_id);
    assert_eq!(after_noise.ledger_record, next_skip.ledger_record);
    assert_eq!(
        find_run_record_exact_bounded(temp.path(), &after_noise.run_id)
            .await
            .unwrap()
            .expect("no duplicate diagnostic terminal is appended"),
        next_skip.ledger_record
    );
}

#[tokio::test]
async fn scheduler_lock_skip_uses_a_diagnostic_identity_outside_the_effect_occurrence() {
    let temp = tempfile::TempDir::new().unwrap();
    let job = interval_job();
    let config = AutomationSettingsV1::default();
    let occurrence = "locked-occurrence";
    let started_at = "200".to_owned();
    let skip = super::scheduler_gate::record_scheduler_lock_skip(
        temp.path(),
        &config,
        None,
        &job,
        occurrence,
        &started_at,
        // No scheduler-effectful terminal existed when this occurrence's
        // identity was minted, so the diagnostic scan is unbounded.
        None,
    )
    .await
    .unwrap();
    assert_ne!(skip.run_id, occurrence);
    append_run_record(
        temp.path(),
        &ledger_record(occurrence, AutomationRunStatus::Succeeded, None, 201),
    )
    .await
    .unwrap();
    assert_eq!(
        find_run_record_exact_bounded(temp.path(), &skip.run_id)
            .await
            .unwrap()
            .expect("lock diagnostic is exact")
            .status,
        AutomationRunStatus::Skipped
    );
    assert_eq!(
        find_run_record_exact_bounded(temp.path(), occurrence)
            .await
            .unwrap()
            .expect("effect occurrence remains exact")
            .status,
        AutomationRunStatus::Succeeded
    );
}

fn interval_job() -> AutomationJob {
    AutomationJob {
        id: "nightly".to_owned(),
        name: "Nightly summary".to_owned(),
        prompt: "summarize".to_owned(),
        schedule: Some("interval".to_owned()),
        enabled: true,
        interval_secs: Some(60),
        cooldown_secs: None,
        skill_ids: Vec::new(),
        pre_run_command: None,
        delivery: JobDelivery::default(),
        created_at: 0,
        updated_at: 0,
        extra: std::collections::BTreeMap::default(),
    }
}

fn ledger_record(
    run_id: &str,
    status: AutomationRunStatus,
    error: Option<&str>,
    completed_at: i64,
) -> AutomationRunLedgerRecord {
    serde_json::from_value(json!({
        "schema_version": 2,
        "run_id": run_id,
        "trigger": AutomationTrigger::Scheduler,
        "task": AgentTaskKind::UserJob,
        "task_key": "user_job:nightly",
        "backend": "codex_app_server",
        "status": status,
        "accepted_count": 0,
        "rejected_count": 0,
        "error": error,
        "started_at": completed_at.to_string(),
        "completed_at": completed_at.to_string(),
        "completed_at_micros": completed_at * 1_000_000
    }))
    .unwrap()
}

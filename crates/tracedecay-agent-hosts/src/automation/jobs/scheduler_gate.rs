use std::path::Path;

use super::{
    AutomationConfig, AutomationJob, AutomationRunLedgerRecord, AutomationTrigger,
    JOB_LEDGER_LOOKBACK, JobRunContext, Result, TraceDecayError, UserJobAutomationRun,
    current_timestamp, job_schedule_decision, job_task_key, latest_terminal_job_record,
    load_run_records_for_task_key, validate_job,
};

/// Records one scheduler due-decision before the daemon reserves an external
/// effect. Time-relative skips remain observable in the shared ledger without
/// creating an outer journal terminal for an occurrence that has not begun.
pub async fn evaluate_and_record_scheduler_skip(
    dashboard_root: &Path,
    config: &AutomationConfig,
    job: &AutomationJob,
    run_id: &str,
) -> Result<Option<UserJobAutomationRun>> {
    evaluate_and_record_scheduler_skip_at(dashboard_root, config, job, run_id, current_timestamp())
        .await
}

pub(super) async fn evaluate_and_record_scheduler_skip_at(
    dashboard_root: &Path,
    config: &AutomationConfig,
    job: &AutomationJob,
    run_id: &str,
    now_secs: i64,
) -> Result<Option<UserJobAutomationRun>> {
    validate_job(job)?;
    let mut records =
        load_run_records_for_task_key(dashboard_root, &job_task_key(&job.id), JOB_LEDGER_LOOKBACK)
            .await?;
    super::include_scheduler_anchor(dashboard_root, job, &mut records).await?;
    let Some(reason) = job_schedule_decision(job, &records, now_secs) else {
        return Ok(None);
    };
    let started_at = now_secs.to_string();
    let diagnostic_run_id = scheduler_skip_run_id(run_id, reason)?;
    let context = JobRunContext {
        dashboard_root,
        config,
        job,
        run_id: &diagnostic_run_id,
        trigger: AutomationTrigger::Scheduler,
        started_at: &started_at,
    };
    context
        .scheduler_diagnostic_skipped(reason, &records)
        .await
        .map(Some)
}

pub(super) async fn record_scheduler_lock_skip(
    dashboard_root: &Path,
    config: &AutomationConfig,
    job: &AutomationJob,
    occurrence_run_id: &str,
    started_at: &str,
    records: &[AutomationRunLedgerRecord],
) -> Result<UserJobAutomationRun> {
    let reason = "scheduler_lock_active";
    let diagnostic_run_id = scheduler_skip_run_id(occurrence_run_id, reason)?;
    JobRunContext {
        dashboard_root,
        config,
        job,
        run_id: &diagnostic_run_id,
        trigger: AutomationTrigger::Scheduler,
        started_at,
    }
    .scheduler_diagnostic_skipped(reason, records)
    .await
}

pub(super) fn scheduler_skip_run_id(run_id: &str, reason: &str) -> Result<String> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.scheduler.user-job-skip.v1",
        run_id,
        reason,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("scheduler skip identity is invalid: {error}"),
    })?;
    Ok(format!(
        "user_job_skip_{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
}

pub fn latest_effectful_scheduler_job_record<'a>(
    records: &'a [AutomationRunLedgerRecord],
    task_key: &str,
) -> Option<&'a AutomationRunLedgerRecord> {
    latest_terminal_job_record(records, task_key, Some(AutomationTrigger::Scheduler))
}

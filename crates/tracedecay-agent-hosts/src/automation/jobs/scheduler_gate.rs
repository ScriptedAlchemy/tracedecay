use std::path::Path;

use super::{
    AgentTaskKind, AutomationConfig, AutomationJob, AutomationRunLedgerPublication,
    AutomationRunLedgerRecord, AutomationTrigger, JobRunContext, Result, TraceDecayError,
    UserJobAutomationRun, config_skip_reason, current_timestamp, job_schedule_decision,
    job_task_key, latest_terminal_job_record, load_run_ledger_task_summary,
    try_acquire_job_task_lock, validate_job,
};

/// Applies the configuration, canonical job-lock, and schedule gates before
/// the daemon reserves an external effect. Every skip is recorded under a
/// derived diagnostic identity, so observability never terminalizes an outer
/// occurrence that has not begun.
pub async fn evaluate_and_record_scheduler_skip(
    dashboard_root: &Path,
    config: &AutomationConfig,
    job: &AutomationJob,
    run_id: &str,
) -> Result<Option<UserJobAutomationRun>> {
    validate_job(job)?;
    if let Some(reason) = config_skip_reason(config) {
        return record_scheduler_diagnostic(
            dashboard_root,
            config,
            job,
            run_id,
            reason,
            &current_timestamp().to_string(),
            &[],
        )
        .await
        .map(Some);
    }

    let lock_time = current_timestamp();
    let Some(_task_lock) = try_acquire_job_task_lock(dashboard_root, &job.id, lock_time).await?
    else {
        let summary = load_scheduler_summary(dashboard_root, job).await?;
        return record_scheduler_lock_skip(
            dashboard_root,
            config,
            job,
            run_id,
            &current_timestamp().to_string(),
            summary.records(),
        )
        .await
        .map(Some);
    };

    let summary = load_scheduler_summary(dashboard_root, job).await?;
    let decision_time = current_timestamp();
    let Some(reason) = job_schedule_decision(job, summary.records(), decision_time) else {
        return Ok(None);
    };
    record_scheduler_diagnostic(
        dashboard_root,
        config,
        job,
        run_id,
        reason,
        &decision_time.to_string(),
        summary.records(),
    )
    .await
    .map(Some)
}

#[cfg(test)]
pub(super) async fn evaluate_and_record_scheduler_skip_at(
    dashboard_root: &Path,
    config: &AutomationConfig,
    job: &AutomationJob,
    run_id: &str,
    now_secs: i64,
) -> Result<Option<UserJobAutomationRun>> {
    validate_job(job)?;
    if let Some(reason) = config_skip_reason(config) {
        return record_scheduler_diagnostic(
            dashboard_root,
            config,
            job,
            run_id,
            reason,
            &now_secs.to_string(),
            &[],
        )
        .await
        .map(Some);
    }

    let Some(_task_lock) = try_acquire_job_task_lock(dashboard_root, &job.id, now_secs).await?
    else {
        let summary = load_scheduler_summary(dashboard_root, job).await?;
        return record_scheduler_lock_skip(
            dashboard_root,
            config,
            job,
            run_id,
            &now_secs.to_string(),
            summary.records(),
        )
        .await
        .map(Some);
    };

    let summary = load_scheduler_summary(dashboard_root, job).await?;
    let Some(reason) = job_schedule_decision(job, summary.records(), now_secs) else {
        return Ok(None);
    };
    record_scheduler_diagnostic(
        dashboard_root,
        config,
        job,
        run_id,
        reason,
        &now_secs.to_string(),
        summary.records(),
    )
    .await
    .map(Some)
}

async fn load_scheduler_summary(
    dashboard_root: &Path,
    job: &AutomationJob,
) -> Result<super::AutomationRunLedgerTaskSummary> {
    load_run_ledger_task_summary(
        dashboard_root,
        AgentTaskKind::UserJob,
        &job_task_key(&job.id),
    )
    .await
}

async fn record_scheduler_diagnostic(
    dashboard_root: &Path,
    config: &AutomationConfig,
    job: &AutomationJob,
    occurrence_run_id: &str,
    reason: &'static str,
    started_at: &str,
    records: &[AutomationRunLedgerRecord],
) -> Result<UserJobAutomationRun> {
    let diagnostic_run_id = scheduler_skip_run_id(occurrence_run_id, reason)?;
    JobRunContext {
        dashboard_root,
        config,
        job,
        run_id: &diagnostic_run_id,
        trigger: AutomationTrigger::Scheduler,
        started_at,
        ledger_publication: AutomationRunLedgerPublication::Immediate,
    }
    .scheduler_diagnostic_skipped(reason, records)
    .await
}

pub(super) async fn record_scheduler_lock_skip(
    dashboard_root: &Path,
    config: &AutomationConfig,
    job: &AutomationJob,
    occurrence_run_id: &str,
    started_at: &str,
    records: &[AutomationRunLedgerRecord],
) -> Result<UserJobAutomationRun> {
    record_scheduler_diagnostic(
        dashboard_root,
        config,
        job,
        occurrence_run_id,
        "scheduler_lock_active",
        started_at,
        records,
    )
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

pub(super) fn latest_effectful_scheduler_job_record<'a>(
    records: &'a [AutomationRunLedgerRecord],
    task_key: &str,
) -> Result<Option<&'a AutomationRunLedgerRecord>> {
    latest_terminal_job_record(records, task_key, Some(AutomationTrigger::Scheduler))
        .map(|latest| latest.map(|(record, _)| record))
}

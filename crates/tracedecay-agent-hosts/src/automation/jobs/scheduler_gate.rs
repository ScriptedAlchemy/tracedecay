use std::path::Path;

use super::{
    AgentTaskKind, AutomationConfig, AutomationJob, AutomationRunLedgerPublication,
    AutomationTrigger, JobRunContext, Result, TraceDecayError, UserJobAutomationRun,
    config_skip_reason, current_timestamp, job_schedule_decision, job_task_key,
    load_run_ledger_task_summary, try_acquire_job_task_lock, validate_job,
};
#[cfg(test)]
use super::{AutomationRunLedgerRecord, latest_terminal_job_record};

/// Applies the configuration, canonical job-lock, and schedule gates before
/// the daemon reserves an external effect. Every skip is recorded under a
/// derived diagnostic identity, so observability never terminalizes an outer
/// occurrence that has not begun.
///
/// `occurrence_anchor_run_id` is the latest scheduler-effectful terminal that
/// was visible in the **same** ledger snapshot the caller minted `run_id`
/// from. That anchor — never one derived from a fresher read taken here —
/// bounds the anti-duplicate scan behind every diagnostic appended by this
/// gate.
///
/// Invariant: a diagnostic row can only carry a run_id derived from anchor
/// `A` if `A` was already durable when that id was minted, so the row was
/// appended after `A`'s row. Scanning back to `A` therefore always covers
/// every row that could share this occurrence's diagnostic run_id, even when
/// newer effectful terminals landed afterwards. Re-deriving the anchor from a
/// second snapshot can narrow the window past an already-appended row with the
/// same identity, which appends a byte-different duplicate and poisons the
/// exact-lookup read paths for that run_id. `None` means "this identity has no
/// anchor" and scans the whole ledger, which is always a safe superset.
///
/// The schedule *decision* below deliberately still uses a freshly loaded
/// summary: a newer view can only make the decision more correct. Only the
/// scan anchor is pinned to the occurrence snapshot.
pub async fn evaluate_and_record_scheduler_skip(
    dashboard_root: &Path,
    config: &AutomationConfig,
    job: &AutomationJob,
    run_id: &str,
    occurrence_anchor_run_id: Option<&str>,
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
            occurrence_anchor_run_id,
        )
        .await
        .map(Some);
    }

    let lock_time = current_timestamp();
    let Some(_task_lock) = try_acquire_job_task_lock(dashboard_root, &job.id, lock_time).await?
    else {
        return record_scheduler_lock_skip(
            dashboard_root,
            config,
            job,
            run_id,
            &current_timestamp().to_string(),
            occurrence_anchor_run_id,
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
        occurrence_anchor_run_id,
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
    occurrence_anchor_run_id: Option<&str>,
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
            occurrence_anchor_run_id,
        )
        .await
        .map(Some);
    }

    let Some(_task_lock) = try_acquire_job_task_lock(dashboard_root, &job.id, now_secs).await?
    else {
        return record_scheduler_lock_skip(
            dashboard_root,
            config,
            job,
            run_id,
            &now_secs.to_string(),
            occurrence_anchor_run_id,
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
        occurrence_anchor_run_id,
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

/// Appends (or reuses) one scheduler diagnostic under an identity derived from
/// `occurrence_run_id`. `effectful_anchor_run_id` must be the anchor of the
/// snapshot `occurrence_run_id` was minted from; see
/// [`evaluate_and_record_scheduler_skip`] for the invariant this preserves.
async fn record_scheduler_diagnostic(
    dashboard_root: &Path,
    config: &AutomationConfig,
    job: &AutomationJob,
    occurrence_run_id: &str,
    reason: &'static str,
    started_at: &str,
    effectful_anchor_run_id: Option<&str>,
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
    .scheduler_diagnostic_skipped(reason, effectful_anchor_run_id)
    .await
}

/// `effectful_anchor_run_id` carries the same contract as
/// [`record_scheduler_diagnostic`]: it is the anchor of the ledger snapshot
/// `occurrence_run_id` was minted from, not one re-derived here.
pub(super) async fn record_scheduler_lock_skip(
    dashboard_root: &Path,
    config: &AutomationConfig,
    job: &AutomationJob,
    occurrence_run_id: &str,
    started_at: &str,
    effectful_anchor_run_id: Option<&str>,
) -> Result<UserJobAutomationRun> {
    record_scheduler_diagnostic(
        dashboard_root,
        config,
        job,
        occurrence_run_id,
        "scheduler_lock_active",
        started_at,
        effectful_anchor_run_id,
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

/// Test-only anchor derivation. Production callers must NOT re-derive an
/// anchor at diagnostic-append time: the anchor has to come from the same
/// ledger snapshot that minted the occurrence identity (see
/// [`evaluate_and_record_scheduler_skip`]). The daemon obtains it from
/// `load_latest_scheduler_effectful_for_task_key` in the same read that mints
/// the occurrence run_id.
#[cfg(test)]
pub(super) fn latest_effectful_scheduler_job_record<'a>(
    records: &'a [AutomationRunLedgerRecord],
    task_key: &str,
) -> Result<Option<&'a AutomationRunLedgerRecord>> {
    latest_terminal_job_record(records, task_key, Some(AutomationTrigger::Scheduler))
        .map(|latest| latest.map(|(record, _)| record))
}

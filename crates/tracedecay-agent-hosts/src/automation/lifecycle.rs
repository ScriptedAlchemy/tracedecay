use std::{
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::{Value, json};

use super::artifacts::{sha256_json, write_improvement_artifacts};
use super::backend::{
    AgentTaskKind, AgentTaskRequest, AgentTaskResponse, AgentTaskRetryReport, BackendRetryPolicy,
    agent_task_contract, classify_agent_task_error_message, extract_json_object_prefix,
    prompt_version, run_agent_task_with_retry_report, task_key,
};
use super::config::{AutomationBackend, AutomationConfig, AutomationHostMode};
#[cfg(test)]
use super::run_ledger::load_run_records;
use super::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger, append_run_record,
    load_run_records_for_task_key,
};
use super::scheduler::{
    AutomationScheduleDecision, AutomationTaskLock, load_session_activity, schedule_decision,
    stale_lock_secs,
};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::current_timestamp;
use tracedecay_global_db::RegisteredGlobalDb;

static RUN_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) enum SchedulerGate {
    Proceed(Option<AutomationTaskLock>),
    Skip(&'static str),
}

pub(crate) enum BackendTaskRun {
    Response {
        response: AgentTaskResponse,
        retry_report: AgentTaskRetryReport,
    },
    Fallback(Box<AutomationRunLedgerRecord>),
}

pub(crate) struct AgentTaskRunContext<'a> {
    pub(crate) run_id: String,
    pub(crate) trigger: AutomationTrigger,
    pub(crate) dashboard_root: PathBuf,
    /// Exact registered LCM session shard whose newest message timestamp is
    /// the scheduler activity signal.
    sessions_db: Arc<RegisteredGlobalDb>,
    config: &'a AutomationConfig,
    task: AgentTaskKind,
    started_at: String,
    /// Ledger records loaded once by [`Self::gate`] on the scheduler path.
    /// Both gate-level and post-gate skips compute their repeat-skip dedup
    /// from these cached records, so the append path never re-reads the
    /// ledger.
    ledger_records: Option<Vec<AutomationRunLedgerRecord>>,
}

impl<'a> AgentTaskRunContext<'a> {
    pub(crate) fn new(
        dashboard_root: PathBuf,
        sessions_db: Arc<RegisteredGlobalDb>,
        run_id: Option<String>,
        run_id_prefix: &'static str,
        trigger: AutomationTrigger,
        config: &'a AutomationConfig,
        task: AgentTaskKind,
    ) -> Self {
        Self {
            run_id: run_id.unwrap_or_else(|| generated_run_id(run_id_prefix)),
            trigger,
            dashboard_root,
            sessions_db,
            config,
            task,
            started_at: current_timestamp().to_string(),
            ledger_records: None,
        }
    }

    pub(crate) fn started_at(&self) -> &str {
        &self.started_at
    }

    pub(crate) async fn gate(&mut self) -> Result<SchedulerGate> {
        let (gate, records) = task_run_gate(
            self.config,
            &self.dashboard_root,
            self.sessions_db.as_ref(),
            self.task,
            self.trigger,
        )
        .await?;
        self.ledger_records = records;
        Ok(gate)
    }

    pub(crate) async fn skipped_parts(
        &self,
        evidence_hash: Option<String>,
        reason: &str,
        report_task_key: Option<&'static str>,
    ) -> Result<(Value, AutomationRunLedgerRecord)> {
        skipped_run_parts(self, evidence_hash, reason, report_task_key).await
    }

    /// Computes the repeat-skip dedup decision from the records cached by
    /// [`Self::gate`], with no ledger I/O. A scheduler-trigger context whose
    /// gate has not run yet has no cached records and conservatively persists
    /// the skip.
    fn scheduler_skip_is_repeat(&self, reason: &str) -> bool {
        self.trigger == AutomationTrigger::Scheduler
            && self
                .ledger_records
                .as_deref()
                .is_some_and(|records| is_repeat_scheduler_skip(records, self.task, reason))
    }

    pub(crate) fn finalizer(&self, input_hash: Option<String>) -> AgentRunFinalizer<'_> {
        AgentRunFinalizer::new(
            &self.dashboard_root,
            &self.run_id,
            self.trigger,
            self.config,
            self.task,
            self.started_at(),
            input_hash,
        )
    }
}

pub(crate) fn task_skip_reason(
    config: &AutomationConfig,
    task: AgentTaskKind,
) -> Option<&'static str> {
    if !config.enabled {
        return Some("automation_disabled");
    }
    if task_disabled(config, task) {
        return Some(task_disabled_reason(task));
    }
    if config.host_mode == AutomationHostMode::DelegatedHost {
        return Some("delegated_host_mode");
    }
    if config.backend == AutomationBackend::Disabled {
        return Some("backend_disabled");
    }
    None
}

/// Evaluates the scheduler gate, returning the ledger records it loaded so
/// callers can reuse them for skip dedup instead of re-reading the ledger.
/// The ledger is read at most once per gate evaluation.
pub(crate) async fn scheduler_gate(
    config: &AutomationConfig,
    dashboard_root: &Path,
    sessions_db: &RegisteredGlobalDb,
    task: AgentTaskKind,
    trigger: AutomationTrigger,
) -> Result<(SchedulerGate, Option<Vec<AutomationRunLedgerRecord>>)> {
    if !matches!(
        trigger,
        AutomationTrigger::Scheduler | AutomationTrigger::HostReceipt
    ) {
        return Ok((SchedulerGate::Proceed(None), None));
    }

    let now_secs = current_timestamp();
    let records = load_run_records_for_task_key(dashboard_root, task_key(task), 200).await?;
    let Some(lock) = AutomationTaskLock::try_acquire(
        dashboard_root,
        task,
        stale_lock_secs(config, task),
        now_secs,
    )
    .await?
    else {
        return Ok((SchedulerGate::Skip("scheduler_lock_active"), Some(records)));
    };

    let activity = load_session_activity(sessions_db).await;
    let decision = if trigger == AutomationTrigger::HostReceipt {
        super::scheduler::host_receipt_decision(config, task, &records, activity, now_secs)
    } else {
        schedule_decision(config, task, &records, activity, now_secs)
    };
    if let Some(reason) = scheduler_skip_reason(&decision, task) {
        return Ok((SchedulerGate::Skip(reason), Some(records)));
    }

    Ok((SchedulerGate::Proceed(Some(lock)), Some(records)))
}

pub(crate) async fn task_run_gate(
    config: &AutomationConfig,
    dashboard_root: &Path,
    sessions_db: &RegisteredGlobalDb,
    task: AgentTaskKind,
    trigger: AutomationTrigger,
) -> Result<(SchedulerGate, Option<Vec<AutomationRunLedgerRecord>>)> {
    let (gate, records) =
        scheduler_gate(config, dashboard_root, sessions_db, task, trigger).await?;
    let gate = match gate {
        SchedulerGate::Skip(reason) => SchedulerGate::Skip(reason),
        SchedulerGate::Proceed(lock) => match task_skip_reason(config, task) {
            Some(reason) => SchedulerGate::Skip(reason),
            None => SchedulerGate::Proceed(lock),
        },
    };
    Ok((gate, records))
}

/// Appends a skipped run record unless the caller already determined it is a
/// repeat scheduler skip. Performs no ledger reads: `is_repeat` must be
/// computed from the records the gate evaluation loaded.
pub(crate) async fn append_skipped_record(
    run: &AgentTaskRunContext<'_>,
    evidence_hash: Option<String>,
    reason: &str,
    is_repeat: bool,
) -> Result<AutomationRunLedgerRecord> {
    let record = run.finalizer(None).record(RunRecordOutcome {
        model: None,
        status: AutomationRunStatus::Skipped,
        evidence_hash,
        proposed_ops: None,
        accepted_count: 0,
        rejected_count: 0,
        error: Some(reason.to_string()),
    });
    // Scheduler ticks re-evaluate every task every few seconds, so a standing
    // skip condition (interval not elapsed, task disabled, ...) would append
    // thousands of identical records and drown real runs out of the ledger.
    // Persist only the first record of each consecutive identical skip.
    //
    // The gate's ledger read and this append are not atomic: two concurrent
    // writers can both observe no prior skip and each append the "first"
    // record. The duplicate is benign, so no cross-process locking is done.
    if run.trigger == AutomationTrigger::Scheduler && is_repeat {
        return Ok(record);
    }
    append_run_record(&run.dashboard_root, &record).await?;
    Ok(record)
}

/// True when the most recent ledger record for `task` is already a scheduler
/// skip with the same reason.
///
/// The skip reason is read out of `record.error`, inheriting the pre-existing
/// modeling wart that skipped runs store their reason in the error field.
fn is_repeat_scheduler_skip(
    records: &[AutomationRunLedgerRecord],
    task: AgentTaskKind,
    reason: &str,
) -> bool {
    records
        .iter()
        .find(|record| record.task == task)
        .is_some_and(|record| {
            record.trigger == AutomationTrigger::Scheduler
                && record.status == AutomationRunStatus::Skipped
                && record.error.as_deref() == Some(reason)
        })
}

pub(crate) async fn skipped_run_parts(
    run: &AgentTaskRunContext<'_>,
    evidence_hash: Option<String>,
    reason: &str,
    report_task_key: Option<&'static str>,
) -> Result<(Value, AutomationRunLedgerRecord)> {
    let mut report = json!({
        "status": "skipped",
        "reason": reason,
        "dry_run": true,
    });
    if let Some(task_key) = report_task_key
        && let Some(object) = report.as_object_mut()
    {
        object.insert("task".to_string(), json!(task_key));
    }
    let record = append_skipped_record(
        run,
        evidence_hash,
        reason,
        run.scheduler_skip_is_repeat(reason),
    )
    .await?;
    Ok((report, record))
}

pub(crate) fn failed_backend_fallback_report(record: &AutomationRunLedgerRecord) -> Value {
    json!({
        "status": "failed",
        "run_id": record.run_id,
        "task": record.task_key.as_deref().unwrap_or_else(|| task_key(record.task)),
        "fallback_status": record.fallback_status,
        "error": record.error,
        "proposed_ops": record.proposed_ops,
        "accepted_count": record.accepted_count,
        "rejected_count": record.rejected_count,
        "reviewed_count": record.reviewed_count,
    })
}

struct RunRecordOutcome {
    model: Option<String>,
    status: AutomationRunStatus,
    evidence_hash: Option<String>,
    proposed_ops: Option<Value>,
    accepted_count: usize,
    rejected_count: usize,
    error: Option<String>,
}

pub(crate) struct AgentRunFinalizer<'a> {
    dashboard_root: &'a Path,
    run_id: &'a str,
    trigger: AutomationTrigger,
    config: &'a AutomationConfig,
    task: AgentTaskKind,
    started_at: &'a str,
    input_hash: Option<String>,
    /// When set, this finalizer records one half of a combined
    /// reflector+skill run: ledger records keep their per-task `task` and
    /// `task_key` (so per-task last-run bookkeeping still works) but carry
    /// the combined contract's `prompt_version`/`response_schema` plus a
    /// `combined_run_id` correlation in `report_ref`.
    combined_run_id: Option<String>,
}

impl<'a> AgentRunFinalizer<'a> {
    pub(crate) fn new(
        dashboard_root: &'a Path,
        run_id: &'a str,
        trigger: AutomationTrigger,
        config: &'a AutomationConfig,
        task: AgentTaskKind,
        started_at: &'a str,
        input_hash: Option<String>,
    ) -> Self {
        Self {
            dashboard_root,
            run_id,
            trigger,
            config,
            task,
            started_at,
            input_hash,
            combined_run_id: None,
        }
    }

    #[must_use]
    pub(crate) fn for_combined_run(mut self, combined_run_id: String) -> Self {
        self.combined_run_id = Some(combined_run_id);
        self
    }

    pub(crate) fn dashboard_root(&self) -> &Path {
        self.dashboard_root
    }

    pub(crate) fn run_id(&self) -> &str {
        self.run_id
    }

    pub(crate) fn config(&self) -> &AutomationConfig {
        self.config
    }

    pub(crate) async fn append_backend_fallback_record(
        &self,
        evidence_hash: Option<String>,
        error: String,
        retry_report: &AgentTaskRetryReport,
    ) -> Result<AutomationRunLedgerRecord> {
        let fallback_output = noop_output_for_task(self.task);
        let mut record = self.record(RunRecordOutcome {
            model: None,
            status: AutomationRunStatus::Failed,
            evidence_hash,
            proposed_ops: Some(fallback_output),
            accepted_count: 0,
            rejected_count: 0,
            error: Some(error),
        });
        record.input_hash.clone_from(&self.input_hash);
        record.output_hash = record.proposed_ops.as_ref().map(sha256_json);
        record.fallback_status = Some("backend_failed_noop".to_string());
        apply_retry_report(&mut record, retry_report);
        self.annotate_combined_run(&mut record);
        append_run_record(self.dashboard_root, &record).await?;
        Ok(record)
    }

    pub(crate) async fn run_backend_or_fallback(
        &self,
        backend: &dyn super::backend::AgentTaskBackend,
        request: &AgentTaskRequest,
        evidence_hash: Option<String>,
    ) -> Result<BackendTaskRun> {
        let retry_policy = BackendRetryPolicy::from_timeout_secs(self.config.timeout_secs);
        let mut retry_report = AgentTaskRetryReport::default();
        match run_agent_task_with_retry_report(backend, request, &retry_policy, &mut retry_report)
            .await
        {
            Ok(response) => Ok(BackendTaskRun::Response {
                response,
                retry_report,
            }),
            Err(err) => self
                .append_backend_fallback_record(evidence_hash, err.to_string(), &retry_report)
                .await
                .map(Box::new)
                .map(BackendTaskRun::Fallback),
        }
    }

    pub(crate) async fn append_failed_record(
        &self,
        model: Option<String>,
        evidence_hash: Option<String>,
        proposed_ops: Option<Value>,
        error: String,
        retry_report: &AgentTaskRetryReport,
    ) -> Result<AutomationRunLedgerRecord> {
        let mut record = self.record(RunRecordOutcome {
            model,
            status: AutomationRunStatus::Failed,
            evidence_hash,
            proposed_ops,
            accepted_count: 0,
            rejected_count: 0,
            error: Some(error),
        });
        apply_retry_report(&mut record, retry_report);
        self.finish_record(&mut record);
        append_run_record(self.dashboard_root, &record).await?;
        Ok(record)
    }

    pub(crate) fn success_record(
        &self,
        response: &AgentTaskResponse,
        evidence_hash: Option<String>,
        proposed_ops: Option<Value>,
        accepted_count: usize,
        rejected_count: usize,
    ) -> AutomationRunLedgerRecord {
        self.record(RunRecordOutcome {
            model: response.model.clone(),
            status: AutomationRunStatus::Succeeded,
            evidence_hash,
            proposed_ops,
            accepted_count,
            rejected_count,
            error: None,
        })
    }

    pub(crate) async fn append_success_record(
        &self,
        request: &AgentTaskRequest,
        response: &AgentTaskResponse,
        retry_report: &AgentTaskRetryReport,
        mut record: AutomationRunLedgerRecord,
    ) -> Result<AutomationRunLedgerRecord> {
        apply_retry_report(&mut record, retry_report);
        self.finish_record(&mut record);
        record.artifacts = write_improvement_artifacts(
            self.dashboard_root,
            self.run_id,
            self.task,
            request,
            response,
            &record,
        )
        .await?;
        append_run_record(self.dashboard_root, &record).await?;
        Ok(record)
    }

    pub(crate) async fn response_output_json(
        &self,
        response: &AgentTaskResponse,
        evidence_hash: Option<String>,
        retry_report: &AgentTaskRetryReport,
    ) -> Result<Value> {
        match response
            .output_json
            .clone()
            .map_or_else(|| extract_json_object_prefix(&response.output_text), Ok)
        {
            Ok(output) => Ok(output),
            Err(err) => {
                self.append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    None,
                    err.to_string(),
                    retry_report,
                )
                .await?;
                Err(err)
            }
        }
    }

    pub(crate) async fn response_output_array(
        &self,
        response: &AgentTaskResponse,
        evidence_hash: Option<String>,
        retry_report: &AgentTaskRetryReport,
        field: &'static str,
        missing_array_message: &'static str,
    ) -> Result<(Value, Vec<Value>)> {
        let output = self
            .response_output_json(response, evidence_hash.clone(), retry_report)
            .await?;
        if let Some(values) = output.get(field).and_then(Value::as_array).cloned() {
            return Ok((output, values));
        }

        let err = TraceDecayError::Config {
            message: missing_array_message.to_string(),
        };
        self.append_failed_record(
            response.model.clone(),
            evidence_hash,
            Some(output),
            err.to_string(),
            retry_report,
        )
        .await?;
        Err(err)
    }

    fn finish_record(&self, record: &mut AutomationRunLedgerRecord) {
        record.input_hash.clone_from(&self.input_hash);
        record.output_hash = record.proposed_ops.as_ref().map(sha256_json);
        self.annotate_combined_run(record);
    }

    fn record(&self, outcome: RunRecordOutcome) -> AutomationRunLedgerRecord {
        let completed_at = current_timestamp().to_string();
        let error_classification = (outcome.status == AutomationRunStatus::Failed)
            .then(|| {
                outcome
                    .error
                    .as_deref()
                    .map(classify_agent_task_error_message)
            })
            .flatten();
        let contract = agent_task_contract(self.task);
        AutomationRunLedgerRecord {
            schema_version: 2,
            run_id: self.run_id.to_string(),
            trigger: self.trigger,
            task: self.task,
            task_key: Some(task_key(self.task).to_string()),
            backend: self.config.backend.as_str().to_string(),
            host_mode: Some(self.config.host_mode.as_str().to_string()),
            prompt_version: Some(prompt_version(self.task).to_string()),
            response_schema: Some(contract.response_schema),
            strict_json: Some(contract.strict_json),
            model: outcome.model,
            status: outcome.status,
            evidence_hash: outcome.evidence_hash,
            input_hash: None,
            output_hash: None,
            proposed_ops: outcome.proposed_ops,
            applied_ops: None,
            rejected_ops: None,
            validation_report: None,
            reviewed_count: outcome.accepted_count + outcome.rejected_count,
            accepted_count: outcome.accepted_count,
            rejected_count: outcome.rejected_count,
            skipped_count: usize::from(outcome.status == AutomationRunStatus::Skipped),
            fallback_status: (outcome.status == AutomationRunStatus::Skipped)
                .then(|| outcome.error.clone())
                .flatten(),
            error: outcome.error,
            error_classification,
            error_retryable: error_classification
                .map(super::backend::AgentTaskFailureClass::is_retryable),
            backend_attempt_count: 0,
            backend_attempts: Vec::new(),
            report_ref: Some(json!({
                "dashboard_runs": "/api/plugins/holographic/curation/runs",
                "run_id": self.run_id,
            })),
            artifacts: Vec::new(),
            started_at: self.started_at.to_string(),
            completed_at,
        }
    }

    fn annotate_combined_run(&self, record: &mut AutomationRunLedgerRecord) {
        let Some(combined_run_id) = &self.combined_run_id else {
            return;
        };
        let contract = agent_task_contract(AgentTaskKind::CombinedReview);
        record.prompt_version = Some(contract.prompt_version);
        record.response_schema = Some(contract.response_schema);
        if let Some(report_ref) = record.report_ref.as_mut().and_then(Value::as_object_mut) {
            report_ref.insert("combined_run_id".to_string(), json!(combined_run_id));
            report_ref.insert(
                "combined_task_key".to_string(),
                json!(task_key(AgentTaskKind::CombinedReview)),
            );
        }
    }
}

fn apply_retry_report(record: &mut AutomationRunLedgerRecord, retry_report: &AgentTaskRetryReport) {
    record.backend_attempt_count = retry_report.attempt_count();
    record.backend_attempts = retry_report.attempts().to_vec();
}

fn task_disabled(config: &AutomationConfig, task: AgentTaskKind) -> bool {
    match task {
        AgentTaskKind::MemoryCurator => !config.tasks.memory_curator.enabled,
        AgentTaskKind::SessionReflector => !config.tasks.session_reflector.enabled,
        AgentTaskKind::SkillWriter => !config.tasks.skill_writer.enabled,
        AgentTaskKind::CombinedReview => {
            !config.tasks.session_reflector.enabled || !config.tasks.skill_writer.enabled
        }
        // User jobs carry their own enabled flag on the job record; the job
        // runner gates on it before reaching this config-level check.
        AgentTaskKind::UserJob => false,
    }
}

fn task_disabled_reason(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator_disabled",
        AgentTaskKind::SessionReflector => "session_reflector_disabled",
        AgentTaskKind::SkillWriter => "skill_writer_disabled",
        AgentTaskKind::CombinedReview => "combined_review_disabled",
        AgentTaskKind::UserJob => "user_job_disabled",
    }
}

fn scheduler_skip_reason(
    decision: &AutomationScheduleDecision,
    task: AgentTaskKind,
) -> Option<&'static str> {
    match decision.skip_reason() {
        Some("task_disabled") => Some(task_disabled_reason(task)),
        reason => reason,
    }
}

pub(crate) fn generated_run_id(prefix: &str) -> String {
    let mut random = [0u8; 8];
    let entropy = match getrandom::getrandom(&mut random) {
        Ok(()) => hex::encode(random),
        Err(_) => std::process::id().to_string(),
    };
    let counter = RUN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{}_{counter}_{entropy}", current_timestamp())
}

fn noop_output_for_task(task: AgentTaskKind) -> Value {
    match task {
        AgentTaskKind::MemoryCurator => json!({ "ops": [] }),
        AgentTaskKind::SessionReflector => json!({ "facts": [] }),
        AgentTaskKind::SkillWriter => json!({ "skills": [] }),
        AgentTaskKind::CombinedReview => json!({ "facts": [], "skills": [] }),
        AgentTaskKind::UserJob => json!({ "content": "" }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::super::config::{AutomationTaskConfig, AutomationTaskSet};
    use super::*;

    struct TestSessionsDb {
        db: Arc<RegisteredGlobalDb>,
        _runtime: tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime,
    }

    async fn test_sessions_db(root: &Path) -> TestSessionsDb {
        let nonce = RUN_ID_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        let profile_root = root.join(format!("profile-{nonce}"));
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
            &profile_root,
        )
        .await
        .expect("registered profile test runtime");
        let db = runtime.profile_database_arc();
        TestSessionsDb {
            db,
            _runtime: runtime,
        }
    }

    #[test]
    fn generated_run_ids_are_unique_for_same_prefix() {
        let first = generated_run_id("memory_curator");
        let second = generated_run_id("memory_curator");

        assert_ne!(first, second);
    }

    /// Runs the production skip path: evaluate the gate (which caches ledger
    /// records for scheduler triggers) and then record the skip, exactly as a
    /// gate-level skip does in the task runners.
    async fn append_skip(
        dashboard_root: &Path,
        run_id: &str,
        trigger: AutomationTrigger,
        task: AgentTaskKind,
        reason: &str,
    ) -> AutomationRunLedgerRecord {
        let config = AutomationConfig::default();
        let sessions = test_sessions_db(dashboard_root).await;
        let mut run = AgentTaskRunContext::new(
            dashboard_root.to_path_buf(),
            Arc::clone(&sessions.db),
            Some(run_id.to_string()),
            "test",
            trigger,
            &config,
            task,
        );
        run.gate().await.expect("gate");
        let (_report, record) = run
            .skipped_parts(None, reason, None)
            .await
            .expect("append skipped record");
        record
    }

    #[tokio::test]
    async fn consecutive_identical_scheduler_skips_persist_once() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = temp.path();
        let task = AgentTaskKind::MemoryCurator;

        append_skip(
            root,
            "run-1",
            AutomationTrigger::Scheduler,
            task,
            "scheduler_interval_not_elapsed",
        )
        .await;
        append_skip(
            root,
            "run-2",
            AutomationTrigger::Scheduler,
            task,
            "scheduler_interval_not_elapsed",
        )
        .await;

        let records = load_run_records(root, 50).await.expect("load records");
        assert_eq!(
            records.len(),
            1,
            "repeat scheduler skip must not append a second record"
        );
        assert_eq!(records[0].run_id, "run-1");
    }

    #[tokio::test]
    async fn scheduler_skips_with_new_reason_or_task_still_persist() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = temp.path();

        append_skip(
            root,
            "run-1",
            AutomationTrigger::Scheduler,
            AgentTaskKind::MemoryCurator,
            "scheduler_interval_not_elapsed",
        )
        .await;
        append_skip(
            root,
            "run-2",
            AutomationTrigger::Scheduler,
            AgentTaskKind::MemoryCurator,
            "scheduler_cooldown_active",
        )
        .await;
        append_skip(
            root,
            "run-3",
            AutomationTrigger::Scheduler,
            AgentTaskKind::SessionReflector,
            "scheduler_interval_not_elapsed",
        )
        .await;

        let records = load_run_records(root, 50).await.expect("load records");
        assert_eq!(records.len(), 3, "distinct skip conditions must persist");
    }

    #[tokio::test]
    async fn manual_trigger_skips_always_persist() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = temp.path();
        let task = AgentTaskKind::SkillWriter;

        append_skip(
            root,
            "run-1",
            AutomationTrigger::ManualCli,
            task,
            "skill_writer_disabled",
        )
        .await;
        append_skip(
            root,
            "run-2",
            AutomationTrigger::ManualCli,
            task,
            "skill_writer_disabled",
        )
        .await;

        let records = load_run_records(root, 50).await.expect("load records");
        assert_eq!(records.len(), 2, "manual skips must always be recorded");
    }

    fn scheduler_enabled_config() -> AutomationConfig {
        AutomationConfig {
            enabled: true,
            backend: AutomationBackend::CodexAppServer,
            host_mode: AutomationHostMode::Standalone,
            tasks: AutomationTaskSet {
                memory_curator: AutomationTaskConfig {
                    enabled: true,
                    schedule: Some("hourly".to_string()),
                    ..AutomationTaskConfig::default()
                },
                ..AutomationTaskSet::default()
            },
            ..AutomationConfig::default()
        }
    }

    /// Runs the production post-gate skip path: the gate proceeds (caching
    /// ledger records), and the task body later decides to skip.
    async fn post_gate_scheduler_skip(dashboard_root: &Path, run_id: &str, reason: &str) {
        let config = scheduler_enabled_config();
        let sessions = test_sessions_db(dashboard_root).await;
        let mut run = AgentTaskRunContext::new(
            dashboard_root.to_path_buf(),
            Arc::clone(&sessions.db),
            Some(run_id.to_string()),
            "test",
            AutomationTrigger::Scheduler,
            &config,
            AgentTaskKind::MemoryCurator,
        );
        let SchedulerGate::Proceed(lock) = run.gate().await.expect("gate") else {
            panic!("gate must proceed so the skip is decided post-gate");
        };
        run.skipped_parts(None, reason, None)
            .await
            .expect("append post-gate skip");
        drop(lock);
    }

    #[tokio::test]
    async fn consecutive_identical_post_gate_scheduler_skips_persist_once() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = temp.path();

        post_gate_scheduler_skip(root, "run-1", "nothing_to_review").await;
        post_gate_scheduler_skip(root, "run-2", "nothing_to_review").await;

        let records = load_run_records(root, 50).await.expect("load records");
        assert_eq!(
            records.len(),
            1,
            "repeat post-gate scheduler skip must not append a second record"
        );
        assert_eq!(records[0].run_id, "run-1");
    }

    #[tokio::test]
    async fn append_path_relies_solely_on_caller_computed_repeat_flag() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = temp.path();
        let config = AutomationConfig::default();
        let task = AgentTaskKind::MemoryCurator;
        let sessions = test_sessions_db(root).await;

        // Both identical scheduler skips persist when the caller reports
        // is_repeat=false, even though the second is a repeat on disk: the
        // append path must not perform its own ledger read to second-guess
        // the flag computed from the gate's cached records.
        for run_id in ["run-1", "run-2"] {
            let run = AgentTaskRunContext::new(
                root.to_path_buf(),
                Arc::clone(&sessions.db),
                Some(run_id.to_string()),
                "memory_curator",
                AutomationTrigger::Scheduler,
                &config,
                task,
            );
            append_skipped_record(&run, None, "scheduler_interval_not_elapsed", false)
                .await
                .expect("append skipped record");
        }
        let records = load_run_records(root, 50).await.expect("load records");
        assert_eq!(
            records.len(),
            2,
            "append path must trust the caller-computed repeat flag"
        );

        let run = AgentTaskRunContext::new(
            root.to_path_buf(),
            Arc::clone(&sessions.db),
            Some("run-3".to_string()),
            "memory_curator",
            AutomationTrigger::Scheduler,
            &config,
            task,
        );
        append_skipped_record(&run, None, "scheduler_interval_not_elapsed", true)
            .await
            .expect("append skipped record");
        let records = load_run_records(root, 50).await.expect("load records");
        assert_eq!(records.len(), 2, "is_repeat=true must suppress the append");
    }
}

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::{Arc, OnceLock},
};

use serde_json::{Value, json};

use super::artifacts::{sha256_json, write_improvement_artifacts};
use super::backend::{
    AgentTaskFailureClass, AgentTaskKind, AgentTaskRequest, AgentTaskResponse,
    AgentTaskRetryReport, BackendRetryPolicy, agent_task_contract,
    classify_agent_task_error_message, extract_json_object_prefix, prompt_version,
    run_agent_task_with_retry_report, task_key,
};
use super::config::{AutomationBackend, AutomationConfig, AutomationHostMode};
use super::config_error;
use super::jobs::effect_receipt::ExternalAutomationEffectReceipt;
use super::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunLedgerTaskSummary, AutomationRunStatus,
    AutomationTrigger, append_run_record, load_run_ledger_task_summary,
};
use super::scheduler::{
    AutomationScheduleDecision, AutomationTaskLock, load_session_activity, schedule_decision,
    stale_lock_secs,
};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::current_timestamp;
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_store::{
    FactReadControl, FactWriteControl, ProjectMemoryAutomaticFactApplyResultV1,
    ProjectMemoryFactCurationReceiptV1,
};

static RUN_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// One automation run's caller-owned fact operation controls.
///
/// Every fact read observes the same live interruption predicate. Each fact
/// mutation receives a new one-shot commit admission so independent effects do
/// not share a commit token.
pub struct AutomationRunControl {
    interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
    read_control: FactReadControl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutomationCommittedReceipt {
    MemoryCuration(ProjectMemoryFactCurationReceiptV1),
    AutomaticFacts(Box<NonEmptyAutomaticFactReceipts>),
    UserJobDelivery(ExternalAutomationEffectReceipt),
    SkillWriting(ExternalAutomationEffectReceipt),
}

/// One or more canonical automatic-fact authority results.
///
/// Keeping the first receipt outside the tail makes it impossible for a
/// partial-effect terminal to carry an empty automatic-fact commit set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonEmptyAutomaticFactReceipts {
    first: ProjectMemoryAutomaticFactApplyResultV1,
    rest: Vec<ProjectMemoryAutomaticFactApplyResultV1>,
}

impl NonEmptyAutomaticFactReceipts {
    pub fn from_vec(receipts: Vec<ProjectMemoryAutomaticFactApplyResultV1>) -> Option<Self> {
        let mut receipts = receipts.into_iter();
        let first = receipts.next()?;
        Some(Self {
            first,
            rest: receipts.collect(),
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = &ProjectMemoryAutomaticFactApplyResultV1> {
        std::iter::once(&self.first).chain(self.rest.iter())
    }
}

#[derive(Debug)]
pub enum AutomationRunError {
    Runtime(TraceDecayError),
    /// A canonical failed terminal was constructed. Deferred retained callers
    /// bind it to typed application settlement before ledger publication.
    RecordedFailure {
        error: TraceDecayError,
        ledger_record: Box<AutomationRunLedgerRecord>,
    },
    PartialEffect {
        run_id: String,
        committed_receipt: Box<AutomationCommittedReceipt>,
        ledger_record: Option<Box<AutomationRunLedgerRecord>>,
        detail: &'static str,
    },
}

pub type AutomationRunResult<T> = std::result::Result<T, AutomationRunError>;

#[derive(Clone, Debug, PartialEq)]
pub struct ReusedSchedulerSkip {
    pub requested_run_id: String,
    pub task_key: String,
    pub reason: String,
    pub prior_record: Box<AutomationRunLedgerRecord>,
}

struct RetainedAutomationSettlementState {
    task_lock: OnceLock<AutomationTaskLock>,
    reused_scheduler_skip: OnceLock<ReusedSchedulerSkip>,
}

type RetainedAutomationState = Arc<RetainedAutomationSettlementState>;

/// Keeps one task's canonical scheduler lock alive until the retained
/// application settlement authority has durably published its terminal.
///
/// The guard is intentionally opaque and single-owner. Dropping it releases
/// the underlying filesystem lock through [`AutomationTaskLock`]'s RAII
/// authority; it is never cloned into or serialized with a public run DTO.
#[must_use = "dropping the settlement guard releases the automation task lock"]
pub struct AutomationRunSettlementGuard {
    state: RetainedAutomationState,
}

impl AutomationRunSettlementGuard {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(RetainedAutomationSettlementState {
                task_lock: OnceLock::new(),
                reused_scheduler_skip: OnceLock::new(),
            }),
        }
    }

    fn retention(&self) -> RetainedAutomationState {
        Arc::clone(&self.state)
    }

    /// Transfers one exact canonical scheduler lock into this settlement
    /// guard. Keyed task authorities use the same single-owner path as fixed
    /// tasks rather than recreating or exposing their filesystem lock.
    pub(crate) fn retain_task_lock(&self, task_lock: AutomationTaskLock) -> Result<()> {
        self.state.task_lock.set(task_lock).map_err(|_| {
            config_error("automation settlement guard already owns a canonical task lock")
        })
    }

    fn reused_scheduler_skip(&self) -> Option<ReusedSchedulerSkip> {
        self.state.reused_scheduler_skip.get().cloned()
    }
}

pub enum RetainedAutomationSettlementDisposition<T> {
    Current {
        result: AutomationRunResult<T>,
        settlement_guard: AutomationRunSettlementGuard,
    },
    ReusedSchedulerSkip {
        reused: ReusedSchedulerSkip,
        settlement_guard: AutomationRunSettlementGuard,
    },
}

/// An automation runner result whose task lock remains owned by its caller
/// through typed outer-effect settlement.
#[must_use = "retained automation runs must be settled before releasing their task guard"]
pub struct RetainedAutomationRun<T> {
    result: AutomationRunResult<T>,
    settlement_guard: AutomationRunSettlementGuard,
}

impl<T> RetainedAutomationRun<T> {
    pub(crate) fn new(
        result: AutomationRunResult<T>,
        settlement_guard: AutomationRunSettlementGuard,
    ) -> Self {
        Self {
            result,
            settlement_guard,
        }
    }

    pub fn into_parts(self) -> (AutomationRunResult<T>, AutomationRunSettlementGuard) {
        (self.result, self.settlement_guard)
    }

    pub fn into_settlement_disposition(self) -> RetainedAutomationSettlementDisposition<T> {
        match self.settlement_guard.reused_scheduler_skip() {
            Some(reused) => RetainedAutomationSettlementDisposition::ReusedSchedulerSkip {
                reused,
                settlement_guard: self.settlement_guard,
            },
            None => RetainedAutomationSettlementDisposition::Current {
                result: self.result,
                settlement_guard: self.settlement_guard,
            },
        }
    }
}

/// Ledger publication mode plus the optional retained settlement guard that
/// defers terminal publication until typed application settlement.
pub(crate) struct AutomationRunPublication<'a> {
    pub(crate) ledger: AutomationRunLedgerPublication,
    pub(crate) settlement_guard: Option<&'a AutomationRunSettlementGuard>,
}

/// Selects the authority that publishes one runner terminal to the durable
/// automation ledger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AutomationRunLedgerPublication {
    #[default]
    Immediate,
    DeferredUntilApplicationSettlement,
}

impl From<TraceDecayError> for AutomationRunError {
    fn from(error: TraceDecayError) -> Self {
        Self::Runtime(error)
    }
}

impl AutomationRunError {
    /// Returns the exact failed ledger terminal when the runner constructed
    /// one, allowing retained callers to bind it to application settlement.
    pub fn ledger_record(&self) -> Option<&AutomationRunLedgerRecord> {
        match self {
            Self::Runtime(_) => None,
            Self::RecordedFailure { ledger_record, .. } => Some(ledger_record),
            Self::PartialEffect { ledger_record, .. } => ledger_record.as_deref(),
        }
    }
}

impl std::fmt::Display for AutomationRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Runtime(error) | Self::RecordedFailure { error, .. } => {
                std::fmt::Display::fmt(error, formatter)
            }
            Self::PartialEffect { detail, .. } => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for AutomationRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runtime(error) | Self::RecordedFailure { error, .. } => Some(error),
            Self::PartialEffect { .. } => None,
        }
    }
}

impl AutomationRunControl {
    pub fn from_interrupted(interrupted: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            read_control: FactReadControl::new(Arc::clone(&interrupted)),
            interrupted,
        }
    }

    pub fn read_control(&self) -> &FactReadControl {
        &self.read_control
    }

    /// Returns admission controls for one independent fact mutation.
    ///
    /// The caller's live interruption predicate is checked before the local
    /// one-shot gate. Once that gate wins, it remains consumed even if the run
    /// is interrupted later.
    pub fn write_control(&self) -> FactWriteControl {
        let interrupted = Arc::clone(&self.interrupted);
        let commit_interrupted = Arc::clone(&self.interrupted);
        let commit_admitted = Arc::new(AtomicBool::new(false));
        let commit_gate = Arc::clone(&commit_admitted);
        FactWriteControl::new(
            interrupted,
            Arc::new(move || {
                if commit_interrupted() {
                    return false;
                }
                commit_gate
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }),
        )
    }
}

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
    sessions_db: RegisteredGlobalDbLeaseV1,
    config: &'a AutomationConfig,
    task: AgentTaskKind,
    started_at: String,
    ledger_publication: AutomationRunLedgerPublication,
    retained_state: Option<RetainedAutomationState>,
    /// Complete-ledger bounded summary loaded once by [`Self::gate`] on the
    /// scheduler path. Gate decisions reuse its selected logical records and
    /// post-gate skip dedup reads its explicit latest-activity authority.
    ledger_summary: Option<AutomationRunLedgerTaskSummary>,
}

impl<'a> AgentTaskRunContext<'a> {
    pub(crate) fn new(
        dashboard_root: PathBuf,
        sessions_db: RegisteredGlobalDbLeaseV1,
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
            ledger_publication: AutomationRunLedgerPublication::Immediate,
            retained_state: None,
            ledger_summary: None,
        }
    }

    #[must_use]
    pub(crate) fn with_ledger_publication(
        mut self,
        publication: AutomationRunLedgerPublication,
    ) -> Self {
        self.ledger_publication = publication;
        self
    }

    #[must_use]
    pub(crate) fn with_settlement_guard(
        mut self,
        guard: Option<&AutomationRunSettlementGuard>,
    ) -> Self {
        self.retained_state = guard.map(AutomationRunSettlementGuard::retention);
        self
    }

    pub(crate) fn started_at(&self) -> &str {
        &self.started_at
    }

    pub(crate) async fn gate(&mut self) -> Result<SchedulerGate> {
        let (gate, summary) = task_run_gate_with_lock_retention(
            self.config,
            &self.dashboard_root,
            self.sessions_db.as_ref(),
            self.task,
            self.trigger,
            self.retained_state.as_ref(),
        )
        .await?;
        self.ledger_summary = summary;
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

    pub(crate) async fn skipped_parts_with_validation_report(
        &self,
        evidence_hash: Option<String>,
        reason: &str,
        report_task_key: Option<&'static str>,
        validation_report: Value,
    ) -> Result<(Value, AutomationRunLedgerRecord)> {
        skipped_run_parts_with_validation_report(
            self,
            evidence_hash,
            reason,
            report_task_key,
            Some(validation_report),
            false,
        )
        .await
    }

    /// Computes the repeat-skip dedup decision from the summary cached by
    /// [`Self::gate`], with no ledger I/O. A scheduler-trigger context whose
    /// gate has not run yet has no summary and conservatively persists the
    /// skip.
    fn repeated_scheduler_skip(&self, reason: &str) -> Option<&AutomationRunLedgerRecord> {
        if self.trigger != AutomationTrigger::Scheduler {
            return None;
        }
        self.ledger_summary
            .as_ref()
            .and_then(AutomationRunLedgerTaskSummary::latest_logical_activity)
            .filter(|record| is_repeat_scheduler_skip(record, self.task, reason))
    }

    fn retain_repeated_scheduler_skip(
        &self,
        prior_record: &AutomationRunLedgerRecord,
        reason: &str,
    ) -> Result<bool> {
        let Some(state) = self.retained_state.as_ref() else {
            return Ok(false);
        };
        let reused = ReusedSchedulerSkip {
            requested_run_id: self.run_id.clone(),
            task_key: task_key(self.task).to_owned(),
            reason: reason.to_owned(),
            prior_record: Box::new(prior_record.clone()),
        };
        state.reused_scheduler_skip.set(reused).map_err(|_| {
            config_error("automation retained settlement already selected a scheduler skip")
        })?;
        Ok(true)
    }

    pub(crate) fn finalizer(&self, input_hash: Option<String>) -> Result<AgentRunFinalizer<'_>> {
        AgentRunFinalizer::new(
            &self.dashboard_root,
            &self.run_id,
            self.trigger,
            self.config,
            self.task,
            self.started_at(),
            input_hash,
        )
        .map(|finalizer| finalizer.with_ledger_publication(self.ledger_publication))
    }

    async fn publish_terminal_record(&self, record: &AutomationRunLedgerRecord) -> Result<()> {
        match self.ledger_publication {
            AutomationRunLedgerPublication::Immediate => {
                append_run_record(&self.dashboard_root, record).await
            }
            AutomationRunLedgerPublication::DeferredUntilApplicationSettlement => Ok(()),
        }
    }
}

pub(crate) fn task_skip_reason(
    config: &AutomationConfig,
    _task: AgentTaskKind,
) -> Option<&'static str> {
    if config.host_mode == AutomationHostMode::DelegatedHost {
        return Some("delegated_host_mode");
    }
    if config.backend == AutomationBackend::Disabled {
        return Some("backend_disabled");
    }
    None
}

async fn scheduler_gate_with_lock_retention(
    config: &AutomationConfig,
    dashboard_root: &Path,
    sessions_db: &RegisteredGlobalDb,
    task: AgentTaskKind,
    trigger: AutomationTrigger,
    retained_state: Option<&RetainedAutomationState>,
) -> Result<(SchedulerGate, Option<AutomationRunLedgerTaskSummary>)> {
    let scheduled = matches!(
        trigger,
        AutomationTrigger::Scheduler | AutomationTrigger::HostReceipt
    );

    let lock_now_secs = current_timestamp();
    let Some(lock) = AutomationTaskLock::try_acquire(
        dashboard_root,
        task,
        stale_lock_secs(config, task),
        lock_now_secs,
    )
    .await?
    else {
        let summary = if scheduled {
            Some(load_run_ledger_task_summary(dashboard_root, task, task_key(task)).await?)
        } else {
            None
        };
        return Ok((SchedulerGate::Skip("scheduler_lock_active"), summary));
    };
    let lock = retain_task_lock(lock, retained_state)?;
    if !scheduled {
        return Ok((SchedulerGate::Proceed(lock), None));
    }

    let summary = load_run_ledger_task_summary(dashboard_root, task, task_key(task)).await?;
    let activity = load_session_activity(sessions_db).await;
    let decision_now_secs = current_timestamp();
    let decision = if trigger == AutomationTrigger::HostReceipt {
        super::scheduler::host_receipt_decision(
            config,
            task,
            summary.records(),
            activity,
            decision_now_secs,
        )
    } else {
        schedule_decision(config, task, summary.records(), activity, decision_now_secs)
    };
    if let Some(reason) = scheduler_skip_reason(&decision, task) {
        return Ok((SchedulerGate::Skip(reason), Some(summary)));
    }

    Ok((SchedulerGate::Proceed(lock), Some(summary)))
}

pub(crate) async fn task_run_gate(
    config: &AutomationConfig,
    dashboard_root: &Path,
    sessions_db: &RegisteredGlobalDb,
    task: AgentTaskKind,
    trigger: AutomationTrigger,
) -> Result<(SchedulerGate, Option<AutomationRunLedgerTaskSummary>)> {
    task_run_gate_with_lock_retention(config, dashboard_root, sessions_db, task, trigger, None)
        .await
}

pub(crate) async fn task_run_gate_for_retained_settlement(
    config: &AutomationConfig,
    dashboard_root: &Path,
    sessions_db: &RegisteredGlobalDb,
    task: AgentTaskKind,
    trigger: AutomationTrigger,
    settlement_guard: &AutomationRunSettlementGuard,
) -> Result<(SchedulerGate, Option<AutomationRunLedgerTaskSummary>)> {
    let retention = settlement_guard.retention();
    task_run_gate_with_lock_retention(
        config,
        dashboard_root,
        sessions_db,
        task,
        trigger,
        Some(&retention),
    )
    .await
}

async fn task_run_gate_with_lock_retention(
    config: &AutomationConfig,
    dashboard_root: &Path,
    sessions_db: &RegisteredGlobalDb,
    task: AgentTaskKind,
    trigger: AutomationTrigger,
    retained_state: Option<&RetainedAutomationState>,
) -> Result<(SchedulerGate, Option<AutomationRunLedgerTaskSummary>)> {
    let (gate, records) = scheduler_gate_with_lock_retention(
        config,
        dashboard_root,
        sessions_db,
        task,
        trigger,
        retained_state,
    )
    .await?;
    let gate = match gate {
        SchedulerGate::Skip(reason) => SchedulerGate::Skip(reason),
        SchedulerGate::Proceed(lock) => {
            let enablement_skip = if trigger.is_on_demand() {
                None
            } else if !config.enabled {
                Some("automation_disabled")
            } else if task_disabled(config, task) {
                Some(task_disabled_reason(task))
            } else {
                None
            };
            match enablement_skip.or_else(|| task_skip_reason(config, task)) {
                Some(reason) => SchedulerGate::Skip(reason),
                None => SchedulerGate::Proceed(lock),
            }
        }
    };
    Ok((gate, records))
}

fn retain_task_lock(
    task_lock: AutomationTaskLock,
    retained_state: Option<&RetainedAutomationState>,
) -> Result<Option<AutomationTaskLock>> {
    let Some(retained_state) = retained_state else {
        return Ok(Some(task_lock));
    };
    retained_state.task_lock.set(task_lock).map_err(|_| {
        config_error("automation settlement guard already owns a canonical task lock")
    })?;
    Ok(None)
}

/// Appends a skipped run record unless the caller already determined it is a
/// repeat scheduler skip. Performs no ledger reads: `is_repeat` must be
/// computed from the records the gate evaluation loaded.
#[cfg(test)]
pub(crate) async fn append_skipped_record(
    run: &AgentTaskRunContext<'_>,
    evidence_hash: Option<String>,
    reason: &str,
    is_repeat: bool,
) -> Result<AutomationRunLedgerRecord> {
    append_skipped_record_with_validation(run, evidence_hash, reason, is_repeat, None).await
}

async fn append_skipped_record_with_validation(
    run: &AgentTaskRunContext<'_>,
    evidence_hash: Option<String>,
    reason: &str,
    is_repeat: bool,
    validation_report: Option<Value>,
) -> Result<AutomationRunLedgerRecord> {
    let mut record = run.finalizer(None)?.record(RunRecordOutcome {
        model: None,
        status: AutomationRunStatus::Skipped,
        evidence_hash,
        proposed_ops: None,
        accepted_count: 0,
        rejected_count: 0,
        error: Some(reason.to_string()),
    })?;
    record.validation_report = validation_report;
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
    run.publish_terminal_record(&record).await?;
    Ok(record)
}

/// True when the most recent ledger record for `task` is already a scheduler
/// skip with the same reason.
///
/// The skip reason is read out of `record.error`, inheriting the pre-existing
/// modeling wart that skipped runs store their reason in the error field.
fn is_repeat_scheduler_skip(
    record: &AutomationRunLedgerRecord,
    task: AgentTaskKind,
    reason: &str,
) -> bool {
    record.task == task
        && record.trigger == AutomationTrigger::Scheduler
        && record.status == AutomationRunStatus::Skipped
        && record.error.as_deref() == Some(reason)
}

pub(crate) async fn skipped_run_parts(
    run: &AgentTaskRunContext<'_>,
    evidence_hash: Option<String>,
    reason: &str,
    report_task_key: Option<&'static str>,
) -> Result<(Value, AutomationRunLedgerRecord)> {
    skipped_run_parts_with_validation_report(
        run,
        evidence_hash,
        reason,
        report_task_key,
        None,
        true,
    )
    .await
}

async fn skipped_run_parts_with_validation_report(
    run: &AgentTaskRunContext<'_>,
    evidence_hash: Option<String>,
    reason: &str,
    report_task_key: Option<&'static str>,
    validation_report: Option<Value>,
    dedupe_repeat: bool,
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
    let repeated = dedupe_repeat
        .then(|| run.repeated_scheduler_skip(reason))
        .flatten()
        .cloned();
    let record = match repeated {
        Some(prior_record) if run.retain_repeated_scheduler_skip(&prior_record, reason)? => {
            prior_record
        }
        Some(_) => {
            append_skipped_record_with_validation(
                run,
                evidence_hash,
                reason,
                true,
                validation_report,
            )
            .await?
        }
        None => {
            append_skipped_record_with_validation(
                run,
                evidence_hash,
                reason,
                false,
                validation_report,
            )
            .await?
        }
    };
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
    ledger_publication: AutomationRunLedgerPublication,
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
    ) -> Result<Self> {
        Self::new_at(
            dashboard_root,
            run_id,
            trigger,
            config,
            task,
            started_at,
            input_hash,
            std::time::SystemTime::now(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_at(
        dashboard_root: &'a Path,
        run_id: &'a str,
        trigger: AutomationTrigger,
        config: &'a AutomationConfig,
        task: AgentTaskKind,
        started_at: &'a str,
        input_hash: Option<String>,
        completion_time: std::time::SystemTime,
    ) -> Result<Self> {
        super::run_ledger::timestamp_micros_at(completion_time)?;
        Ok(Self {
            dashboard_root,
            run_id,
            trigger,
            config,
            task,
            started_at,
            input_hash,
            combined_run_id: None,
            ledger_publication: AutomationRunLedgerPublication::Immediate,
        })
    }

    #[must_use]
    pub(crate) fn with_ledger_publication(
        mut self,
        publication: AutomationRunLedgerPublication,
    ) -> Self {
        self.ledger_publication = publication;
        self
    }

    async fn publish_terminal_record(&self, record: &AutomationRunLedgerRecord) -> Result<()> {
        match self.ledger_publication {
            AutomationRunLedgerPublication::Immediate => {
                append_run_record(self.dashboard_root, record).await
            }
            AutomationRunLedgerPublication::DeferredUntilApplicationSettlement => Ok(()),
        }
    }

    #[must_use]
    pub(crate) fn for_combined_run(mut self, combined_run_id: String) -> Self {
        self.combined_run_id = Some(combined_run_id);
        self
    }

    pub(crate) fn run_id(&self) -> &str {
        self.run_id
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
        })?;
        record.input_hash.clone_from(&self.input_hash);
        record.output_hash = record.proposed_ops.as_ref().map(sha256_json);
        record.fallback_status = Some("backend_failed_noop".to_string());
        apply_retry_report(&mut record, retry_report);
        let exact_failure_class = retry_report
            .attempts()
            .last()
            .and_then(|attempt| attempt.failure_classification);
        record.error_classification = exact_failure_class;
        record.error_retryable = exact_failure_class.map(AgentTaskFailureClass::is_retryable);
        self.annotate_combined_run(&mut record);
        self.publish_terminal_record(&record).await?;
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
        })?;
        apply_retry_report(&mut record, retry_report);
        self.finish_record(&mut record);
        self.publish_terminal_record(&record).await?;
        Ok(record)
    }

    /// Records a terminal failure after a store mutation has already committed.
    /// The applied effects stay on the failed record so callers can diagnose
    /// the partial outcome without retrying a mutation blindly.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn append_failed_record_with_effects(
        &self,
        model: Option<String>,
        evidence_hash: Option<String>,
        proposed_ops: Option<Value>,
        error: String,
        retry_report: &AgentTaskRetryReport,
        applied_ops: Option<Value>,
        rejected_ops: Option<Value>,
        validation_report: Option<Value>,
        accepted_count: usize,
        rejected_count: usize,
    ) -> Result<AutomationRunLedgerRecord> {
        let mut record = self.record(RunRecordOutcome {
            model,
            status: AutomationRunStatus::Failed,
            evidence_hash,
            proposed_ops,
            accepted_count,
            rejected_count,
            error: Some(error),
        })?;
        record.applied_ops = applied_ops;
        record.rejected_ops = rejected_ops;
        record.validation_report = validation_report;
        apply_retry_report(&mut record, retry_report);
        self.finish_record(&mut record);
        self.publish_terminal_record(&record).await?;
        Ok(record)
    }

    pub(crate) fn success_record(
        &self,
        response: &AgentTaskResponse,
        evidence_hash: Option<String>,
        proposed_ops: Option<Value>,
        accepted_count: usize,
        rejected_count: usize,
    ) -> Result<AutomationRunLedgerRecord> {
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

    pub(crate) fn completion_timestamp_micros(&self) -> Result<i64> {
        super::run_ledger::current_timestamp_micros()
    }

    pub(crate) fn success_record_at(
        &self,
        response: &AgentTaskResponse,
        evidence_hash: Option<String>,
        proposed_ops: Option<Value>,
        accepted_count: usize,
        rejected_count: usize,
        completed_at_micros: i64,
    ) -> AutomationRunLedgerRecord {
        self.record_from_micros(
            RunRecordOutcome {
                model: response.model.clone(),
                status: AutomationRunStatus::Succeeded,
                evidence_hash,
                proposed_ops,
                accepted_count,
                rejected_count,
                error: None,
            },
            completed_at_micros,
        )
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
        self.publish_terminal_record(&record).await?;
        Ok(record)
    }

    pub(crate) async fn append_prebuilt_failed_record(
        &self,
        mut record: AutomationRunLedgerRecord,
        retry_report: &AgentTaskRetryReport,
    ) -> Result<AutomationRunLedgerRecord> {
        apply_retry_report(&mut record, retry_report);
        self.finish_record(&mut record);
        self.publish_terminal_record(&record).await?;
        Ok(record)
    }

    pub(crate) async fn response_output_json(
        &self,
        response: &AgentTaskResponse,
        evidence_hash: Option<String>,
        retry_report: &AgentTaskRetryReport,
    ) -> AutomationRunResult<Value> {
        match response
            .output_json
            .clone()
            .map_or_else(|| extract_json_object_prefix(&response.output_text), Ok)
        {
            Ok(output) => Ok(output),
            Err(err) => {
                let ledger_record = self
                    .append_failed_record(
                        response.model.clone(),
                        evidence_hash,
                        None,
                        err.to_string(),
                        retry_report,
                    )
                    .await?;
                Err(AutomationRunError::RecordedFailure {
                    error: err,
                    ledger_record: Box::new(ledger_record),
                })
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
    ) -> AutomationRunResult<(Value, Vec<Value>)> {
        let output = self
            .response_output_json(response, evidence_hash.clone(), retry_report)
            .await?;
        if let Some(values) = output.get(field).and_then(Value::as_array).cloned() {
            return Ok((output, values));
        }

        let err = TraceDecayError::Config {
            message: missing_array_message.to_string(),
        };
        let ledger_record = self
            .append_failed_record(
                response.model.clone(),
                evidence_hash,
                Some(failed_output_projection(self.task, field, &output)),
                err.to_string(),
                retry_report,
            )
            .await?;
        Err(AutomationRunError::RecordedFailure {
            error: err,
            ledger_record: Box::new(ledger_record),
        })
    }

    fn finish_record(&self, record: &mut AutomationRunLedgerRecord) {
        record.input_hash.clone_from(&self.input_hash);
        record.output_hash = record.proposed_ops.as_ref().map(sha256_json);
        self.annotate_combined_run(record);
    }

    fn record(&self, outcome: RunRecordOutcome) -> Result<AutomationRunLedgerRecord> {
        self.record_at(outcome, std::time::SystemTime::now())
    }

    fn record_at(
        &self,
        outcome: RunRecordOutcome,
        completion_time: std::time::SystemTime,
    ) -> Result<AutomationRunLedgerRecord> {
        let completed_at_micros = super::run_ledger::timestamp_micros_at(completion_time)?;
        Ok(self.record_from_micros(outcome, completed_at_micros))
    }

    fn record_from_micros(
        &self,
        outcome: RunRecordOutcome,
        completed_at_micros: i64,
    ) -> AutomationRunLedgerRecord {
        let completed_at = (completed_at_micros / 1_000_000).to_string();
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
                "dashboard_runs": "/api/automation/runs",
                "run_id": self.run_id,
            })),
            artifacts: Vec::new(),
            started_at: self.started_at.to_string(),
            completed_at,
            completed_at_micros: Some(completed_at_micros),
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

fn failed_output_projection(task: AgentTaskKind, field: &str, output: &Value) -> Value {
    if task == AgentTaskKind::SessionReflector {
        json!({
            "schema_version": 1,
            "expected_field": field,
            "output_sha256": sha256_json(output),
            "output_kind": if output.is_object() { "object" } else { "non_object" },
        })
    } else {
        output.clone()
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
mod recorded_failure_tests {
    use super::*;

    fn assert_send_static<T: Send + 'static>() {}

    #[test]
    fn retained_settlement_types_are_send_and_static() {
        assert_send_static::<AutomationRunSettlementGuard>();
        assert_send_static::<RetainedAutomationRun<()>>();
        assert_send_static::<RetainedAutomationSettlementDisposition<()>>();
    }

    #[test]
    fn retained_settlement_disposition_carries_exact_reused_scheduler_skip() {
        let config = AutomationConfig::default();
        let prior_record = AgentRunFinalizer::new_at(
            Path::new("/unused"),
            "prior_scheduler_skip",
            AutomationTrigger::Scheduler,
            &config,
            AgentTaskKind::SessionReflector,
            "0",
            None,
            std::time::SystemTime::UNIX_EPOCH,
        )
        .expect("prior finalizer")
        .record(RunRecordOutcome {
            model: None,
            status: AutomationRunStatus::Skipped,
            evidence_hash: None,
            proposed_ops: None,
            accepted_count: 0,
            rejected_count: 0,
            error: Some("interval_not_elapsed".to_owned()),
        })
        .expect("prior scheduler skip");
        let settlement_guard = AutomationRunSettlementGuard::new();
        settlement_guard
            .state
            .reused_scheduler_skip
            .set(ReusedSchedulerSkip {
                requested_run_id: "current_scheduler_run".to_owned(),
                task_key: task_key(AgentTaskKind::SessionReflector).to_owned(),
                reason: "interval_not_elapsed".to_owned(),
                prior_record: Box::new(prior_record.clone()),
            })
            .expect("single reuse marker");

        let disposition =
            RetainedAutomationRun::new(Ok(()), settlement_guard).into_settlement_disposition();
        let RetainedAutomationSettlementDisposition::ReusedSchedulerSkip { reused, .. } =
            disposition
        else {
            panic!("retained scheduler repeat must not expose a current result")
        };
        assert_eq!(reused.requested_run_id, "current_scheduler_run");
        assert_eq!(reused.task_key, "session_reflector");
        assert_eq!(reused.reason, "interval_not_elapsed");
        assert_eq!(*reused.prior_record, prior_record);
    }

    #[tokio::test]
    async fn retained_settlement_guard_owns_task_lock_until_drop() {
        let root = tempfile::tempdir().expect("temporary dashboard root");
        let settlement_guard = AutomationRunSettlementGuard::new();
        let retention = settlement_guard.retention();
        let task_lock = AutomationTaskLock::try_acquire(
            root.path(),
            AgentTaskKind::SessionReflector,
            Some(3_600),
            current_timestamp(),
        )
        .await
        .expect("first lock acquisition")
        .expect("first lock");
        assert!(
            retain_task_lock(task_lock, Some(&retention))
                .expect("retain lock")
                .is_none()
        );
        drop(retention);
        let retained_run = RetainedAutomationRun::<()>::new(
            Err(AutomationRunError::Runtime(TraceDecayError::Config {
                message: "runner failed after acquiring its task lock".to_owned(),
            })),
            settlement_guard,
        );
        let (result, settlement_guard) = retained_run.into_parts();
        assert!(matches!(result, Err(AutomationRunError::Runtime(_))));

        assert!(
            AutomationTaskLock::try_acquire(
                root.path(),
                AgentTaskKind::SessionReflector,
                Some(3_600),
                current_timestamp(),
            )
            .await
            .expect("competing lock acquisition")
            .is_none()
        );

        drop(settlement_guard);
        assert!(
            AutomationTaskLock::try_acquire(
                root.path(),
                AgentTaskKind::SessionReflector,
                Some(3_600),
                current_timestamp(),
            )
            .await
            .expect("post-settlement lock acquisition")
            .is_some()
        );
    }

    #[test]
    fn recorded_failure_exposes_only_its_constructed_terminal() {
        let config = AutomationConfig::default();
        let finalizer = AgentRunFinalizer::new_at(
            Path::new("/unused"),
            "recorded_failure_run",
            AutomationTrigger::Dashboard,
            &config,
            AgentTaskKind::SessionReflector,
            "0",
            None,
            std::time::SystemTime::UNIX_EPOCH,
        )
        .expect("test finalizer");
        let ledger_record = finalizer
            .record(RunRecordOutcome {
                model: None,
                status: AutomationRunStatus::Failed,
                evidence_hash: None,
                proposed_ops: None,
                accepted_count: 0,
                rejected_count: 0,
                error: Some("failed".to_owned()),
            })
            .expect("test ledger record");
        let recorded = AutomationRunError::RecordedFailure {
            error: TraceDecayError::Config {
                message: "failed".to_owned(),
            },
            ledger_record: Box::new(ledger_record),
        };
        let runtime = AutomationRunError::Runtime(TraceDecayError::Config {
            message: "failed before terminal construction".to_owned(),
        });

        assert_eq!(
            recorded
                .ledger_record()
                .map(|record| record.run_id.as_str()),
            Some("recorded_failure_run")
        );
        assert!(runtime.ledger_record().is_none());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

use std::io::{Read, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::Dir;
#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use cap_std::time::SystemClock;
use serde::{Deserialize, Serialize};
use tracedecay_automation::config::validate_schedule as validate_leaf_schedule;
pub use tracedecay_automation::config::{AutomationSchedule, CronSchedule, parse_schedule};
use tracedecay_automation::evidence_budget::{
    SESSION_EVIDENCE_BUDGET_EXHAUSTED, SESSION_EVIDENCE_BUDGET_SUPPRESSED,
    SessionEvidenceBudgetBackoff, SessionEvidenceBudgetExceeded, SessionEvidenceBudgetGate,
};

use super::backend::{AgentTaskKind, agent_task_failure_disposition, task_key};
use super::config::{
    AutomationBackend, AutomationConfig, AutomationHostMode, AutomationTaskConfig,
};
use super::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    canonical_record_started_at_seconds, latest_record_by_canonical_completion,
    latest_record_by_canonical_completion_key,
};
use crate::errors::{Result, TraceDecayError};
use crate::ports::session_store::AutomationSessionStore;

const DEFAULT_FAILURE_COOLDOWN_SECS: u64 = 300;
const DEFAULT_STALE_LOCK_SECS: u64 = 6 * 60 * 60;
const AUTOMATION_TASK_LOCK_TOKEN_BYTES: usize = 32;
const MAX_AUTOMATION_TASK_LOCK_BYTES: u64 = 1024;
const AUTOMATION_TASK_LOCK_CLEANUP_ATTEMPTS: u32 = 3;
const SCHEDULER_CONTROL_FILENAME: &str = "automation_scheduler_control.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AutomationSchedulerControl {
    #[serde(default)]
    pub paused: bool,
}

/// Most recent LCM session ingest activity for the project, in unix seconds.
///
/// `None` means the session store does not exist yet or holds no timestamped
/// messages; gates that need an activity signal treat that as "no activity
/// observed" rather than an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionActivity {
    pub last_activity_secs: Option<i64>,
}

impl SessionActivity {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn at(last_activity_secs: i64) -> Self {
        Self {
            last_activity_secs: Some(last_activity_secs),
        }
    }
}

/// Reads the session-activity signal from the exact registered LCM session shard.
///
/// This reads from the read-only store using bounded indexed timestamp lookups,
/// so it is cheap and race-safe to call from every scheduler tick; concurrent
/// ingest writers only ever move the value forward.
pub async fn load_session_activity(sessions_db: &dyn AutomationSessionStore) -> SessionActivity {
    SessionActivity {
        last_activity_secs: sessions_db.latest_session_activity_secs().await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomationScheduleDecision {
    skip_reason: Option<&'static str>,
}

impl AutomationScheduleDecision {
    pub fn due() -> Self {
        Self { skip_reason: None }
    }

    pub fn skipped(reason: &'static str) -> Self {
        Self {
            skip_reason: Some(reason),
        }
    }

    pub fn skip_reason(&self) -> Option<&'static str> {
        self.skip_reason
    }

    pub fn is_due(&self) -> bool {
        self.skip_reason.is_none()
    }
}

#[derive(Debug)]
pub struct AutomationTaskLock {
    path: PathBuf,
    ownership_token: String,
    staging_path: Option<PathBuf>,
}

#[derive(Debug)]
enum TaskLockPublicationError {
    Definite(std::io::Error),
    CommitUncertain {
        error: std::io::Error,
        cleanup_owner: AutomationTaskLock,
    },
}

enum TaskLockStagingCreationError {
    Definite(std::io::Error),
    #[cfg(windows)]
    CommitUncertain {
        error: std::io::Error,
        staging_path: PathBuf,
    },
}

impl From<std::io::Error> for TaskLockPublicationError {
    fn from(error: std::io::Error) -> Self {
        Self::Definite(error)
    }
}

impl From<std::io::Error> for TaskLockStagingCreationError {
    fn from(error: std::io::Error) -> Self {
        Self::Definite(error)
    }
}

pub fn scheduler_control_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(SCHEDULER_CONTROL_FILENAME)
}

pub async fn load_scheduler_control(dashboard_root: &Path) -> Result<AutomationSchedulerControl> {
    let path = scheduler_control_path(dashboard_root);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to parse automation scheduler control '{}': {e}",
                path.display()
            ),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(AutomationSchedulerControl::default())
        }
        Err(e) => Err(TraceDecayError::Config {
            message: format!(
                "failed to read automation scheduler control '{}': {e}",
                path.display()
            ),
        }),
    }
}

pub async fn save_scheduler_control(
    dashboard_root: &Path,
    control: &AutomationSchedulerControl,
) -> Result<()> {
    let path = scheduler_control_path(dashboard_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to create automation scheduler control directory '{}': {e}",
                    parent.display()
                ),
            })?;
    }
    let bytes = serde_json::to_vec_pretty(control).map_err(|e| TraceDecayError::Config {
        message: format!("failed to encode automation scheduler control: {e}"),
    })?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to write automation scheduler control '{}': {e}",
                path.display()
            ),
        })
}

impl AutomationTaskLock {
    pub async fn try_acquire(
        dashboard_root: &Path,
        task: AgentTaskKind,
        stale_after_secs: Option<u64>,
        now_secs: i64,
    ) -> Result<Option<Self>> {
        Self::try_acquire_keyed(dashboard_root, task_key(task), stale_after_secs, now_secs).await
    }

    /// Acquires a lock under an arbitrary key. User-defined jobs lock per
    /// job (`user_job_<id>`) so concurrent jobs never serialize on the shared
    /// fixed-task lock name.
    pub async fn try_acquire_keyed(
        dashboard_root: &Path,
        key: &str,
        stale_after_secs: Option<u64>,
        now_secs: i64,
    ) -> Result<Option<Self>> {
        let lock_dir = dashboard_root.join("automation_locks");
        let path = lock_dir.join(format!("{key}.lock"));
        let ownership_token = new_automation_task_lock_token()?;
        let error_path = path.clone();
        tokio::task::spawn_blocking(move || {
            try_acquire_task_lock_blocking(&path, &ownership_token, stale_after_secs, now_secs)
        })
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("automation task-lock acquisition failed to join: {error}"),
        })?
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to acquire automation lock '{}': {error}",
                error_path.display()
            ),
        })
    }
}

/// Runs a synchronous task-lock cleanup body without starving the tokio worker
/// that happens to own the guard.
///
/// Task-lock release is deliberately synchronous: callers (and tests such as
/// `retained_settlement_guard_owns_task_lock_until_drop`) rely on the lock file
/// being gone the instant `drop` returns. The cleanup itself is genuinely
/// blocking though — an fs2 coordination lock, `sync_all`/parent-directory
/// fsyncs, and `std::thread::sleep` backoff between retries — and guards are
/// routinely dropped when an async fn's future completes on a runtime worker
/// (`_run_lock`, `_reflector_lock`, `_skill_lock`, `_task_lock`). Blocking
/// inline there stalls every unrelated task queued on that worker.
///
/// `block_in_place` lets tokio hand this worker's run queue to another thread
/// for the duration, but it panics outside a multi-thread runtime, so the
/// flavor is checked first. Empirically verified against tokio 1.53.1:
/// - multi-thread worker: `Handle` present, flavor `MultiThread`, offload works;
/// - `spawn_blocking` thread on a multi-thread runtime (the settlement-owner
///   pattern in `daemon::automation_effect`): `Handle` present, flavor
///   `MultiThread`, `block_in_place` is a no-op passthrough and does *not*
///   panic;
/// - current-thread runtime: `Handle` present, flavor `CurrentThread`,
///   `block_in_place` panics — hence the inline fallback;
/// - no runtime at all: no `Handle`, inline fallback.
///
/// The remaining `block_in_place` panic case is a `LocalSet` on a multi-thread
/// runtime; this workspace has none, and introducing one would need this guard
/// revisited.
fn run_blocking_cleanup(cleanup: impl FnOnce()) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(cleanup);
        }
        _ => cleanup(),
    }
}

impl Drop for AutomationTaskLock {
    fn drop(&mut self) {
        let path = &self.path;
        let ownership_token = &self.ownership_token;
        let staging_slot = &mut self.staging_path;
        run_blocking_cleanup(move || {
            match retry_exact_task_lock_cleanup(|| {
                remove_owned_task_lock_blocking(path, ownership_token)
            }) {
                Ok(()) => {
                    if let Some(staging_path) = staging_slot.take()
                        && let Err(error) = retry_exact_task_lock_cleanup(|| {
                            tracedecay_runtime_core::storage::PrivateStoreIo::remove_file_durable(
                                &staging_path,
                            )
                            .map(|_| ())
                        })
                    {
                        tracing::warn!(
                            path = %path.display(),
                            staging_path = %staging_path.display(),
                            error = %error,
                            "failed to retire exact automation task-lock staging ownership"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        staging_path = ?staging_slot.as_deref(),
                        error = %error,
                        "failed to release exact automation task-lock ownership; preserving retained staging evidence"
                    );
                }
            }
        });
    }
}

fn retry_exact_task_lock_cleanup<Operation>(mut operation: Operation) -> std::io::Result<()>
where
    Operation: FnMut() -> std::io::Result<()>,
{
    let mut attempt = 1_u32;
    loop {
        match operation() {
            Ok(()) => return Ok(()),
            Err(_) if attempt < AUTOMATION_TASK_LOCK_CLEANUP_ATTEMPTS => {
                std::thread::sleep(std::time::Duration::from_millis(5_u64 << (attempt - 1)));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn schedule_decision(
    config: &AutomationConfig,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
) -> AutomationScheduleDecision {
    schedule_decision_or_history_denial(config, task, records, activity, now_secs, true)
}

pub fn host_receipt_decision(
    config: &AutomationConfig,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
) -> AutomationScheduleDecision {
    schedule_decision_or_history_denial(config, task, records, activity, now_secs, false)
}

fn schedule_decision_or_history_denial(
    config: &AutomationConfig,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
    enforce_schedule: bool,
) -> AutomationScheduleDecision {
    match schedule_decision_for_trigger(config, task, records, activity, now_secs, enforce_schedule)
    {
        Ok(decision) => decision,
        Err(_) => AutomationScheduleDecision::skipped("scheduler_history_invalid"),
    }
}

fn schedule_decision_for_trigger(
    config: &AutomationConfig,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
    enforce_schedule: bool,
) -> Result<AutomationScheduleDecision> {
    if !config.enabled {
        return Ok(AutomationScheduleDecision::skipped("automation_disabled"));
    }
    if config.host_mode == AutomationHostMode::DelegatedHost {
        return Ok(AutomationScheduleDecision::skipped("delegated_host_mode"));
    }
    if config.backend == AutomationBackend::Disabled {
        return Ok(AutomationScheduleDecision::skipped("backend_disabled"));
    }
    let Some(task_config) = task_config(config, task) else {
        return Ok(AutomationScheduleDecision::skipped("task_not_schedulable"));
    };
    if !task_config.enabled {
        return Ok(AutomationScheduleDecision::skipped("task_disabled"));
    }

    let (interval_secs, cron) = if enforce_schedule {
        let Ok(schedule) = parse_schedule(task_config.schedule.as_deref()) else {
            return Ok(AutomationScheduleDecision::skipped(
                "scheduler_schedule_invalid",
            ));
        };
        let timing = match schedule {
            AutomationSchedule::Manual => {
                return Ok(AutomationScheduleDecision::skipped(
                    "scheduler_schedule_manual",
                ));
            }
            AutomationSchedule::ConfiguredInterval => (task_config.interval_secs, None),
            AutomationSchedule::Interval { every_secs } => (Some(every_secs), None),
            AutomationSchedule::Cron(cron) => (None, Some(cron)),
        };
        if timing.0.is_none() && timing.1.is_none() {
            return Ok(AutomationScheduleDecision::skipped(
                "scheduler_schedule_manual",
            ));
        }
        timing
    } else {
        (None, None)
    };

    // `min_idle_secs` is a true idle window: the project must have been quiet
    // (no LCM session ingest activity) for at least this long. An unknown
    // activity signal (no session store yet) counts as idle.
    if let Some(min_idle_secs) = task_config.min_idle_secs
        && let Some(last_activity) = activity.last_activity_secs
        && elapsed_secs(last_activity, now_secs) < min_idle_secs
    {
        return Ok(AutomationScheduleDecision::skipped(
            "scheduler_idle_window_active",
        ));
    }

    let latest_successful = latest_successful_record(records, task)?;

    // Session-evidence tasks are event-driven across every supported host.
    // Cursor, Claude, Codex, and Hermes all ingest completed-turn evidence
    // into the project session store; a newer activity watermark should wake
    // reflection as soon as the idle window closes instead of waiting for the
    // periodic interval. The interval remains the repair/backstop schedule.
    let fresh_session_activity = task_consumes_session_evidence(task)
        && latest_successful
            .map(|(record, _)| parse_started_at(record))
            .transpose()?
            .is_some_and(|started_at| {
                activity
                    .last_activity_secs
                    .is_some_and(|last_activity| last_activity > started_at)
            });

    // Budget exhaustion and failure cooldown are independent typed states.
    // Evaluate the live budget anchor first so an older failed run whose
    // ordinary cooldown has elapsed cannot bypass a still-active evidence
    // backoff window. Suppressed ticks carry their own typed skip reason:
    // the exhausted-attempt label stays reserved for runs that actually ran
    // the retrieval, and the failure cooldown keeps its own label.
    if task_consumes_session_evidence(task)
        && let Some(exceeded) = live_session_evidence_budget_exhaustion(records, task)?
    {
        // Zero is rejected by settings validation and unrepresentable in the
        // typed contract; a value that somehow bypassed validation saturates
        // to the contract's default window rather than becoming "always try".
        let backoff = task_config
            .session_evidence_budget_backoff_secs
            .and_then(NonZeroU64::new)
            .map_or_else(
                SessionEvidenceBudgetBackoff::default,
                SessionEvidenceBudgetBackoff::new,
            );
        if let SessionEvidenceBudgetGate::Suppressed { .. } = backoff.gate(exceeded, now_secs) {
            return Ok(AutomationScheduleDecision::skipped(
                SESSION_EVIDENCE_BUDGET_SUPPRESSED,
            ));
        }
    }

    if let Some((record, completed_at)) = latest_non_skipped_record(
        records,
        task,
        enforce_schedule.then_some(AutomationTrigger::Scheduler),
    )? {
        if record.status == AutomationRunStatus::Failed {
            let failure = agent_task_failure_disposition(
                record.error_classification,
                record.error_retryable,
                record.error.as_deref(),
            );
            if failure.is_non_retryable() {
                return Ok(AutomationScheduleDecision::skipped(
                    "scheduler_non_retryable_failure",
                ));
            }
            let cooldown_secs = task_config
                .cooldown_secs
                .unwrap_or(DEFAULT_FAILURE_COOLDOWN_SECS);
            if elapsed_secs(completed_at, now_secs) < cooldown_secs {
                return Ok(AutomationScheduleDecision::skipped(
                    "scheduler_cooldown_active",
                ));
            }
            return Ok(AutomationScheduleDecision::due());
        }
        if let Some(interval_secs) = interval_secs.filter(|_| !fresh_session_activity)
            && elapsed_secs(completed_at, now_secs) < interval_secs
        {
            return Ok(AutomationScheduleDecision::skipped(
                "scheduler_interval_not_elapsed",
            ));
        }
        if let Some(cron) = cron.filter(|_| !fresh_session_activity)
            && !cron_is_due(&cron, Some(completed_at), now_secs)
        {
            return Ok(AutomationScheduleDecision::skipped(
                "scheduler_cron_not_due",
            ));
        }
    } else if let Some(cron) = cron
        && !cron_is_due(&cron, None, now_secs)
    {
        return Ok(AutomationScheduleDecision::skipped(
            "scheduler_cron_not_due",
        ));
    }

    // Session-evidence tasks only re-run when new session activity landed
    // after their last successful run started; a run without fresh evidence
    // would re-review the same transcript slices. Skips do not consume the
    // interval clock, so the task fires on the first tick after new activity.
    if task_consumes_session_evidence(task)
        && let Some((record, _)) = latest_successful
    {
        let started_at = parse_started_at(record)?;
        let has_new_activity = activity
            .last_activity_secs
            .is_some_and(|last_activity| last_activity > started_at);
        if !has_new_activity {
            return Ok(AutomationScheduleDecision::skipped(
                "no_new_session_activity",
            ));
        }
    }

    Ok(AutomationScheduleDecision::due())
}

/// The standing session-evidence budget-exhausted state for `task`, if any.
///
/// The state anchors on the most recent skip that actually attempted the
/// retrieval and observed exhaustion. An effectful run (success or failure)
/// completing at or after that attempt supersedes the state: retrieval
/// demonstrably ran again, so the scheduler must not keep backing off from a
/// stale anchor.
fn live_session_evidence_budget_exhaustion(
    records: &[AutomationRunLedgerRecord],
    task: AgentTaskKind,
) -> Result<Option<SessionEvidenceBudgetExceeded>> {
    let Some((_, exhausted_key)) =
        latest_record_by_canonical_completion_key(records.iter().filter(|record| {
            record.task == task
                && record.status == AutomationRunStatus::Skipped
                && record.error.as_deref() == Some(SESSION_EVIDENCE_BUDGET_EXHAUSTED)
        }))?
    else {
        return Ok(None);
    };
    let effectful = latest_record_by_canonical_completion_key(records.iter().filter(|record| {
        record.task == task
            && matches!(
                record.status,
                AutomationRunStatus::Succeeded | AutomationRunStatus::Failed
            )
    }))?;
    if effectful.is_some_and(|(_, effectful_key)| effectful_key >= exhausted_key) {
        return Ok(None);
    }
    Ok(Some(SessionEvidenceBudgetExceeded {
        observed_at_secs: exhausted_key.0,
    }))
}

/// Tasks whose evidence comes from the LCM session store; they are gated on
/// new session activity since their last successful run.
fn task_consumes_session_evidence(task: AgentTaskKind) -> bool {
    match task {
        AgentTaskKind::SessionReflector
        | AgentTaskKind::SkillWriter
        | AgentTaskKind::CombinedReview => true,
        AgentTaskKind::MemoryCurator | AgentTaskKind::UserJob => false,
    }
}

/// A cron schedule is due when a matching wall-clock minute has occurred
/// since the last completed run (or at all, when there is no prior run).
pub fn cron_is_due(cron: &CronSchedule, last_completed_at: Option<i64>, now_secs: i64) -> bool {
    match cron.previous_occurrence(now_secs) {
        Some(occurrence) => last_completed_at.is_none_or(|completed| occurrence > completed),
        None => false,
    }
}

pub fn stale_lock_secs(config: &AutomationConfig, task: AgentTaskKind) -> Option<u64> {
    task_config(config, task)
        .and_then(|task_config| task_config.stale_lock_secs)
        .or(Some(DEFAULT_STALE_LOCK_SECS))
}

pub fn validate_schedule(schedule: Option<&str>) -> Result<()> {
    Ok(validate_leaf_schedule(schedule)?)
}

/// User jobs carry their own schedule/enabled state (see
/// `automation::jobs`), so the fixed-task config lookup falls back to a
/// disabled default that makes the fixed-task gates skip them.
const USER_JOB_TASK_CONFIG: AutomationTaskConfig = AutomationTaskConfig {
    enabled: false,
    schedule: None,
    interval_secs: None,
    cooldown_secs: None,
    min_idle_secs: None,
    stale_lock_secs: None,
    session_evidence_budget_backoff_secs: None,
};

fn task_config(config: &AutomationConfig, task: AgentTaskKind) -> Option<&AutomationTaskConfig> {
    match task {
        AgentTaskKind::MemoryCurator => Some(&config.tasks.memory_curator),
        AgentTaskKind::SessionReflector => Some(&config.tasks.session_reflector),
        AgentTaskKind::SkillWriter => Some(&config.tasks.skill_writer),
        // The combined review has no schedule of its own; it is dispatched
        // when the two per-task schedules are both due in the same tick.
        AgentTaskKind::CombinedReview => None,
        AgentTaskKind::UserJob => Some(&USER_JOB_TASK_CONFIG),
    }
}

fn latest_successful_record(
    records: &[AutomationRunLedgerRecord],
    task: AgentTaskKind,
) -> Result<Option<(&AutomationRunLedgerRecord, i64)>> {
    latest_record_by_canonical_completion(
        records.iter().filter(|record| {
            record.task == task && record.status == AutomationRunStatus::Succeeded
        }),
    )
}

fn latest_non_skipped_record(
    records: &[AutomationRunLedgerRecord],
    task: AgentTaskKind,
    trigger: Option<AutomationTrigger>,
) -> Result<Option<(&AutomationRunLedgerRecord, i64)>> {
    latest_record_by_canonical_completion(records.iter().filter(|record| {
        record.task == task
            && trigger.is_none_or(|trigger| record.trigger == trigger)
            && matches!(
                record.status,
                AutomationRunStatus::Succeeded | AutomationRunStatus::Failed
            )
    }))
}

fn parse_started_at(record: &AutomationRunLedgerRecord) -> Result<i64> {
    canonical_record_started_at_seconds(record, &format!("run '{}' started_at", record.run_id))
}

fn elapsed_secs(completed_at: i64, now_secs: i64) -> u64 {
    if now_secs < completed_at {
        return 0;
    }
    (now_secs - completed_at) as u64
}

fn new_automation_task_lock_token() -> Result<String> {
    let mut random = [0_u8; AUTOMATION_TASK_LOCK_TOKEN_BYTES];
    getrandom::getrandom(&mut random).map_err(|error| TraceDecayError::Config {
        message: format!("failed to generate automation task-lock ownership token: {error}"),
    })?;
    Ok(hex::encode(random))
}

fn try_acquire_task_lock_blocking(
    path: &Path,
    ownership_token: &str,
    stale_after_secs: Option<u64>,
    now_secs: i64,
) -> std::io::Result<Option<AutomationTaskLock>> {
    if let Some(parent) = path.parent() {
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(parent)?;
    }
    let coordination = acquire_task_lock_coordination(path)?;
    for attempt in 0..2 {
        match create_task_lock_file(path, ownership_token, now_secs) {
            Ok(task_lock) => return Ok(Some(task_lock)),
            Err(TaskLockPublicationError::Definite(error))
                if error.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                let Some(snapshot) = read_task_lock_snapshot(path)? else {
                    continue;
                };
                if attempt == 0 && task_lock_is_reclaimable(&snapshot, stale_after_secs, now_secs) {
                    tracedecay_runtime_core::storage::PrivateStoreIo::remove_file_durable(path)?;
                    continue;
                }
                return Ok(None);
            }
            Err(TaskLockPublicationError::Definite(error)) => return Err(error),
            Err(TaskLockPublicationError::CommitUncertain {
                error,
                cleanup_owner,
            }) => {
                drop(coordination);
                drop(cleanup_owner);
                return Err(error);
            }
        }
    }
    Ok(None)
}

fn create_task_lock_file(
    path: &Path,
    ownership_token: &str,
    now_secs: i64,
) -> std::result::Result<AutomationTaskLock, TaskLockPublicationError> {
    let (task_lock, staging_path) = publish_task_lock_file(path, ownership_token, now_secs)?;
    let cleanup_result =
        tracedecay_runtime_core::storage::PrivateStoreIo::remove_file_durable(&staging_path)
            .map(|_| ());
    let sync_result = tracedecay_private_fs::framed_log::sync_parent_directory(
        path,
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::Strict,
    );
    Ok(task_lock.settle_published_staging(&staging_path, cleanup_result, sync_result))
}

#[cfg(test)]
fn create_task_lock_file_with_post_publish<Cleanup, SyncParent>(
    path: &Path,
    ownership_token: &str,
    now_secs: i64,
    cleanup_published_staging: Cleanup,
    sync_published_parent: SyncParent,
) -> std::result::Result<AutomationTaskLock, TaskLockPublicationError>
where
    Cleanup: FnOnce(&Path) -> std::io::Result<()>,
    SyncParent: FnOnce(&Path) -> std::io::Result<()>,
{
    let (task_lock, staging_path) = publish_task_lock_file(path, ownership_token, now_secs)?;
    let cleanup_result = cleanup_published_staging(&staging_path);
    let sync_result = sync_published_parent(path);
    Ok(task_lock.settle_published_staging(&staging_path, cleanup_result, sync_result))
}

fn publish_task_lock_file(
    path: &Path,
    ownership_token: &str,
    now_secs: i64,
) -> std::result::Result<(AutomationTaskLock, PathBuf), TaskLockPublicationError> {
    let prepared =
        prepare_task_lock_publication(path, ownership_token, now_secs).map_err(|failure| {
            match failure {
                TaskLockStagingCreationError::Definite(error) => {
                    TaskLockPublicationError::Definite(error)
                }
                #[cfg(windows)]
                TaskLockStagingCreationError::CommitUncertain {
                    error,
                    staging_path,
                } => {
                    let cleanup_error = cleanup_unpublished_task_lock_staging(
                        &staging_path,
                        "automation task-lock staging creation failed",
                        error,
                    );
                    TaskLockPublicationError::Definite(cleanup_error)
                }
            }
        })?;
    let link_result = prepared.parent.hard_link(
        &prepared.staging_name,
        &prepared.parent,
        &prepared.destination_name,
    );
    settle_task_lock_link_result(path, ownership_token, prepared.staging_path, link_result)
}

#[cfg(test)]
fn publish_task_lock_file_after_committed_link_error(
    path: &Path,
    ownership_token: &str,
    now_secs: i64,
) -> std::result::Result<(AutomationTaskLock, PathBuf), TaskLockPublicationError> {
    let prepared =
        prepare_task_lock_publication(path, ownership_token, now_secs).map_err(|failure| {
            match failure {
                TaskLockStagingCreationError::Definite(error) => {
                    TaskLockPublicationError::Definite(error)
                }
                #[cfg(windows)]
                TaskLockStagingCreationError::CommitUncertain {
                    error,
                    staging_path,
                } => TaskLockPublicationError::Definite(cleanup_unpublished_task_lock_staging(
                    &staging_path,
                    "automation task-lock staging creation failed",
                    error,
                )),
            }
        })?;
    prepared
        .parent
        .hard_link(
            &prepared.staging_name,
            &prepared.parent,
            &prepared.destination_name,
        )
        .map_err(TaskLockPublicationError::Definite)?;
    settle_task_lock_link_result(
        path,
        ownership_token,
        prepared.staging_path,
        Err(std::io::Error::other(
            "injected commit-uncertain hard-link result",
        )),
    )
}

struct PreparedTaskLockPublication {
    parent: Dir,
    destination_name: std::ffi::OsString,
    staging_name: std::ffi::OsString,
    staging_path: PathBuf,
}

fn prepare_task_lock_publication(
    path: &Path,
    ownership_token: &str,
    now_secs: i64,
) -> std::result::Result<PreparedTaskLockPublication, TaskLockStagingCreationError> {
    tracedecay_runtime_core::storage::reject_symlink_components(path, "automation task lock")?;
    let parent_path = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "automation task-lock path has no parent",
        )
    })?;
    let destination_name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "automation task-lock path has no file name",
            )
        })?;
    let parent = Dir::open_ambient_dir(parent_path, ambient_authority())?;
    let staging_path = task_lock_staging_path(path, ownership_token)?;
    let staging_name = staging_path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "automation task-lock staging path has no file name",
            )
        })?;
    tracedecay_runtime_core::storage::reject_symlink_components(
        &staging_path,
        "automation task-lock staging",
    )?;
    #[cfg(windows)]
    let mut file = match tracedecay_runtime_core::windows_security::create_private_file_retained(
        &staging_path,
    ) {
        Ok(file) => file,
        Err(error) => {
            return if staging_path.try_exists().unwrap_or(true) {
                Err(TaskLockStagingCreationError::CommitUncertain {
                    error: error.into_error(),
                    staging_path,
                })
            } else {
                Err(TaskLockStagingCreationError::Definite(error.into_error()))
            };
        }
    };
    #[cfg(not(windows))]
    let mut file = {
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .create_new(true)
            .write(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        options.mode(0o600);
        parent.open_with(&staging_name, &options)?
    };
    let payload = format!(
        "pid={}\ncreated_at={now_secs}\ntoken={ownership_token}\n",
        std::process::id()
    );
    let write_result = file
        .write_all(payload.as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(error) = write_result {
        drop(file);
        return Err(TaskLockStagingCreationError::Definite(
            cleanup_unpublished_task_lock_staging(
                &staging_path,
                "automation task-lock staging write failed",
                error,
            ),
        ));
    }
    drop(file);

    Ok(PreparedTaskLockPublication {
        parent,
        destination_name,
        staging_name,
        staging_path,
    })
}

fn settle_task_lock_link_result(
    path: &Path,
    ownership_token: &str,
    staging_path: PathBuf,
    link_result: std::io::Result<()>,
) -> std::result::Result<(AutomationTaskLock, PathBuf), TaskLockPublicationError> {
    let owned_task_lock = || AutomationTaskLock {
        path: path.to_path_buf(),
        ownership_token: ownership_token.to_owned(),
        staging_path: None,
    };
    match link_result {
        Ok(()) => Ok((owned_task_lock(), staging_path)),
        Err(link_error) => match read_task_lock_snapshot(path) {
            Ok(Some(snapshot)) if snapshot.ownership_token.as_deref() == Some(ownership_token) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %link_error,
                    "automation task-lock hard-link result was uncertain, but exact ownership is visible"
                );
                Ok((owned_task_lock(), staging_path))
            }
            Ok(_) => Err(TaskLockPublicationError::Definite(
                cleanup_unpublished_task_lock_staging(
                    &staging_path,
                    "automation task-lock publication failed",
                    link_error,
                ),
            )),
            Err(inspect_error) => {
                let mut cleanup_owner = owned_task_lock();
                cleanup_owner.staging_path = Some(staging_path);
                Err(TaskLockPublicationError::CommitUncertain {
                    error: std::io::Error::new(
                        inspect_error.kind(),
                        format!(
                            "automation task-lock hard-link result was uncertain: {link_error}; exact ownership inspection failed: {inspect_error}"
                        ),
                    ),
                    cleanup_owner,
                })
            }
        },
    }
}

fn task_lock_staging_path(path: &Path, ownership_token: &str) -> std::io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "automation task-lock path has no file name",
        )
    })?;
    let mut staging_name = std::ffi::OsString::from(".");
    staging_name.push(file_name);
    staging_name.push(format!(".{ownership_token}.tmp"));
    Ok(path.with_file_name(staging_name))
}

fn cleanup_unpublished_task_lock_staging(
    staging_path: &Path,
    context: &str,
    error: std::io::Error,
) -> std::io::Error {
    match tracedecay_runtime_core::storage::PrivateStoreIo::remove_file_durable(staging_path) {
        Ok(_) => std::io::Error::new(error.kind(), format!("{context}: {error}")),
        Err(cleanup_error) => std::io::Error::other(format!(
            "{context}: {error}; staging cleanup failed: {cleanup_error}"
        )),
    }
}

impl AutomationTaskLock {
    fn settle_published_staging(
        mut self,
        staging_path: &Path,
        cleanup_result: std::io::Result<()>,
        sync_result: std::io::Result<()>,
    ) -> Self {
        if let Err(error) = cleanup_result {
            tracing::warn!(
                path = %self.path.display(),
                staging_path = %staging_path.display(),
                error = %error,
                "automation task lock is owned, but its published staging link could not be retired"
            );
            self.staging_path = Some(staging_path.to_path_buf());
        }
        if let Err(error) = sync_result {
            tracing::warn!(
                path = %self.path.display(),
                error = %error,
                "automation task lock is owned, but its publication durability is uncertain"
            );
        }
        self
    }
}

fn remove_owned_task_lock_blocking(path: &Path, ownership_token: &str) -> std::io::Result<()> {
    let _coordination = acquire_task_lock_coordination(path)?;
    let Some(snapshot) = read_task_lock_snapshot(path)? else {
        return Ok(());
    };
    if snapshot.ownership_token.as_deref() != Some(ownership_token) {
        return Ok(());
    }
    tracedecay_runtime_core::storage::PrivateStoreIo::remove_file_durable(path).map(|_| ())
}

fn acquire_task_lock_coordination(path: &Path) -> std::io::Result<std::fs::File> {
    let coordination_path = tracedecay_runtime_core::storage::append_lock_path(path);
    tracedecay_runtime_core::storage::reject_symlink_components(
        &coordination_path,
        "automation task-lock coordination",
    )?;
    #[cfg(windows)]
    let file = tracedecay_runtime_core::windows_security::open_or_create_private_lock_file(
        &coordination_path,
    )?;
    #[cfg(not(windows))]
    let file = {
        let parent_path = coordination_path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "automation task-lock coordination path has no parent",
            )
        })?;
        let file_name = coordination_path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "automation task-lock coordination path has no file name",
            )
        })?;
        let parent = Dir::open_ambient_dir(parent_path, ambient_authority())?;
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        options.mode(0o600);
        let file = parent.open_with(file_name, &options)?.into_std();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file
    };
    fs2::FileExt::lock_exclusive(&file)?;
    file.sync_all()?;
    tracedecay_private_fs::framed_log::sync_parent_directory(
        &coordination_path,
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::Strict,
    )?;
    Ok(file)
}

struct AutomationTaskLockSnapshot {
    pid: Option<u32>,
    created_at: Option<i64>,
    ownership_token: Option<String>,
}

fn read_task_lock_snapshot(path: &Path) -> std::io::Result<Option<AutomationTaskLockSnapshot>> {
    tracedecay_runtime_core::storage::reject_symlink_components(path, "automation task lock")?;
    let parent_path = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "automation task-lock path has no parent",
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "automation task-lock path has no file name",
        )
    })?;
    let parent = Dir::open_ambient_dir(parent_path, ambient_authority())?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = match parent.open_with(file_name, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    let mut bytes = Vec::with_capacity(MAX_AUTOMATION_TASK_LOCK_BYTES as usize);
    (&mut file)
        .take(MAX_AUTOMATION_TASK_LOCK_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let contents = (bytes.len() as u64 <= MAX_AUTOMATION_TASK_LOCK_BYTES)
        .then(|| std::str::from_utf8(&bytes).ok())
        .flatten();
    let pid = contents
        .and_then(|contents| parse_unique_lock_field(contents, "pid="))
        .and_then(|value| value.parse::<u32>().ok());
    let payload_created_at = contents
        .and_then(|contents| parse_unique_lock_field(contents, "created_at="))
        .and_then(|value| value.parse::<i64>().ok());
    let ownership_token = contents
        .and_then(|contents| parse_unique_lock_field(contents, "token="))
        .filter(|value| valid_automation_task_lock_token(value))
        .map(str::to_owned);
    let created_at = payload_created_at.or_else(|| {
        metadata
            .modified()
            .ok()?
            .duration_since(SystemClock::UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
    });
    Ok(Some(AutomationTaskLockSnapshot {
        pid,
        created_at,
        ownership_token,
    }))
}

fn parse_unique_lock_field<'a>(contents: &'a str, prefix: &str) -> Option<&'a str> {
    let mut values = contents
        .lines()
        .filter_map(|line| line.strip_prefix(prefix).map(str::trim));
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn valid_automation_task_lock_token(token: &str) -> bool {
    token.len() == AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn task_lock_is_reclaimable(
    snapshot: &AutomationTaskLockSnapshot,
    stale_after_secs: Option<u64>,
    now_secs: i64,
) -> bool {
    let Some(stale_after_secs) = stale_after_secs else {
        return false;
    };
    match snapshot.pid {
        Some(pid) => match process_state(pid) {
            // A live (or unknown-liveness, kept conservative) owner still
            // holds the lock regardless of age.
            ProcessState::Live | ProcessState::Unknown => false,
            // A confirmed-dead owner's lock is reclaimable once it is stale.
            ProcessState::Dead => snapshot
                .created_at
                .is_some_and(|created_at| elapsed_secs(created_at, now_secs) >= stale_after_secs),
        },
        // The payload could not yield a pid (missing, oversized, non-UTF-8,
        // or duplicate `pid=` lines). Fall back to age-based staleness using
        // `created_at`, which `read_task_lock_snapshot` already backfills
        // from the file's mtime when the payload has no parseable
        // `created_at=` field.
        None => match snapshot.created_at {
            Some(created_at) => elapsed_secs(created_at, now_secs) >= stale_after_secs,
            // Crash-debris escape hatch: no parseable pid AND no readable
            // creation time (payload and mtime both unavailable) means the
            // lock can never be aged by any other path, so treat it as
            // reclaimable rather than permanently wedging the scheduler
            // tick behind garbage lock contents.
            None => true,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcessState {
    Live,
    Dead,
    Unknown,
}

fn process_state(pid: u32) -> ProcessState {
    if pid == std::process::id() {
        return ProcessState::Live;
    }
    if pid == 0 {
        return ProcessState::Unknown;
    }
    #[cfg(unix)]
    {
        const UNIX_EPERM: i32 = 1;
        const UNIX_ESRCH: i32 = 3;

        let Ok(pid) = i32::try_from(pid) else {
            return ProcessState::Unknown;
        };
        // SAFETY: `pid` is range-checked for the platform ABI and signal zero
        // performs an existence/permission probe without delivering a signal.
        if unsafe { unix_kill(pid, 0) } == 0 {
            return ProcessState::Live;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::PermissionDenied
            || error.raw_os_error() == Some(UNIX_EPERM)
        {
            ProcessState::Live
        } else if error.raw_os_error() == Some(UNIX_ESRCH) {
            ProcessState::Dead
        } else {
            ProcessState::Unknown
        }
    }
    #[cfg(windows)]
    {
        return windows_process_state(pid);
    }
    #[cfg(not(any(unix, windows)))]
    ProcessState::Unknown
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "kill"]
    fn unix_kill(pid: i32, signal: i32) -> i32;
}

#[cfg(windows)]
fn windows_process_state(pid: u32) -> ProcessState {
    use std::ffi::c_void;

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_INVALID_PARAMETER: i32 = 87;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "OpenProcess"]
        fn open_process(access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        #[link_name = "GetExitCodeProcess"]
        fn get_exit_code_process(process: *mut c_void, exit_code: *mut u32) -> i32;
        #[link_name = "CloseHandle"]
        fn close_handle(handle: *mut c_void) -> i32;
    }

    // SAFETY: `pid` already has the Win32 DWORD representation. The returned
    // handle is checked for null and closed exactly once below.
    let process = unsafe { open_process(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return if std::io::Error::last_os_error().raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
            ProcessState::Dead
        } else {
            ProcessState::Unknown
        };
    }
    let mut exit_code = 0_u32;
    // SAFETY: `process` is a live owned handle and `exit_code` is writable for
    // the duration of the call. `CloseHandle` consumes no Rust-owned memory.
    let read = unsafe { get_exit_code_process(process, &mut exit_code) };
    let _ = unsafe { close_handle(process) };
    if read == 0 {
        ProcessState::Unknown
    } else if exit_code == STILL_ACTIVE {
        ProcessState::Live
    } else {
        ProcessState::Dead
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::AutomationTaskSet;
    use super::*;
    use tempfile::tempdir;

    fn session_evidence_task_config() -> AutomationTaskConfig {
        AutomationTaskConfig {
            enabled: true,
            schedule: Some("interval".to_owned()),
            interval_secs: Some(60),
            ..AutomationTaskConfig::default()
        }
    }

    fn session_evidence_config() -> AutomationConfig {
        AutomationConfig {
            enabled: true,
            backend: AutomationBackend::CodexAppServer,
            tasks: AutomationTaskSet {
                session_reflector: session_evidence_task_config(),
                skill_writer: session_evidence_task_config(),
                ..AutomationTaskSet::default()
            },
            ..AutomationConfig::default()
        }
    }

    fn scheduler_ledger_record(
        run_id: &str,
        task: AgentTaskKind,
        status: AutomationRunStatus,
        error: Option<&str>,
        completed_at: i64,
    ) -> AutomationRunLedgerRecord {
        AutomationRunLedgerRecord {
            schema_version: 2,
            run_id: run_id.to_owned(),
            trigger: AutomationTrigger::Scheduler,
            task,
            task_key: None,
            backend: "codex_app_server".to_owned(),
            host_mode: None,
            prompt_version: None,
            response_schema: None,
            strict_json: None,
            model: None,
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
            skipped_count: 0,
            error: error.map(str::to_owned),
            error_classification: None,
            error_retryable: None,
            backend_attempt_count: 0,
            backend_attempts: Vec::new(),
            fallback_status: None,
            report_ref: None,
            artifacts: Vec::new(),
            started_at: completed_at.to_string(),
            completed_at: completed_at.to_string(),
            completed_at_micros: None,
        }
    }

    fn budget_exhausted_skip(
        run_id: &str,
        task: AgentTaskKind,
        completed_at: i64,
    ) -> AutomationRunLedgerRecord {
        scheduler_ledger_record(
            run_id,
            task,
            AutomationRunStatus::Skipped,
            Some(SESSION_EVIDENCE_BUDGET_EXHAUSTED),
            completed_at,
        )
    }

    #[test]
    fn budget_exhausted_skip_holds_the_task_in_a_typed_backoff_window() {
        let config = session_evidence_config();
        for task in [AgentTaskKind::SessionReflector, AgentTaskKind::SkillWriter] {
            let records = vec![budget_exhausted_skip("run-exhausted", task, 2_000)];

            // Every tick inside the window skips without a fresh attempt,
            // under the dedicated suppression reason: not the exhausted
            // label (no attempt ran) and not the failure cooldown.
            for now_secs in [2_060, 2_120, 2_000 + 3_599] {
                assert_eq!(
                    schedule_decision(
                        &config,
                        task,
                        &records,
                        SessionActivity::at(2_500),
                        now_secs
                    )
                    .skip_reason(),
                    Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED),
                    "tick at {now_secs} for {task:?} must stay suppressed"
                );
            }

            // The window elapsing permits exactly the next attempt.
            assert!(
                schedule_decision(
                    &config,
                    task,
                    &records,
                    SessionActivity::at(2_500),
                    2_000 + 3_600,
                )
                .is_due()
            );
        }
    }

    #[test]
    fn configured_budget_backoff_window_overrides_the_one_hour_default() {
        let mut config = session_evidence_config();
        config
            .tasks
            .session_reflector
            .session_evidence_budget_backoff_secs = Some(120);
        let records = vec![budget_exhausted_skip(
            "run-exhausted",
            AgentTaskKind::SessionReflector,
            2_000,
        )];

        assert_eq!(
            schedule_decision(
                &config,
                AgentTaskKind::SessionReflector,
                &records,
                SessionActivity::at(2_500),
                2_119,
            )
            .skip_reason(),
            Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED),
            "the configured 120s window must still suppress its final second"
        );
        assert!(
            schedule_decision(
                &config,
                AgentTaskKind::SessionReflector,
                &records,
                SessionActivity::at(2_500),
                2_120,
            )
            .is_due(),
            "the configured 120s window must end well before the 3600s default"
        );
    }

    #[test]
    fn budget_backoff_suppresses_host_receipt_triggers_too() {
        let config = session_evidence_config();
        let records = vec![budget_exhausted_skip(
            "run-exhausted",
            AgentTaskKind::SessionReflector,
            2_000,
        )];

        assert_eq!(
            host_receipt_decision(
                &config,
                AgentTaskKind::SessionReflector,
                &records,
                SessionActivity::at(2_500),
                2_060,
            )
            .skip_reason(),
            Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED)
        );
    }

    #[test]
    fn effectful_run_after_exhaustion_supersedes_the_backoff_anchor() {
        let config = session_evidence_config();
        let records = vec![
            budget_exhausted_skip("run-exhausted", AgentTaskKind::SessionReflector, 2_000),
            scheduler_ledger_record(
                "run-recovered",
                AgentTaskKind::SessionReflector,
                AutomationRunStatus::Succeeded,
                None,
                2_100,
            ),
        ];

        // Fresh session activity after the successful run makes the task due
        // again; the stale budget anchor must not keep suppressing it.
        assert!(
            schedule_decision(
                &config,
                AgentTaskKind::SessionReflector,
                &records,
                SessionActivity::at(2_500),
                2_130,
            )
            .is_due()
        );
    }

    #[test]
    fn older_failure_cooldown_cannot_bypass_a_live_budget_backoff() {
        let mut config = session_evidence_config();
        config.tasks.session_reflector.cooldown_secs = Some(60);
        let records = vec![
            scheduler_ledger_record(
                "run-failed",
                AgentTaskKind::SessionReflector,
                AutomationRunStatus::Failed,
                Some("provider unavailable"),
                1_900,
            ),
            budget_exhausted_skip("run-exhausted", AgentTaskKind::SessionReflector, 2_000),
        ];

        assert_eq!(
            schedule_decision(
                &config,
                AgentTaskKind::SessionReflector,
                &records,
                SessionActivity::at(2_500),
                2_100,
            )
            .skip_reason(),
            Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED)
        );

        // Without a live exhaustion anchor the failure state keeps its own
        // distinct cooldown reason: the two typed states never share a label.
        let failure_only = vec![scheduler_ledger_record(
            "run-failed",
            AgentTaskKind::SessionReflector,
            AutomationRunStatus::Failed,
            Some("timed out waiting for backend"),
            1_900,
        )];
        assert_eq!(
            schedule_decision(
                &config,
                AgentTaskKind::SessionReflector,
                &failure_only,
                SessionActivity::at(2_500),
                1_930,
            )
            .skip_reason(),
            Some("scheduler_cooldown_active")
        );
    }

    #[test]
    fn earlier_effectful_run_in_the_same_second_does_not_clear_exhaustion() {
        let config = session_evidence_config();
        let mut exhausted =
            budget_exhausted_skip("run-exhausted", AgentTaskKind::SessionReflector, 2_000);
        exhausted.completed_at_micros = Some(2_000_900_000);
        let mut earlier_success = scheduler_ledger_record(
            "run-success",
            AgentTaskKind::SessionReflector,
            AutomationRunStatus::Succeeded,
            None,
            2_000,
        );
        earlier_success.completed_at_micros = Some(2_000_100_000);

        assert_eq!(
            schedule_decision(
                &config,
                AgentTaskKind::SessionReflector,
                &[exhausted, earlier_success],
                SessionActivity::at(2_500),
                2_060,
            )
            .skip_reason(),
            Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED)
        );
    }

    #[test]
    fn suppression_skips_do_not_advance_the_backoff_anchor() {
        let config = session_evidence_config();
        // A suppression skip persisted after the exhausted attempt must not
        // extend the window: the anchor is the last real attempt.
        let records = vec![
            budget_exhausted_skip("run-exhausted", AgentTaskKind::SkillWriter, 2_000),
            scheduler_ledger_record(
                "run-suppressed",
                AgentTaskKind::SkillWriter,
                AutomationRunStatus::Skipped,
                Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED),
                2_060,
            ),
        ];

        assert_eq!(
            schedule_decision(
                &config,
                AgentTaskKind::SkillWriter,
                &records,
                SessionActivity::at(2_500),
                2_120,
            )
            .skip_reason(),
            Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED)
        );
        assert!(
            schedule_decision(
                &config,
                AgentTaskKind::SkillWriter,
                &records,
                SessionActivity::at(2_500),
                2_000 + 3_600,
            )
            .is_due()
        );
    }

    #[test]
    fn suppression_skips_alone_never_anchor_a_backoff_window() {
        let config = session_evidence_config();
        // A lone suppression skip (no exhausted attempt in the ledger)
        // carries no budget state of its own and must not suppress anything:
        // only real exhausted attempts anchor the window.
        let records = vec![scheduler_ledger_record(
            "run-suppressed",
            AgentTaskKind::SkillWriter,
            AutomationRunStatus::Skipped,
            Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED),
            2_000,
        )];

        assert!(
            schedule_decision(
                &config,
                AgentTaskKind::SkillWriter,
                &records,
                SessionActivity::at(2_500),
                2_060,
            )
            .is_due()
        );
    }

    #[test]
    fn other_evidence_skip_reasons_do_not_trigger_the_budget_backoff() {
        let config = session_evidence_config();
        let records = vec![scheduler_ledger_record(
            "run-stale",
            AgentTaskKind::SessionReflector,
            AutomationRunStatus::Skipped,
            Some("session_evidence_stale"),
            2_000,
        )];

        assert!(
            schedule_decision(
                &config,
                AgentTaskKind::SessionReflector,
                &records,
                SessionActivity::at(2_500),
                2_060,
            )
            .is_due()
        );
    }

    #[test]
    fn schedule_validation_maps_leaf_errors_at_the_runtime_boundary() {
        assert!(validate_schedule(Some("hourly")).is_ok());
        assert!(matches!(
            validate_schedule(Some("after lunch")),
            Err(TraceDecayError::Automation(_))
        ));
    }

    #[test]
    fn cron_admission_uses_the_leaf_schedule_type() {
        let AutomationSchedule::Cron(cron) = parse_schedule(Some("*/15 * * * *")).unwrap() else {
            panic!("expected cron schedule");
        };
        assert!(cron_is_due(&cron, Some(3_599), 3_600));
        assert!(!cron_is_due(&cron, Some(3_600), 3_600));
    }

    #[test]
    fn published_task_lock_uncertainty_returns_owner_and_drop_retries_staging() {
        let temp = tempdir().unwrap();
        let lock_dir = temp.path().join("automation_locks");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(&lock_dir)
            .unwrap();
        let lock_path = lock_dir.join("memory_curator.lock");
        let ownership_token = "a".repeat(AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2);
        let staging_path = task_lock_staging_path(&lock_path, &ownership_token).unwrap();
        let task_lock = create_task_lock_file_with_post_publish(
            &lock_path,
            &ownership_token,
            100,
            |_| {
                Err(std::io::Error::other(
                    "injected staging cleanup uncertainty",
                ))
            },
            |_| Err(std::io::Error::other("injected parent sync uncertainty")),
        )
        .unwrap();

        assert!(lock_path.exists());
        assert!(staging_path.exists());
        let contender_token = "b".repeat(AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2);
        assert!(
            try_acquire_task_lock_blocking(&lock_path, &contender_token, Some(10), 200)
                .unwrap()
                .is_none(),
            "publication uncertainty must still return a live token owner"
        );

        drop(task_lock);
        assert!(!lock_path.exists());
        assert!(
            !staging_path.exists(),
            "guard drop must retry its exact staging residue"
        );
    }

    #[test]
    fn exact_task_lock_cleanup_retries_before_abandoning_owner_evidence() {
        let mut attempts = 0_u32;
        retry_exact_task_lock_cleanup(|| {
            attempts += 1;
            if attempts < AUTOMATION_TASK_LOCK_CLEANUP_ATTEMPTS {
                Err(std::io::Error::other("injected transient cleanup failure"))
            } else {
                Ok(())
            }
        })
        .unwrap();

        assert_eq!(attempts, AUTOMATION_TASK_LOCK_CLEANUP_ATTEMPTS);
    }

    #[test]
    fn committed_hard_link_error_adopts_visible_exact_owner() {
        let temp = tempdir().unwrap();
        let lock_dir = temp.path().join("automation_locks");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(&lock_dir)
            .unwrap();
        let lock_path = lock_dir.join("session_reflector.lock");
        let ownership_token = "e".repeat(AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2);

        let (task_lock, staging_path) =
            publish_task_lock_file_after_committed_link_error(&lock_path, &ownership_token, 100)
                .unwrap();
        let cleanup_result =
            tracedecay_runtime_core::storage::PrivateStoreIo::remove_file_durable(&staging_path)
                .map(|_| ());
        let task_lock = task_lock.settle_published_staging(&staging_path, cleanup_result, Ok(()));

        assert_eq!(
            read_task_lock_snapshot(&lock_path)
                .unwrap()
                .unwrap()
                .ownership_token
                .as_deref(),
            Some(ownership_token.as_str()),
        );
        let contender_token = "f".repeat(AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2);
        assert!(
            try_acquire_task_lock_blocking(&lock_path, &contender_token, Some(10), 200)
                .unwrap()
                .is_none(),
            "a commit-uncertain hard link with visible exact ownership must deny contenders"
        );

        drop(task_lock);
        assert!(!lock_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn task_lock_coordination_rejects_final_symlink() {
        let temp = tempdir().unwrap();
        let lock_dir = temp.path().join("automation_locks");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(&lock_dir)
            .unwrap();
        let lock_path = lock_dir.join("memory_curator.lock");
        let coordination_path = tracedecay_runtime_core::storage::append_lock_path(&lock_path);
        let outside = temp.path().join("outside-lock-target");
        std::fs::write(&outside, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&outside, &coordination_path).unwrap();

        let error = try_acquire_task_lock_blocking(
            &lock_path,
            &"1".repeat(AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2),
            Some(10),
            200,
        )
        .unwrap_err();

        assert!(!lock_path.exists());
        assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
        assert!(coordination_path.is_symlink());
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn uncertain_old_task_lock_guard_preserves_replacement_token() {
        let temp = tempdir().unwrap();
        let lock_dir = temp.path().join("automation_locks");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(&lock_dir)
            .unwrap();
        let lock_path = lock_dir.join("skill_writer.lock");
        let old_token = "c".repeat(AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2);
        let old_staging_path = task_lock_staging_path(&lock_path, &old_token).unwrap();
        let old_lock = create_task_lock_file_with_post_publish(
            &lock_path,
            &old_token,
            100,
            |_| {
                Err(std::io::Error::other(
                    "injected staging cleanup uncertainty",
                ))
            },
            |_| Ok(()),
        )
        .unwrap();
        tracedecay_runtime_core::storage::PrivateStoreIo::remove_file_durable(&lock_path).unwrap();

        let replacement_token = "d".repeat(AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2);
        let replacement = create_task_lock_file(&lock_path, &replacement_token, 200).unwrap();
        let replacement_record = std::fs::read_to_string(&lock_path).unwrap();

        drop(old_lock);
        assert_eq!(
            std::fs::read_to_string(&lock_path).unwrap(),
            replacement_record,
            "the uncertain prior guard must not unlink a newer owner token"
        );
        assert!(
            !old_staging_path.exists(),
            "the prior guard must retire only its exact staging residue"
        );

        drop(replacement);
        assert!(!lock_path.exists());
    }

    #[test]
    fn task_lock_is_reclaimable_with_unparseable_pid_and_stale_created_at() {
        let snapshot = AutomationTaskLockSnapshot {
            pid: None,
            created_at: Some(100),
            ownership_token: None,
        };
        assert!(
            task_lock_is_reclaimable(&snapshot, Some(10), 200),
            "an unparseable pid must fall back to created_at staleness"
        );
    }

    #[test]
    fn task_lock_is_reclaimable_with_unparseable_pid_and_fresh_created_at() {
        let snapshot = AutomationTaskLockSnapshot {
            pid: None,
            created_at: Some(195),
            ownership_token: None,
        };
        assert!(
            !task_lock_is_reclaimable(&snapshot, Some(10), 200),
            "a fresh created_at must not be reclaimable even without a pid"
        );
    }

    #[test]
    fn task_lock_is_reclaimable_with_no_pid_and_no_created_at_is_crash_debris() {
        let snapshot = AutomationTaskLockSnapshot {
            pid: None,
            created_at: None,
            ownership_token: None,
        };
        assert!(
            task_lock_is_reclaimable(&snapshot, Some(10), 200),
            "a lock with neither a parseable pid nor any created_at (payload \
             and mtime both unavailable) must not be permanently wedged"
        );
    }

    #[test]
    fn task_lock_is_reclaimable_denies_live_process_regardless_of_age() {
        let snapshot = AutomationTaskLockSnapshot {
            pid: Some(std::process::id()),
            created_at: Some(0),
            ownership_token: None,
        };
        assert!(
            !task_lock_is_reclaimable(&snapshot, Some(10), 200),
            "a live owner must never be reclaimed, no matter how old the lock is"
        );
    }

    #[test]
    fn task_lock_is_reclaimed_end_to_end_when_payload_is_garbage_and_stale_after_is_zero() {
        let temp = tempdir().unwrap();
        let lock_dir = temp.path().join("automation_locks");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(&lock_dir)
            .unwrap();
        let lock_path = lock_dir.join("garbage_payload.lock");

        // A payload larger than MAX_AUTOMATION_TASK_LOCK_BYTES (and not even
        // valid `key=value` text) means read_task_lock_snapshot can parse no
        // pid at all. With stale_after_secs = Some(0), the created_at
        // fallback (the file's own mtime) always qualifies as stale, so the
        // lock must still be reclaimable on the retry attempt inside
        // try_acquire_task_lock_blocking rather than wedging forever.
        let garbage = vec![0xFFu8; 2000];
        assert!(garbage.len() as u64 > MAX_AUTOMATION_TASK_LOCK_BYTES);
        std::fs::write(&lock_path, &garbage).unwrap();

        let contender_token = "9".repeat(AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2);
        let acquired = try_acquire_task_lock_blocking(&lock_path, &contender_token, Some(0), 200)
            .unwrap()
            .expect("a garbage-payload lock with stale_after_secs=0 must be reclaimable");

        let snapshot = read_task_lock_snapshot(&lock_path).unwrap().unwrap();
        assert_eq!(
            snapshot.ownership_token.as_deref(),
            Some(contender_token.as_str()),
            "the contender's fresh token must now own the lock"
        );

        drop(acquired);
        assert!(!lock_path.exists());
    }

    /// Acquires a real task lock off-runtime so each release-path test starts
    /// from the same live-owner state.
    fn acquire_lock_for_release_test(lock_path: &Path) -> AutomationTaskLock {
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(
            lock_path.parent().unwrap(),
        )
        .unwrap();
        try_acquire_task_lock_blocking(
            lock_path,
            &"c".repeat(AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2),
            Some(10),
            100,
        )
        .unwrap()
        .expect("a fresh lock path must be acquirable")
    }

    /// A contender must be able to take the lock the instant the guard's drop
    /// returns — the release stays synchronous even when it is offloaded.
    fn assert_lock_released(lock_path: &Path) {
        assert!(
            !lock_path.exists(),
            "guard drop must remove the lock file synchronously"
        );
        let contender = try_acquire_task_lock_blocking(
            lock_path,
            &"d".repeat(AUTOMATION_TASK_LOCK_TOKEN_BYTES * 2),
            Some(10),
            200,
        )
        .unwrap()
        .expect("a released lock must be immediately acquirable by a contender");
        drop(contender);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_lock_release_offloads_from_a_multi_thread_worker() {
        let temp = tempdir().unwrap();
        let lock_path = temp
            .path()
            .join("automation_locks")
            .join("multi_thread_worker.lock");

        // Acquire the way production does (spawn_blocking), then move the guard
        // back onto an async worker so its drop runs on the runtime.
        let acquire_path = lock_path.clone();
        let guard =
            tokio::task::spawn_blocking(move || acquire_lock_for_release_test(&acquire_path))
                .await
                .unwrap();
        assert!(lock_path.exists());

        // Drop directly on the async worker thread; block_in_place must let
        // tokio migrate this worker's queue instead of stalling it.
        drop(guard);
        assert_lock_released(&lock_path);
    }

    #[test]
    fn task_lock_release_works_with_no_tokio_runtime() {
        let temp = tempdir().unwrap();
        let lock_path = temp.path().join("automation_locks").join("no_runtime.lock");
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "this test must run without a runtime handle so the inline path is exercised"
        );

        let guard = acquire_lock_for_release_test(&lock_path);
        assert!(lock_path.exists());
        drop(guard);
        assert_lock_released(&lock_path);
    }

    #[tokio::test]
    async fn task_lock_release_falls_back_inline_on_a_current_thread_runtime() {
        let temp = tempdir().unwrap();
        let lock_path = temp
            .path()
            .join("automation_locks")
            .join("current_thread.lock");
        assert_eq!(
            tokio::runtime::Handle::current().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::CurrentThread,
            "#[tokio::test] must default to the current-thread flavor for this case"
        );

        // block_in_place panics on a current-thread runtime, so the guard has to
        // fall back to inline cleanup rather than aborting the process.
        let guard = acquire_lock_for_release_test(&lock_path);
        assert!(lock_path.exists());
        drop(guard);
        assert_lock_released(&lock_path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_lock_release_from_spawn_blocking_owner_does_not_panic() {
        let temp = tempdir().unwrap();
        let lock_path = temp
            .path()
            .join("automation_locks")
            .join("settlement_owner.lock");

        // The settlement-owner pattern in daemon::automation_effect acquires and
        // drops the guard entirely inside spawn_blocking. A blocking-pool thread
        // still reports a MultiThread handle, so this exercises block_in_place
        // from outside a worker context.
        let owned_path = lock_path.clone();
        tokio::task::spawn_blocking(move || {
            let guard = acquire_lock_for_release_test(&owned_path);
            assert!(owned_path.exists());
            drop(guard);
            assert!(!owned_path.exists());
        })
        .await
        .expect("dropping the guard inside spawn_blocking must not panic");

        assert_lock_released(&lock_path);
    }
}

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::Dir;
#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use serde::{Deserialize, Serialize};
pub use tracedecay_automation::config::{AutomationSchedule, CronSchedule, resolve_schedule};
use tracedecay_automation::evidence_budget::{
    SessionEvidenceBudgetBackoff, SessionEvidenceBudgetExceeded, SessionEvidenceBudgetGate,
};

use super::backend::{
    AgentTaskFailureClass, AgentTaskKind, agent_task_failure_disposition, task_key,
};
use super::backend_identity::{backend_identity, is_deterministic_failure_class};
use super::config::{
    AdmittedAutomationConfig, AutomationBackend, AutomationConfig, AutomationHostMode,
    AutomationTaskConfig,
};
use super::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    canonical_record_started_at_seconds, is_session_evidence_budget_exhausted_reason,
    latest_record_by_canonical_completion, latest_record_by_canonical_completion_key,
};
use tracedecay_contracts::retained_surfaces::AutomationSkipReasonV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_private_fs::FileLease;

const DEFAULT_FAILURE_COOLDOWN_SECS: u64 = 300;
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
/// ingest writers only ever move the value forward. A failed read is logged and
/// reported as no activity: a store the scheduler cannot read has no observable
/// new activity, so automation stays idle instead of running against it.
#[tracing::instrument(
    name = "automation.run.load_session_activity",
    level = "trace",
    skip_all
)]
pub async fn load_session_activity(sessions_db: &RegisteredGlobalDb) -> SessionActivity {
    let last_activity_secs = match sessions_db.latest_session_activity_secs().await {
        Ok(latest) => latest,
        Err(error) => {
            tracing::warn!(
                database = %sessions_db.db_path().display(),
                %error,
                "session-activity read failed; scheduler observes no new activity"
            );
            None
        }
    };
    SessionActivity { last_activity_secs }
}

/// Consecutive project-open failures after which one scheduler loop exits.
///
/// The next scheduler reconcile respawns the loop, so this bounds one futile
/// retry streak rather than retiring the automation lane.
pub const PROJECT_OPEN_FAILURE_ESCALATION: u32 = 6;

/// Longest delay between retries after a project-open failure.
pub const PROJECT_OPEN_BACKOFF_CEILING: std::time::Duration = std::time::Duration::from_mins(5);

/// Exponential project-open retry backoff, starting from one scheduler tick.
pub fn project_open_backoff(consecutive_failures: u32) -> std::time::Duration {
    let base = std::time::Duration::from_secs(super::config::DEFAULT_SCHEDULER_TICK_SECS);
    let steps = consecutive_failures.saturating_sub(1).min(16);
    base.saturating_mul(1_u32.checked_shl(steps).unwrap_or(u32::MAX))
        .min(PROJECT_OPEN_BACKOFF_CEILING)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomationScheduleDecision {
    skip_reason: Option<AutomationSkipReasonV1>,
    activity_gate: Option<SessionActivityGate>,
}

/// The two timestamps a `no_new_session_activity` skip compared, so a starved
/// gate is visible from the scheduler journal instead of looking idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionActivityGate {
    pub last_activity_secs: Option<i64>,
    pub cadence_started_at: Option<i64>,
}

impl AutomationScheduleDecision {
    pub fn due() -> Self {
        Self {
            skip_reason: None,
            activity_gate: None,
        }
    }

    pub fn skipped(reason: AutomationSkipReasonV1) -> Self {
        Self {
            skip_reason: Some(reason),
            activity_gate: None,
        }
    }

    pub const fn activity_gate(&self) -> Option<SessionActivityGate> {
        self.activity_gate
    }

    pub const fn skip_reason(&self) -> Option<AutomationSkipReasonV1> {
        self.skip_reason
    }

    pub const fn is_due(&self) -> bool {
        self.skip_reason.is_none()
    }
}

pub(crate) fn task_disabled_skip(task: AgentTaskKind) -> AutomationSkipReasonV1 {
    match task {
        AgentTaskKind::MemoryCurator => AutomationSkipReasonV1::MemoryCuratorDisabled,
        AgentTaskKind::SessionReflector => AutomationSkipReasonV1::SessionReflectorDisabled,
        AgentTaskKind::SkillWriter => AutomationSkipReasonV1::SkillWriterDisabled,
        AgentTaskKind::CombinedReview => AutomationSkipReasonV1::CombinedReviewDisabled,
        AgentTaskKind::UserJob => AutomationSkipReasonV1::UserJobDisabled,
    }
}

/// Exclusive admission for one automation task key. The task owns this value
/// for its whole lifetime, including retained effect settlement; dropping it
/// releases the cross-process lease.
#[derive(Debug)]
pub struct AutomationTaskLock {
    _lease: FileLease,
}

pub fn scheduler_control_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(SCHEDULER_CONTROL_FILENAME)
}

#[tracing::instrument(name = "automation.scheduler.load_control", level = "trace", skip_all)]
pub async fn load_scheduler_control(dashboard_root: &Path) -> Result<AutomationSchedulerControl> {
    let path = scheduler_control_path(dashboard_root);
    let read_path = path.clone();
    match crate::automation::blocking_fs(move || std::fs::read(read_path)).await {
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

#[tracing::instrument(name = "automation.scheduler.save_control", level = "trace", skip_all)]
pub async fn save_scheduler_control(
    dashboard_root: &Path,
    control: &AutomationSchedulerControl,
) -> Result<()> {
    let path = scheduler_control_path(dashboard_root);
    let bytes = serde_json::to_vec_pretty(control).map_err(|e| TraceDecayError::Config {
        message: format!("failed to encode automation scheduler control: {e}"),
    })?;
    let directory = dashboard_root.to_path_buf();
    crate::automation::blocking_fs(move || save_scheduler_control_blocking(&directory, &bytes))
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to write automation scheduler control '{}': {e}",
                path.display()
            ),
        })
}

fn save_scheduler_control_blocking(dashboard_root: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // Resolve existing OS aliases before the private-store authority validates
    // the path, while leaving the control file itself subject to its no-follow
    // publication checks. A new dashboard and every missing ancestor must be
    // durable before a pause can be acknowledged.
    let directory = tracedecay_runtime_core::path_safety::canonicalize_path_or_existing_parent(
        &std::path::absolute(dashboard_root)?,
    );
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(&directory)?;
    tracedecay_private_fs::framed_log::atomic_write(
        &scheduler_control_path(&directory),
        "scheduler-control",
        bytes,
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::Strict,
    )
}

impl AutomationTaskLock {
    pub async fn try_acquire(dashboard_root: &Path, task: AgentTaskKind) -> Result<Option<Self>> {
        Self::try_acquire_keyed(dashboard_root, task_key(task)).await
    }

    /// Acquires a lock under an arbitrary key. User-defined jobs lock per
    /// job (`user_job_<id>`) so concurrent jobs never serialize on the shared
    /// fixed-task lock name.
    #[tracing::instrument(name = "automation.scheduler.task_lock", level = "trace", skip_all)]
    pub async fn try_acquire_keyed(dashboard_root: &Path, key: &str) -> Result<Option<Self>> {
        // cap-std ambient opens walk each component. On macOS `/var` is a
        // firmlink to `/private/var`; opening the unresolved tempfile spelling
        // can ENOENT under concurrent create. Resolve the existing prefix first.
        let lock_dir = tracedecay_runtime_core::path_safety::canonicalize_path_or_existing_parent(
            &dashboard_root.join("automation_locks"),
        );
        let key = key.to_owned();
        let error_dir = lock_dir.clone();
        tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
            try_acquire_task_lock_blocking(&lock_dir, &key)
        })
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("automation task-lock acquisition failed to join: {error}"),
        })?
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to acquire automation lock under '{}': {error}",
                error_dir.display()
            ),
        })
    }
}

#[tracing::instrument(name = "automation.scheduler.decision", level = "trace", skip_all)]
/// Decides whether `task` is due under `config`, given the ledger `records`
/// and the executable the backend now in force would spawn
/// (`AgentTaskBackend::executable`); the latter is part of the backend
/// identity a settled deterministic failure is judged against.
pub fn schedule_decision(
    config: &AdmittedAutomationConfig,
    executable: Option<&Path>,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
) -> AutomationScheduleDecision {
    schedule_decision_or_history_denial(config, executable, task, records, activity, now_secs, true)
}

pub fn host_receipt_decision(
    config: &AdmittedAutomationConfig,
    executable: Option<&Path>,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
) -> AutomationScheduleDecision {
    schedule_decision_or_history_denial(
        config, executable, task, records, activity, now_secs, false,
    )
}

fn schedule_decision_or_history_denial(
    config: &AdmittedAutomationConfig,
    executable: Option<&Path>,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
    enforce_schedule: bool,
) -> AutomationScheduleDecision {
    match schedule_decision_for_trigger(
        config,
        executable,
        task,
        records,
        activity,
        now_secs,
        enforce_schedule,
    ) {
        Ok(decision) => decision,
        Err(_) => {
            AutomationScheduleDecision::skipped(AutomationSkipReasonV1::SchedulerHistoryInvalid)
        }
    }
}

fn schedule_decision_for_trigger(
    config: &AdmittedAutomationConfig,
    executable: Option<&Path>,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
    enforce_schedule: bool,
) -> Result<AutomationScheduleDecision> {
    if !config.enabled {
        return Ok(AutomationScheduleDecision::skipped(
            AutomationSkipReasonV1::AutomationDisabled,
        ));
    }
    if config.host_mode == AutomationHostMode::DelegatedHost {
        return Ok(AutomationScheduleDecision::skipped(
            AutomationSkipReasonV1::DelegatedHostMode,
        ));
    }
    if config.backend == AutomationBackend::Disabled {
        return Ok(AutomationScheduleDecision::skipped(
            AutomationSkipReasonV1::BackendDisabled,
        ));
    }
    let Some(task_config) = task_config(config, task) else {
        return Ok(AutomationScheduleDecision::skipped(
            AutomationSkipReasonV1::TaskNotSchedulable,
        ));
    };
    if !task_config.enabled {
        return Ok(AutomationScheduleDecision::skipped(task_disabled_skip(
            task,
        )));
    }

    let schedule = if enforce_schedule {
        match config.schedule(task) {
            Some(AutomationSchedule::Manual) => {
                return Ok(AutomationScheduleDecision::skipped(
                    AutomationSkipReasonV1::SchedulerScheduleManual,
                ));
            }
            Some(schedule) => Some(schedule),
            None => {
                return Ok(AutomationScheduleDecision::skipped(
                    AutomationSkipReasonV1::TaskNotSchedulable,
                ));
            }
        }
    } else {
        None
    };

    // `min_idle_secs` is a true idle window: the project must have been quiet
    // (no LCM session ingest activity) for at least this long. An unknown
    // activity signal (no session store yet) counts as idle.
    if let Some(min_idle_secs) = task_config.min_idle_secs
        && let Some(last_activity) = activity.last_activity_secs
        && elapsed_secs(last_activity, now_secs) < min_idle_secs
    {
        return Ok(AutomationScheduleDecision::skipped(
            AutomationSkipReasonV1::SchedulerIdleWindowActive,
        ));
    }

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
                AutomationSkipReasonV1::SessionEvidenceBudgetSuppressed,
            ));
        }
    }

    let latest_cadence = latest_cadence_terminal_record(
        records,
        task,
        enforce_schedule.then_some(AutomationTrigger::Scheduler),
    )?;
    let retryable_cadence_terminal = latest_cadence.is_some_and(|(record, _)| {
        record.status == AutomationRunStatus::Failed
            || skipped_terminal_failure_class(record)
                .is_some_and(AgentTaskFailureClass::is_retryable)
    });
    if let Some((record, completed_at)) = latest_cadence {
        if record.status == AutomationRunStatus::Failed {
            let failure =
                agent_task_failure_disposition(record.error_classification, record.error_retryable);
            // Identity-stand first: a deterministic failure stamped under the
            // current backend stays suppressed until that identity changes.
            match deterministic_backend_failure_standing(record, config, executable) {
                Ok(BackendFailureStanding::Stands) => {
                    return Ok(AutomationScheduleDecision::skipped(
                        AutomationSkipReasonV1::BackendIdentitySuppressed,
                    ));
                }
                Ok(BackendFailureStanding::IdentityChanged) => {
                    // The stamped backend/configuration is gone; the failure
                    // is not evidence about the identity now in force, so the
                    // task re-admits through the ordinary cooldown below.
                }
                Ok(BackendFailureStanding::NotSettled) => {
                    // Denied is configuration/policy state and uses the
                    // ordinary cooldown so a changed grant can recover without
                    // a backend identity stamp. Other non-retryable failures
                    // remain suppressed.
                    if failure.is_non_retryable()
                        && !matches!(failure.classification, Some(AgentTaskFailureClass::Denied))
                    {
                        return Ok(AutomationScheduleDecision::skipped(
                            AutomationSkipReasonV1::SchedulerNonRetryableFailure,
                        ));
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "failed to derive automation backend identity for standing-failure admission"
                    );
                    return Err(error);
                }
            }
        }
        if retryable_cadence_terminal {
            let cooldown_secs = task_config
                .cooldown_secs
                .unwrap_or(DEFAULT_FAILURE_COOLDOWN_SECS);
            if elapsed_secs(completed_at, now_secs) < cooldown_secs {
                return Ok(AutomationScheduleDecision::skipped(
                    AutomationSkipReasonV1::SchedulerCooldownActive,
                ));
            }
        }
    }

    // Session-evidence tasks are event-driven across every supported host.
    // Activity is fresh only relative to the latest terminal that consumed a
    // scheduling opportunity, including an effectful skip. That prevents an
    // older successful run from making every post-skip tick look fresh.
    let fresh_session_activity = task_consumes_session_evidence(task)
        && latest_cadence
            .map(|(record, _)| parse_started_at(record))
            .transpose()?
            .is_some_and(|started_at| {
                activity
                    .last_activity_secs
                    .is_some_and(|last_activity| last_activity > started_at)
            });

    if let Some((_, completed_at)) = latest_cadence {
        // A retryable terminal that has cleared its policy and cooldown gates
        // is due now; it does not wait for the ordinary interval.
        if !retryable_cadence_terminal && !fresh_session_activity {
            match schedule {
                Some(AutomationSchedule::Interval { every_secs })
                    if elapsed_secs(completed_at, now_secs) < every_secs =>
                {
                    return Ok(AutomationScheduleDecision::skipped(
                        AutomationSkipReasonV1::SchedulerIntervalNotElapsed,
                    ));
                }
                Some(AutomationSchedule::Cron(cron))
                    if !cron_is_due(&cron, Some(completed_at), now_secs) =>
                {
                    return Ok(AutomationScheduleDecision::skipped(
                        AutomationSkipReasonV1::SchedulerCronNotDue,
                    ));
                }
                _ => {}
            }
        }
    } else if let Some(AutomationSchedule::Cron(cron)) = schedule
        && !cron_is_due(&cron, None, now_secs)
    {
        return Ok(AutomationScheduleDecision::skipped(
            AutomationSkipReasonV1::SchedulerCronNotDue,
        ));
    }

    // Authority is required after failure policy/cooldown gates, so a task
    // cannot become due merely because its cooldown elapsed while the
    // canonical message-search activity signal is unavailable. Retryable
    // terminals may reuse existing evidence; successes and other effectful
    // skips require genuinely newer activity.
    if task_consumes_session_evidence(task) {
        let has_activity_authority = activity.last_activity_secs.is_some();
        let requires_fresh_activity = latest_cadence.is_some() && !retryable_cadence_terminal;
        if !has_activity_authority || (requires_fresh_activity && !fresh_session_activity) {
            return Ok(AutomationScheduleDecision {
                skip_reason: Some(AutomationSkipReasonV1::NoNewSessionActivity),
                activity_gate: Some(SessionActivityGate {
                    last_activity_secs: activity.last_activity_secs,
                    cadence_started_at: latest_cadence
                        .map(|(record, _)| parse_started_at(record))
                        .transpose()?,
                }),
            });
        }
    }

    Ok(AutomationScheduleDecision::due())
}

/// How a deterministic backend failure relates to the identity now in force.
enum BackendFailureStanding {
    /// Settled under the current backend identity: stays suppressed until
    /// that identity changes.
    Stands,
    /// Stamped under a different backend identity: the failure is not
    /// evidence about the identity now in force, so the task re-admits
    /// through the ordinary failure cooldown.
    IdentityChanged,
    /// Not identity-scoped evidence at all: unstamped, not a deterministic
    /// class, or the ladder did not reproduce a single class. The ordinary
    /// retry policy for the failure class applies.
    NotSettled,
}

/// Classify whether `record` is a settled deterministic backend failure that
/// still stands under the backend and configuration now in force.
///
/// Three facts must hold together for [`BackendFailureStanding::Stands`], and
/// each rules out a different kind of false positive:
///
/// 1. The failure class is deterministic under a fixed backend and
///    configuration, typed `Permanent` only. `Unavailable`, `Denied`,
///    `Disconnected`, `MalformedOutput`, `Timeout`, and `Retryable` keep the
///    ordinary failure cooldown.
/// 2. Every attempt the backend made failed that same way. A class that got
///    its retries and reproduced them all is settled; one that recovered
///    mid-ladder, or that never actually reached the backend, is not.
/// 3. The identity stamped on the record equals the identity now configured.
///    This is what makes the suppression self-clearing: change the backend,
///    the executable, any effective setting, or the build that owns the
///    protocol, and the task is re-admitted on the next tick.
///
/// A record with no stamped identity (written before the field existed) never
/// suppresses and never re-admits early: a failure that cannot be attributed
/// to any identity is judged by its class alone. Hasher errors stay `Err` so
/// callers cannot treat them as unstamped-legacy and silently re-admit.
fn deterministic_backend_failure_standing(
    record: &AutomationRunLedgerRecord,
    config: &AutomationConfig,
    executable: Option<&Path>,
) -> Result<BackendFailureStanding> {
    let Some(classification) = record.error_classification else {
        return Ok(BackendFailureStanding::NotSettled);
    };
    if !is_deterministic_failure_class(classification) {
        return Ok(BackendFailureStanding::NotSettled);
    }
    let Some(recorded_identity) = record.backend_identity.as_deref() else {
        return Ok(BackendFailureStanding::NotSettled);
    };
    if backend_identity(config, executable)? != recorded_identity {
        return Ok(BackendFailureStanding::IdentityChanged);
    }
    let ladder_reproduced_the_class = !record.backend_attempts.is_empty()
        && record.backend_attempts.iter().all(|attempt| {
            !attempt.succeeded && attempt.failure_classification == Some(classification)
        });
    Ok(if ladder_reproduced_the_class {
        BackendFailureStanding::Stands
    } else {
        BackendFailureStanding::NotSettled
    })
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
                && is_session_evidence_budget_exhausted_reason(record.error.as_deref())
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

/// Classifies effectful session-evidence skips through the same retry
/// authority used by failed backend attempts. Retrieval timeout is transient
/// and keeps the ordinary cooldown; cancellation is intentionally absent
/// because it remains an effectful skip awaiting fresh activity.
fn skipped_terminal_failure_class(
    record: &AutomationRunLedgerRecord,
) -> Option<AgentTaskFailureClass> {
    if record.status != AutomationRunStatus::Skipped || !task_consumes_session_evidence(record.task)
    {
        return None;
    }
    AutomationSkipReasonV1::from_ledger_reason(record.error.as_deref()?)?
        .is_retryable_retrieval_timeout()
        .then_some(AgentTaskFailureClass::Timeout)
}

/// A cron schedule is due when a matching wall-clock minute has occurred
/// since the last completed run (or at all, when there is no prior run).
pub fn cron_is_due(cron: &CronSchedule, last_completed_at: Option<i64>, now_secs: i64) -> bool {
    match cron.previous_occurrence(now_secs) {
        Some(occurrence) => last_completed_at.is_none_or(|completed| occurrence > completed),
        None => false,
    }
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
    legacy_stale_lock_secs: None,
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

fn latest_cadence_terminal_record(
    records: &[AutomationRunLedgerRecord],
    task: AgentTaskKind,
    trigger: Option<AutomationTrigger>,
) -> Result<Option<(&AutomationRunLedgerRecord, i64)>> {
    latest_record_by_canonical_completion(records.iter().filter(|record| {
        record.task == task
            && trigger.is_none_or(|trigger| record.trigger == trigger)
            && (matches!(
                record.status,
                AutomationRunStatus::Succeeded | AutomationRunStatus::Failed
            ) || (record.status == AutomationRunStatus::Skipped
                && !is_scheduler_diagnostic_skip(record.error.as_deref())))
    }))
}

/// Whether a skipped row records scheduler admission rather than an attempted
/// task terminal. Admission diagnostics must never move cadence: doing so
/// would make each `interval_not_elapsed`, disabled, or lock skip postpone the
/// next real attempt. Other settled skips did enter the task and consume its
/// evidence/review opportunity, so their completion starts the next interval.
fn is_scheduler_diagnostic_skip(reason: Option<&str>) -> bool {
    reason
        .and_then(AutomationSkipReasonV1::from_ledger_reason)
        .is_some_and(AutomationSkipReasonV1::is_cadence_diagnostic)
}

fn parse_started_at(record: &AutomationRunLedgerRecord) -> Result<i64> {
    canonical_record_started_at_seconds(record, &format!("run '{}' started_at", record.run_id))
}

pub(crate) fn elapsed_secs(completed_at: i64, now_secs: i64) -> u64 {
    if now_secs < completed_at {
        return 0;
    }
    (now_secs - completed_at) as u64
}

/// Holds the task lease through task settlement. The lease file is never
/// unlinked, so exclusion cannot split across inodes.
fn try_acquire_task_lock_blocking(
    lock_dir: &Path,
    key: &str,
) -> std::io::Result<Option<AutomationTaskLock>> {
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(lock_dir)?;
    let Some(lease) = open_task_lease(&lock_dir.join(format!("{key}.lease")))? else {
        return Ok(None);
    };
    Ok(Some(AutomationTaskLock { _lease: lease }))
}

fn open_task_lease(path: &Path) -> std::io::Result<Option<FileLease>> {
    tracedecay_runtime_core::storage::reject_symlink_components(path, "automation task lease")?;
    #[cfg(windows)]
    let file = tracedecay_runtime_core::windows_security::open_or_create_private_lock_file(path)?;
    #[cfg(not(windows))]
    let file = {
        let parent_path = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "automation task lease path has no parent",
            )
        })?;
        let file_name = path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "automation task lease path has no file name",
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
        // Concurrent acquirers race the first creation of this file; the
        // shared helper absorbs the spurious Darwin `ENOENT`.
        let file = tracedecay_private_fs::capability_dir::open_or_create_with(
            &parent, file_name, &options,
        )?
        .into_std();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(FileLease::held(file, "automation.task_lock"))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Confirmed-dead process, as opposed to a live or unreadable one.
///
/// Skill-overlay crash residue and other convergent startup scans use this
/// so they adopt a dead exporter's backup without stealing a live one's.
pub(crate) fn foreign_process_is_dead(pid: u32) -> bool {
    pid != std::process::id() && matches!(process_state(pid), ProcessState::Dead)
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
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::super::backend::{AgentTaskFailureClass, AgentTaskRetryAttempt};
    use super::super::backend_identity::BACKEND_IDENTITY_SUPPRESSED;
    use super::super::config::AutomationTaskSet;
    use super::*;
    use tempfile::tempdir;
    use tracedecay_automation::evidence_budget::{
        SESSION_EVIDENCE_BUDGET_EXHAUSTED, SESSION_EVIDENCE_BUDGET_SUPPRESSED,
    };
    use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, with_owned_temp_publish};
    #[cfg(unix)]
    use tracedecay_runtime_core::storage::with_durable_namespace_sync_fault_for_test;

    #[tokio::test]
    async fn scheduler_control_round_trips_atomically_and_keeps_corruption_typed() {
        let dir = tempdir().unwrap();
        assert!(!load_scheduler_control(dir.path()).await.unwrap().paused);
        for paused in [true, false, true] {
            save_scheduler_control(dir.path(), &AutomationSchedulerControl { paused })
                .await
                .unwrap();
            assert_eq!(
                load_scheduler_control(dir.path()).await.unwrap().paused,
                paused
            );
        }
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "staging files must be cleaned up");
        std::fs::write(scheduler_control_path(dir.path()), b"{").unwrap();
        assert!(matches!(
            load_scheduler_control(dir.path()).await,
            Err(TraceDecayError::Config { .. })
        ));
    }

    #[tokio::test]
    async fn scheduler_control_creates_private_missing_ancestors() {
        let root = tempdir().unwrap();
        let profile = root.path().join("profile");
        let dashboard = profile.join("dashboard");
        assert!(!dashboard.exists());

        save_scheduler_control(&dashboard, &AutomationSchedulerControl { paused: true })
            .await
            .unwrap();

        assert!(load_scheduler_control(&dashboard).await.unwrap().paused);
        tracedecay_private_fs::validate_private_directory(&profile).unwrap();
        tracedecay_private_fs::validate_private_directory(&dashboard).unwrap();
        assert_eq!(std::fs::read_dir(&dashboard).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn scheduler_control_does_not_publish_after_an_ancestor_sync_fails() {
        // The first barrier covers the existing prefix; the following two
        // publish the new profile and dashboard entries, respectively.
        for barrier in [2, 3] {
            let root = tempdir().unwrap();
            let dashboard = root.path().join("profile").join("dashboard");
            let bytes = br#"{"paused":true}"#;
            let error = with_durable_namespace_sync_fault_for_test(barrier, || {
                save_scheduler_control_blocking(&dashboard, bytes)
            })
            .expect_err("an uncommitted ancestor must prevent acknowledgement");

            assert_eq!(error.kind(), std::io::ErrorKind::Other);
            assert!(!scheduler_control_path(&dashboard).exists());
            save_scheduler_control_blocking(&dashboard, bytes).unwrap();
            assert_eq!(
                std::fs::read(scheduler_control_path(&dashboard)).unwrap(),
                bytes
            );
        }
    }

    #[tokio::test]
    async fn scheduler_control_readers_observe_complete_states_during_replacement() {
        let dir = tempdir().unwrap();
        save_scheduler_control(dir.path(), &AutomationSchedulerControl { paused: true })
            .await
            .unwrap();
        let ready = tokio::sync::Barrier::new(2);
        let finished = AtomicBool::new(false);
        let writer = async {
            ready.wait().await;
            let mut result = Ok(());
            for update in 0..64 {
                result = save_scheduler_control(
                    dir.path(),
                    &AutomationSchedulerControl {
                        paused: update % 2 != 0,
                    },
                )
                .await;
                if result.is_err() {
                    break;
                }
            }
            finished.store(true, Ordering::Release);
            result
        };
        let reader = async {
            assert!(load_scheduler_control(dir.path()).await.unwrap().paused);
            ready.wait().await;
            let mut reads = 0;
            while !finished.load(Ordering::Acquire) {
                load_scheduler_control(dir.path())
                    .await
                    .expect("a replacement must not expose empty or partial JSON");
                reads += 1;
            }
            reads
        };
        let (written, reads) = tokio::join!(writer, reader);
        written.unwrap();
        assert!(reads > 0, "the reader must overlap the update sequence");
        assert!(load_scheduler_control(dir.path()).await.unwrap().paused);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn scheduler_control_staging_failures_preserve_the_committed_pause() {
        let dir = tempdir().unwrap();
        save_scheduler_control(dir.path(), &AutomationSchedulerControl { paused: true })
            .await
            .unwrap();
        let path = scheduler_control_path(dir.path());
        let committed = std::fs::read(&path).unwrap();
        // Inject faults at the existing publication primitive used by
        // atomic_write, without adding a fault hook to the scheduler API.
        for fail_during_write in [true, false] {
            let error = with_owned_temp_publish(
                &path,
                "scheduler-control",
                |_, _| Err(std::io::Error::other("injected publication failure")),
                |staging| {
                    staging.write_all(b"{")?;
                    if fail_during_write {
                        Err(std::io::Error::other("injected partial-write failure"))
                    } else {
                        staging.write_all(b"\"paused\": false}")
                    }
                },
                DirectorySyncPolicy::Strict,
            )
            .expect_err("failed staging must be reported to the writer");
            assert_eq!(error.kind(), std::io::ErrorKind::Other);
            assert_eq!(std::fs::read(&path).unwrap(), committed);
            assert!(load_scheduler_control(dir.path()).await.unwrap().paused);
            assert_eq!(
                std::fs::read_dir(dir.path()).unwrap().count(),
                1,
                "failed publication must remove its owned staging file"
            );
        }
    }

    #[test]
    fn backoff_starts_at_one_tick_and_grows() {
        let tick =
            std::time::Duration::from_secs(super::super::config::DEFAULT_SCHEDULER_TICK_SECS);
        assert_eq!(project_open_backoff(1), tick);
        assert_eq!(project_open_backoff(2), tick * 2);
        assert_eq!(project_open_backoff(3), tick * 4);
    }

    #[test]
    fn backoff_is_capped_and_never_regresses() {
        let mut previous = std::time::Duration::ZERO;
        for attempt in 1..=64 {
            let backoff = project_open_backoff(attempt);
            assert!(backoff >= previous, "backoff must be monotonic");
            assert!(
                backoff <= PROJECT_OPEN_BACKOFF_CEILING,
                "backoff must stay under its ceiling"
            );
            previous = backoff;
        }
        assert_eq!(project_open_backoff(64), PROJECT_OPEN_BACKOFF_CEILING);
    }

    #[test]
    fn escalation_bounds_the_total_futile_retry_window() {
        let total = (1..=PROJECT_OPEN_FAILURE_ESCALATION)
            .map(project_open_backoff)
            .sum::<std::time::Duration>();
        let tick =
            std::time::Duration::from_secs(super::super::config::DEFAULT_SCHEDULER_TICK_SECS);
        assert!(
            total > tick,
            "escalation must allow more than one retry before exiting"
        );
        assert!(
            total <= std::time::Duration::from_hours(1),
            "a futile streak must not run for hours before escalating"
        );
    }

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
            backend_identity: None,
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
            session_evidence_budget_stage: None,
            report_ref: None,
            artifacts: Vec::new(),
            started_at: completed_at.to_string(),
            completed_at: completed_at.to_string(),
            completed_at_micros: None,
        }
    }

    /// A `memory_curator` config on a plain interval schedule: the task that
    /// burned the measured 26.9 runs/hour, and the one with no session-evidence
    /// gate in front of it.
    fn curator_config() -> AutomationConfig {
        AutomationConfig {
            enabled: true,
            backend: AutomationBackend::CodexAppServer,
            tasks: AutomationTaskSet {
                memory_curator: AutomationTaskConfig {
                    enabled: true,
                    schedule: Some("interval".to_owned()),
                    interval_secs: Some(60),
                    ..AutomationTaskConfig::default()
                },
                ..AutomationTaskSet::default()
            },
            ..AutomationConfig::default()
        }
    }

    /// One terminal failure whose retry ladder reproduced `classification` on
    /// every attempt, stamped with `identity`.
    fn settled_backend_failure(
        config: &AutomationConfig,
        executable: Option<&Path>,
        error: &str,
        classification: AgentTaskFailureClass,
        attempts: u32,
        completed_at: i64,
        identity: Option<String>,
    ) -> AutomationRunLedgerRecord {
        let mut record = scheduler_ledger_record(
            "run-settled",
            AgentTaskKind::MemoryCurator,
            AutomationRunStatus::Failed,
            Some(error),
            completed_at,
        );
        record.error_classification = Some(classification);
        record.error_retryable = Some(classification.is_retryable());
        record.backend_attempt_count = attempts as usize;
        record.backend_attempts = (1..=attempts)
            .map(|attempt| AgentTaskRetryAttempt {
                attempt,
                succeeded: false,
                failure_classification: Some(classification),
                backoff_millis: 0,
            })
            .collect();
        record.backend_identity = identity.or_else(|| backend_identity(config, executable).ok());
        record
    }

    /// The exact transport failure the measured ledger recorded 18 times.
    const DISCONNECT_ERROR: &str = "agent_task_backend port error: agent task backend \
disconnected: config error: codex app-server closed stdout before completing";

    #[test]
    fn deterministic_backend_failure_settles_once_and_never_relaunches() {
        let config = curator_config();
        let records = vec![settled_backend_failure(
            &config,
            None,
            PERMANENT_PROTOCOL_ERROR,
            AgentTaskFailureClass::Permanent,
            3,
            2_000,
            None,
        )];

        // The failure cooldown is the only thing that used to gate this, and
        // it elapses. Every tick after it, including a full day later, must
        // still refuse to spawn the backend again.
        for now_secs in [
            2_001,
            2_000 + DEFAULT_FAILURE_COOLDOWN_SECS as i64,
            2_000 + 10 * DEFAULT_FAILURE_COOLDOWN_SECS as i64,
            2_000 + 86_400,
        ] {
            assert_eq!(
                schedule_decision(
                    &config,
                    None,
                    AgentTaskKind::MemoryCurator,
                    &records,
                    SessionActivity::none(),
                    now_secs,
                )
                .skip_reason()
                .map(AutomationSkipReasonV1::as_str),
                Some(BACKEND_IDENTITY_SUPPRESSED),
                "tick at {now_secs} must stay settled, not relaunch",
            );
        }
    }

    #[test]
    fn an_unstamped_legacy_failure_keeps_the_ordinary_cooldown() {
        let config = curator_config();
        let mut record = settled_backend_failure(
            &config,
            None,
            DISCONNECT_ERROR,
            AgentTaskFailureClass::Disconnected,
            3,
            2_000,
            None,
        );
        record.backend_identity = None;
        let records = vec![record];

        assert!(
            schedule_decision(
                &config,
                None,
                AgentTaskKind::MemoryCurator,
                &records,
                SessionActivity::none(),
                2_000 + DEFAULT_FAILURE_COOLDOWN_SECS as i64,
            )
            .is_due(),
            "a failure that cannot be attributed to this identity is not \
evidence about it",
        );
    }

    const PERMANENT_PROTOCOL_ERROR: &str =
        "typed protocol contract violation: handshake rejected the turn";

    #[test]
    fn same_path_executable_replacement_readmits_a_permanent_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex-backend");
        std::fs::write(&path, b"backend-revision-one").unwrap();
        let executable = Some(path.as_path());
        let config = curator_config();
        let records = vec![settled_backend_failure(
            &config,
            executable,
            PERMANENT_PROTOCOL_ERROR,
            AgentTaskFailureClass::Permanent,
            3,
            2_000,
            None,
        )];
        let now_secs = 2_000 + DEFAULT_FAILURE_COOLDOWN_SECS as i64;
        assert_eq!(
            schedule_decision(
                &config,
                executable,
                AgentTaskKind::MemoryCurator,
                &records,
                SessionActivity::none(),
                now_secs,
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
            Some(BACKEND_IDENTITY_SUPPRESSED),
        );
        std::fs::write(&path, b"backend-revision-two-replaced").unwrap();
        assert!(
            schedule_decision(
                &config,
                executable,
                AgentTaskKind::MemoryCurator,
                &records,
                SessionActivity::none(),
                now_secs,
            )
            .is_due(),
            "replacing the same-path executable must re-admit the task",
        );
    }

    #[test]
    fn a_transient_backend_failure_still_retries_after_its_cooldown() {
        let config = curator_config();
        for (error, classification) in [
            (
                "agent task timed out: timed out waiting for codex app-server",
                AgentTaskFailureClass::Timeout,
            ),
            (
                "backend temporarily unavailable, try again later",
                AgentTaskFailureClass::Retryable,
            ),
            (
                "codex executable was not found on PATH",
                AgentTaskFailureClass::Unavailable,
            ),
            (
                "provider denied the automation request",
                AgentTaskFailureClass::Denied,
            ),
        ] {
            let records = vec![settled_backend_failure(
                &config,
                None,
                error,
                classification,
                3,
                2_000,
                None,
            )];

            // Inside the cooldown the transient failure still backs off …
            assert_eq!(
                schedule_decision(
                    &config,
                    None,
                    AgentTaskKind::MemoryCurator,
                    &records,
                    SessionActivity::none(),
                    2_000 + DEFAULT_FAILURE_COOLDOWN_SECS as i64 - 1,
                )
                .skip_reason()
                .map(AutomationSkipReasonV1::as_str),
                Some("scheduler_cooldown_active"),
                "transient {classification:?} must keep the ordinary cooldown",
            );
            // … and once it elapses the task retries, exactly as before.
            assert!(
                schedule_decision(
                    &config,
                    None,
                    AgentTaskKind::MemoryCurator,
                    &records,
                    SessionActivity::none(),
                    2_000 + DEFAULT_FAILURE_COOLDOWN_SECS as i64,
                )
                .is_due(),
                "transient {classification:?} must still retry",
            );
        }
    }

    #[test]
    fn a_failure_that_recovered_mid_ladder_is_not_settled() {
        let config = curator_config();
        let mut record = settled_backend_failure(
            &config,
            None,
            DISCONNECT_ERROR,
            AgentTaskFailureClass::Disconnected,
            3,
            2_000,
            None,
        );
        // The ladder did not reproduce one class: the run is not settled
        // evidence about a deterministic fault.
        record.backend_attempts[1].failure_classification = Some(AgentTaskFailureClass::Timeout);
        let records = vec![record];

        assert!(
            schedule_decision(
                &config,
                None,
                AgentTaskKind::MemoryCurator,
                &records,
                SessionActivity::none(),
                2_000 + DEFAULT_FAILURE_COOLDOWN_SECS as i64,
            )
            .is_due(),
        );
    }

    fn budget_exhausted_skip(
        run_id: &str,
        task: AgentTaskKind,
        completed_at: i64,
    ) -> AutomationRunLedgerRecord {
        budget_exhausted_skip_with_reason(
            run_id,
            task,
            SESSION_EVIDENCE_BUDGET_EXHAUSTED,
            completed_at,
        )
    }

    fn budget_exhausted_skip_with_reason(
        run_id: &str,
        task: AgentTaskKind,
        reason: &'static str,
        completed_at: i64,
    ) -> AutomationRunLedgerRecord {
        scheduler_ledger_record(
            run_id,
            task,
            AutomationRunStatus::Skipped,
            Some(reason),
            completed_at,
        )
    }

    #[test]
    fn only_the_canonical_budget_token_activates_the_backoff() {
        let config = session_evidence_config();
        let decision_at = |reason: &'static str, now: i64| {
            schedule_decision(
                &config,
                None,
                AgentTaskKind::SessionReflector,
                &[budget_exhausted_skip_with_reason(
                    "run-exhausted",
                    AgentTaskKind::SessionReflector,
                    reason,
                    2_000,
                )],
                SessionActivity::at(2_500),
                now,
            )
        };

        assert_eq!(
            decision_at(SESSION_EVIDENCE_BUDGET_EXHAUSTED, 2_060)
                .skip_reason()
                .map(AutomationSkipReasonV1::as_str),
            Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED),
        );
        assert!(decision_at(SESSION_EVIDENCE_BUDGET_EXHAUSTED, 2_000 + 3_600).is_due());
        assert_ne!(
            decision_at(
                "session_evidence_budget_exhausted_request_candidate_bytes",
                2_060
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
            Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED),
            "stage-suffixed reasons are not a budget-exhaustion wire form",
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
                None,
                AgentTaskKind::SessionReflector,
                &records,
                SessionActivity::at(2_500),
                2_130,
            )
            .is_due()
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
                None,
                AgentTaskKind::SessionReflector,
                &[exhausted, earlier_success],
                SessionActivity::at(2_500),
                2_060,
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
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
                None,
                AgentTaskKind::SkillWriter,
                &records,
                SessionActivity::at(2_500),
                2_120,
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
            Some(SESSION_EVIDENCE_BUDGET_SUPPRESSED)
        );
        assert!(
            schedule_decision(
                &config,
                None,
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
                None,
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
                None,
                AgentTaskKind::SessionReflector,
                &records,
                SessionActivity::at(2_500),
                2_060,
            )
            .is_due()
        );
    }

    #[test]
    fn evidence_timeout_retries_after_cooldown_without_conflating_cancellation() {
        let config = session_evidence_config();
        let timeout = scheduler_ledger_record(
            "run-evidence-timeout",
            AgentTaskKind::SessionReflector,
            AutomationRunStatus::Skipped,
            Some("session_evidence_timed_out"),
            2_000,
        );

        assert_eq!(
            skipped_terminal_failure_class(&timeout),
            Some(AgentTaskFailureClass::Timeout),
            "ledger timeout must classify through AutomationSkipReasonV1, not an inline string match",
        );
        assert_eq!(
            AutomationSkipReasonV1::from_ledger_reason("session_evidence_timed_out"),
            Some(AutomationSkipReasonV1::SessionEvidenceTimedOut),
        );

        assert_eq!(
            schedule_decision(
                &config,
                None,
                AgentTaskKind::SessionReflector,
                std::slice::from_ref(&timeout),
                SessionActivity::at(1_500),
                2_000 + DEFAULT_FAILURE_COOLDOWN_SECS as i64 - 1,
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
            Some("scheduler_cooldown_active"),
        );
        assert!(
            schedule_decision(
                &config,
                None,
                AgentTaskKind::SessionReflector,
                std::slice::from_ref(&timeout),
                SessionActivity::at(1_500),
                2_000 + DEFAULT_FAILURE_COOLDOWN_SECS as i64,
            )
            .is_due(),
            "canonical timeout retry may reuse the evidence request after cooldown",
        );

        let cancelled = scheduler_ledger_record(
            "run-evidence-cancelled",
            AgentTaskKind::SessionReflector,
            AutomationRunStatus::Skipped,
            Some("session_evidence_cancelled"),
            2_000,
        );
        assert_eq!(
            skipped_terminal_failure_class(&cancelled),
            None,
            "cancellation must not enter the retryable failure class",
        );
        assert_eq!(
            schedule_decision(
                &config,
                None,
                AgentTaskKind::SessionReflector,
                &[cancelled],
                SessionActivity::at(1_500),
                2_000 + DEFAULT_FAILURE_COOLDOWN_SECS as i64,
            )
            .skip_reason()
            .map(AutomationSkipReasonV1::as_str),
            Some("no_new_session_activity"),
            "cancellation remains an effectful skip that needs fresh activity",
        );
    }

    #[test]
    fn no_new_activity_skip_reports_the_timestamps_it_compared() {
        let config = session_evidence_config();
        let anchor = scheduler_ledger_record(
            "run-anchor",
            AgentTaskKind::SessionReflector,
            AutomationRunStatus::Succeeded,
            None,
            2_000,
        );
        let decision = schedule_decision(
            &config,
            None,
            AgentTaskKind::SessionReflector,
            &[anchor],
            SessionActivity::at(1_500),
            10_000,
        );
        assert_eq!(
            decision.skip_reason(),
            Some(AutomationSkipReasonV1::NoNewSessionActivity)
        );
        assert_eq!(
            decision.activity_gate(),
            Some(SessionActivityGate {
                last_activity_secs: Some(1_500),
                cadence_started_at: Some(2_000),
            })
        );
    }

    #[test]
    fn cron_admission_uses_the_leaf_schedule_type() {
        let AutomationSchedule::Cron(cron) = resolve_schedule(Some("*/15 * * * *"), None).unwrap()
        else {
            panic!("expected cron schedule");
        };
        assert!(cron_is_due(&cron, Some(3_599), 3_600));
        assert!(!cron_is_due(&cron, Some(3_600), 3_600));
    }

    fn schedule_decision(
        config: &AutomationConfig,
        executable: Option<&Path>,
        task: AgentTaskKind,
        records: &[AutomationRunLedgerRecord],
        activity: SessionActivity,
        now_secs: i64,
    ) -> AutomationScheduleDecision {
        super::schedule_decision(
            &super::super::config::admit_config(config).unwrap(),
            executable,
            task,
            records,
            activity,
            now_secs,
        )
    }

    async fn acquire(root: &Path, key: &str) -> Option<AutomationTaskLock> {
        AutomationTaskLock::try_acquire_keyed(root, key)
            .await
            .expect("task-lock acquisition")
    }

    #[tokio::test]
    async fn same_key_admits_one_holder_until_release_and_other_keys_are_independent() {
        let root = tempdir().unwrap();
        let first = acquire(root.path(), "memory_curator")
            .await
            .expect("first holder");
        assert!(acquire(root.path(), "memory_curator").await.is_none());
        let other = acquire(root.path(), "session_reflector")
            .await
            .expect("a different key is independent");

        drop(first);
        let next = acquire(root.path(), "memory_curator")
            .await
            .expect("explicit release admits the next holder");
        drop((other, next));
    }

    #[tokio::test]
    async fn cancellation_releases_the_task_lock() {
        let root = tempdir().unwrap();
        let holder_root = root.path().to_path_buf();
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let holder = tokio::spawn(async move {
            let _guard = acquire(&holder_root, "cancelled").await.unwrap();
            held_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        held_rx.await.unwrap();
        assert!(acquire(root.path(), "cancelled").await.is_none());
        holder.abort();
        assert!(holder.await.unwrap_err().is_cancelled());
        assert!(acquire(root.path(), "cancelled").await.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_same_key_admission_has_exactly_one_winner() {
        let root = tempdir().unwrap();
        let attempts = (0..16).map(|_| {
            let root = root.path().to_path_buf();
            tokio::spawn(async move { acquire(&root, "contended").await })
        });
        let mut winners = 0;
        let mut held = Vec::new();
        for attempt in attempts {
            if let Some(lock) = attempt.await.unwrap() {
                winners += 1;
                held.push(lock);
            }
        }
        assert_eq!(winners, 1);
    }

    #[tokio::test]
    async fn release_keeps_the_lease_file_so_exclusion_never_splits_across_inodes() {
        let root = tempdir().unwrap();
        let lease_path = root.path().join("automation_locks/memory_curator.lease");
        drop(acquire(root.path(), "memory_curator").await.unwrap());
        assert!(
            lease_path.exists(),
            "release must not unlink the lease file"
        );
        #[cfg(unix)]
        let before = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&lease_path).unwrap().ino()
        };
        let held = acquire(root.path(), "memory_curator").await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&lease_path).unwrap().ino(), before);
        }
        let foreign_open = std::fs::File::open(&lease_path).unwrap();
        assert!(
            matches!(
                foreign_open.try_lock_shared(),
                Err(std::fs::TryLockError::WouldBlock)
            ),
            "the lease is a kernel lock any other process observes"
        );
        drop(held);
        foreign_open
            .try_lock_shared()
            .expect("release unlocks for other open file descriptions");
    }

    #[test]
    fn release_needs_no_tokio_runtime() {
        let temp = tempdir().unwrap();
        let lock_dir = temp.path().join("automation_locks");
        let guard = try_acquire_task_lock_blocking(&lock_dir, "no_runtime")
            .unwrap()
            .expect("fresh key");
        drop(guard);
        try_acquire_task_lock_blocking(&lock_dir, "no_runtime")
            .unwrap()
            .expect("drop released synchronously");
    }

    #[cfg(unix)]
    #[test]
    fn task_coordination_symlinks_are_rejected_without_touching_their_targets() {
        let temp = tempdir().unwrap();
        let lock_dir = temp.path().join("automation_locks");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all_durable(&lock_dir)
            .unwrap();
        let outside = temp.path().join("outside-lock-target");
        std::fs::write(&outside, b"unchanged").unwrap();
        let key = "lease_symlink";
        {
            let path = lock_dir.join(format!("{key}.lease"));
            std::os::unix::fs::symlink(&outside, &path).unwrap();

            let error = try_acquire_task_lock_blocking(&lock_dir, key).unwrap_err();

            assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
            assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
            assert!(path.is_symlink());
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_task_locks_acquire_through_symlinked_dashboard_prefix() {
        // A prefix symlink (`tmp/link` -> `tmp/real`, or macOS `/var` ->
        // `/private/var`) must resolve before cap-std walks the components or
        // concurrent acquires ENOENT.
        let temp = tempdir().unwrap();
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let dashboard_root = link.join("dashboard");

        let first_root = dashboard_root.clone();
        let second_root = dashboard_root;
        let first = tokio::spawn(async move { acquire(&first_root, "concurrent-a").await });
        let second = tokio::spawn(async move { acquire(&second_root, "concurrent-b").await });
        assert!(
            first.await.unwrap().is_some() && second.await.unwrap().is_some(),
            "distinct keys under a shared lock dir must both acquire"
        );
    }
}

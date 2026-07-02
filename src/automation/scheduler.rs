use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use super::backend::{agent_task_failure_disposition, task_key, AgentTaskKind};
use super::config::{
    AutomationBackend, AutomationConfig, AutomationHostMode, AutomationTaskConfig,
};
use super::run_ledger::{AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;

const DEFAULT_FAILURE_COOLDOWN_SECS: u64 = 300;
const DEFAULT_STALE_LOCK_SECS: u64 = 6 * 60 * 60;
const SCHEDULER_CONTROL_FILENAME: &str = "automation_scheduler_control.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AutomationSchedulerControl {
    #[serde(default)]
    pub paused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationSchedule {
    Manual,
    ConfiguredInterval,
    Interval { every_secs: u64 },
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

/// Reads the session-activity signal from the LCM sessions database.
///
/// This is a single indexed `ORDER BY timestamp DESC LIMIT 1` lookup against
/// the read-only store, so it is cheap and race-safe to call from every
/// scheduler tick; concurrent ingest writers only ever move the value forward.
pub async fn load_session_activity(sessions_db_path: &Path) -> SessionActivity {
    let Some(db) = GlobalDb::open_read_only_at(sessions_db_path).await else {
        return SessionActivity::none();
    };
    SessionActivity {
        last_activity_secs: db.latest_session_activity_secs().await,
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

pub struct AutomationTaskLock {
    path: PathBuf,
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
        let lock_dir = dashboard_root.join("automation_locks");
        tokio::fs::create_dir_all(&lock_dir)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to create automation lock directory '{}': {e}",
                    lock_dir.display()
                ),
            })?;
        let path = lock_dir.join(format!("{}.lock", task_key(task)));
        for attempt in 0..2 {
            match create_lock_file(&path, now_secs).await {
                Ok(()) => return Ok(Some(Self { path })),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == 0 && lock_is_stale(&path, stale_after_secs, now_secs).await? {
                        match tokio::fs::remove_file(&path).await {
                            Ok(()) => continue,
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                            Err(e) => {
                                return Err(TraceDecayError::Config {
                                    message: format!(
                                        "failed to remove stale automation lock '{}': {e}",
                                        path.display()
                                    ),
                                })
                            }
                        }
                    }
                    return Ok(None);
                }
                Err(e) => {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "failed to acquire automation lock '{}': {e}",
                            path.display()
                        ),
                    })
                }
            }
        }
        Ok(None)
    }
}

impl Drop for AutomationTaskLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn schedule_decision(
    config: &AutomationConfig,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
) -> AutomationScheduleDecision {
    if !config.enabled {
        return AutomationScheduleDecision::skipped("automation_disabled");
    }
    if config.host_mode == AutomationHostMode::DelegatedHost {
        return AutomationScheduleDecision::skipped("delegated_host_mode");
    }
    if config.backend == AutomationBackend::Disabled {
        return AutomationScheduleDecision::skipped("backend_disabled");
    }
    let task_config = task_config(config, task);
    if !task_config.enabled {
        return AutomationScheduleDecision::skipped("task_disabled");
    }

    let Ok(schedule) = parse_schedule(task_config.schedule.as_deref()) else {
        return AutomationScheduleDecision::skipped("scheduler_schedule_invalid");
    };
    let interval_secs = match schedule {
        AutomationSchedule::Manual => {
            return AutomationScheduleDecision::skipped("scheduler_schedule_manual")
        }
        AutomationSchedule::ConfiguredInterval => task_config.interval_secs,
        AutomationSchedule::Interval { every_secs } => Some(every_secs),
    };
    let Some(interval_secs) = interval_secs else {
        return AutomationScheduleDecision::skipped("scheduler_schedule_manual");
    };

    // `min_idle_secs` is a true idle window: the project must have been quiet
    // (no LCM session ingest activity) for at least this long. An unknown
    // activity signal (no session store yet) counts as idle.
    if let Some(min_idle_secs) = task_config.min_idle_secs {
        if let Some(last_activity) = activity.last_activity_secs {
            if elapsed_secs(last_activity, now_secs) < min_idle_secs {
                return AutomationScheduleDecision::skipped("scheduler_idle_window_active");
            }
        }
    }

    if let Some(record) =
        latest_non_skipped_record(records, task, Some(AutomationTrigger::Scheduler))
    {
        let completed_at = record.completed_at.parse::<i64>().ok().unwrap_or(0);
        if record.status == AutomationRunStatus::Failed {
            let failure = agent_task_failure_disposition(
                record.error_classification,
                record.error_retryable,
                record.error.as_deref(),
            );
            if failure.is_non_retryable() {
                return AutomationScheduleDecision::skipped("scheduler_non_retryable_failure");
            }
            let cooldown_secs = task_config
                .cooldown_secs
                .unwrap_or(DEFAULT_FAILURE_COOLDOWN_SECS);
            if elapsed_secs(completed_at, now_secs) < cooldown_secs {
                return AutomationScheduleDecision::skipped("scheduler_cooldown_active");
            }
            return AutomationScheduleDecision::due();
        }
        if elapsed_secs(completed_at, now_secs) < interval_secs {
            return AutomationScheduleDecision::skipped("scheduler_interval_not_elapsed");
        }
    }

    // Session-evidence tasks only re-run when new session activity landed
    // after their last successful run started; a run without fresh evidence
    // would re-review the same transcript slices. Skips do not consume the
    // interval clock, so the task fires on the first tick after new activity.
    if task_consumes_session_evidence(task) {
        if let Some(record) = latest_successful_record(records, task) {
            let started_at = record.started_at.parse::<i64>().ok().unwrap_or(0);
            let has_new_activity = activity
                .last_activity_secs
                .is_some_and(|last_activity| last_activity > started_at);
            if !has_new_activity {
                return AutomationScheduleDecision::skipped("no_new_session_activity");
            }
        }
    }

    AutomationScheduleDecision::due()
}

/// Tasks whose evidence comes from the LCM session store; they are gated on
/// new session activity since their last successful run.
fn task_consumes_session_evidence(task: AgentTaskKind) -> bool {
    match task {
        AgentTaskKind::SessionReflector | AgentTaskKind::SkillWriter => true,
        AgentTaskKind::MemoryCurator => false,
    }
}

pub fn stale_lock_secs(config: &AutomationConfig, task: AgentTaskKind) -> Option<u64> {
    task_config(config, task)
        .stale_lock_secs
        .or(Some(DEFAULT_STALE_LOCK_SECS))
}

pub fn validate_schedule(schedule: Option<&str>) -> Result<()> {
    parse_schedule(schedule).map(|_| ())
}

pub fn parse_schedule(schedule: Option<&str>) -> Result<AutomationSchedule> {
    let Some(raw) = schedule.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(AutomationSchedule::Manual);
    };
    let normalized = raw.to_ascii_lowercase();
    match normalized.as_str() {
        "manual" | "off" | "disabled" => return Ok(AutomationSchedule::Manual),
        "interval" => return Ok(AutomationSchedule::ConfiguredInterval),
        "hourly" => {
            return Ok(AutomationSchedule::Interval {
                every_secs: 60 * 60,
            })
        }
        "daily" => {
            return Ok(AutomationSchedule::Interval {
                every_secs: 24 * 60 * 60,
            })
        }
        "weekly" => {
            return Ok(AutomationSchedule::Interval {
                every_secs: 7 * 24 * 60 * 60,
            })
        }
        _ => {}
    }

    let duration = normalized
        .strip_prefix("every ")
        .or_else(|| normalized.strip_prefix("every:"))
        .or_else(|| normalized.strip_prefix("interval "))
        .or_else(|| normalized.strip_prefix("interval:"))
        .unwrap_or(normalized.as_str());
    let Some(every_secs) = parse_schedule_duration_secs(duration) else {
        return Err(TraceDecayError::Config {
            message: format!(
                "invalid automation schedule '{raw}'; use manual, interval, hourly, daily, weekly, or every <duration>"
            ),
        });
    };
    if every_secs == 0 {
        return Err(TraceDecayError::Config {
            message: "automation schedule interval must be greater than zero".to_string(),
        });
    }
    Ok(AutomationSchedule::Interval { every_secs })
}

fn task_config(config: &AutomationConfig, task: AgentTaskKind) -> &AutomationTaskConfig {
    match task {
        AgentTaskKind::MemoryCurator => &config.tasks.memory_curator,
        AgentTaskKind::SessionReflector => &config.tasks.session_reflector,
        AgentTaskKind::SkillWriter => &config.tasks.skill_writer,
    }
}

fn latest_successful_record(
    records: &[AutomationRunLedgerRecord],
    task: AgentTaskKind,
) -> Option<&AutomationRunLedgerRecord> {
    records
        .iter()
        .filter(|record| record.task == task && record.status == AutomationRunStatus::Succeeded)
        .max_by_key(|record| record.completed_at.parse::<i64>().ok().unwrap_or(0))
}

fn latest_non_skipped_record(
    records: &[AutomationRunLedgerRecord],
    task: AgentTaskKind,
    trigger: Option<AutomationTrigger>,
) -> Option<&AutomationRunLedgerRecord> {
    records
        .iter()
        .filter(|record| {
            record.task == task
                && trigger.is_none_or(|trigger| record.trigger == trigger)
                && matches!(
                    record.status,
                    AutomationRunStatus::Succeeded | AutomationRunStatus::Failed
                )
        })
        .max_by_key(|record| record.completed_at.parse::<i64>().ok().unwrap_or(0))
}

fn elapsed_secs(completed_at: i64, now_secs: i64) -> u64 {
    if now_secs < completed_at {
        return 0;
    }
    (now_secs - completed_at) as u64
}

fn parse_schedule_duration_secs(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let idx = value.find(|c: char| !c.is_ascii_digit())?;
    let (amount, unit) = value.split_at(idx);
    let amount = amount.parse::<u64>().ok()?;
    if amount == 0 {
        return Some(0);
    }
    let unit = unit.trim();
    let multiplier = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 60 * 60,
        "d" | "day" | "days" => 24 * 60 * 60,
        _ => return None,
    };
    Some(amount.saturating_mul(multiplier))
}

async fn create_lock_file(path: &Path, now_secs: i64) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .await?;
    let payload = format!("pid={}\ncreated_at={now_secs}\n", std::process::id());
    file.write_all(payload.as_bytes()).await
}

async fn lock_is_stale(path: &Path, stale_after_secs: Option<u64>, now_secs: i64) -> Result<bool> {
    let Some(stale_after_secs) = stale_after_secs else {
        return Ok(false);
    };
    if let Some(pid) = lock_pid(path).await? {
        if process_is_live(pid) {
            return Ok(false);
        }
    }
    let Some(created_at) = lock_created_at(path).await? else {
        return Ok(true);
    };
    Ok(elapsed_secs(created_at, now_secs) >= stale_after_secs)
}

async fn lock_pid(path: &Path) -> Result<Option<u32>> {
    let contents = match tokio::fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to read automation lock '{}': {e}", path.display()),
            })
        }
    };
    Ok(contents.lines().find_map(|line| {
        line.strip_prefix("pid=")
            .and_then(|value| value.trim().parse::<u32>().ok())
    }))
}

async fn lock_created_at(path: &Path) -> Result<Option<i64>> {
    if let Ok(contents) = tokio::fs::read_to_string(path).await {
        if let Some(created_at) = contents.lines().find_map(|line| {
            line.strip_prefix("created_at=")
                .and_then(|value| value.trim().parse::<i64>().ok())
        }) {
            return Ok(Some(created_at));
        }
    }

    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to inspect automation lock '{}': {e}",
                    path.display()
                ),
            })
        }
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(None);
    };
    let Ok(duration) = modified.duration_since(UNIX_EPOCH) else {
        return Ok(None);
    };
    Ok(Some(duration.as_secs() as i64))
}

fn process_is_live(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        PathBuf::from(format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

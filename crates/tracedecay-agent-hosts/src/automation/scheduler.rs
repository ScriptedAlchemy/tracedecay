use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use super::backend::{AgentTaskKind, agent_task_failure_disposition, task_key};
use super::config::{
    AutomationBackend, AutomationConfig, AutomationHostMode, AutomationTaskConfig,
};
use super::run_ledger::{AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger};
use crate::errors::{Result, TraceDecayError};

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
    Cron(CronSchedule),
}

/// Parsed standard 5-field cron expression (minute hour day-of-month month
/// day-of-week), evaluated against the wall clock in UTC. Fields are stored
/// as allow-bitmasks. Numeric values only (no JAN/MON aliases); day-of-week
/// accepts 0-7 with both 0 and 7 meaning Sunday. Day-of-month and
/// day-of-week combine with vixie-cron OR semantics when both are
/// restricted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CronSchedule {
    minutes: u64,
    hours: u32,
    days_of_month: u32,
    months: u16,
    days_of_week: u8,
    dom_is_wildcard: bool,
    dow_is_wildcard: bool,
}

/// How far back [`CronSchedule::previous_occurrence`] searches for a match.
/// A year plus a day covers every satisfiable standard cron expression.
const CRON_LOOKBACK_DAYS: i64 = 367;

impl CronSchedule {
    /// Returns whether the UTC minute containing `now_secs` matches.
    pub fn matches(&self, now_secs: i64) -> bool {
        let days = now_secs.div_euclid(86_400);
        let secs_of_day = now_secs.rem_euclid(86_400);
        let minute = (secs_of_day / 60 % 60) as u32;
        let hour = (secs_of_day / 3_600) as u32;
        self.minutes & (1 << minute) != 0 && self.hours & (1 << hour) != 0 && self.day_matches(days)
    }

    /// Returns the start (unix seconds, UTC) of the most recent matching
    /// minute at or before `now_secs`, searching back one year.
    pub fn previous_occurrence(&self, now_secs: i64) -> Option<i64> {
        let now_minute = now_secs - now_secs.rem_euclid(60);
        let now_days = now_secs.div_euclid(86_400);
        for day_offset in 0..CRON_LOOKBACK_DAYS {
            let days = now_days - day_offset;
            if !self.day_matches(days) {
                continue;
            }
            let day_start = days * 86_400;
            for hour in (0..24u32).rev() {
                if self.hours & (1 << hour) == 0 {
                    continue;
                }
                for minute in (0..60u32).rev() {
                    if self.minutes & (1 << minute) == 0 {
                        continue;
                    }
                    let candidate = day_start + i64::from(hour) * 3_600 + i64::from(minute) * 60;
                    if candidate <= now_minute {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }

    fn day_matches(&self, days_since_epoch: i64) -> bool {
        let (_, month, day) = civil_from_days(days_since_epoch);
        if self.months & (1 << month) == 0 {
            return false;
        }
        // Sunday = 0; 1970-01-01 (day 0) was a Thursday (4).
        let weekday = ((days_since_epoch + 4).rem_euclid(7)) as u32;
        let dom_ok = self.days_of_month & (1 << day) != 0;
        let dow_ok = self.days_of_week & (1 << weekday) != 0;
        match (self.dom_is_wildcard, self.dow_is_wildcard) {
            // vixie cron: when both fields are restricted, either may match.
            (false, false) => dom_ok || dow_ok,
            _ => dom_ok && dow_ok,
        }
    }
}

/// Converts days since the unix epoch to (year, month 1-12, day 1-31) using
/// Howard Hinnant's civil-from-days algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
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
/// This reads from the read-only store using bounded indexed timestamp lookups,
/// so it is cheap and race-safe to call from every scheduler tick; concurrent
/// ingest writers only ever move the value forward.
pub async fn load_session_activity(sessions_db_path: &Path) -> SessionActivity {
    SessionActivity {
        last_activity_secs: crate::ports::latest_session_activity(sessions_db_path).await,
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
        tokio::fs::create_dir_all(&lock_dir)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to create automation lock directory '{}': {e}",
                    lock_dir.display()
                ),
            })?;
        let path = lock_dir.join(format!("{key}.lock"));
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
                                });
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
                    });
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
    schedule_decision_for_trigger(config, task, records, activity, now_secs, true)
}

pub fn host_receipt_decision(
    config: &AutomationConfig,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
) -> AutomationScheduleDecision {
    schedule_decision_for_trigger(config, task, records, activity, now_secs, false)
}

fn schedule_decision_for_trigger(
    config: &AutomationConfig,
    task: AgentTaskKind,
    records: &[AutomationRunLedgerRecord],
    activity: SessionActivity,
    now_secs: i64,
    enforce_schedule: bool,
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
    let Some(task_config) = task_config(config, task) else {
        return AutomationScheduleDecision::skipped("task_not_schedulable");
    };
    if !task_config.enabled {
        return AutomationScheduleDecision::skipped("task_disabled");
    }

    let (interval_secs, cron) = if enforce_schedule {
        let Ok(schedule) = parse_schedule(task_config.schedule.as_deref()) else {
            return AutomationScheduleDecision::skipped("scheduler_schedule_invalid");
        };
        let timing = match schedule {
            AutomationSchedule::Manual => {
                return AutomationScheduleDecision::skipped("scheduler_schedule_manual");
            }
            AutomationSchedule::ConfiguredInterval => (task_config.interval_secs, None),
            AutomationSchedule::Interval { every_secs } => (Some(every_secs), None),
            AutomationSchedule::Cron(cron) => (None, Some(cron)),
        };
        if timing.0.is_none() && timing.1.is_none() {
            return AutomationScheduleDecision::skipped("scheduler_schedule_manual");
        }
        timing
    } else {
        (None, None)
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

    // Session-evidence tasks are event-driven across every supported host.
    // Cursor, Claude, Codex, and Hermes all ingest completed-turn evidence
    // into the project session store; a newer activity watermark should wake
    // reflection as soon as the idle window closes instead of waiting for the
    // periodic interval. The interval remains the repair/backstop schedule.
    let fresh_session_activity = task_consumes_session_evidence(task)
        && latest_successful_record(records, task).is_some_and(|record| {
            let started_at = record.started_at.parse::<i64>().ok().unwrap_or(0);
            activity
                .last_activity_secs
                .is_some_and(|last_activity| last_activity > started_at)
        });

    if let Some(record) = latest_non_skipped_record(
        records,
        task,
        enforce_schedule.then_some(AutomationTrigger::Scheduler),
    ) {
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
        if let Some(interval_secs) = interval_secs.filter(|_| !fresh_session_activity) {
            if elapsed_secs(completed_at, now_secs) < interval_secs {
                return AutomationScheduleDecision::skipped("scheduler_interval_not_elapsed");
            }
        }
        if let Some(cron) = cron.filter(|_| !fresh_session_activity) {
            if !cron_is_due(&cron, Some(completed_at), now_secs) {
                return AutomationScheduleDecision::skipped("scheduler_cron_not_due");
            }
        }
    } else if let Some(cron) = cron {
        if !cron_is_due(&cron, None, now_secs) {
            return AutomationScheduleDecision::skipped("scheduler_cron_not_due");
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
            });
        }
        "daily" => {
            return Ok(AutomationSchedule::Interval {
                every_secs: 24 * 60 * 60,
            });
        }
        "weekly" => {
            return Ok(AutomationSchedule::Interval {
                every_secs: 7 * 24 * 60 * 60,
            });
        }
        _ => {}
    }

    if normalized.split_whitespace().count() == 5 {
        return parse_cron_expression(&normalized);
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

fn parse_cron_expression(raw: &str) -> Result<AutomationSchedule> {
    let fields: Vec<&str> = raw.split_whitespace().collect();
    let [minute, hour, dom, month, dow] = fields.as_slice() else {
        return Err(cron_error(raw, "expected 5 fields"));
    };
    let minutes = parse_cron_field(minute, 0, 59, raw)?.0;
    let hours = parse_cron_field(hour, 0, 23, raw)?.0 as u32;
    let (dom_bits, dom_is_wildcard) = parse_cron_field(dom, 1, 31, raw)?;
    let (month_bits, _) = parse_cron_field(month, 1, 12, raw)?;
    let (dow_bits_raw, dow_is_wildcard) = parse_cron_field(dow, 0, 7, raw)?;
    // Fold day-of-week 7 (Sunday) onto 0.
    let mut days_of_week = (dow_bits_raw & 0x7f) as u8;
    if dow_bits_raw & (1 << 7) != 0 {
        days_of_week |= 1;
    }
    Ok(AutomationSchedule::Cron(CronSchedule {
        minutes,
        hours,
        days_of_month: dom_bits as u32,
        months: month_bits as u16,
        days_of_week,
        dom_is_wildcard,
        dow_is_wildcard,
    }))
}

/// Parses one cron field into an allow-bitmask; returns the mask and whether
/// the field was an unrestricted wildcard (`*` or `*/1`).
fn parse_cron_field(field: &str, min: u32, max: u32, raw: &str) -> Result<(u64, bool)> {
    let mut bits = 0u64;
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step = step
                    .parse::<u32>()
                    .ok()
                    .filter(|step| *step > 0)
                    .ok_or_else(|| cron_error(raw, "step must be a positive integer"))?;
                (range, step)
            }
            None => (part, 1),
        };
        let (start, end) = if range == "*" {
            (min, max)
        } else if let Some((start, end)) = range.split_once('-') {
            let start = parse_cron_value(start, min, max, raw)?;
            let end = parse_cron_value(end, min, max, raw)?;
            if start > end {
                return Err(cron_error(raw, "range start exceeds range end"));
            }
            (start, end)
        } else {
            let value = parse_cron_value(range, min, max, raw)?;
            (value, value)
        };
        let mut value = start;
        while value <= end {
            bits |= 1 << value;
            value += step;
        }
    }
    if bits == 0 {
        return Err(cron_error(raw, "field selects no values"));
    }
    let wildcard = bits == full_cron_field_mask(min, max);
    Ok((bits, wildcard))
}

fn full_cron_field_mask(min: u32, max: u32) -> u64 {
    (min..=max).fold(0, |bits, value| bits | (1 << value))
}

fn parse_cron_value(value: &str, min: u32, max: u32, raw: &str) -> Result<u32> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| cron_error(raw, "values must be integers"))?;
    if parsed < min || parsed > max {
        return Err(cron_error(
            raw,
            &format!("value {parsed} is outside {min}-{max}"),
        ));
    }
    Ok(parsed)
}

fn cron_error(raw: &str, detail: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("invalid cron schedule '{raw}': {detail}"),
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
            });
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
            });
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

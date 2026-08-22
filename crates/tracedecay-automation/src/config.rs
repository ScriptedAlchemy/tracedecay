use serde::{Deserialize, Deserializer, Serialize};
pub use tracedecay_domain::configuration::{
    AutomationBackendV1 as AutomationBackend, AutomationHostModeV1 as AutomationHostMode,
    AutomationSettingsV1 as AutomationConfig, AutomationTaskSetV1 as AutomationTaskSet,
    AutomationTaskSettingsV1 as AutomationTaskConfig,
};

use crate::{AutomationError, Result, config_error};

pub const DEFAULT_SCHEDULER_TICK_SECS: u64 = 60;
pub const DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS: u32 = 180;
pub const DEFAULT_LEGACY_SESSION_RETENTION_DAYS: u32 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionConfig {
    #[serde(default = "default_analytics_events_days")]
    pub analytics_events_days: Option<u32>,
    #[serde(default = "default_legacy_session_days")]
    pub session_messages_days: Option<u32>,
    #[serde(default = "default_legacy_session_days")]
    pub lcm_raw_messages_days: Option<u32>,
}

#[allow(clippy::unnecessary_wraps)]
fn default_analytics_events_days() -> Option<u32> {
    Some(DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS)
}

#[allow(clippy::unnecessary_wraps)]
fn default_legacy_session_days() -> Option<u32> {
    Some(DEFAULT_LEGACY_SESSION_RETENTION_DAYS)
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            analytics_events_days: default_analytics_events_days(),
            session_messages_days: default_legacy_session_days(),
            lcm_raw_messages_days: default_legacy_session_days(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AutomationTaskPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub schedule: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub interval_secs: Option<Option<u64>>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub cooldown_secs: Option<Option<u64>>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub min_idle_secs: Option<Option<u64>>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub stale_lock_secs: Option<Option<u64>>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub session_evidence_budget_backoff_secs: Option<Option<u64>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AutomationConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<AutomationBackend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_mode: Option<AutomationHostMode>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub model_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_tick_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combine_due_tasks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_job_commands: Option<bool>,
    #[serde(default)]
    pub memory_curator: AutomationTaskPatch,
    #[serde(default)]
    pub session_reflector: AutomationTaskPatch,
    #[serde(default)]
    pub skill_writer: AutomationTaskPatch,
}

#[allow(clippy::option_option)]
fn deserialize_clearable_field<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

pub fn effective_config(
    global: &AutomationConfig,
    project: Option<&AutomationConfigPatch>,
) -> Result<AutomationConfig> {
    let mut config = global.clone();
    if let Some(patch) = project {
        apply_patch(&mut config, patch);
    }
    validate_config(&config)?;
    Ok(config)
}

pub fn merge_project_config(
    current: Option<AutomationConfigPatch>,
    patch: AutomationConfigPatch,
) -> AutomationConfigPatch {
    let mut merged = current.unwrap_or_default();
    merge_patch(&mut merged, patch);
    merged
}

pub fn validate_config(config: &AutomationConfig) -> Result<()> {
    if config.timeout_secs == 0 {
        return Err(config_error(
            "automation timeout_secs must be greater than zero",
        ));
    }
    if config.scheduler_tick_secs == 0 {
        return Err(config_error(
            "automation scheduler_tick_secs must be greater than zero",
        ));
    }
    validate_task_config("memory_curator", &config.tasks.memory_curator)?;
    validate_task_config("session_reflector", &config.tasks.session_reflector)?;
    validate_task_config("skill_writer", &config.tasks.skill_writer)?;
    config
        .validate()
        .map_err(|error| config_error(format!("invalid automation settings: {error}")))?;
    Ok(())
}

fn apply_patch(config: &mut AutomationConfig, patch: &AutomationConfigPatch) {
    if let Some(enabled) = patch.enabled {
        config.enabled = enabled;
    }
    if let Some(backend) = patch.backend {
        config.backend = backend;
    }
    if let Some(host_mode) = patch.host_mode {
        config.host_mode = host_mode;
    }
    if let Some(model_id) = &patch.model_id {
        config.model_id.clone_from(model_id);
    }
    if let Some(timeout_secs) = patch.timeout_secs {
        config.timeout_secs = timeout_secs;
    }
    if let Some(scheduler_tick_secs) = patch.scheduler_tick_secs {
        config.scheduler_tick_secs = scheduler_tick_secs;
    }
    if let Some(combine_due_tasks) = patch.combine_due_tasks {
        config.combine_due_tasks = combine_due_tasks;
    }
    if let Some(allow_job_commands) = patch.allow_job_commands {
        config.allow_job_commands = allow_job_commands;
    }
    apply_task_patch(&mut config.tasks.memory_curator, &patch.memory_curator);
    apply_task_patch(
        &mut config.tasks.session_reflector,
        &patch.session_reflector,
    );
    apply_task_patch(&mut config.tasks.skill_writer, &patch.skill_writer);
    if config.backend != AutomationBackend::CodexAppServer {
        config.model_id = None;
    }
}

fn apply_task_patch(config: &mut AutomationTaskConfig, patch: &AutomationTaskPatch) {
    if let Some(enabled) = patch.enabled {
        config.enabled = enabled;
    }
    if let Some(schedule) = &patch.schedule {
        config.schedule.clone_from(schedule);
    }
    if let Some(interval_secs) = patch.interval_secs {
        config.interval_secs = interval_secs;
    }
    if let Some(cooldown_secs) = patch.cooldown_secs {
        config.cooldown_secs = cooldown_secs;
    }
    if let Some(min_idle_secs) = patch.min_idle_secs {
        config.min_idle_secs = min_idle_secs;
    }
    if let Some(stale_lock_secs) = patch.stale_lock_secs {
        config.stale_lock_secs = stale_lock_secs;
    }
    if let Some(session_evidence_budget_backoff_secs) = patch.session_evidence_budget_backoff_secs {
        config.session_evidence_budget_backoff_secs = session_evidence_budget_backoff_secs;
    }
}

fn merge_patch(config: &mut AutomationConfigPatch, patch: AutomationConfigPatch) {
    merge_optional_field(&mut config.enabled, patch.enabled);
    merge_optional_field(&mut config.backend, patch.backend);
    merge_optional_field(&mut config.host_mode, patch.host_mode);
    merge_optional_field(&mut config.model_id, patch.model_id);
    merge_optional_field(&mut config.timeout_secs, patch.timeout_secs);
    merge_optional_field(&mut config.scheduler_tick_secs, patch.scheduler_tick_secs);
    merge_optional_field(&mut config.combine_due_tasks, patch.combine_due_tasks);
    merge_optional_field(&mut config.allow_job_commands, patch.allow_job_commands);
    merge_task_patch(&mut config.memory_curator, patch.memory_curator);
    merge_task_patch(&mut config.session_reflector, patch.session_reflector);
    merge_task_patch(&mut config.skill_writer, patch.skill_writer);
    if config
        .backend
        .is_some_and(|backend| backend != AutomationBackend::CodexAppServer)
    {
        config.model_id = Some(None);
    }
}

fn merge_task_patch(config: &mut AutomationTaskPatch, patch: AutomationTaskPatch) {
    merge_optional_field(&mut config.enabled, patch.enabled);
    merge_optional_field(&mut config.schedule, patch.schedule);
    merge_optional_field(&mut config.interval_secs, patch.interval_secs);
    merge_optional_field(&mut config.cooldown_secs, patch.cooldown_secs);
    merge_optional_field(&mut config.min_idle_secs, patch.min_idle_secs);
    merge_optional_field(&mut config.stale_lock_secs, patch.stale_lock_secs);
    merge_optional_field(
        &mut config.session_evidence_budget_backoff_secs,
        patch.session_evidence_budget_backoff_secs,
    );
}

fn merge_optional_field<T>(current: &mut Option<T>, patch: Option<T>) {
    if patch.is_some() {
        *current = patch;
    }
}

fn validate_task_config(task: &str, config: &AutomationTaskConfig) -> Result<()> {
    if matches!(config.interval_secs, Some(0)) {
        return Err(config_error(format!(
            "{task} interval_secs must be greater than zero"
        )));
    }
    if matches!(config.cooldown_secs, Some(0)) {
        return Err(config_error(format!(
            "{task} cooldown_secs must be greater than zero"
        )));
    }
    if matches!(config.min_idle_secs, Some(0)) {
        return Err(config_error(format!(
            "{task} min_idle_secs must be greater than zero"
        )));
    }
    if matches!(config.stale_lock_secs, Some(0)) {
        return Err(config_error(format!(
            "{task} stale_lock_secs must be greater than zero"
        )));
    }
    if matches!(config.session_evidence_budget_backoff_secs, Some(0)) {
        return Err(config_error(format!(
            "{task} session_evidence_budget_backoff_secs must be greater than zero"
        )));
    }
    let schedule = parse_schedule(config.schedule.as_deref())
        .map_err(|error| AutomationError::config(format!("{task} schedule is invalid: {error}")))?;
    if schedule == AutomationSchedule::ConfiguredInterval && config.interval_secs.is_none() {
        return Err(config_error(format!(
            "{task} interval_secs is required when schedule is interval"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationSchedule {
    Manual,
    ConfiguredInterval,
    Interval { every_secs: u64 },
    Cron(CronSchedule),
}

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

const CRON_LOOKBACK_DAYS: i64 = 367;

impl CronSchedule {
    pub fn matches(&self, now_secs: i64) -> bool {
        let days = now_secs.div_euclid(86_400);
        let secs_of_day = now_secs.rem_euclid(86_400);
        let minute = (secs_of_day / 60 % 60) as u32;
        let hour = (secs_of_day / 3_600) as u32;
        self.minutes & (1 << minute) != 0 && self.hours & (1 << hour) != 0 && self.day_matches(days)
    }

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
        let weekday = ((days_since_epoch + 4).rem_euclid(7)) as u32;
        let dom_ok = self.days_of_month & (1 << day) != 0;
        let dow_ok = self.days_of_week & (1 << weekday) != 0;
        match (self.dom_is_wildcard, self.dow_is_wildcard) {
            (false, false) => dom_ok || dow_ok,
            _ => dom_ok && dow_ok,
        }
    }
}

/// Hinnant's `civil_from_days`, written with `div_euclid`/`rem_euclid` rather
/// than the branch form used by the canonical copy in
/// `tracedecay_capture::timestamp`. The two agree for every input; this crate
/// keeps its own so it can stay free of workspace dependencies, which is worth
/// more than deduplicating twelve lines of pure arithmetic.
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
        return Err(config_error(format!(
            "invalid automation schedule '{raw}'; use manual, interval, hourly, daily, weekly, or every <duration>"
        )));
    };
    if every_secs == 0 {
        return Err(config_error(
            "automation schedule interval must be greater than zero",
        ));
    }
    Ok(AutomationSchedule::Interval { every_secs })
}

pub fn validate_schedule(schedule: Option<&str>) -> Result<()> {
    parse_schedule(schedule).map(|_| ())
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

fn cron_error(raw: &str, detail: &str) -> AutomationError {
    AutomationError::config(format!("invalid cron schedule '{raw}': {detail}"))
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn automation_defaults_mount_the_final_v2_curators() {
        let config = AutomationConfig::default();

        assert!(config.enabled);
        assert_eq!(config.backend, AutomationBackend::CodexAppServer);
        assert_eq!(config.host_mode, AutomationHostMode::Standalone);
        assert_eq!(config.timeout_secs, 60);
        assert_eq!(config.scheduler_tick_secs, 60);
        assert!(config.combine_due_tasks);
        assert_eq!(config.tasks.memory_curator.interval_secs, Some(900));
        assert_eq!(config.tasks.session_reflector.interval_secs, Some(900));
        assert_eq!(config.tasks.skill_writer.interval_secs, Some(3_600));
        assert_eq!(config.tasks.skill_writer.min_idle_secs, Some(900));
    }

    #[test]
    fn project_config_patch_merges_without_clearing_omitted_fields() {
        let current = AutomationConfigPatch {
            enabled: Some(true),
            memory_curator: AutomationTaskPatch {
                enabled: Some(true),
                schedule: Some(Some("manual".to_string())),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        };
        let patch = AutomationConfigPatch {
            timeout_secs: Some(120),
            scheduler_tick_secs: Some(20),
            memory_curator: AutomationTaskPatch {
                schedule: Some(None),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        };

        let merged = merge_project_config(Some(current), patch);

        assert_eq!(merged.enabled, Some(true));
        assert_eq!(merged.model_id, None);
        assert_eq!(merged.timeout_secs, Some(120));
        assert_eq!(merged.scheduler_tick_secs, Some(20));
        assert_eq!(merged.memory_curator.enabled, Some(true));
        assert_eq!(merged.memory_curator.schedule, Some(None));
    }

    #[test]
    fn session_evidence_budget_backoff_is_patchable_clearable_and_nonzero() {
        let base = AutomationConfig::default();
        assert_eq!(
            base.tasks
                .session_reflector
                .session_evidence_budget_backoff_secs,
            None,
            "the window must default to unset so the typed contract's default applies"
        );

        let override_patch = AutomationConfigPatch {
            session_reflector: AutomationTaskPatch {
                session_evidence_budget_backoff_secs: Some(Some(120)),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        };
        let effective = effective_config(&base, Some(&override_patch)).unwrap();
        assert_eq!(
            effective
                .tasks
                .session_reflector
                .session_evidence_budget_backoff_secs,
            Some(120)
        );

        let clear_patch = AutomationConfigPatch {
            session_reflector: AutomationTaskPatch {
                session_evidence_budget_backoff_secs: Some(None),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        };
        let merged = merge_project_config(Some(override_patch.clone()), clear_patch);
        assert_eq!(
            merged
                .session_reflector
                .session_evidence_budget_backoff_secs,
            Some(None),
            "a later clear must override the earlier value in the merged project patch"
        );

        let zero_patch = AutomationConfigPatch {
            session_reflector: AutomationTaskPatch {
                session_evidence_budget_backoff_secs: Some(Some(0)),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        };
        let error = effective_config(&base, Some(&zero_patch)).unwrap_err();
        assert!(
            error.to_string().contains(
                "session_reflector session_evidence_budget_backoff_secs must be greater than zero"
            ),
            "zero windows must be rejected: {error}"
        );
    }

    #[test]
    fn non_codex_backend_wins_over_model_in_the_same_patch() {
        let current = AutomationConfig {
            enabled: true,
            backend: AutomationBackend::CodexAppServer,
            model_id: Some("gpt-5.6-mini".to_owned()),
            ..AutomationConfig::default()
        };
        let patch = AutomationConfigPatch {
            backend: Some(AutomationBackend::Disabled),
            model_id: Some(Some("must-not-survive".to_owned())),
            ..AutomationConfigPatch::default()
        };

        let merged = merge_project_config(None, patch.clone());
        let effective =
            effective_config(&current, Some(&patch)).expect("backend patch should be canonical");

        assert_eq!(merged.model_id, Some(None));
        assert_eq!(effective.backend, AutomationBackend::Disabled);
        assert_eq!(effective.model_id, None);
    }

    #[test]
    fn model_only_patch_is_cleared_while_backend_is_disabled() {
        let patch = AutomationConfigPatch {
            model_id: Some(Some("must-not-survive".to_owned())),
            ..AutomationConfigPatch::default()
        };

        let merged = merge_project_config(
            Some(AutomationConfigPatch {
                backend: Some(AutomationBackend::Disabled),
                ..AutomationConfigPatch::default()
            }),
            patch.clone(),
        );
        let disabled = AutomationConfig {
            enabled: false,
            backend: AutomationBackend::Disabled,
            model_id: None,
            tasks: AutomationTaskSet::default(),
            ..AutomationConfig::default()
        };
        let effective = effective_config(&disabled, Some(&patch))
            .expect("disabled backend should normalize away a model-only patch");

        assert_eq!(merged.model_id, Some(None));
        assert_eq!(effective.backend, AutomationBackend::Disabled);
        assert_eq!(effective.model_id, None);
    }

    #[test]
    fn merged_non_codex_backend_records_an_explicit_model_clear() {
        let current = AutomationConfigPatch {
            backend: Some(AutomationBackend::CodexAppServer),
            model_id: Some(Some("gpt-5.6-mini".to_owned())),
            ..AutomationConfigPatch::default()
        };
        let patch = AutomationConfigPatch {
            backend: Some(AutomationBackend::Disabled),
            ..AutomationConfigPatch::default()
        };

        let merged = merge_project_config(Some(current), patch);

        assert_eq!(merged.backend, Some(AutomationBackend::Disabled));
        assert_eq!(merged.model_id, Some(None));
    }

    #[test]
    fn codex_model_set_and_clear_preserve_explicit_patch_intent() {
        let codex = AutomationConfigPatch {
            backend: Some(AutomationBackend::CodexAppServer),
            ..AutomationConfigPatch::default()
        };
        let set = merge_project_config(
            Some(codex),
            AutomationConfigPatch {
                model_id: Some(Some("gpt-5.6-mini".to_owned())),
                ..AutomationConfigPatch::default()
            },
        );
        assert_eq!(
            set.model_id,
            Some(Some("gpt-5.6-mini".to_owned())),
            "Codex retains an explicitly pinned model"
        );

        let cleared = merge_project_config(
            Some(set),
            AutomationConfigPatch {
                model_id: Some(None),
                ..AutomationConfigPatch::default()
            },
        );
        assert_eq!(
            cleared.model_id,
            Some(None),
            "Codex retains an explicit model clear for validation"
        );
    }

    #[test]
    fn validation_rejects_invalid_task_schedule() {
        let patch = AutomationConfigPatch {
            skill_writer: AutomationTaskPatch {
                enabled: Some(true),
                schedule: Some(Some("after lunch".to_string())),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        };

        let error = effective_config(&AutomationConfig::default(), Some(&patch)).unwrap_err();
        assert!(error.to_string().contains("skill_writer schedule"));
    }

    #[test]
    fn validation_preserves_supported_cron_syntax() {
        for schedule in ["*/15 9-17 * * 1-5", "0 0 * * 7", "0 12 * * *"] {
            let patch = AutomationConfigPatch {
                skill_writer: AutomationTaskPatch {
                    schedule: Some(Some(schedule.to_string())),
                    ..AutomationTaskPatch::default()
                },
                ..AutomationConfigPatch::default()
            };
            effective_config(&AutomationConfig::default(), Some(&patch)).unwrap();
        }
        for schedule in ["60 * * * *", "*/0 * * * *", "5-1 * * * *"] {
            let patch = AutomationConfigPatch {
                skill_writer: AutomationTaskPatch {
                    schedule: Some(Some(schedule.to_string())),
                    ..AutomationTaskPatch::default()
                },
                ..AutomationConfigPatch::default()
            };
            assert!(effective_config(&AutomationConfig::default(), Some(&patch)).is_err());
        }
    }

    #[test]
    fn clearable_patch_fields_preserve_explicit_null() {
        let patch: AutomationConfigPatch = serde_json::from_value(serde_json::json!({
            "model_id": null,
            "memory_curator": {
                "schedule": null
            }
        }))
        .unwrap();

        assert_eq!(patch.model_id, Some(None));
        assert_eq!(patch.memory_curator.schedule, Some(None));
    }

    #[test]
    fn validation_rejects_zero_global_and_task_durations() {
        let mut config = AutomationConfig {
            timeout_secs: 0,
            ..AutomationConfig::default()
        };
        assert!(validate_config(&config).is_err());

        config.timeout_secs = 60;
        config.scheduler_tick_secs = 0;
        assert!(validate_config(&config).is_err());

        config.scheduler_tick_secs = 60;
        config.tasks.memory_curator.interval_secs = Some(0);
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn public_schedule_parser_preserves_interval_and_cron_behavior() {
        assert_eq!(
            parse_schedule(Some("hourly")).unwrap(),
            AutomationSchedule::Interval { every_secs: 3600 }
        );
        let AutomationSchedule::Cron(cron) = parse_schedule(Some("*/15 9-17 * * 1-5")).unwrap()
        else {
            panic!("expected cron schedule");
        };
        assert!(cron.matches(32_400));
        assert_eq!(cron.previous_occurrence(32_459), Some(32_400));
        assert!(validate_schedule(Some("hourly")).is_ok());
        assert!(validate_schedule(Some("after lunch")).is_err());
    }
}

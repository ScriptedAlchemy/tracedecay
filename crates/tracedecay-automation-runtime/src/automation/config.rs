//! Runtime adapter for daemon-owned automation configuration.

pub use tracedecay_automation::config::{
    AutomationBackend, AutomationConfig, AutomationConfigPatch, AutomationHostMode,
    AutomationSchedule, AutomationTaskConfig, AutomationTaskPatch, AutomationTaskSet, CronSchedule,
    DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS, DEFAULT_LEGACY_SESSION_RETENTION_DAYS,
    DEFAULT_SCHEDULER_TICK_SECS, RetentionConfig, parse_schedule,
};

use crate::errors::{Result, TraceDecayError};
use tracedecay_domain::configuration::{
    AUTOMATION_SETTINGS_SETTING_KEY, ConfigurationSnapshotV1, ConfigurationValueV1, SettingKey,
};

pub fn from_configuration_snapshot(snapshot: &ConfigurationSnapshotV1) -> Result<AutomationConfig> {
    snapshot
        .validate()
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid pinned configuration snapshot: {error}"),
        })?;
    let key = SettingKey::new(AUTOMATION_SETTINGS_SETTING_KEY).map_err(|error| {
        TraceDecayError::Config {
            message: format!("invalid automation setting key: {error}"),
        }
    })?;
    let ConfigurationValueV1::AutomationSettings(config) = snapshot
        .effective_values
        .get(&key)
        .ok_or_else(|| TraceDecayError::Config {
            message: "pinned configuration snapshot is missing automation settings".to_owned(),
        })?
    else {
        return Err(TraceDecayError::Config {
            message: "pinned automation setting has the wrong value kind".to_owned(),
        });
    };
    validate_config(config)?;
    Ok((**config).clone())
}

pub fn effective_config(
    global: &AutomationConfig,
    project: Option<&AutomationConfigPatch>,
) -> Result<AutomationConfig> {
    Ok(tracedecay_automation::config::effective_config(
        global, project,
    )?)
}

pub fn validate_config(config: &AutomationConfig) -> Result<()> {
    Ok(tracedecay_automation::config::validate_config(config)?)
}

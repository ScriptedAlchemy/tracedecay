//! Runtime adapter for daemon-owned automation configuration.

pub use tracedecay_automation::config::{
    AdmittedAutomationConfig, AutomationBackendV1, AutomationConfigPatch, AutomationHostModeV1,
    AutomationSchedule, AutomationSettingsV1, AutomationTaskPatch, AutomationTaskSetV1,
    AutomationTaskSettingsV1, CronSchedule, DEFAULT_SCHEDULER_TICK_SECS, RetentionConfig,
    ScheduleResolutionError, admit_config,
};

use tracedecay_domain::errors::Result;

pub fn effective_config(
    global: &AutomationSettingsV1,
    project: Option<&AutomationConfigPatch>,
) -> Result<AutomationSettingsV1> {
    Ok(tracedecay_automation::config::effective_config(
        global, project,
    )?)
}

pub fn validate_config(config: &AutomationSettingsV1) -> Result<()> {
    Ok(tracedecay_automation::config::validate_config(config)?)
}

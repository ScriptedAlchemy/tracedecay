//! Runtime adapter for daemon-owned automation configuration.

pub use tracedecay_automation::config::{
    AdmittedAutomationConfig, AutomationBackend, AutomationConfig, AutomationConfigPatch,
    AutomationHostMode, AutomationSchedule, AutomationTaskConfig, AutomationTaskPatch,
    AutomationTaskSet, CronSchedule, DEFAULT_SCHEDULER_TICK_SECS, RetentionConfig,
    ScheduleResolutionError, admit_config,
};

use tracedecay_domain::errors::Result;

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

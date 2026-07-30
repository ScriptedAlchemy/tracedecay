//! Runtime persistence adapters for leaf-owned automation configuration.

use std::path::{Path, PathBuf};

pub use tracedecay_automation::config::{
    AutomationBackend, AutomationConfig, AutomationConfigPatch, AutomationHostMode,
    AutomationSchedule, AutomationTaskConfig, AutomationTaskPatch, AutomationTaskSet, CronSchedule,
    DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS, DEFAULT_LEGACY_SESSION_RETENTION_DAYS,
    DEFAULT_SCHEDULER_TICK_SECS, RetentionConfig, default_user_automation_config, parse_schedule,
};

use crate::errors::{Result, TraceDecayError};

const PROJECT_CONFIG_FILENAME: &str = "automation_config.json";

pub fn effective_config(
    global: &AutomationConfig,
    project: Option<&AutomationConfigPatch>,
) -> Result<AutomationConfig> {
    Ok(tracedecay_automation::config::effective_config(
        global, project,
    )?)
}

pub fn merge_project_config(
    current: Option<AutomationConfigPatch>,
    patch: AutomationConfigPatch,
) -> AutomationConfigPatch {
    tracedecay_automation::config::merge_project_config(current, patch)
}

pub fn validate_config(config: &AutomationConfig) -> Result<()> {
    Ok(tracedecay_automation::config::validate_config(config)?)
}

pub fn project_config_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(PROJECT_CONFIG_FILENAME)
}

pub async fn effective_user_automation_config(
    profile_root: &Path,
    global: &AutomationConfig,
    global_configured: bool,
) -> Result<AutomationConfig> {
    let base = if global_configured {
        global.clone()
    } else {
        default_user_automation_config()
    };
    let dashboard_root = crate::automation::runner::user_automation_root(profile_root);
    let profile_patch = load_project_config(&dashboard_root).await?;
    effective_config(&base, profile_patch.as_ref())
}

pub async fn apply_project_config_patch(
    dashboard_root: &Path,
    global: &AutomationConfig,
    patch: AutomationConfigPatch,
) -> Result<(AutomationConfigPatch, AutomationConfig)> {
    let current = load_project_config(dashboard_root).await?;
    let project = merge_project_config(current, patch);
    let effective = effective_config(global, Some(&project))?;
    save_project_config(dashboard_root, &project).await?;
    Ok((project, effective))
}

pub async fn load_project_config(dashboard_root: &Path) -> Result<Option<AutomationConfigPatch>> {
    let path = project_config_path(dashboard_root);
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "failed to parse automation config '{}': {error}",
                        path.display()
                    ),
                })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TraceDecayError::Config {
            message: format!(
                "failed to read automation config '{}': {error}",
                path.display()
            ),
        }),
    }
}

pub async fn save_project_config(
    dashboard_root: &Path,
    config: &AutomationConfigPatch,
) -> Result<()> {
    let path = project_config_path(dashboard_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to create automation config directory '{}': {error}",
                    parent.display()
                ),
            })?;
    }
    let bytes = serde_json::to_vec_pretty(config).map_err(|error| TraceDecayError::Config {
        message: format!("failed to serialize automation config: {error}"),
    })?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to write automation config '{}': {error}",
                path.display()
            ),
        })
}

pub async fn clear_project_config(dashboard_root: &Path) -> Result<()> {
    let path = project_config_path(dashboard_root);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TraceDecayError::Config {
            message: format!(
                "failed to remove automation config '{}': {error}",
                path.display()
            ),
        }),
    }
}

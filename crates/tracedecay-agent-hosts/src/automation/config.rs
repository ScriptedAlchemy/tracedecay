use std::path::{Path, PathBuf};

pub use tracedecay_automation::config::*;

use crate::errors::{Result, TraceDecayError};

const PROJECT_CONFIG_FILENAME: &str = "automation_config.json";

pub fn project_config_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(PROJECT_CONFIG_FILENAME)
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

fn default_user_automation_config() -> AutomationConfig {
    let task = || AutomationTaskConfig {
        enabled: true,
        schedule: Some("manual".to_string()),
        ..AutomationTaskConfig::default()
    };
    AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        auto_apply_memory_ops: true,
        auto_enable_skills: true,
        tasks: AutomationTaskSet {
            memory_curator: task(),
            session_reflector: task(),
            skill_writer: task(),
        },
        ..AutomationConfig::default()
    }
}

pub fn merge_project_config(
    current: Option<AutomationConfigPatch>,
    patch: AutomationConfigPatch,
) -> AutomationConfigPatch {
    let mut merged = current.unwrap_or_default();
    merge_patch(&mut merged, patch);
    merged
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

pub fn validate_config(config: &AutomationConfig) -> Result<()> {
    if config.timeout_secs == 0 {
        return config_error("automation timeout_secs must be greater than zero");
    }
    if config.scheduler_tick_secs == 0 {
        return config_error("automation scheduler_tick_secs must be greater than zero");
    }
    validate_task_config("memory_curator", &config.tasks.memory_curator)?;
    validate_task_config("session_reflector", &config.tasks.session_reflector)?;
    validate_task_config("skill_writer", &config.tasks.skill_writer)?;
    Ok(())
}

pub async fn load_project_config(dashboard_root: &Path) -> Result<Option<AutomationConfigPatch>> {
    let path = project_config_path(dashboard_root);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "failed to parse automation config '{}': {error}",
                    path.display()
                ),
            }
        }),
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

fn apply_patch(config: &mut AutomationConfig, patch: &AutomationConfigPatch) {
    if let Some(value) = patch.enabled {
        config.enabled = value;
    }
    if let Some(value) = patch.backend {
        config.backend = value;
    }
    if let Some(value) = patch.host_mode {
        config.host_mode = value;
    }
    if let Some(value) = patch.timeout_secs {
        config.timeout_secs = value;
    }
    if let Some(value) = patch.scheduler_tick_secs {
        config.scheduler_tick_secs = value;
    }
    if let Some(value) = patch.auto_apply_memory_ops {
        config.auto_apply_memory_ops = value;
    }
    if let Some(value) = patch.auto_enable_skills {
        config.auto_enable_skills = value;
    }
    if let Some(value) = patch.export_memory_digest {
        config.export_memory_digest = value;
    }
    if let Some(value) = patch.combine_due_tasks {
        config.combine_due_tasks = value;
    }
    if let Some(value) = patch.allow_job_commands {
        config.allow_job_commands = value;
    }
    apply_task_patch(&mut config.tasks.memory_curator, &patch.memory_curator);
    apply_task_patch(
        &mut config.tasks.session_reflector,
        &patch.session_reflector,
    );
    apply_task_patch(&mut config.tasks.skill_writer, &patch.skill_writer);
}

fn apply_task_patch(config: &mut AutomationTaskConfig, patch: &AutomationTaskPatch) {
    if let Some(value) = patch.enabled {
        config.enabled = value;
    }
    if let Some(value) = &patch.schedule {
        config.schedule.clone_from(value);
    }
    if let Some(value) = patch.interval_secs {
        config.interval_secs = value;
    }
    if let Some(value) = patch.cooldown_secs {
        config.cooldown_secs = value;
    }
    if let Some(value) = patch.min_idle_secs {
        config.min_idle_secs = value;
    }
    if let Some(value) = patch.stale_lock_secs {
        config.stale_lock_secs = value;
    }
}

fn merge_patch(config: &mut AutomationConfigPatch, patch: AutomationConfigPatch) {
    merge_optional_field(&mut config.enabled, patch.enabled);
    merge_optional_field(&mut config.backend, patch.backend);
    merge_optional_field(&mut config.host_mode, patch.host_mode);
    merge_optional_field(&mut config.timeout_secs, patch.timeout_secs);
    merge_optional_field(&mut config.scheduler_tick_secs, patch.scheduler_tick_secs);
    merge_optional_field(
        &mut config.auto_apply_memory_ops,
        patch.auto_apply_memory_ops,
    );
    merge_optional_field(&mut config.auto_enable_skills, patch.auto_enable_skills);
    merge_optional_field(&mut config.export_memory_digest, patch.export_memory_digest);
    merge_optional_field(&mut config.combine_due_tasks, patch.combine_due_tasks);
    merge_optional_field(&mut config.allow_job_commands, patch.allow_job_commands);
    merge_task_patch(&mut config.memory_curator, patch.memory_curator);
    merge_task_patch(&mut config.session_reflector, patch.session_reflector);
    merge_task_patch(&mut config.skill_writer, patch.skill_writer);
}

fn merge_task_patch(config: &mut AutomationTaskPatch, patch: AutomationTaskPatch) {
    merge_optional_field(&mut config.enabled, patch.enabled);
    merge_optional_field(&mut config.schedule, patch.schedule);
    merge_optional_field(&mut config.interval_secs, patch.interval_secs);
    merge_optional_field(&mut config.cooldown_secs, patch.cooldown_secs);
    merge_optional_field(&mut config.min_idle_secs, patch.min_idle_secs);
    merge_optional_field(&mut config.stale_lock_secs, patch.stale_lock_secs);
}

fn merge_optional_field<T>(current: &mut Option<T>, patch: Option<T>) {
    if patch.is_some() {
        *current = patch;
    }
}

fn config_error<T>(message: impl Into<String>) -> Result<T> {
    Err(TraceDecayError::Config {
        message: message.into(),
    })
}

fn validate_task_config(task: &str, config: &AutomationTaskConfig) -> Result<()> {
    if matches!(config.interval_secs, Some(0)) {
        return config_error(format!("{task} interval_secs must be greater than zero"));
    }
    if matches!(config.cooldown_secs, Some(0)) {
        return config_error(format!("{task} cooldown_secs must be greater than zero"));
    }
    if matches!(config.min_idle_secs, Some(0)) {
        return config_error(format!("{task} min_idle_secs must be greater than zero"));
    }
    if matches!(config.stale_lock_secs, Some(0)) {
        return config_error(format!("{task} stale_lock_secs must be greater than zero"));
    }
    let schedule = super::scheduler::parse_schedule(config.schedule.as_deref()).map_err(|error| {
        TraceDecayError::Config {
            message: format!("{task} schedule is invalid: {error}"),
        }
    })?;
    if schedule == super::scheduler::AutomationSchedule::ConfiguredInterval
        && config.interval_secs.is_none()
    {
        return config_error(format!(
            "{task} interval_secs is required when schedule is interval"
        ));
    }
    Ok(())
}

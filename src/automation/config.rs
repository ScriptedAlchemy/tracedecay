use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

use crate::errors::{Result, TraceDecayError};

const PROJECT_CONFIG_FILENAME: &str = "automation_config.json";
pub const DEFAULT_SCHEDULER_TICK_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationBackend {
    #[default]
    Disabled,
    CodexAppServer,
    ExternalCommand,
}

impl AutomationBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::CodexAppServer => "codex_app_server",
            Self::ExternalCommand => "external_command",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationHostMode {
    #[default]
    Standalone,
    #[serde(alias = "hermes_hosted")]
    DelegatedHost,
}

impl AutomationHostMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::DelegatedHost => "delegated_host",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AutomationTaskConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_idle_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_lock_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AutomationTaskSet {
    #[serde(default)]
    pub memory_curator: AutomationTaskConfig,
    #[serde(default)]
    pub session_reflector: AutomationTaskConfig,
    #[serde(default)]
    pub skill_writer: AutomationTaskConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub backend: AutomationBackend,
    #[serde(default)]
    pub host_mode: AutomationHostMode,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_scheduler_tick_secs")]
    pub scheduler_tick_secs: u64,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default = "default_true")]
    pub require_dashboard_approval: bool,
    #[serde(default)]
    pub auto_apply_memory_ops: bool,
    #[serde(default)]
    pub auto_enable_skills: bool,
    /// When true (the default), a scheduler tick that finds both the session
    /// reflector and the skill writer due runs them as one combined backend
    /// call with shared evidence instead of two sequential runs.
    #[serde(default = "default_true")]
    pub combine_due_tasks: bool,
    #[serde(default)]
    pub tasks: AutomationTaskSet,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: AutomationBackend::Disabled,
            host_mode: AutomationHostMode::Standalone,
            model: None,
            timeout_secs: default_timeout_secs(),
            scheduler_tick_secs: default_scheduler_tick_secs(),
            max_tokens: None,
            temperature: None,
            require_dashboard_approval: true,
            auto_apply_memory_ops: false,
            auto_enable_skills: false,
            combine_due_tasks: true,
            tasks: AutomationTaskSet::default(),
        }
    }
}

impl AutomationConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
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
    pub model: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_tick_secs: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_tokens: Option<Option<u32>>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub temperature: Option<Option<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_dashboard_approval: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_apply_memory_ops: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_enable_skills: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combine_due_tasks: Option<bool>,
    #[serde(default)]
    pub memory_curator: AutomationTaskPatch,
    #[serde(default)]
    pub session_reflector: AutomationTaskPatch,
    #[serde(default)]
    pub skill_writer: AutomationTaskPatch,
}

fn default_true() -> bool {
    true
}

fn default_timeout_secs() -> u64 {
    60
}

fn default_scheduler_tick_secs() -> u64 {
    DEFAULT_SCHEDULER_TICK_SECS
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

pub fn merge_project_config(
    current: Option<AutomationConfigPatch>,
    patch: AutomationConfigPatch,
) -> AutomationConfigPatch {
    let mut merged = current.unwrap_or_default();
    merge_patch(&mut merged, patch);
    merged
}

/// Canonical load -> merge -> validate -> save pipeline for the project
/// automation sidecar. Returns the merged project patch and the validated
/// effective config; nothing is saved when validation fails.
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
    if matches!(config.max_tokens, Some(0)) {
        return config_error("automation max_tokens must be greater than zero");
    }
    if let Some(temperature) = config.temperature {
        if !temperature.is_finite() || temperature < 0.0 {
            return config_error("automation temperature must be finite and non-negative");
        }
    }
    if config.auto_enable_skills && !config.require_dashboard_approval {
        return config_error(
            "auto_enable_skills requires require_dashboard_approval until automation is trusted",
        );
    }
    validate_task_config("memory_curator", &config.tasks.memory_curator)?;
    validate_task_config("session_reflector", &config.tasks.session_reflector)?;
    validate_task_config("skill_writer", &config.tasks.skill_writer)?;
    Ok(())
}

pub async fn load_project_config(dashboard_root: &Path) -> Result<Option<AutomationConfigPatch>> {
    let path = project_config_path(dashboard_root);
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| TraceDecayError::Config {
                    message: format!(
                        "failed to parse automation config '{}': {e}",
                        path.display()
                    ),
                })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(TraceDecayError::Config {
            message: format!("failed to read automation config '{}': {e}", path.display()),
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
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to create automation config directory '{}': {e}",
                    parent.display()
                ),
            })?;
    }
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| TraceDecayError::Config {
        message: format!("failed to serialize automation config: {e}"),
    })?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to write automation config '{}': {e}",
                path.display()
            ),
        })
}

pub async fn clear_project_config(dashboard_root: &Path) -> Result<()> {
    let path = project_config_path(dashboard_root);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(TraceDecayError::Config {
            message: format!(
                "failed to remove automation config '{}': {e}",
                path.display()
            ),
        }),
    }
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
    if let Some(model) = &patch.model {
        config.model.clone_from(model);
    }
    if let Some(timeout_secs) = patch.timeout_secs {
        config.timeout_secs = timeout_secs;
    }
    if let Some(scheduler_tick_secs) = patch.scheduler_tick_secs {
        config.scheduler_tick_secs = scheduler_tick_secs;
    }
    if let Some(max_tokens) = patch.max_tokens {
        config.max_tokens = max_tokens;
    }
    if let Some(temperature) = patch.temperature {
        config.temperature = temperature;
    }
    if let Some(require_dashboard_approval) = patch.require_dashboard_approval {
        config.require_dashboard_approval = require_dashboard_approval;
    }
    if let Some(auto_apply_memory_ops) = patch.auto_apply_memory_ops {
        config.auto_apply_memory_ops = auto_apply_memory_ops;
    }
    if let Some(auto_enable_skills) = patch.auto_enable_skills {
        config.auto_enable_skills = auto_enable_skills;
    }
    if let Some(combine_due_tasks) = patch.combine_due_tasks {
        config.combine_due_tasks = combine_due_tasks;
    }
    apply_task_patch(&mut config.tasks.memory_curator, &patch.memory_curator);
    apply_task_patch(
        &mut config.tasks.session_reflector,
        &patch.session_reflector,
    );
    apply_task_patch(&mut config.tasks.skill_writer, &patch.skill_writer);
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
}

fn merge_patch(config: &mut AutomationConfigPatch, patch: AutomationConfigPatch) {
    merge_optional_field(&mut config.enabled, patch.enabled);
    merge_optional_field(&mut config.backend, patch.backend);
    merge_optional_field(&mut config.host_mode, patch.host_mode);
    merge_optional_field(&mut config.model, patch.model);
    merge_optional_field(&mut config.timeout_secs, patch.timeout_secs);
    merge_optional_field(&mut config.scheduler_tick_secs, patch.scheduler_tick_secs);
    merge_optional_field(&mut config.max_tokens, patch.max_tokens);
    merge_optional_field(&mut config.temperature, patch.temperature);
    merge_optional_field(
        &mut config.require_dashboard_approval,
        patch.require_dashboard_approval,
    );
    merge_optional_field(
        &mut config.auto_apply_memory_ops,
        patch.auto_apply_memory_ops,
    );
    merge_optional_field(&mut config.auto_enable_skills, patch.auto_enable_skills);
    merge_optional_field(&mut config.combine_due_tasks, patch.combine_due_tasks);
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
    let schedule = super::scheduler::parse_schedule(config.schedule.as_deref()).map_err(|err| {
        TraceDecayError::Config {
            message: format!("{task} schedule is invalid: {err}"),
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

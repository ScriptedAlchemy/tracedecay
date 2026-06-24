use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{Result, TraceDecayError};

const PROJECT_CONFIG_FILENAME: &str = "automation_config.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationBackend {
    #[default]
    Disabled,
    CodexAppServer,
    ExternalCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationHostMode {
    #[default]
    Standalone,
    HermesHosted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AutomationTaskConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub schedule: Option<String>,
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
            max_tokens: None,
            temperature: None,
            require_dashboard_approval: true,
            auto_apply_memory_ops: false,
            auto_enable_skills: false,
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
pub struct AutomationTaskPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AutomationConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<AutomationBackend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_mode: Option<AutomationHostMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<Option<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<Option<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_dashboard_approval: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_apply_memory_ops: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_enable_skills: Option<bool>,
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

pub fn validate_config(config: &AutomationConfig) -> Result<()> {
    if config.timeout_secs == 0 {
        return config_error("automation timeout_secs must be greater than zero");
    }
    if config.auto_apply_memory_ops && !config.require_dashboard_approval {
        return config_error(
            "auto_apply_memory_ops requires require_dashboard_approval until automation is trusted",
        );
    }
    if config.auto_enable_skills && !config.require_dashboard_approval {
        return config_error(
            "auto_enable_skills requires require_dashboard_approval until automation is trusted",
        );
    }
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
}

fn merge_patch(config: &mut AutomationConfigPatch, patch: AutomationConfigPatch) {
    if patch.enabled.is_some() {
        config.enabled = patch.enabled;
    }
    if patch.backend.is_some() {
        config.backend = patch.backend;
    }
    if patch.host_mode.is_some() {
        config.host_mode = patch.host_mode;
    }
    if patch.model.is_some() {
        config.model = patch.model;
    }
    if patch.timeout_secs.is_some() {
        config.timeout_secs = patch.timeout_secs;
    }
    if patch.max_tokens.is_some() {
        config.max_tokens = patch.max_tokens;
    }
    if patch.temperature.is_some() {
        config.temperature = patch.temperature;
    }
    if patch.require_dashboard_approval.is_some() {
        config.require_dashboard_approval = patch.require_dashboard_approval;
    }
    if patch.auto_apply_memory_ops.is_some() {
        config.auto_apply_memory_ops = patch.auto_apply_memory_ops;
    }
    if patch.auto_enable_skills.is_some() {
        config.auto_enable_skills = patch.auto_enable_skills;
    }
    merge_task_patch(&mut config.memory_curator, patch.memory_curator);
    merge_task_patch(&mut config.session_reflector, patch.session_reflector);
    merge_task_patch(&mut config.skill_writer, patch.skill_writer);
}

fn merge_task_patch(config: &mut AutomationTaskPatch, patch: AutomationTaskPatch) {
    if patch.enabled.is_some() {
        config.enabled = patch.enabled;
    }
    if patch.schedule.is_some() {
        config.schedule = patch.schedule;
    }
}

fn config_error<T>(message: impl Into<String>) -> Result<T> {
    Err(TraceDecayError::Config {
        message: message.into(),
    })
}

use serde::{Deserialize, Deserializer, Serialize};

use crate::retention::RetentionConfig;

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
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_scheduler_tick_secs")]
    pub scheduler_tick_secs: u64,
    /// Legacy compatibility setting. Autonomous memory curation always
    /// validates and applies accepted operations; explicit preview APIs remain
    /// read-only until their caller requests apply.
    #[serde(default = "default_true")]
    pub auto_apply_memory_ops: bool,
    #[serde(default)]
    pub auto_enable_skills: bool,
    /// Export the trust-ranked durable-facts memory digest into host
    /// prompts alongside managed skills. See `automation::memory_digest`.
    #[serde(default = "default_true")]
    pub export_memory_digest: bool,
    /// When true (the default), a scheduler tick that finds both the session
    /// reflector and the skill writer due runs them as one combined backend
    /// call with shared evidence instead of two sequential runs.
    #[serde(default = "default_true")]
    pub combine_due_tasks: bool,
    /// Allows user-defined jobs to run their optional pre-run shell command.
    /// Off by default: jobs with a command are refused until the operator
    /// opts in.
    #[serde(default)]
    pub allow_job_commands: bool,
    /// Scheduled retention windows for the largest append-only telemetry
    /// tables. Analytics keeps 180 days by default; the lossless session
    /// tables are never pruned unless the operator sets an explicit window.
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub tasks: AutomationTaskSet,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: AutomationBackend::Disabled,
            host_mode: AutomationHostMode::Standalone,
            timeout_secs: default_timeout_secs(),
            scheduler_tick_secs: default_scheduler_tick_secs(),
            auto_apply_memory_ops: true,
            auto_enable_skills: false,
            export_memory_digest: true,
            combine_due_tasks: true,
            allow_job_commands: false,
            retention: RetentionConfig::default(),
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
        skip_serializing
    )]
    pub model: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_tick_secs: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing
    )]
    pub max_tokens: Option<Option<u32>>,
    #[serde(
        default,
        deserialize_with = "deserialize_clearable_field",
        skip_serializing
    )]
    pub temperature: Option<Option<f32>>,
    /// Deprecated: automation applies its output without any human approval, so
    /// this flag no longer gates anything. It is still parsed from legacy
    /// on-disk configs for back-compat (and never re-serialized) but is ignored
    /// by `apply_patch`/`merge_patch`. The effective, autonomous apply policy is
    /// surfaced by `tracedecay automation config get`
    /// (`explanation.effective_apply_policy`).
    #[serde(default, skip_serializing)]
    pub require_dashboard_approval: Option<bool>,
    /// Legacy compatibility setting; autonomous memory curation ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_apply_memory_ops: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_enable_skills: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_memory_digest: Option<bool>,
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

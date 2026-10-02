//! Managed-skill packages, their per-skill usage summaries and the
//! stale/improvement recommendations the skill-inspection tools return.

use std::collections::BTreeSet;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SkillInstallTarget {
    Cursor,
    Codex,
    Claude,
    Agents,
    #[serde(rename = "opencode")]
    OpenCode,
    Kimi,
    Kiro,
    Hermes,
}

impl SkillInstallTarget {
    pub fn is_native_overlay(self) -> bool {
        matches!(self, Self::Cursor | Self::Codex | Self::Hermes)
    }

    pub fn prompt_label(self) -> &'static str {
        match self {
            Self::Cursor => "Cursor",
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Agents => "AGENTS.md",
            Self::OpenCode => "OpenCode",
            Self::Kimi => "Kimi",
            Self::Kiro => "Kiro",
            Self::Hermes => "Hermes",
        }
    }
}

pub fn default_managed_skill_targets() -> Vec<SkillInstallTarget> {
    vec![
        SkillInstallTarget::Cursor,
        SkillInstallTarget::Codex,
        SkillInstallTarget::Claude,
        SkillInstallTarget::Agents,
        SkillInstallTarget::OpenCode,
        SkillInstallTarget::Kimi,
        SkillInstallTarget::Kiro,
        SkillInstallTarget::Hermes,
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSkillSource {
    AutomationRun,
    User,
    Import,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSkillState {
    Active,
    Disabled,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSkillMaterializationScope {
    #[default]
    Global,
    Project,
}

impl ManagedSkillMaterializationScope {
    pub fn materializes_into_projects(self) -> bool {
        matches!(self, Self::Project)
    }

    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn is_global(&self) -> bool {
        matches!(self, Self::Global)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManagedSkillProvenance {
    pub source: ManagedSkillSource,
    pub actor: String,
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManagedSupportFile {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManagedSkillMetadata {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub routing_description: String,
    pub category: String,
    #[serde(default = "default_managed_skill_targets")]
    pub targets: Vec<SkillInstallTarget>,
    pub state: ManagedSkillState,
    #[serde(
        default,
        skip_serializing_if = "ManagedSkillMaterializationScope::is_global"
    )]
    pub materialization_scope: ManagedSkillMaterializationScope,
    pub pinned: bool,
    pub checksum: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absorbed_into: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_reason: Option<String>,
    pub provenance: ManagedSkillProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManagedSkill {
    pub metadata: ManagedSkillMetadata,
    pub body_markdown: String,
    pub support_files: Vec<ManagedSupportFile>,
}

/// One skill's durable usage counters. The ledger stores one per skill; a
/// summary is the same record merged with the skill's current metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillUsageRecord {
    pub schema_version: u32,
    pub skill_id: String,
    pub title: Option<String>,
    pub category: Option<String>,
    pub state: Option<ManagedSkillState>,
    pub pinned: bool,
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_source: Option<ManagedSkillSource>,
    #[serde(default)]
    pub targets: Vec<String>,
    pub view_count: u64,
    pub use_count: u64,
    pub patch_count: u64,
    pub first_seen_at: i64,
    pub last_activity_at: i64,
    pub last_viewed_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub last_patched_at: Option<i64>,
    /// When the skill last transitioned into the active state; mirrors the
    /// managed skill metadata so outcome scoring works from summaries alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activated_at: Option<i64>,
    /// View/use totals captured at activation time so activity since activation
    /// is an exact delta rather than a heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_count_at_activation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_count_at_activation: Option<u64>,
    /// Import keys that already counted toward this skill. They are not a
    /// store-wide set: each key names this skill, so another skill must not
    /// share the write.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub imported_analytics_events: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillStaleRecommendation {
    pub skill_id: String,
    pub stale: bool,
    pub recommendation: String,
    pub reason: String,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillImprovementRecommendation {
    pub skill_id: String,
    pub improvement: bool,
    pub recommendation: String,
    pub reason: String,
    pub priority: String,
    #[serde(default)]
    pub evidence: Vec<String>,
}

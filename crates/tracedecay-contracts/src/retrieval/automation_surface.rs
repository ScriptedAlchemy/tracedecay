//! Canonical CLI/MCP wire contracts for the automation-run, managed-skill and
//! Hermes-inventory reads the project's graph-tool owner answers.
//!
//! Presentation-only transport keys such as `format` are removed before these
//! request bodies are decoded.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::automation::{
    AgentTaskKind, AutomationRunArtifact, AutomationRunArtifactKind, AutomationRunLedgerRecord,
    AutomationRunStatus, AutomationTrigger, HermesSkillBridgeSnapshot, ManagedSkill,
    ManagedSkillMetadata, ManagedSkillState, SkillImprovementRecommendation,
    SkillStaleRecommendation, SkillUsageRecord,
};

/// Largest automation run page one list call returns.
pub const AUTOMATION_RUN_LIST_MAX_LIMIT: u32 = 200;
/// Automation run page size when the caller names none.
pub const AUTOMATION_RUN_LIST_DEFAULT_LIMIT: u32 = 50;

/// The settled status every successful read in this family reports.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationReadStatusV1 {
    Ok,
}

/// Automation run reads answer from the active project's ledger only.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunScopeV1 {
    ActiveProject,
}

/// Whether a run page read every ledger row it examined.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRunPageCompletenessV1 {
    /// Every examined row decoded.
    Known,
    /// The page skipped malformed rows or more runs exist past it.
    Partial,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunListSurfaceRequestV1 {
    /// Maximum run records to return (default: 50, max: 200).
    #[schemars(range(min = 1, max = 200))]
    pub limit: Option<u32>,
}

/// One run as the list shows it: the ledger record's identity, outcome and
/// artifact kinds, without the artifact payloads or operation bodies.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunSummaryV1 {
    pub run_id: String,
    pub task: AgentTaskKind,
    pub task_key: Option<String>,
    pub trigger: AutomationTrigger,
    pub backend: String,
    pub model: Option<String>,
    pub status: AutomationRunStatus,
    pub reviewed_count: usize,
    pub accepted_count: usize,
    pub rejected_count: usize,
    pub skipped_count: usize,
    pub error: Option<String>,
    pub started_at: String,
    pub completed_at: String,
    pub artifact_kinds: Vec<String>,
}

impl AutomationRunSummaryV1 {
    pub fn of(record: &AutomationRunLedgerRecord) -> Self {
        Self {
            run_id: record.run_id.clone(),
            task: record.task,
            task_key: record.task_key.clone(),
            trigger: record.trigger,
            backend: record.backend.clone(),
            model: record.model.clone(),
            status: record.status,
            reviewed_count: record.reviewed_count,
            accepted_count: record.accepted_count,
            rejected_count: record.rejected_count,
            skipped_count: record.skipped_count,
            error: record.error.clone(),
            started_at: record.started_at.clone(),
            completed_at: record.completed_at.clone(),
            artifact_kinds: record
                .artifacts
                .iter()
                .map(|artifact| artifact.kind.clone())
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunListResultV1 {
    pub status: AutomationReadStatusV1,
    pub scope: AutomationRunScopeV1,
    pub runs: Vec<AutomationRunSummaryV1>,
    pub count: usize,
    pub limit: u32,
    pub has_more: bool,
    pub malformed_row_count: usize,
    pub completeness: AutomationRunPageCompletenessV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunViewSurfaceRequestV1 {
    /// Exact automation run id to inspect.
    #[schemars(length(min = 1))]
    pub run_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunViewResultV1 {
    pub status: AutomationReadStatusV1,
    pub scope: AutomationRunScopeV1,
    pub run: AutomationRunLedgerRecord,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunArtifactViewSurfaceRequestV1 {
    /// Automation run id to inspect.
    pub run_id: String,
    /// Artifact kind to read.
    pub kind: AutomationRunArtifactKind,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunArtifactViewResultV1 {
    pub status: AutomationReadStatusV1,
    pub run_id: String,
    pub artifact: AutomationRunArtifact,
    /// The hash-verified artifact document, as the run wrote it.
    pub payload: Value,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillListSurfaceRequestV1 {
    /// Optional managed-skill lifecycle state filter.
    pub state: Option<ManagedSkillState>,
    /// If true, include each skill's body_markdown in the list response
    /// (default: false).
    #[serde(default)]
    pub include_body: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillListEntryV1 {
    pub metadata: ManagedSkillMetadata,
    pub support_file_count: usize,
    pub support_file_paths: Vec<String>,
    pub usage_summary: SkillUsageRecord,
    pub stale_recommendation: Option<SkillStaleRecommendation>,
    pub improvement_recommendation: Option<SkillImprovementRecommendation>,
    /// Present only when the request asked for bodies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_markdown: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillListResultV1 {
    pub status: AutomationReadStatusV1,
    pub profile_root: PathBuf,
    pub count: usize,
    pub skills: Vec<SkillListEntryV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillViewSurfaceRequestV1 {
    /// Managed skill id to read.
    pub id: String,
    /// Include support-file byte payloads. Default false; the response still
    /// lists each path and byte length.
    #[serde(default)]
    pub include_support_files: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSupportFileSummaryV1 {
    pub path: String,
    pub byte_len: usize,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillViewResultV1 {
    pub status: AutomationReadStatusV1,
    pub profile_root: PathBuf,
    /// The skill package; `support_files` is empty unless the request
    /// included them.
    pub skill: ManagedSkill,
    pub usage_summary: SkillUsageRecord,
    pub stale_recommendation: Option<SkillStaleRecommendation>,
    pub improvement_recommendation: Option<SkillImprovementRecommendation>,
    pub support_files_included: bool,
    pub support_file_summaries: Vec<SkillSupportFileSummaryV1>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HermesSkillBridgeSurfaceRequestV1 {
    /// Include bounded SKILL.md contents (default: false).
    #[serde(default)]
    pub include_skill_bodies: bool,
    /// Include staged Hermes skill-write payloads (default: false).
    #[serde(default)]
    pub include_pending_payloads: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HermesSkillBridgeResultV1 {
    pub status: AutomationReadStatusV1,
    pub bridge: HermesSkillBridgeSnapshot,
}

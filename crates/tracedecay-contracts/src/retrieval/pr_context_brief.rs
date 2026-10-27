//! Compact PR orientation shared by MCP, CLI, and generated SDKs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{GitCommitSubjectV1, GitFileRoleV1, PrimitiveSearchFreshnessV1};

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrContextViewV1 {
    #[default]
    Brief,
    /// Exact changed-symbol enumeration, with its existing continuation cursor.
    Symbols,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrContextSourceStateV1 {
    Ready,
    Partial,
    Stale,
    Unavailable,
    NotPublished,
    Denied,
    NotMounted,
    Ambiguous,
    NotFound,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextSourceV1 {
    pub state: PrContextSourceStateV1,
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextPullRequestV1 {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub draft: bool,
    /// Sanitized retained description, when the provider supplied it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub description_truncated: bool,
    pub observed_head: String,
    pub observed_at: i64,
    pub matches_head: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextDiscussionV1 {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub kind: String,
    pub lifecycle: String,
    pub review_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub text_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_anchor: Option<String>,
    pub observed_at: i64,
    pub matches_head: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextCheckV1 {
    pub name: String,
    pub workflow: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
    pub head: String,
    pub matches_head: bool,
    pub observed_at: i64,
    pub evidence_anchor: String,
    pub locations: Vec<PrContextLocationV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextHostedV1 {
    pub source: PrContextSourceV1,
    pub ci_source: PrContextSourceV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PrContextPullRequestV1>,
    pub discussions: Vec<PrContextDiscussionV1>,
    pub checks: Vec<PrContextCheckV1>,
    pub discussions_total: usize,
    pub checks_total: usize,
    /// A retained CI inventory is not an enumeration of required checks.
    pub required_checks_known: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextLocationV1 {
    pub file: String,
    pub line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextExcerptV1 {
    pub revision: String,
    pub location: PrContextLocationV1,
    pub text: String,
    /// True when this is only an excerpt of the declaration.
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextEntryPointV1 {
    pub location: PrContextLocationV1,
    pub revision: String,
    pub symbol: String,
    pub change: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_excerpt: Option<PrContextExcerptV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_excerpt: Option<PrContextExcerptV1>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextFileV1 {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_file: Option<String>,
    pub change: String,
    pub role: GitFileRoleV1,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub symbols: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextAreaV1 {
    pub area: String,
    pub files: usize,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextImpactV1 {
    pub changed_symbol: String,
    pub consumer: String,
    pub location: PrContextLocationV1,
    pub relation: String,
    /// Whether the consumer's file is in the PR diff; this does not imply
    /// that the consumer declaration itself changed.
    pub consumer_changed: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextQualityV1 {
    pub signal: String,
    pub observation: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextBriefBudgetV1 {
    pub requested_tokens: u32,
    /// Whole-result JSON byte estimate, divided by four; not a tokenizer count.
    pub estimated_tokens: u32,
    pub over_budget: bool,
    pub files_omitted: usize,
    pub entry_points_omitted: usize,
    pub discussions_omitted: usize,
    pub checks_omitted: usize,
    pub impact_omitted: usize,
    pub commits_omitted: usize,
    pub areas_omitted: usize,
    pub quality_omitted: usize,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextBriefV1 {
    pub view: PrContextViewV1,
    pub base: String,
    pub head: String,
    pub base_oid: String,
    pub head_oid: String,
    pub merge_base: String,
    pub files_changed: usize,
    pub additions: u64,
    pub deletions: u64,
    pub commits: Vec<GitCommitSubjectV1>,
    pub hosted: PrContextHostedV1,
    pub areas: Vec<PrContextAreaV1>,
    pub changes: Vec<PrContextFileV1>,
    /// Prioritized places to begin reading the change, with exact revisions.
    pub entry_points: Vec<PrContextEntryPointV1>,
    pub impact: Vec<PrContextImpactV1>,
    pub quality: Vec<PrContextQualityV1>,
    pub source_analysis: PrContextSourceV1,
    pub graph_analysis: PrContextSourceV1,
    pub next_steps: Vec<String>,
    pub budget: PrContextBriefBudgetV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<PrimitiveSearchFreshnessV1>,
}

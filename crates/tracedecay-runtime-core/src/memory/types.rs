use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::FactCategoryV1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    General,
    UserPref,
    Project,
    Tool,
    Decision,
    CodeArea,
}

impl MemoryCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::UserPref => "user_pref",
            Self::Project => "project",
            Self::Tool => "tool",
            Self::Decision => "decision",
            Self::CodeArea => "code_area",
        }
    }

    pub fn from_proposal_label(value: &str) -> Result<Self, ParseMemoryCategoryError> {
        if let Ok(category) = value.parse::<Self>() {
            return Ok(category);
        }
        let normalized = normalized_category_label(value);
        match normalized.as_str() {
            "tool_guidance" | "tool_use" | "tool_usage" | "tooling" => Ok(Self::Tool),
            "workflow_preference" | "workflow_preferences" | "user_workflow_preference" => {
                Ok(Self::UserPref)
            }
            "workflow_requirement" | "workflow_policy" | "project_requirement" => {
                Ok(Self::Decision)
            }
            "workflow" | "process" | "procedure" | "guidance" => Ok(Self::General),
            _ => Err(ParseMemoryCategoryError {
                value: value.to_string(),
            }),
        }
    }
}

impl fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The V1 compatibility projection of a canonical fact category.
///
/// `MemoryCategory` is the permanent V1 surface for the canonical
/// [`FactCategoryV1`]; the two enums are kept variant-for-variant identical on
/// purpose. This pair of conversions is the only mapping between them, so a
/// new canonical variant fails to compile here instead of drifting across
/// hand-written tables in the automation, dashboard, and storage paths.
impl From<FactCategoryV1> for MemoryCategory {
    fn from(category: FactCategoryV1) -> Self {
        match category {
            FactCategoryV1::General => Self::General,
            FactCategoryV1::UserPref => Self::UserPref,
            FactCategoryV1::Project => Self::Project,
            FactCategoryV1::Tool => Self::Tool,
            FactCategoryV1::Decision => Self::Decision,
            FactCategoryV1::CodeArea => Self::CodeArea,
        }
    }
}

impl From<MemoryCategory> for FactCategoryV1 {
    fn from(category: MemoryCategory) -> Self {
        match category {
            MemoryCategory::General => Self::General,
            MemoryCategory::UserPref => Self::UserPref,
            MemoryCategory::Project => Self::Project,
            MemoryCategory::Tool => Self::Tool,
            MemoryCategory::Decision => Self::Decision,
            MemoryCategory::CodeArea => Self::CodeArea,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseMemoryCategoryError {
    value: String,
}

impl fmt::Display for ParseMemoryCategoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown memory category: {}", self.value)
    }
}

impl std::error::Error for ParseMemoryCategoryError {}

impl FromStr for MemoryCategory {
    type Err = ParseMemoryCategoryError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = normalized_category_label(value);
        match normalized.as_str() {
            "general" => Ok(Self::General),
            "user_pref" | "user_preference" | "user_preferences" => Ok(Self::UserPref),
            "project" => Ok(Self::Project),
            "tool" => Ok(Self::Tool),
            "decision" => Ok(Self::Decision),
            "code_area" | "code" => Ok(Self::CodeArea),
            _ => Err(ParseMemoryCategoryError {
                value: value.to_string(),
            }),
        }
    }
}

fn normalized_category_label(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FactRecord {
    pub fact_id: i64,
    pub content: String,
    pub category: MemoryCategory,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub trust_score: f64,
    pub source: Option<String>,
    pub retrieval_count: i64,
    /// Times this fact was RETURNED from a recall search (`FactRetriever::
    /// search`), as opposed to `retrieval_count`, which also counts probe,
    /// list, related, and reason scans.
    #[serde(default)]
    pub access_count: i64,
    pub helpful_count: i64,
    pub unhelpful_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_retrieved_at: Option<i64>,
    /// Timestamp of the most recent recall-search return (see `access_count`).
    #[serde(default)]
    pub last_recalled_at: Option<i64>,
    pub last_feedback_at: Option<i64>,
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntityRecord {
    pub entity_id: i64,
    pub name: String,
    pub normalized_name: String,
    pub entity_type: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactRelationKind {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
}

impl FactRelationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::Supersedes => "supersedes",
            Self::DerivedFrom => "derived_from",
        }
    }
}

impl fmt::Display for FactRelationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FactRelationKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "supports" => Ok(Self::Supports),
            "contradicts" => Ok(Self::Contradicts),
            "supersedes" => Ok(Self::Supersedes),
            "derived_from" => Ok(Self::DerivedFrom),
            other => Err(format!("unsupported fact relation kind: {other}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FactRelationRecord {
    pub source_fact_id: i64,
    pub target_fact_id: i64,
    pub relation: FactRelationKind,
    pub confidence: f64,
    pub source: String,
    pub metadata: Value,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EntityGroomingResult {
    pub winner_entity_id: i64,
    pub merged_entity_ids: Vec<i64>,
    pub aliases: Vec<String>,
    pub rewired_fact_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MemoryGroomingOperation {
    NormalizeTags {
        fact_id: i64,
        tags: Vec<String>,
        evidence_fact_ids: Vec<i64>,
        confidence: f64,
    },
    MergeEntities {
        winner_entity_id: i64,
        loser_entity_ids: Vec<i64>,
        evidence_fact_ids: Vec<i64>,
        confidence: f64,
    },
    AddAlias {
        entity_id: i64,
        alias: String,
        evidence_fact_ids: Vec<i64>,
        confidence: f64,
    },
    LinkFacts {
        source_fact_id: i64,
        target_fact_id: i64,
        relation: FactRelationKind,
        evidence_fact_ids: Vec<i64>,
        confidence: f64,
        source: String,
        #[serde(default)]
        metadata: Value,
    },
    RepairVector {
        fact_id: i64,
        evidence_fact_ids: Vec<i64>,
        confidence: f64,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryGroomingReport {
    pub normalized_tags: usize,
    pub merged_entities: usize,
    pub aliases_added: usize,
    pub facts_linked: usize,
    pub vectors_repaired: usize,
    pub derived_repair: MemoryRepairStats,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FactSearchResult {
    pub fact: FactRecord,
    pub score: f64,
    pub fts_score: f64,
    pub jaccard_score: f64,
    pub holographic_score: f64,
    pub trust_score: f64,
    pub why: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContradictionResult {
    pub existing_fact: FactRecord,
    pub new_content: String,
    pub score: f64,
    pub why: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackAction {
    Helpful,
    Unhelpful,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedbackRequest {
    pub fact_id: i64,
    pub action: FeedbackAction,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default, alias = "reason")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedbackResult {
    pub event_id: i64,
    pub fact_id: i64,
    pub action: FeedbackAction,
    pub old_trust: f64,
    pub new_trust: f64,
    pub trust_delta: f64,
    pub helpful_count: i64,
    pub unhelpful_count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrustHistoryEntry {
    pub timestamp: i64,
    pub action: FeedbackAction,
    pub old_trust: f64,
    pub new_trust: f64,
    pub delta: f64,
    pub source: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryRepairStats {
    pub missing_vectors_repaired: usize,
    pub banks_rebuilt: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryStatus {
    pub fact_count: usize,
    pub entity_count: usize,
    pub bank_count: usize,
    pub algebra_name: String,
    pub hrr_dim: usize,
    pub estimated_capacity: usize,
    pub trust_0_025_count: usize,
    pub trust_025_050_count: usize,
    pub trust_050_075_count: usize,
    pub trust_075_100_count: usize,
    pub below_default_recall_threshold_count: usize,
    pub helpful_count: usize,
    pub unhelpful_count: usize,
    pub missing_vector_count: usize,
    pub repair: MemoryRepairStats,
    /// Adoption-funnel numbers for the fact store's feedback loop: how many
    /// facts get seen (retrieved) vs. how many ever get rated. Surfaced so a
    /// model or user can see funnel health in one call instead of running
    /// ad-hoc SQL against `memory_facts` / `memory_feedback_events`.
    pub feedback_funnel: MemoryFeedbackFunnel,
}

/// Fact-store adoption funnel: seen (retrieved/accessed) vs. rated
/// (helpful/unhelpful). A dead funnel — facts seen many times but never
/// rated — means trust scores stay entirely seed-time values, never earned.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryFeedbackFunnel {
    /// Sum of `memory_facts.retrieval_count` across all facts — total times
    /// any fact was returned by a recall/search query.
    pub retrieval_count_total: i64,
    /// Sum of `memory_facts.access_count` across all facts — total
    /// recall-search-return accesses (see [`crate::memory::types::FactRecord::access_count`]).
    pub access_count_total: i64,
    /// Facts that have ever been retrieved at least once
    /// (`retrieval_count > 0`).
    pub retrieved_fact_count: usize,
    /// Facts that have ever received at least one helpful or unhelpful
    /// rating via `tracedecay_fact_feedback`.
    pub rated_fact_count: usize,
    /// `helpful_count + unhelpful_count` summed across all facts — total
    /// feedback events recorded.
    pub feedback_total: usize,
    /// `(retrieval_count_total + access_count_total) / feedback_total`,
    /// rounded down. `None` when `feedback_total` is zero (undefined ratio;
    /// the loop is entirely dead rather than merely sparse).
    pub seen_to_feedback_ratio: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddFactRequest {
    pub content: String,
    pub category: MemoryCategory,
    pub source: Option<String>,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub trust: Option<f64>,
    pub metadata: Value,
}

/// How a newly added fact relates to the existing store. Returned as part of
/// [`AddFactOutcome`]; purely a report — `add_fact` never auto-merges or
/// auto-deletes based on it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddFactDiffKind {
    /// New information; no strong overlap with stored facts.
    Add,
    /// Strong overlap (similarity > 0.9) with an existing fact.
    NearDuplicate,
    /// Similar (≥ 0.7) to an existing fact AND one side carries a
    /// negation/state-change cue — likely supersession or contradiction.
    PossibleConflict,
    /// The content matched a conservative secret-likeness rule and was NOT
    /// stored.
    RejectedSecretLike,
}

impl AddFactDiffKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::NearDuplicate => "near_duplicate",
            Self::PossibleConflict => "possible_conflict",
            Self::RejectedSecretLike => "rejected_secret_like",
        }
    }
}

/// Write-time diff report attached to every `add_fact` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddFactDiff {
    pub diff: AddFactDiffKind,
    /// The strongest existing match, when one scored above the report floor.
    pub closest_fact_id: Option<i64>,
    pub similarity: Option<f64>,
    pub reason: Option<String>,
}

impl AddFactDiff {
    pub(crate) const fn plain_add() -> Self {
        Self {
            diff: AddFactDiffKind::Add,
            closest_fact_id: None,
            similarity: None,
            reason: None,
        }
    }
}

/// Result of an `add_fact` call: the stored (or pre-existing) fact plus the
/// write-time diff report. `fact` is `None` only for `rejected_secret_like`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddFactOutcome {
    pub fact: Option<FactRecord>,
    pub diff: AddFactDiff,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchFactsRequest {
    pub query: String,
    pub category: Option<MemoryCategory>,
    pub limit: Option<usize>,
    pub min_trust: Option<f64>,
    pub include_why: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateFactRequest {
    pub fact_id: i64,
    pub content: Option<String>,
    pub category: Option<MemoryCategory>,
    pub tags: Option<Vec<String>>,
    pub entities: Option<Vec<String>>,
    pub trust: Option<f64>,
    pub source: Option<String>,
    pub metadata: Option<Value>,
}

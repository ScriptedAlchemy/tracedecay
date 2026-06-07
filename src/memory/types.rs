use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
}

impl fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
        let normalized = value.trim().to_ascii_lowercase().replace(['-', ' '], "_");
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
    pub helpful_count: i64,
    pub unhelpful_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_retrieved_at: Option<i64>,
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
pub struct MemoryRepairStats {
    pub missing_vectors_repaired: usize,
    pub dirty_banks_rebuilt: usize,
    pub full_banks_rebuilt: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    pub legacy_backfill_complete: bool,
    pub repair: MemoryRepairStats,
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

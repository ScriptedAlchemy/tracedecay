use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::RetainedOutcomeStatusV1;
use crate::retained_surfaces::{FactCategoryV1, FactFeedbackActionV1, FactMetadataV1};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactCommitDispositionV1 {
    Committed,
    IdempotentReplay,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FactCommitOwnerV1 {
    Profile,
    Project { project_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactCommitReceiptV1 {
    pub disposition: FactCommitDispositionV1,
    pub fact_id: String,
    pub owner: FactCommitOwnerV1,
    pub expected_last_event_id: Option<String>,
    pub committed_event_ids: Vec<String>,
    pub last_event_id: String,
    pub active_assertion_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactMutationReceiptV1 {
    pub operation_id: String,
    pub input_digest: String,
    pub commit: FactCommitReceiptV1,
    pub expected_last_event_id: Option<String>,
    pub committed_generation: String,
    pub replayed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactV1 {
    pub fact_id: i64,
    pub content: String,
    pub category: FactCategoryV1,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub trust_score: f64,
    pub source: Option<String>,
    pub retrieval_count: i64,
    pub access_count: i64,
    pub helpful_count: i64,
    pub unhelpful_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_retrieved_at: Option<i64>,
    pub last_recalled_at: Option<i64>,
    pub last_feedback_at: Option<i64>,
    pub metadata: FactMetadataV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactSearchHitV1 {
    pub fact: FactV1,
    pub score: f64,
    pub fts_score: f64,
    pub jaccard_score: f64,
    pub holographic_score: f64,
    pub trust_score: f64,
    pub why: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactContradictionV1 {
    pub existing_fact: FactV1,
    pub new_content: String,
    pub score: f64,
    pub why: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum FactCollectionEntryV1 {
    Search(FactSearchHitV1),
    Contradiction(FactContradictionV1),
    Fact(FactV1),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactDiffKindV1 {
    Add,
    NearDuplicate,
    PossibleConflict,
    RejectedSecretLike,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrustHistoryEntryV1 {
    pub timestamp: i64,
    pub action: FactFeedbackActionV1,
    pub old_trust: f64,
    pub new_trust: f64,
    pub delta: f64,
    pub source: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreResultV1 {
    pub action: String,
    pub count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closest_fact_id: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<FactDiffKindV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact: Option<Option<FactV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Vec<FactCollectionEntryV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<FactCollectionEntryV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity: Option<Option<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_history: Option<Vec<TrustHistoryEntryV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation: Option<FactMutationReceiptV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreAddResultV1 {
    pub count: usize,
    pub fact: Option<FactV1>,
    pub diff: FactDiffKindV1,
    pub closest_fact_id: Option<i64>,
    pub similarity: Option<f64>,
    pub reason: Option<String>,
    pub mutation: Option<FactMutationReceiptV1>,
}

macro_rules! fact_collection_result {
    ($name:ident) => {
        #[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub count: usize,
            pub facts: Vec<FactCollectionEntryV1>,
            pub results: Vec<FactCollectionEntryV1>,
        }
    };
}

fact_collection_result!(FactStoreSearchResultV1);
fact_collection_result!(FactStoreProbeResultV1);
fact_collection_result!(FactStoreRelatedResultV1);
fact_collection_result!(FactStoreReasonResultV1);
fact_collection_result!(FactStoreContradictResultV1);
fact_collection_result!(FactStoreListResultV1);

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreGetResultV1 {
    pub count: usize,
    pub fact: Option<FactV1>,
    pub trust_history: Vec<TrustHistoryEntryV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreUpdateResultV1 {
    pub count: usize,
    pub fact: Option<FactV1>,
    pub diff: Option<FactDiffKindV1>,
    pub reason: Option<String>,
    pub error: Option<String>,
    pub mutation: Option<FactMutationReceiptV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreRemoveResultV1 {
    pub count: usize,
    pub removed: bool,
    pub mutation: Option<FactMutationReceiptV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactFeedbackV1 {
    pub event_id: i64,
    pub fact_id: i64,
    pub action: FactFeedbackActionV1,
    pub old_trust: f64,
    pub new_trust: f64,
    pub trust_delta: f64,
    pub helpful_count: i64,
    pub unhelpful_count: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactFeedbackResultV1 {
    pub status: RetainedOutcomeStatusV1,
    pub feedback: FactFeedbackV1,
    pub mutation: FactMutationReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryRepairStatsV1 {
    pub missing_vectors_repaired: usize,
    pub banks_rebuilt: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryFeedbackFunnelV1 {
    pub retrieval_count_total: i64,
    pub access_count_total: i64,
    pub retrieved_fact_count: usize,
    pub rated_fact_count: usize,
    pub feedback_total: usize,
    pub seen_to_feedback_ratio: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryStatusV1 {
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
    pub repair: MemoryRepairStatsV1,
    pub feedback_funnel: MemoryFeedbackFunnelV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryStatusResultV1 {
    pub status: RetainedOutcomeStatusV1,
    pub memory: MemoryStatusV1,
}

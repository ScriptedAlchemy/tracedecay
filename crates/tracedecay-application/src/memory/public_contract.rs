//! Canonical public fact projection and search contracts shared by retained
//! memory operations and composite retrieval surfaces.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    FactAssertionId, FactEventId, FactId, LocatorDigest, ProjectId, ProvenanceId,
    RetrievalAnchorId, UtcMicros,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactCategoryV1 {
    General,
    UserPref,
    Project,
    Tool,
    Decision,
    CodeArea,
}

/// JSON metadata is open only within the fact payload's bounded metadata
/// field; it is never an operation envelope.
pub type FactMetadataV1 = BTreeMap<String, serde_json::Value>;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactCommitOwnerV1 {
    Profile,
    Project { project_id: ProjectId },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactIdentitySourceResultV1 {
    Evidence {
        anchor_id: RetrievalAnchorId,
        stable_key: LocatorDigest,
    },
    Application {
        operation_id: ProvenanceId,
    },
}

/// Payload states that structurally cannot expose an available fact payload.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactPayloadAccessV1 {
    Redacted,
    Quarantined,
    RetentionExpired,
    Deleted,
    Unavailable,
    Ambiguous,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactTelemetryV1 {
    pub retrieval_count: u64,
    pub access_count: u64,
    pub helpful_count: u64,
    pub unhelpful_count: u64,
    pub created_at: UtcMicros,
    pub updated_at: UtcMicros,
    pub last_retrieved_at: Option<UtcMicros>,
    pub last_recalled_at: Option<UtcMicros>,
    pub last_feedback_at: Option<UtcMicros>,
}

/// Available fact projection. Unavailable payload states use
/// [`FactProjectionV1::Unavailable`] and cannot fabricate payload fields.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactV1 {
    pub owner: FactCommitOwnerV1,
    pub fact_id: FactId,
    pub content: String,
    pub category: FactCategoryV1,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub trust_score_millionths: u32,
    pub source: FactIdentitySourceResultV1,
    pub source_label: Option<String>,
    pub active_assertion_id: FactAssertionId,
    pub last_event_id: FactEventId,
    pub projected_as_of: UtcMicros,
    pub telemetry: FactTelemetryV1,
    pub metadata: FactMetadataV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactStatusV1 {
    pub owner: FactCommitOwnerV1,
    pub fact_id: FactId,
    pub payload_access: FactPayloadAccessV1,
    pub projected_as_of: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactProjectionV1 {
    Available { fact: Box<FactV1> },
    Unavailable { status: FactStatusV1 },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactSearchCursorV1 {
    pub score_millionths: u32,
    pub updated_at: UtcMicros,
    pub fact_id: FactId,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactSearchScoresV1 {
    pub score_millionths: u32,
    pub fts_score_millionths: u32,
    pub jaccard_score_millionths: u32,
    pub holographic_score_millionths: u32,
    pub trust_score_millionths: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactSearchHitV1 {
    pub fact: FactV1,
    pub scores: FactSearchScoresV1,
    pub why: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactSearchGraphDegradationV1 {
    Conflict,
    Unavailable,
    BudgetExhausted,
    DeadlineExceeded,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactSearchGraphCoverageV1 {
    NotApplicable,
    NotMounted,
    Complete {
        root_count: usize,
        relation_count: usize,
        expanded_fact_count: usize,
    },
    Degraded {
        reason: FactSearchGraphDegradationV1,
    },
}

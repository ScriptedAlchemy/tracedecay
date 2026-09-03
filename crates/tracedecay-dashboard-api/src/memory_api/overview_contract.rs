use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::{FactId, PayloadAccessState};

use crate::memory_service::{MemoryFactsCoverageV1, MemoryGraphPayloadV1};
use crate::read_model::DashboardDomainStateV1;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryCategoryCountV1 {
    pub(super) category: String,
    pub(super) count: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryTrustBucketV1 {
    pub(super) bucket: u64,
    pub(super) label: String,
    pub(super) count: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryGrowthPointV1 {
    pub(super) date: String,
    pub(super) facts: u64,
    pub(super) cumulative_facts: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryOverviewSummaryV1 {
    pub(super) facts: u64,
    pub(super) entities: u64,
    pub(super) categories: Vec<MemoryCategoryCountV1>,
    pub(super) trust_histogram: Vec<MemoryTrustBucketV1>,
    pub(super) growth: Vec<MemoryGrowthPointV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryReadStatusV1 {
    pub(super) state: DashboardDomainStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

impl MemoryReadStatusV1 {
    pub(super) fn new(state: DashboardDomainStateV1) -> Self {
        Self {
            state,
            code: None,
            error: None,
        }
    }

    pub(super) fn failed(
        state: DashboardDomainStateV1,
        code: Option<&str>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            state,
            code: code.map(str::to_owned),
            error: Some(error.into()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryFactRowV1 {
    pub(super) fact_id: FactId,
    pub(super) payload_access: PayloadAccessState,
    pub(super) trust_score: Option<f64>,
    pub(super) retrieval_count: Option<u64>,
    pub(super) access_count: Option<u64>,
    pub(super) helpful_count: Option<u64>,
    pub(super) unhelpful_count: Option<u64>,
    pub(super) created_at: Option<i64>,
    pub(super) updated_at: Option<i64>,
    pub(super) projected_as_of: i64,
    pub(super) last_recalled_at: Option<i64>,
    pub(super) content: Option<String>,
    pub(super) category: Option<String>,
    pub(super) tags: Option<Vec<String>>,
    pub(super) metadata: Option<Value>,
    pub(super) source_label: Option<String>,
    pub(super) entities: Option<Vec<String>>,
    pub(super) linked_entities: Option<Vec<MemoryEntityRowV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) score_millionths: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) why: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryEntityRowV1 {
    pub(super) entity_id: String,
    pub(super) name: String,
    pub(super) fact_count: u64,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryHolographicPayloadV1 {
    pub(super) path: String,
    pub(super) exists: bool,
    pub(super) overview: Option<MemoryOverviewSummaryV1>,
    pub(super) facts: Vec<MemoryFactRowV1>,
    pub(super) entities: Vec<MemoryEntityRowV1>,
    pub(super) graph: MemoryGraphPayloadV1,
    pub(super) error: String,
    pub(super) reads: BTreeMap<String, MemoryReadStatusV1>,
    pub(super) facts_coverage: MemoryFactsCoverageV1,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryOverviewPayloadV1 {
    pub(super) providers: BTreeMap<String, Value>,
    pub(super) query: String,
    pub(super) limit: i64,
    pub(super) holographic: MemoryHolographicPayloadV1,
}

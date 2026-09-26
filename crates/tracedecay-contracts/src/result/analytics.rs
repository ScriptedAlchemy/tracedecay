//! Analytics an operation reports beside its result for the invocation
//! ledger. Surfaces record them; they never render into the result a client
//! reads.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::retrieval::PrSymbolPageV1;

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InvocationAnalyticsV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_memory: Option<ContextMemoryAnalyticsV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_context: Option<PrContextAnalyticsV1>,
}

impl InvocationAnalyticsV1 {
    /// The metadata object the invocation ledger records for this call.
    pub fn ledger_value(&self) -> Value {
        let mut value = json!({});
        if let Some(context_memory) = &self.context_memory {
            value["context_memory"] = context_memory.ledger_value();
        }
        if let Some(pr_context) = &self.pr_context {
            value["stage_timings_us"] = json!(pr_context.stage_timings_us);
            value["symbol_coverage"] = json!(pr_context.symbol_coverage);
        }
        value
    }
}

/// How a context call searched project memory and which facts it surfaced.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextMemoryAnalyticsV1 {
    pub include_memory: bool,
    pub limit: u32,
    pub min_trust_millionths: u32,
    pub fact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ContextMemoryAnalyticsV1 {
    fn ledger_value(&self) -> Value {
        json!({
            "include_memory": self.include_memory,
            "limit": self.limit,
            "min_trust": f64::from(self.min_trust_millionths) / 1_000_000.0,
            "match_count": self.fact_ids.len(),
            "fact_ids": self.fact_ids,
            "error": self.error,
        })
    }
}

/// Where a PR-context call spent its time and how much of the changed
/// symbol set its page covered.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrContextAnalyticsV1 {
    pub stage_timings_us: PrContextStageTimingsV1,
    pub symbol_coverage: PrSymbolPageV1,
}

/// Microseconds per PR-context stage. The graph-enrichment stages are absent
/// when the verified graph was still warming and only git evidence answered.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrContextStageTimingsV1 {
    pub git: u64,
    pub graph: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_diff: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_annotations: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_page: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assemble: Option<u64>,
    pub total: u64,
}

//! Analytics an operation reports beside its result for the invocation
//! ledger. Surfaces record them; they never render into the result a client
//! reads.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InvocationAnalyticsV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_memory: Option<ContextMemoryAnalyticsV1>,
}

impl InvocationAnalyticsV1 {
    /// The metadata object the invocation ledger records for this call.
    pub fn ledger_value(&self) -> Value {
        let mut value = json!({});
        if let Some(context_memory) = &self.context_memory {
            value["context_memory"] = context_memory.ledger_value();
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

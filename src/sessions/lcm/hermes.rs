//! JSON bridge contracts used by the generated Hermes context engine.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmCompressionRequest {
    pub provider: String,
    pub session_id: String,
    pub messages: Vec<Value>,
    pub current_tokens: Option<i64>,
    pub focus_topic: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_session_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stateless_session_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_message_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_current_frontier_store_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_assembly_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_chunk_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_source_messages: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_fan_in: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_tail_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_leaf_chunk_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_leaf_chunk_max: Option<i64>,
    pub summarizer: LcmSummarizerMode,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LcmSummarizerMode {
    Noop,
    Fake {
        summary_text: String,
    },
    Provided {
        summary_text: String,
        route: Option<String>,
    },
    HermesAuxiliary,
}

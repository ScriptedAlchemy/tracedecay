//! JSON bridge contracts used by the generated Hermes context engine.

use serde_json::Value;

use crate::types::{LcmCompressionOverrides, LcmExtractionResult};

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
    #[serde(flatten)]
    pub overrides: LcmCompressionOverrides,
    pub summarizer: LcmSummarizerMode,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LcmSummarizerMode {
    Noop,
    Provided {
        summary_text: String,
        route: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extraction_result: Option<LcmExtractionResult>,
    },
    HermesAuxiliary,
}

impl LcmSummarizerMode {
    pub fn has_nonempty_provided_summary(&self) -> bool {
        matches!(
            self,
            Self::Provided { summary_text, .. } if !summary_text.trim().is_empty()
        )
    }
}

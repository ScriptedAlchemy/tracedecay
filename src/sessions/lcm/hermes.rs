//! JSON bridge contracts used by the generated Hermes context engine.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LcmCompressionRequest {
    pub provider: String,
    pub session_id: String,
    pub messages: Vec<Value>,
    pub current_tokens: Option<i64>,
    pub focus_topic: Option<String>,
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

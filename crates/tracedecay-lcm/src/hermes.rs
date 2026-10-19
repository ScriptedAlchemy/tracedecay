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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_overrides_keep_flat_json_fields() {
        let request = LcmCompressionRequest {
            provider: "cursor".to_string(),
            session_id: "session-1".to_string(),
            messages: Vec::new(),
            current_tokens: None,
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            overrides: LcmCompressionOverrides {
                threshold_tokens: Some(1),
                max_assembly_tokens: Some(2),
                leaf_chunk_tokens: Some(3),
                max_source_messages: Some(4),
                summary_fan_in: Some(5),
                incremental_max_depth: Some(6),
                fresh_tail_count: Some(7),
                dynamic_leaf_chunk_enabled: Some(false),
                dynamic_leaf_chunk_max: Some(8),
                context_length: Some(9),
                reserve_tokens_floor: Some(10),
            },
            summarizer: LcmSummarizerMode::Noop,
        };

        let encoded = serde_json::to_value(&request).expect("serialize compression request");
        assert!(encoded.get("overrides").is_none());
        assert_eq!(encoded["threshold_tokens"], 1);
        assert_eq!(encoded["max_assembly_tokens"], 2);
        assert_eq!(encoded["leaf_chunk_tokens"], 3);
        assert_eq!(encoded["max_source_messages"], 4);
        assert_eq!(encoded["summary_fan_in"], 5);
        assert_eq!(encoded["incremental_max_depth"], 6);
        assert_eq!(encoded["fresh_tail_count"], 7);
        assert_eq!(encoded["dynamic_leaf_chunk_enabled"], false);
        assert_eq!(encoded["dynamic_leaf_chunk_max"], 8);
        assert_eq!(encoded["context_length"], 9);
        assert_eq!(encoded["reserve_tokens_floor"], 10);
        assert_eq!(
            serde_json::from_value::<LcmCompressionRequest>(encoded)
                .expect("deserialize flat compression overrides"),
            request
        );
    }

    #[test]
    fn preflight_overrides_preserve_null_unknown_and_duplicate_behavior() {
        let request: crate::types::LcmPreflightRequest =
            serde_json::from_value(serde_json::json!({
                "provider": "cursor",
                "session_id": "session-1",
                "threshold_tokens": null,
                "dynamic_leaf_chunk_enabled": false,
                "future_override": 99
            }))
            .expect("deserialize flat preflight overrides");
        assert_eq!(request.overrides.threshold_tokens, None);
        assert_eq!(request.overrides.dynamic_leaf_chunk_enabled, Some(false));
        assert!(serde_json::from_str::<crate::types::LcmPreflightRequest>(
            r#"{"provider":"cursor","session_id":"session-1","threshold_tokens":1,"threshold_tokens":2}"#
        )
        .is_err());
    }
}

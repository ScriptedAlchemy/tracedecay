//! Exact route-selected fact-store request bodies.
//!
//! The route selects the operation, so each request accepts only that
//! operation's canonical fields.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{FactEventId, FactId};

use super::{
    FactCategoryV1, FactMetadataV1, FactReadOptionsV1, FactSearchCursorV1, MemoryScopeV1,
    RetainedProjectSelectorV1,
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactSourceLabelPatchV1 {
    Set { value: String },
    Clear,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreAddRequestV1 {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<MemoryScopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<FactCategoryV1>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<FactMetadataV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<RetainedProjectSelectorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreSearchRequestV1 {
    pub query: String,
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<FactSearchCursorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreProbeRequestV1 {
    pub entity: String,
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<FactSearchCursorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreRelatedRequestV1 {
    pub entity: String,
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<FactSearchCursorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreReasonRequestV1 {
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<FactSearchCursorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreContradictRequestV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_millionths: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<MemoryScopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<FactCategoryV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<RetainedProjectSelectorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreGetRequestV1 {
    pub fact_id: FactId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<MemoryScopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<RetainedProjectSelectorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreUpdateRequestV1 {
    pub fact_id: FactId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_last_event_id: Option<FactEventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<MemoryScopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<FactCategoryV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<FactSourceLabelPatchV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<FactMetadataV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<RetainedProjectSelectorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreRemoveRequestV1 {
    pub fact_id: FactId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_last_event_id: Option<FactEventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<MemoryScopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<RetainedProjectSelectorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreListRequestV1 {
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_fact_id: Option<FactId>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        FactStoreAddRequestV1, FactStoreContradictRequestV1, FactStoreGetRequestV1,
        FactStoreReasonRequestV1, FactStoreSearchRequestV1, FactStoreUpdateRequestV1,
    };
    use crate::retained_surfaces::FactFeedbackRequestV1;

    #[test]
    fn route_selected_fact_request_rejects_an_action_tag() {
        assert!(
            serde_json::from_value::<FactStoreSearchRequestV1>(json!({
                "action": "search",
                "query": "session"
            }))
            .is_err()
        );
    }

    #[test]
    fn exact_fact_requests_reject_legacy_aliases_and_numeric_ids() {
        for alias in [
            json!({"content": "remember", "entity": "compiler"}),
            json!({"content": "remember", "source": "operator"}),
            json!({"content": "remember", "project_id": "project.alpha"}),
            json!({"content": "remember", "project_path": "/tmp/project"}),
            json!({"content": "remember", "format": "json"}),
        ] {
            assert!(serde_json::from_value::<FactStoreAddRequestV1>(alias).is_err());
        }
        assert!(
            serde_json::from_value::<FactStoreAddRequestV1>(json!({
                "content": "remember",
                "project_selector": {"path": "/tmp/project"}
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<FactStoreSearchRequestV1>(json!({
                "query": "remember",
                "format": "json"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<FactStoreReasonRequestV1>(json!({
                "entity": "compiler"
            }))
            .is_err()
        );
        assert!(serde_json::from_value::<FactStoreGetRequestV1>(json!({"fact_id": 41})).is_err());
        for ignored in [
            json!({"fact_id": "fact.test", "category": "decision"}),
            json!({"fact_id": "fact.test", "min_trust": 0.5}),
            json!({"fact_id": "fact.test", "limit": 10}),
        ] {
            assert!(serde_json::from_value::<FactStoreGetRequestV1>(ignored).is_err());
        }
        assert!(
            serde_json::from_value::<FactFeedbackRequestV1>(json!({
                "fact_id": "fact.test",
                "helpful": true
            }))
            .is_err()
        );
    }

    #[test]
    fn exact_fact_mutations_accept_canonical_identity_and_cas_fields() {
        let add = serde_json::from_value::<FactStoreAddRequestV1>(json!({
            "content": "remember the chosen approach",
            "memory_scope": "project",
            "category": "decision",
            "source_label": "operator",
            "project_selector": {"project_id": "project.alpha"}
        }))
        .expect("canonical add request");

        assert_eq!(
            serde_json::to_value(add).expect("request serializes"),
            json!({
                "content": "remember the chosen approach",
                "memory_scope": "project",
                "category": "decision",
                "tags": [],
                "entities": [],
                "source_label": "operator",
                "project_selector": {"project_id": "project.alpha"}
            })
        );

        serde_json::from_value::<FactStoreUpdateRequestV1>(json!({
            "fact_id": "fact.test",
            "expected_last_event_id": "event.test",
            "content": "updated",
            "source_label": {"kind": "set", "value": "operator"}
        }))
        .expect("canonical update request");
        serde_json::from_value::<FactStoreUpdateRequestV1>(json!({
            "fact_id": "fact.test",
            "source_label": {"kind": "clear"}
        }))
        .expect("canonical clear-source update request");
        assert!(
            serde_json::from_value::<FactStoreUpdateRequestV1>(json!({
                "fact_id": "fact.test",
                "source_label": "operator"
            }))
            .is_err()
        );
        serde_json::from_value::<FactFeedbackRequestV1>(json!({
            "fact_id": "fact.test",
            "expected_last_event_id": "event.test",
            "action": "helpful",
            "source_label": "operator",
            "reason": "confirmed by the user"
        }))
        .expect("canonical feedback request");
    }

    #[test]
    fn exact_contradiction_accepts_only_supported_bounded_fields() {
        let request = serde_json::from_value::<FactStoreContradictRequestV1>(json!({
            "threshold_millionths": 800_000,
            "memory_scope": "project",
            "category": "decision",
            "limit": 25,
            "project_selector": {"project_id": "project.alpha"}
        }))
        .expect("canonical contradiction request");
        assert_eq!(
            serde_json::to_value(request).expect("contradiction request serializes"),
            json!({
                "threshold_millionths": 800_000,
                "memory_scope": "project",
                "category": "decision",
                "limit": 25,
                "project_selector": {"project_id": "project.alpha"}
            })
        );

        for unsupported in [
            json!({"min_trust": 0.5}),
            json!({"after": {"fact_id": "fact.test"}}),
            json!({"cursor": "cursor.test"}),
            json!({"next_after": "cursor.test"}),
            json!({"next_cursor": "cursor.test"}),
            json!({"after_fact_id": "fact.test"}),
            json!({"threshold": 0.8}),
            json!({"format": "json"}),
            json!({"action": "contradict"}),
            json!({"project_id": "project.alpha"}),
            json!({"project_path": "/tmp/project"}),
            json!({"project_selector": {"path": "/tmp/project"}}),
            json!({"project_selector": {"project_path": "/tmp/project"}}),
        ] {
            assert!(serde_json::from_value::<FactStoreContradictRequestV1>(unsupported).is_err());
        }
    }
}

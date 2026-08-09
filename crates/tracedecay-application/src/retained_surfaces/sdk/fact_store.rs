//! Exact route-selected fact-store request bodies.
//!
//! Current HTTP operations select the action in the route, so their public
//! request schemas must not accept another action's body. The tagged
//! [`FactStoreRequestV1`] remains only for the legacy broad MCP translator.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    FactCategoryV1, FactMetadataV1, FactReadOptionsV1, MemoryScopeV1, RetainedFactIdV1,
    RetainedOutputFormatV1,
};
use crate::retained_surfaces::RetainedSurfaceOperation;

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
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<FactMetadataV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<super::RetainedProjectSelectorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreSearchRequestV1 {
    pub query: String,
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreProbeRequestV1 {
    pub entity: String,
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreRelatedRequestV1 {
    pub entity: String,
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreReasonRequestV1 {
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreContradictRequestV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreGetRequestV1 {
    pub fact_id: RetainedFactIdV1,
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreUpdateRequestV1 {
    pub fact_id: RetainedFactIdV1,
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
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<FactMetadataV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<super::RetainedProjectSelectorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreRemoveRequestV1 {
    pub fact_id: RetainedFactIdV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<MemoryScopeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_selector: Option<super::RetainedProjectSelectorV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RetainedOutputFormatV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreListRequestV1 {
    #[serde(flatten)]
    pub options: FactReadOptionsV1,
    #[serde(default)]
    pub format: Option<RetainedOutputFormatV1>,
}

/// Legacy MCP request form. It preserves the broad action tag but composes
/// the exact action fields used by route-selected HTTP operations.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields, tag = "action", rename_all = "snake_case")]
pub enum FactStoreRequestV1 {
    Add(FactStoreAddRequestV1),
    Search(FactStoreSearchRequestV1),
    Probe(FactStoreProbeRequestV1),
    Related(FactStoreRelatedRequestV1),
    Reason(FactStoreReasonRequestV1),
    Contradict(FactStoreContradictRequestV1),
    Get(FactStoreGetRequestV1),
    Update(FactStoreUpdateRequestV1),
    Remove(FactStoreRemoveRequestV1),
    List(FactStoreListRequestV1),
}

impl FactStoreRequestV1 {
    pub const fn operation(&self) -> RetainedSurfaceOperation {
        match self {
            Self::Add(_) => RetainedSurfaceOperation::FactStoreAdd,
            Self::Search(_) => RetainedSurfaceOperation::FactStoreSearch,
            Self::Probe(_) => RetainedSurfaceOperation::FactStoreProbe,
            Self::Related(_) => RetainedSurfaceOperation::FactStoreRelated,
            Self::Reason(_) => RetainedSurfaceOperation::FactStoreReason,
            Self::Contradict(_) => RetainedSurfaceOperation::FactStoreContradict,
            Self::Get(_) => RetainedSurfaceOperation::FactStoreGet,
            Self::Update(_) => RetainedSurfaceOperation::FactStoreUpdate,
            Self::Remove(_) => RetainedSurfaceOperation::FactStoreRemove,
            Self::List(_) => RetainedSurfaceOperation::FactStoreList,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{FactStoreRequestV1, FactStoreSearchRequestV1};

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
    fn legacy_fact_translator_retains_the_action_tag() {
        let request = serde_json::from_value::<FactStoreRequestV1>(json!({
            "action": "search",
            "query": "session"
        }))
        .expect("legacy tagged request");
        assert!(matches!(request, FactStoreRequestV1::Search(_)));
    }

    #[test]
    fn fact_add_round_trip_omits_absent_selectors_from_legacy_handler_args() {
        let request = serde_json::from_value::<FactStoreRequestV1>(json!({
            "action": "add",
            "content": "remember the chosen approach",
            "memory_scope": "project",
            "category": "decision"
        }))
        .expect("legacy tagged request");

        assert_eq!(
            serde_json::to_value(request).expect("request serializes"),
            json!({
                "action": "add",
                "content": "remember the chosen approach",
                "memory_scope": "project",
                "category": "decision",
                "tags": [],
                "entities": []
            })
        );
    }
}

//! Exact public request and result bodies for the mounted advisory-cycle route.
//!
//! The daemon owns cycle execution and evidence envelopes. These types own
//! only the stable request body and payload that the daemon serializes inside
//! that envelope, so catalog schemas, HTTP, MCP, and generated SDKs share one
//! wire authority.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::RetrievalAnchorId;
use tracedecay_domain::feedback::{FeedbackCycleResultV1, FeedbackFindingId};

use crate::error::ApplicationContractError;

const MAX_ADVISORY_DOCUMENT_URI_BYTES_V1: usize = 4_096;

/// Explicit advisory-cycle trigger for one document in the admitted project.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackAdvisoryCycleSurfaceRequestV1 {
    pub document_uri: String,
}

impl FeedbackAdvisoryCycleSurfaceRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.document_uri.is_empty()
            || self.document_uri.trim() != self.document_uri
            || self.document_uri.len() > MAX_ADVISORY_DOCUMENT_URI_BYTES_V1
            || self.document_uri.chars().any(char::is_control)
        {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "feedback advisory document URI",
            });
        }
        Ok(())
    }
}

/// One canonical cycle result plus whether its durable publication committed.
///
/// `published` intentionally remains adjacent to the cycle fields on the
/// wire: publication is a property of this exact cycle, not of a page or a
/// later lookup.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackAdvisoryCycleWireV1 {
    #[serde(flatten)]
    #[schemars(flatten)]
    pub cycle: FeedbackCycleResultV1,
    pub published: bool,
}

/// One daemon-minted read-handle pair for a finding in the completed cycle.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackAdvisoryFindingHandleV1 {
    pub finding_id: FeedbackFindingId,
    pub retrieval_anchor_id: Option<RetrievalAnchorId>,
    pub get_handle: String,
    pub expansion_handle: Option<String>,
}

/// Exact advisory-cycle payload serialized by the mounted daemon route.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackAdvisoryCycleSurfaceResultV1 {
    pub cycle: FeedbackAdvisoryCycleWireV1,
    pub finding_handles: Vec<FeedbackAdvisoryFindingHandleV1>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_cycle_request_has_exact_typed_wire_and_schema() {
        fn assert_json_schema<T: JsonSchema>() {}

        assert_json_schema::<FeedbackAdvisoryCycleSurfaceRequestV1>();
        assert_json_schema::<FeedbackAdvisoryCycleSurfaceResultV1>();

        let request = FeedbackAdvisoryCycleSurfaceRequestV1 {
            document_uri: "file:///workspace/src/lib.rs".to_owned(),
        };
        request.validate().expect("valid document URI");

        let encoded = serde_json::to_value(&request).expect("serialize request");
        let decoded: FeedbackAdvisoryCycleSurfaceRequestV1 =
            serde_json::from_value(encoded.clone()).expect("deserialize request");
        assert_eq!(decoded, request);

        let mut unknown = encoded;
        unknown["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<FeedbackAdvisoryCycleSurfaceRequestV1>(unknown).is_err());
        assert!(
            FeedbackAdvisoryCycleSurfaceRequestV1 {
                document_uri: " file:///workspace/src/lib.rs".to_owned(),
            }
            .validate()
            .is_err()
        );
    }
}

use serde::{Deserialize, Serialize};
use tracedecay_domain::VectorGenerationIdV1;

use super::SemanticVectorStageRecord;
use super::super::{
    GraphDependencyGenerationIdentityV1, GraphProjectionIdentityV1,
    GraphPublicationIdempotencyKeyV1, GraphVerifiedHeadV1, StorageRuntimeContractErrorV1,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorSourceDependencyV1 {
    pub generation: GraphDependencyGenerationIdentityV1,
    pub idempotency_key: GraphPublicationIdempotencyKeyV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorPublishedGenerationKey {
    pub projection: GraphProjectionIdentityV1,
    pub semantic_generation_id: VectorGenerationIdV1,
}

impl SemanticVectorPublishedGenerationKey {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.semantic_generation_id.validate().map_err(|_| {
            StorageRuntimeContractErrorV1::NonCanonical {
                field: "semantic vector generation id",
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorPublishedGenerationLookup {
    Missing,
    Published {
        record: SemanticVectorStageRecord,
        verified_head: GraphVerifiedHeadV1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorStageResumeOutcome {
    Missing,
    Pending(SemanticVectorStageRecord),
    Ready(SemanticVectorStageRecord),
    Published {
        record: SemanticVectorStageRecord,
        verified_head: GraphVerifiedHeadV1,
    },
    Cancelled(SemanticVectorStageRecord),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorStageBeginOutcome {
    Begun(SemanticVectorStageRecord),
    ExactReplay(SemanticVectorStageRecord),
    Published {
        record: SemanticVectorStageRecord,
        verified_head: GraphVerifiedHeadV1,
    },
    InputConflict {
        existing: SemanticVectorStageRecord,
    },
    SemanticGenerationConflict {
        existing: SemanticVectorStageRecord,
    },
    PublicationConflict,
    PriorVerifiedHeadConflict {
        actual: Option<GraphVerifiedHeadV1>,
    },
}

use tracedecay_policy::retrieval_selection::{
    RetrievalAvailabilityV1, RetrievalRequirementV1, RetrievalSelectionV1, select_retrieval,
};

use crate::query::retrieval::semantic::{
    SemanticRetrievalAvailabilityV1, SemanticRetrievalRequirementV1,
    SemanticRetrievalSelectionPolicyV1, SemanticRetrievalSelectionV1,
};

pub(crate) const PRODUCTION_SEMANTIC_RETRIEVAL_SELECTION_POLICY:
    ProductionSemanticRetrievalSelectionPolicyV1 = ProductionSemanticRetrievalSelectionPolicyV1;

pub(crate) struct ProductionSemanticRetrievalSelectionPolicyV1;

impl SemanticRetrievalSelectionPolicyV1 for ProductionSemanticRetrievalSelectionPolicyV1 {
    fn select(
        &self,
        availability: SemanticRetrievalAvailabilityV1,
        requirement: SemanticRetrievalRequirementV1,
    ) -> SemanticRetrievalSelectionV1 {
        let availability = match availability {
            SemanticRetrievalAvailabilityV1::Ready => RetrievalAvailabilityV1::Ready,
            SemanticRetrievalAvailabilityV1::Unavailable => RetrievalAvailabilityV1::Unavailable,
            SemanticRetrievalAvailabilityV1::Indexing => RetrievalAvailabilityV1::Indexing,
            SemanticRetrievalAvailabilityV1::Degraded => RetrievalAvailabilityV1::Degraded,
            SemanticRetrievalAvailabilityV1::Failed => RetrievalAvailabilityV1::Failed,
            SemanticRetrievalAvailabilityV1::Stale => RetrievalAvailabilityV1::Stale,
            SemanticRetrievalAvailabilityV1::Incompatible => RetrievalAvailabilityV1::Incompatible,
        };
        let requirement = match requirement {
            SemanticRetrievalRequirementV1::FallbackAllowed => {
                RetrievalRequirementV1::FallbackAllowed
            }
            SemanticRetrievalRequirementV1::StrictSemantic => {
                RetrievalRequirementV1::StrictSemantic
            }
        };
        match select_retrieval(availability, requirement) {
            RetrievalSelectionV1::Semantic => SemanticRetrievalSelectionV1::Semantic,
            RetrievalSelectionV1::FrozenFallback => SemanticRetrievalSelectionV1::FrozenFallback,
            RetrievalSelectionV1::Unavailable => SemanticRetrievalSelectionV1::Unavailable,
        }
    }
}

use std::collections::BTreeSet;

use tracedecay_domain::VectorGenerationIdV1;

use crate::config::retrieval::RetrievalProfileStateV1;

/// Published vector generations retained by the durable retrieval
/// configuration authority.
///
/// The active and rollback profiles are the only persisted vector consumers:
/// model lifecycle pins artifacts, while direct-evaluator vectors are
/// process-local. A store reclaim operation must additionally retain its
/// active pointer and every staged build/base generation internally. Missing
/// generations named here are corruption/reset-required states, not an empty
/// retention result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemanticRetainedVectorGenerationsV1 {
    generation_ids: BTreeSet<VectorGenerationIdV1>,
}

impl SemanticRetainedVectorGenerationsV1 {
    pub fn from_profile_state(state: &RetrievalProfileStateV1) -> Self {
        let generation_ids = [
            state.active().compatibility().semantic.as_ref(),
            state
                .rollback_profile()
                .and_then(|profile| profile.compatibility().semantic.as_ref()),
        ]
        .into_iter()
        .flatten()
        .map(|semantic| semantic.vector_generation_id.clone())
        .collect();
        Self { generation_ids }
    }

    pub fn generation_ids(&self) -> &BTreeSet<VectorGenerationIdV1> {
        &self.generation_ids
    }
}

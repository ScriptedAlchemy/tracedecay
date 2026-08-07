use std::sync::Arc;
use std::time::Instant;

use tracedecay_domain::VectorGenerationIdV1;
use tracedecay_graph_db::GraphCancellation;
use tracedecay_store::{
    SemanticVectorSourceGenerationId, SemanticVectorStageCensusCursor, StoreShardIdV1,
};

use super::persistence::map_graph_error;
use super::{GRAPH_OPERATION_DEADLINE, GraphVectorGenerationStoreV1};
use crate::semantic_runtime::SemanticGraphExecutionAuthorityV1;
use crate::store::vector_generations::VectorGenerationStoreErrorV1;

fn operation_authority(
    cancellation: Arc<dyn GraphCancellation>,
) -> SemanticGraphExecutionAuthorityV1 {
    SemanticGraphExecutionAuthorityV1::new(cancellation, Instant::now() + GRAPH_OPERATION_DEADLINE)
}

impl GraphVectorGenerationStoreV1 {
    pub fn reserve_one_generation(
        &self,
        after: Option<SemanticVectorStageCensusCursor>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionStep, VectorGenerationStoreErrorV1>
    {
        let authority = operation_authority(cancellation);
        self.runtime
            .reserve_one_generation(after, &authority)
            .map_err(map_graph_error)
    }

    pub fn finalize_reserved_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
        authorization: &crate::semantic_runtime::SemanticVectorRetentionAuthorizationV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionAction, VectorGenerationStoreErrorV1>
    {
        let authority = operation_authority(cancellation);
        self.runtime
            .finalize_reserved_generation(reservation, authorization, &authority)
            .map_err(map_graph_error)
    }

    pub fn release_reserved_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        self.runtime
            .release_reserved_generation(reservation)
            .map_err(map_graph_error)
    }

    pub fn source_generation_is_live(
        &self,
        generation: &SemanticVectorSourceGenerationId,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let authority = operation_authority(cancellation);
        self.runtime
            .source_generation_has_live_reference(generation, expected_revision, &authority)
            .map_err(map_graph_error)
    }

    pub fn source_scope_is_live(
        &self,
        source_scope: &StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let authority = operation_authority(cancellation);
        self.runtime
            .source_scope_has_live_reference(source_scope, expected_revision, &authority)
            .map_err(map_graph_error)
    }

    pub fn published_generation_dependency(
        &self,
        generation: &VectorGenerationIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<
        tracedecay_store::SemanticVectorPublishedGenerationDependencyLookup,
        VectorGenerationStoreErrorV1,
    > {
        let authority = operation_authority(cancellation);
        self.runtime
            .published_generation_dependency(generation, expected_revision, &authority)
            .map_err(map_graph_error)
    }

    pub fn validate_project_census_revision(
        &self,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        let authority = operation_authority(cancellation);
        self.runtime
            .validate_project_census_revision(expected_revision, &authority)
            .map_err(map_graph_error)
    }

    pub fn source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<
        tracedecay_store::SemanticVectorSourceScopeBindingLookup,
        VectorGenerationStoreErrorV1,
    > {
        let authority = operation_authority(cancellation);
        self.runtime
            .source_scope_binding(code_scope_hash, expected_revision, &authority)
            .map_err(map_graph_error)
    }

    pub fn remove_source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        source_scope: &tracedecay_store::StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let authority = operation_authority(cancellation);
        self.runtime
            .remove_source_scope_binding(
                code_scope_hash,
                source_scope,
                expected_revision,
                &authority,
            )
            .map_err(map_graph_error)
    }
}

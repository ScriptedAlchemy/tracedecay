use std::sync::Arc;
use std::time::Instant;

use tracedecay_domain::VectorGenerationIdV1;
use tracedecay_graph_db::{
    GraphCancellation, SemanticVectorRetentionAction, SemanticVectorRetentionStep,
    SemanticVectorRetirementReservation,
};
use tracedecay_store::{
    SemanticVectorCodeScopeHash, SemanticVectorPublishedGenerationDependencyLookup,
    SemanticVectorSourceGenerationId, SemanticVectorSourceScopeBindingLookup,
    SemanticVectorStageCensusCursor, SemanticVectorStageCensusRevision, StoreShardIdV1,
};

use super::persistence::map_graph_error;
use super::{
    GRAPH_OPERATION_DEADLINE, GraphVectorGenerationStoreStateV1, GraphVectorGenerationStoreV1,
};
use crate::semantic_runtime::{
    SemanticGraphExecutionAuthorityV1, SemanticVectorRetentionAuthorizationV1,
};
use crate::store::vector_generations::VectorGenerationStoreErrorV1;

fn operation_authority(
    cancellation: Arc<dyn GraphCancellation>,
) -> SemanticGraphExecutionAuthorityV1 {
    SemanticGraphExecutionAuthorityV1::new(cancellation, Instant::now() + GRAPH_OPERATION_DEADLINE)
}

impl GraphVectorGenerationStoreStateV1 {
    fn reserve_one_generation_records(
        &self,
        after: Option<SemanticVectorStageCensusCursor>,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorRetentionStep, VectorGenerationStoreErrorV1> {
        self.runtime
            .reserve_one_generation(after, authority)
            .map_err(map_graph_error)
    }

    fn finalize_reserved_generation_records(
        &self,
        reservation: SemanticVectorRetirementReservation,
        authorization: &SemanticVectorRetentionAuthorizationV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorRetentionAction, VectorGenerationStoreErrorV1> {
        self.runtime
            .finalize_reserved_generation(reservation, authorization, authority)
            .map_err(map_graph_error)
    }

    fn release_reserved_generation_records(
        &self,
        reservation: SemanticVectorRetirementReservation,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        self.runtime
            .release_reserved_generation(reservation)
            .map_err(map_graph_error)
    }

    fn source_generation_is_live_records(
        &self,
        generation: &SemanticVectorSourceGenerationId,
        expected_revision: SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.runtime
            .source_generation_has_live_reference(generation, expected_revision, authority)
            .map_err(map_graph_error)
    }

    fn source_scope_is_live_records(
        &self,
        source_scope: &StoreShardIdV1,
        expected_revision: SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.runtime
            .source_scope_has_live_reference(source_scope, expected_revision, authority)
            .map_err(map_graph_error)
    }

    fn published_generation_dependency_records(
        &self,
        generation: &VectorGenerationIdV1,
        expected_revision: SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorPublishedGenerationDependencyLookup, VectorGenerationStoreErrorV1>
    {
        self.runtime
            .published_generation_dependency(generation, expected_revision, authority)
            .map_err(map_graph_error)
    }

    fn validate_project_census_revision_records(
        &self,
        expected_revision: SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        self.runtime
            .validate_project_census_revision(expected_revision, authority)
            .map_err(map_graph_error)
    }

    fn source_scope_binding_records(
        &self,
        code_scope_hash: &SemanticVectorCodeScopeHash,
        expected_revision: SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorSourceScopeBindingLookup, VectorGenerationStoreErrorV1> {
        self.runtime
            .source_scope_binding(code_scope_hash, expected_revision, authority)
            .map_err(map_graph_error)
    }

    fn remove_source_scope_binding_records(
        &self,
        code_scope_hash: &SemanticVectorCodeScopeHash,
        source_scope: &StoreShardIdV1,
        expected_revision: SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.runtime
            .remove_source_scope_binding(
                code_scope_hash,
                source_scope,
                expected_revision,
                authority,
            )
            .map_err(map_graph_error)
    }
}

impl GraphVectorGenerationStoreV1 {
    #[hotpath::measure(label = "usecases.store.reserve_generation", future = true)]
    pub async fn reserve_one_generation(
        &self,
        after: Option<SemanticVectorStageCensusCursor>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<SemanticVectorRetentionStep, VectorGenerationStoreErrorV1> {
        let authority = operation_authority(cancellation);
        self.dispatch(move |state| state.reserve_one_generation_records(after, &authority))
            .await
    }

    #[hotpath::measure(label = "usecases.store.finalize_generation", future = true)]
    pub async fn finalize_reserved_generation(
        &self,
        reservation: SemanticVectorRetirementReservation,
        authorization: &SemanticVectorRetentionAuthorizationV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<SemanticVectorRetentionAction, VectorGenerationStoreErrorV1> {
        let authorization = authorization.clone();
        let authority = operation_authority(cancellation);
        self.dispatch(move |state| {
            state.finalize_reserved_generation_records(reservation, &authorization, &authority)
        })
        .await
    }

    #[hotpath::measure(label = "usecases.store.release_generation", future = true)]
    pub async fn release_reserved_generation(
        &self,
        reservation: SemanticVectorRetirementReservation,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        self.dispatch(move |state| state.release_reserved_generation_records(reservation))
            .await
    }

    pub async fn source_generation_is_live(
        &self,
        generation: &SemanticVectorSourceGenerationId,
        expected_revision: SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let generation = generation.clone();
        let authority = operation_authority(cancellation);
        self.dispatch(move |state| {
            state.source_generation_is_live_records(&generation, expected_revision, &authority)
        })
        .await
    }

    pub async fn source_scope_is_live(
        &self,
        source_scope: &StoreShardIdV1,
        expected_revision: SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let source_scope = source_scope.clone();
        let authority = operation_authority(cancellation);
        self.dispatch(move |state| {
            state.source_scope_is_live_records(&source_scope, expected_revision, &authority)
        })
        .await
    }

    pub async fn published_generation_dependency(
        &self,
        generation: &VectorGenerationIdV1,
        expected_revision: SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<SemanticVectorPublishedGenerationDependencyLookup, VectorGenerationStoreErrorV1>
    {
        let generation = generation.clone();
        let authority = operation_authority(cancellation);
        self.dispatch(move |state| {
            state.published_generation_dependency_records(
                &generation,
                expected_revision,
                &authority,
            )
        })
        .await
    }

    pub async fn validate_project_census_revision(
        &self,
        expected_revision: SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        let authority = operation_authority(cancellation);
        self.dispatch(move |state| {
            state.validate_project_census_revision_records(expected_revision, &authority)
        })
        .await
    }

    pub async fn source_scope_binding(
        &self,
        code_scope_hash: &SemanticVectorCodeScopeHash,
        expected_revision: SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<SemanticVectorSourceScopeBindingLookup, VectorGenerationStoreErrorV1> {
        let code_scope_hash = code_scope_hash.clone();
        let authority = operation_authority(cancellation);
        self.dispatch(move |state| {
            state.source_scope_binding_records(&code_scope_hash, expected_revision, &authority)
        })
        .await
    }

    pub async fn remove_source_scope_binding(
        &self,
        code_scope_hash: &SemanticVectorCodeScopeHash,
        source_scope: &StoreShardIdV1,
        expected_revision: SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let code_scope_hash = code_scope_hash.clone();
        let source_scope = source_scope.clone();
        let authority = operation_authority(cancellation);
        self.dispatch(move |state| {
            state.remove_source_scope_binding_records(
                &code_scope_hash,
                &source_scope,
                expected_revision,
                &authority,
            )
        })
        .await
    }
}

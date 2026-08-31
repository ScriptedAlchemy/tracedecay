use std::sync::Arc;

use tracedecay_graph_db::{
    GraphDbError, GraphWriteBatch, VerifiedGenerationBatchCommit, VerifiedGraphSnapshot,
};
use tracedecay_store::{
    GraphPublicationKeyV1, GraphVerifiedHeadV1, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorStageBatchReceipt,
    SemanticVectorStageCancelOutcome, SemanticVectorStageKey, SemanticVectorStagePlan,
    SemanticVectorStagePublicationPrepareOutcome, SemanticVectorStagePublishOutcome,
    SemanticVectorStagePublishSettlement, SemanticVectorStageRecord,
    SemanticVectorStageResumeOutcome, StoreRuntimeBindingV1, StoreShardIdV1,
};
use tracedecay_usecases::semantic_runtime::{
    SemanticGraphExecutionAuthorityV1, SemanticVectorGraphScopeV1,
    SemanticVectorRetentionAuthorizationV1, VerifiedSemanticVectorGraphRuntimeV1,
};

use super::RetainedCodeGraphRuntimeV1;

pub(crate) struct DaemonVerifiedSemanticVectorGraphRuntimeV1 {
    retained: Arc<RetainedCodeGraphRuntimeV1>,
    scope: SemanticVectorGraphScopeV1,
    source_scope: StoreShardIdV1,
    binding: StoreRuntimeBindingV1,
}

impl DaemonVerifiedSemanticVectorGraphRuntimeV1 {
    pub fn new(
        retained: Arc<RetainedCodeGraphRuntimeV1>,
        scope: SemanticVectorGraphScopeV1,
        source_scope: StoreShardIdV1,
        binding: StoreRuntimeBindingV1,
    ) -> Self {
        Self {
            retained,
            scope,
            source_scope,
            binding,
        }
    }
}

impl VerifiedSemanticVectorGraphRuntimeV1 for DaemonVerifiedSemanticVectorGraphRuntimeV1 {
    fn scope(&self) -> &SemanticVectorGraphScopeV1 {
        &self.scope
    }

    fn recover_verified_snapshot(
        &self,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        self.retained.recover_semantic_vector_projection(
            self.scope.projection(),
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn staging_binding(&self) -> (&StoreShardIdV1, &StoreRuntimeBindingV1) {
        (&self.source_scope, &self.binding)
    }

    fn recover_verified_generation(
        &self,
        publication: &GraphPublicationKeyV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.retained.recover_semantic_vector_generation(
            publication,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn verified_head(
        &self,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<GraphVerifiedHeadV1>, GraphDbError> {
        self.retained.semantic_vector_verified_head(
            self.scope.projection(),
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn begin_stage(
        &self,
        plan: &SemanticVectorStagePlan,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageRecord, GraphDbError> {
        self.retained.begin_semantic_vector_stage(
            plan,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn resume_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageResumeOutcome, GraphDbError> {
        self.retained.resume_semantic_vector_stage(
            stage,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn published_semantic_generation(
        &self,
        key: &SemanticVectorPublishedGenerationKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorPublishedGenerationLookup, GraphDbError> {
        self.retained.published_semantic_vector_generation(
            key,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn append_stage_batch(
        &self,
        receipt: &SemanticVectorStageBatchReceipt,
        batch: GraphWriteBatch,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGenerationBatchCommit, GraphDbError> {
        self.retained.append_semantic_vector_stage_batch(
            receipt,
            batch,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn cancel_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageCancelOutcome, GraphDbError> {
        self.retained.cancel_semantic_vector_stage(
            stage,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn prepare_publication_from_staged_native(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePublicationPrepareOutcome, GraphDbError> {
        self.retained
            .prepare_semantic_vector_publication_from_staged_native(
                stage,
                authority.cancellation(),
                authority.deadline(),
            )
    }

    fn publish_ready_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.retained.publish_ready_semantic_vector_stage(
            stage,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn settle_published(
        &self,
        settlement: &SemanticVectorStagePublishSettlement,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePublishOutcome, GraphDbError> {
        self.retained.settle_published_semantic_vector_stage(
            settlement,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn reserve_one_generation(
        &self,
        after: Option<tracedecay_store::SemanticVectorStageCensusCursor>,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionStep, GraphDbError> {
        self.retained.reserve_one_semantic_vector_generation(
            after,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn finalize_reserved_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
        authorization: &SemanticVectorRetentionAuthorizationV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionAction, GraphDbError> {
        self.retained.finalize_reserved_semantic_vector_generation(
            reservation,
            authorization,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn release_reserved_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
    ) -> Result<(), GraphDbError> {
        self.retained
            .release_reserved_semantic_vector_generation(reservation)
    }

    fn source_generation_has_live_reference(
        &self,
        generation: &tracedecay_store::SemanticVectorSourceGenerationId,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        self.retained.semantic_vector_source_generation_is_live(
            generation,
            expected_revision,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn source_scope_has_live_reference(
        &self,
        source_scope: &StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        self.retained.semantic_vector_source_scope_is_live(
            source_scope,
            expected_revision,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn published_generation_dependency(
        &self,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_store::SemanticVectorPublishedGenerationDependencyLookup, GraphDbError>
    {
        self.retained
            .semantic_vector_published_generation_dependency(
                generation,
                expected_revision,
                authority.cancellation(),
                authority.deadline(),
            )
    }

    fn validate_project_census_revision(
        &self,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<(), GraphDbError> {
        self.retained
            .validate_semantic_vector_project_census_revision(
                expected_revision,
                authority.cancellation(),
                authority.deadline(),
            )
    }

    fn source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_store::SemanticVectorSourceScopeBindingLookup, GraphDbError> {
        self.retained.semantic_vector_source_scope_binding(
            code_scope_hash,
            expected_revision,
            authority.cancellation(),
            authority.deadline(),
        )
    }

    fn remove_source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        source_scope: &StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        self.retained.remove_semantic_vector_source_scope_binding(
            code_scope_hash,
            source_scope,
            expected_revision,
            authority.cancellation(),
            authority.deadline(),
        )
    }
}

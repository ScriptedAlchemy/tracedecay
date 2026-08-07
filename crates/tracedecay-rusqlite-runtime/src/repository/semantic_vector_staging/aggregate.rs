use tracedecay_store::{
    GraphProjectionIdentityV1, GraphPublicationOperationContextV1, GraphPublicationStoreV1,
    SemanticVectorPublicationAuthority, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, StoreRuntimeBindingV1,
};

use super::SemanticVectorStagingExactSqlStorage;
use super::published::*;
use super::support::*;

impl SemanticVectorPublicationAuthority for SemanticVectorStagingExactSqlStorage {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        self.handle.binding()
    }

    fn published_semantic_generation(
        &mut self,
        key: &SemanticVectorPublishedGenerationKey,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::SemanticVectorStagingStoreResult<SemanticVectorPublishedGenerationLookup>
    {
        key.validate()?;
        ensure_live(context)?;
        ensure_projection_binding(&self.handle, &key.projection)?;
        let tx = begin(&self.handle)?;
        let Some(stage) = published_stage_for(&tx, key)? else {
            rollback(tx)?;
            return Ok(SemanticVectorPublishedGenerationLookup::Missing);
        };
        if stage.record.plan.semantic_generation_id != key.semantic_generation_id {
            rollback(tx)?;
            return Err(corrupt(
                "published semantic vector generation normalized identity mismatch",
            ));
        }
        validate_stage_history(&tx, &stage, context)?;
        let verified_head = published_stage_evidence(&tx, &stage)?;
        let record = stage.record;
        rollback(tx)?;
        ensure_live(context)?;
        Ok(SemanticVectorPublishedGenerationLookup::Published {
            record,
            verified_head,
        })
    }
}

impl GraphPublicationStoreV1 for SemanticVectorStagingExactSqlStorage {
    fn append_replay(
        &mut self,
        value: &tracedecay_store::GraphPublicationReplayV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::GraphPublicationStoreResultV1<tracedecay_store::GraphReplayAppendOutcomeV1>
    {
        self.graph_publication.append_replay(value, context)
    }

    fn pending_replay(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::GraphPublicationStoreResultV1<
        Option<tracedecay_store::GraphPublicationReplayRecordV1>,
    > {
        self.graph_publication.pending_replay(projection, context)
    }

    fn replay(
        &mut self,
        key: &tracedecay_store::GraphPublicationKeyV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::GraphPublicationStoreResultV1<
        tracedecay_store::GraphPublicationReplayLookupV1,
    > {
        self.graph_publication.replay(key, context)
    }

    fn replay_page(
        &mut self,
        request: &tracedecay_store::GraphPublicationReplayPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::GraphPublicationStoreResultV1<
        tracedecay_store::GraphPublicationReplayPageV1,
    > {
        self.graph_publication.replay_page(request, context)
    }

    fn projection_page(
        &mut self,
        request: &tracedecay_store::GraphPublicationProjectionPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::GraphPublicationStoreResultV1<
        tracedecay_store::GraphPublicationProjectionPageV1,
    > {
        self.graph_publication.projection_page(request, context)
    }

    fn retire_replay(
        &mut self,
        request: &tracedecay_store::GraphPublicationReplayRetirementV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::GraphPublicationStoreResultV1<
        tracedecay_store::GraphReplayRetirementOutcomeV1,
    > {
        self.graph_publication.retire_replay(request, context)
    }

    fn retired_cleanup_page(
        &mut self,
        request: &tracedecay_store::GraphPublicationRetiredCleanupPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::GraphPublicationStoreResultV1<
        tracedecay_store::GraphPublicationRetiredCleanupPageV1,
    > {
        self.graph_publication
            .retired_cleanup_page(request, context)
    }

    fn finalize_retired_replay_cleanup(
        &mut self,
        request: &tracedecay_store::GraphPublicationReplayRetirementV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::GraphPublicationStoreResultV1<
        tracedecay_store::GraphRetiredReplayCleanupFinalizeOutcomeV1,
    > {
        self.graph_publication
            .finalize_retired_replay_cleanup(request, context)
    }

    fn verified_head(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::GraphPublicationStoreResultV1<
        Option<tracedecay_store::GraphVerifiedHeadV1>,
    > {
        self.graph_publication.verified_head(projection, context)
    }

    fn compare_and_swap_verified_head(
        &mut self,
        request: &tracedecay_store::GraphVerifiedHeadCompareAndSwapV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> tracedecay_store::GraphPublicationStoreResultV1<
        tracedecay_store::GraphVerifiedHeadCasOutcomeV1,
    > {
        self.graph_publication
            .compare_and_swap_verified_head(request, context)
    }
}

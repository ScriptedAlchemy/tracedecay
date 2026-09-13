use thiserror::Error;

use super::super::{
    GraphProjectionIdentityV1, GraphPublicationOperationContextV1, GraphPublicationStoreV1,
    RuntimeInterruptionV1, StorageRuntimeContractErrorV1, StoreRuntimeBindingV1, StoreShardIdV1,
};
use super::{
    SemanticVectorCancelledRetirement, SemanticVectorCancelledRetirementOutcome,
    SemanticVectorPublishedGenerationDependencyLookup, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorPublishedRetirement,
    SemanticVectorPublishedRetirementOutcome, SemanticVectorReadyPublicationPage,
    SemanticVectorReadyPublicationPageRequest, SemanticVectorRetirementCleanupRecord,
    SemanticVectorStageAdoptionPage, SemanticVectorStageAdoptionPageRequest,
    SemanticVectorStageAppendOutcome, SemanticVectorStageBatchKey, SemanticVectorStageBatchPage,
    SemanticVectorStageBatchPageRequest, SemanticVectorStageBatchReceipt,
    SemanticVectorStageBatchReceiptLookup, SemanticVectorStageBeginOutcome,
    SemanticVectorStageCancelOutcome, SemanticVectorStageCensusPage,
    SemanticVectorStageCensusRequest, SemanticVectorStageKey, SemanticVectorStagePendingEffectPage,
    SemanticVectorStagePendingEffectPageRequest, SemanticVectorStagePlan,
    SemanticVectorStagePublicationPrepareOutcome, SemanticVectorStagePublicationPrepareRequest,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageRecord, SemanticVectorStageSettlement, SemanticVectorStageSettlementOutcome,
    SemanticVectorStageWriterAdoption, SemanticVectorStageWriterAdoptionOutcome,
    SemanticVectorWriterFence,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticVectorStagingStoreError {
    #[error("invalid semantic-vector staging request: {0}")]
    InvalidRequest(#[from] StorageRuntimeContractErrorV1),
    #[error("semantic-vector staging interrupted: {0:?}")]
    Interrupted(RuntimeInterruptionV1),
    #[error("semantic-vector staging persistence is unavailable")]
    Infrastructure,
    #[error("semantic-vector staging writer authority was lost")]
    AuthorityLost,
    #[error("semantic-vector staging authority is busy")]
    Busy,
    #[error(
        "semantic-vector census revision changed from {expected:?} to {actual:?}; restart the census"
    )]
    CensusRevisionChanged {
        expected: super::SemanticVectorStageCensusRevision,
        actual: super::SemanticVectorStageCensusRevision,
    },
    #[error("semantic-vector staging operation context was already consumed")]
    ReusedOperationContext,
    #[error("semantic-vector staging persistence is corrupt: {0}")]
    Corrupt(String),
}

pub type SemanticVectorStagingStoreResult<T> = Result<T, SemanticVectorStagingStoreError>;

pub trait SemanticVectorStagingStore {
    fn begin_stage(
        &mut self,
        plan: &SemanticVectorStagePlan,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageBeginOutcome>;

    fn append_stage_batch(
        &mut self,
        receipt: &SemanticVectorStageBatchReceipt,
        fence: &SemanticVectorWriterFence,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageAppendOutcome>;

    fn stage(
        &mut self,
        key: &SemanticVectorStageKey,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<Option<SemanticVectorStageRecord>>;

    fn pending_stage(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<Option<SemanticVectorStageRecord>>;

    fn batch_receipt(
        &mut self,
        key: &SemanticVectorStageBatchKey,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageBatchReceiptLookup>;

    fn batch_page(
        &mut self,
        request: &SemanticVectorStageBatchPageRequest,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageBatchPage>;

    fn pending_effects(
        &mut self,
        request: &SemanticVectorStagePendingEffectPageRequest,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStagePendingEffectPage>;

    fn settle_stage_batch(
        &mut self,
        settlement: &SemanticVectorStageSettlement,
        fence: &SemanticVectorWriterFence,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageSettlementOutcome>;

    fn cancel_stage(
        &mut self,
        key: &SemanticVectorStageKey,
        fence: &SemanticVectorWriterFence,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageCancelOutcome>;

    fn adopt_stage_writer(
        &mut self,
        request: &SemanticVectorStageWriterAdoption,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageWriterAdoptionOutcome>;

    fn prepare_stage_publication(
        &mut self,
        request: &SemanticVectorStagePublicationPrepareRequest,
        fence: &SemanticVectorWriterFence,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStagePublicationPrepareOutcome>;

    fn ready_publications(
        &mut self,
        request: &SemanticVectorReadyPublicationPageRequest,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorReadyPublicationPage>;

    fn stage_census(
        &mut self,
        request: &SemanticVectorStageCensusRequest,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageCensusPage>;

    fn adoptable_stage_page(
        &mut self,
        request: &SemanticVectorStageAdoptionPageRequest,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageAdoptionPage>;

    fn retire_published_generation(
        &mut self,
        request: &SemanticVectorPublishedRetirement,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorPublishedRetirementOutcome>;

    fn remove_cancelled_generation(
        &mut self,
        request: &SemanticVectorCancelledRetirement,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorCancelledRetirementOutcome>;

    fn generation_has_live_base_reference(
        &mut self,
        shard_id: &StoreShardIdV1,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        expected_revision: super::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool>;

    fn published_generation_exists(
        &mut self,
        shard_id: &StoreShardIdV1,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool>;

    fn source_generation_has_live_reference(
        &mut self,
        shard_id: &StoreShardIdV1,
        generation: &super::SemanticVectorSourceGenerationId,
        expected_revision: super::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool>;

    fn source_scope_has_live_reference(
        &mut self,
        shard_id: &StoreShardIdV1,
        source_scope: &StoreShardIdV1,
        expected_revision: super::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool>;

    fn published_generation_dependency(
        &mut self,
        shard_id: &StoreShardIdV1,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        expected_revision: super::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorPublishedGenerationDependencyLookup>;

    fn validate_project_census_revision(
        &mut self,
        shard_id: &StoreShardIdV1,
        expected_revision: super::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<()>;

    fn source_scope_binding(
        &mut self,
        shard_id: &StoreShardIdV1,
        code_scope_hash: &super::SemanticVectorCodeScopeHash,
        expected_revision: super::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<super::SemanticVectorSourceScopeBindingLookup>;

    fn remove_source_scope_binding(
        &mut self,
        shard_id: &StoreShardIdV1,
        code_scope_hash: &super::SemanticVectorCodeScopeHash,
        source_scope: &StoreShardIdV1,
        expected_revision: super::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool>;

    fn pending_retirement_cleanup(
        &mut self,
        shard_id: &StoreShardIdV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<Option<SemanticVectorRetirementCleanupRecord>>;

    fn complete_retirement_cleanup(
        &mut self,
        retirement: &SemanticVectorPublishedRetirement,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool>;

    fn settle_published(
        &mut self,
        settlement: &SemanticVectorStagePublishSettlement,
        fence: &SemanticVectorWriterFence,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStagePublishOutcome>;
}

/// One owner-bound authority for relational staging and graph publication.
///
/// Implementations mint both halves from one retained runtime. Callers cannot
/// compose independent stores into this authority.
pub trait SemanticVectorPublicationAuthority:
    GraphPublicationStoreV1 + SemanticVectorStagingStore
{
    fn binding(&self) -> &StoreRuntimeBindingV1;

    fn published_semantic_generation(
        &mut self,
        key: &SemanticVectorPublishedGenerationKey,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorPublishedGenerationLookup>;
}

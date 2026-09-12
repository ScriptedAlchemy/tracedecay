//! Driver-neutral contracts for daemon-owned storage runtimes.
//!
//! These types describe identity, admission, consistency, operations, effects,
//! errors, and telemetry. They deliberately contain no physical paths,
//! database-driver values, executors, or connection-opening behavior.
//!
//! Canonical domain identities are re-exported instead of copied. Types whose
//! names begin with `Store` or `Runtime` carry storage-only invariants or
//! ownership. Application-layer IDs cross this lower-level dependency boundary
//! through validated lossless representations, never aliases.

mod consistency;
mod error;
mod graph_publication;
mod identity;
mod lifecycle;
mod operation;
mod outbox;
mod ports;
mod repository_read;
mod scope_set;
mod semantic_vector_staging;
mod telemetry;

pub use consistency::{
    CommitSequenceV1, ConsistencyModeV1, FrozenWatermarkCoverageV1,
    FrozenWatermarkVectorV1, ShardWatermarkV1, SnapshotLeaseV1,
    WatermarkCoverageStatusV1,
};
pub use error::{
    CorruptionClassV1, RuntimeCancellationStageV1, SaturationScopeV1,
    StorageRuntimeContractErrorV1, StorageRuntimeErrorV1, UnavailableReasonV1,
};
pub use graph_publication::{
    GraphCanonicalReplaySourceDigestV1,
    GraphDependencyGenerationClosureDigestV1,
    GraphDependencyGenerationIdentityV1, GraphGenerationIdV1, GraphNamespaceV1,
    GraphPendingReplayDiscardOutcomeV1, GraphPendingReplayDiscardV1,
    GraphProjectionIdV1, GraphProjectionIdentityV1,
    GraphPublicationIdempotencyKeyV1, GraphPublicationInputDigestV1,
    GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationProjectionPageRequestV1, GraphPublicationProjectionPageV1,
    GraphPublicationReplayCursorV1, GraphPublicationReplayLookupV1,
    GraphPublicationReplayPageRequestV1, GraphPublicationReplayPageV1,
    GraphPublicationReplayRecordV1, GraphPublicationReplayRetirementV1,
    GraphPublicationReplayTombstoneV1, GraphPublicationReplayV1,
    GraphPublicationRetiredCleanupPageRequestV1,
    GraphPublicationRetiredCleanupPageV1, GraphPublicationSequenceV1,
    GraphPublicationStoreErrorV1, GraphPublicationStoreResultV1,
    GraphPublicationStoreV1, GraphRecoveredGenerationDigestV1,
    GraphReplayAppendOutcomeV1, GraphReplayRetirementOutcomeV1,
    GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
    GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1,
    MAX_GRAPH_PUBLICATION_PROJECTION_PAGE_RECORDS_V1,
    MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1,
    MAX_GRAPH_REPLAY_DIRECT_DEPENDENCY_BYTES_V1,
    MAX_GRAPH_REPLAY_PAGE_RECORDS_V1, MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1,
    MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
};
pub use identity::{
    AuthorityEpoch, BrainId, BrainNodeId, CodeShardScopeV1, LocatorDigest,
    ProjectId, ReaderHealthLeaseIdV1, RefId, RepositoryId,
    RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1,
    RetainedGraphStoreOwnerOperationLeaseErrorV1, RuntimeLeaseIdV1,
    RuntimeMaintenanceTransitionIdV1, RuntimeOperationPermitIdV1,
    RuntimePublicationIdV1, RuntimeTransactionIdV1, SnapshotLeaseIdV1,
    StoreAuthorityEpochV1, StoreClientIdV1, StoreEffectIdV1,
    StoreEffectOrderingKeyV1, StoreIdempotencyKeyV1, StoreIncarnationV1,
    StoreOperationIdV1, StoreRuntimeBindingV1, StoreShardIdV1,
    StoreShardScopeV1, StoreSnapshotIdV1, UserProfileId, VerifiedStoreLocatorV1,
    WorktreeId, canonical_store_locator_digest, graph_store_locator_path,
};
pub use lifecycle::{
    ReaderHealthLeaseV1, RuntimeBatchCompatibilityV1, RuntimeLeaseV1,
    RuntimeMaintenanceTransitionV1, RuntimeOperationPermitV1,
    RuntimeTransactionScopeV1, StoreRuntimeRegistryPublicationV1,
};
pub use operation::{
    AdmissionConfigV1, BACKGROUND_BATCH_MAX_BYTES,
    BACKGROUND_BATCH_MAX_DELAY_MS, BACKGROUND_BATCH_MAX_OPERATIONS,
    BatchBudgetV1, CommandDigestV1, DEFAULT_GLOBAL_QUEUE_BYTES,
    DEFAULT_MAX_GLOBAL_READERS, DEFAULT_MAX_READERS_PER_HOT_SHARD,
    DEFAULT_MIN_GLOBAL_READERS, DEFAULT_MIN_READERS_PER_HOT_SHARD,
    DEFAULT_OPEN_PROJECT_RUNTIMES, DEFAULT_PER_SHARD_QUEUE_BYTES,
    DEFAULT_PER_SHARD_QUEUE_OPERATIONS, DurabilityClassV1,
    FOREGROUND_BATCH_MAX_BYTES, FOREGROUND_BATCH_MAX_DELAY_MS,
    FOREGROUND_BATCH_MAX_OPERATIONS, GlobalQueueProfileV1, GraphNodeV1,
    GraphSearchResultV1, GraphSearchScoreV1, GraphStatsV1,
    IDLE_BURST_READER_RETIRE_MS, IdempotencyIdentityV1,
    MAX_OPEN_PROJECT_RUNTIMES, OperationPriorityV1, QueueBudgetV1,
    ReaderBudgetV1, RepositoryOperationEnvelopeV1, RepositoryWritePayloadV1,
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1,
    RuntimeDeadlineV1, RuntimeRequestControlV1, StoreCommitReceiptV1,
    StoreOperationMetadataV1, WAL_HARD_LIMIT_BYTES, WAL_SOFT_LIMIT_BYTES,
    WORKSTATION_GLOBAL_QUEUE_BYTES, WalBudgetV1,
};
pub use outbox::{
    EffectIdentityV1, InboxEffectDispositionV1, OutboxAcknowledgementReceiptV1,
    OutboxEffectStateV1, RepositoryEffectV1, TransactionalInboxReceiptV1,
    TransactionalOutboxEntryV1,
};
pub use ports::{
    RuntimeInterruptionV1, RuntimeReadCoverageV1, RuntimeReadOperationV1,
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeReadResultV1,
    RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1,
    StorageRuntimePortErrorV1, StorageRuntimePortFutureV1,
    StorageRuntimePortResultV1, StorageRuntimeReadPort,
    single_shard_required_coverage_v1,
};
pub use repository_read::{
    CodeReadOperationV1, CodeReadResultV1, CodeRecoveryCandidatesPageV1,
    CodeRecoveryCandidatesQueryV1, CodeRecoveryRepositoriesPageV1,
    CodeRecoveryRepositoriesQueryV1, DiagnosticReadOperationV1,
    DiagnosticReadResultV1, EffectsInboxCursorV1, EffectsInboxPageQueryV1,
    EffectsInboxPageV1, EffectsOutboxCursorV1, EffectsOutboxPageQueryV1,
    EffectsOutboxPageV1, EffectsReadOperationV1, EffectsReadResultV1,
    ExternalSourceReadOperationV1, ExternalSourceReadResultV1,
    FactReadOperationV1, FactReadResultV1, ObservationReadOperationV1,
    ObservationReadResultV1, ProfileReadOperationV1, ProfileReadResultV1,
    ProjectReadOperationV1, ProjectReadResultV1, ProjectionRebuildProgressV1,
    ProjectionRebuildStateV1, RepositoryReadOperationV1, RepositoryReadResultV1,
    RetrievalAnchorReadOperationV1, RetrievalAnchorReadResultV1,
    StoredObservationRowV1,
};
pub use scope_set::{
    AuthorizedScopeSetRecordV1, MAX_AUTHORIZED_SCOPE_SET_BYTES_V1,
    ScopeSetCasOutcomeV1, ScopeSetCompareAndSwapV1, ScopeSetStoreContractError,
};
pub use semantic_vector_staging::{
    MAX_SEMANTIC_VECTOR_ADOPTION_PAGE_RECORDS,
    MAX_SEMANTIC_VECTOR_CENSUS_PAGE_RECORDS,
    MAX_SEMANTIC_VECTOR_EMBEDDING_DIMENSION,
    MAX_SEMANTIC_VECTOR_PENDING_EFFECT_PAGE_RECORDS,
    MAX_SEMANTIC_VECTOR_STAGE_CHUNKS_PER_BATCH,
    MAX_SEMANTIC_VECTOR_STAGE_PAGE_RECORDS, SemanticEmbeddingProjectionDigestV1,
    SemanticModelArtifactDigestV1, SemanticPrivacyDomainDigestV1,
    SemanticProjectionManifestDigestV1, SemanticVectorBatchInputDigest,
    SemanticVectorBatchOutputDigest, SemanticVectorBatchReceiptDigest,
    SemanticVectorBuildId, SemanticVectorCancelledRetirement,
    SemanticVectorCancelledRetirementOutcome, SemanticVectorCensusDependencyV1,
    SemanticVectorCheckpointDigest, SemanticVectorChunkDigest,
    SemanticVectorChunkId, SemanticVectorChunkManifestAccumulator,
    SemanticVectorChunkManifestDigest, SemanticVectorChunkManifestMember,
    SemanticVectorCodeScopeHash, SemanticVectorEffectFailureDigest,
    SemanticVectorGraphBatchDigest, SemanticVectorOutboxSequence,
    SemanticVectorOutputDigest, SemanticVectorPlanDigest,
    SemanticVectorProjectCensusReceipt, SemanticVectorPublicationAuthority,
    SemanticVectorPublicationIntentDigest,
    SemanticVectorPublishedGenerationDependencyLookup,
    SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorPublishedRetirement,
    SemanticVectorPublishedRetirementOutcome,
    SemanticVectorReadyPublicationCursor, SemanticVectorReadyPublicationPage,
    SemanticVectorReadyPublicationPageRequest,
    SemanticVectorReconstructionRecipe, SemanticVectorRetirementCleanupCursor,
    SemanticVectorRetirementCleanupRecord, SemanticVectorSourceDependencyV1,
    SemanticVectorSourceGenerationId, SemanticVectorSourceManifestDigest,
    SemanticVectorSourceScopeBindingLookup, SemanticVectorStageAdoptionCursor,
    SemanticVectorStageAdoptionPage, SemanticVectorStageAdoptionPageRequest,
    SemanticVectorStageAdoptionRecord, SemanticVectorStageAppendOutcome,
    SemanticVectorStageBatchCursor, SemanticVectorStageBatchKey,
    SemanticVectorStageBatchPage, SemanticVectorStageBatchPageRequest,
    SemanticVectorStageBatchReceipt, SemanticVectorStageBatchReceiptLookup,
    SemanticVectorStageBeginOutcome, SemanticVectorStageCancelOutcome,
    SemanticVectorStageCensusCounts, SemanticVectorStageCensusCursor,
    SemanticVectorStageCensusPage, SemanticVectorStageCensusRecord,
    SemanticVectorStageCensusRequest, SemanticVectorStageCensusRevision,
    SemanticVectorStageChunkOperation, SemanticVectorStageChunkReceipt,
    SemanticVectorStageEffectState, SemanticVectorStageEffectTerminal,
    SemanticVectorStageGraphBatchEffect, SemanticVectorStageIncomplete,
    SemanticVectorStageKey, SemanticVectorStagePendingEffectCursor,
    SemanticVectorStagePendingEffectPage,
    SemanticVectorStagePendingEffectPageRequest, SemanticVectorStagePlan,
    SemanticVectorStagePublicationIntent,
    SemanticVectorStagePublicationPrepareOutcome,
    SemanticVectorStagePublicationPrepareRequest,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageRecord, SemanticVectorStageResumeOutcome,
    SemanticVectorStageSettlement, SemanticVectorStageSettlementOutcome,
    SemanticVectorStageState, SemanticVectorStageWriterAdoption,
    SemanticVectorStageWriterAdoptionOutcome, SemanticVectorStagingStore,
    SemanticVectorStagingStoreError, SemanticVectorStagingStoreResult,
    SemanticVectorWriterFence, semantic_vector_chunk_manifest_digest,
};
pub use telemetry::{
    MaintenanceTelemetryV1, ReaderLaneV1, RuntimeMaintenanceStateV1,
    WalPressureV1,
};

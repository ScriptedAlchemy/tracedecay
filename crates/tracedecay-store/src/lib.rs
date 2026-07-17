//! Store-facing persistence contracts for TraceDecay.
//!
//! This crate owns only persistence contracts and their data transfer objects.
//! Connection ownership, transaction boundaries, recovery policy, and storage
//! resolution remain with the application crate's authoritative store adapter.

pub mod memory;
pub mod observation;
pub mod projection;
pub mod transcript;

pub use memory::{
    CompatibilityDashboardEntityV1, CompatibilityDashboardFactDetailQueryV1,
    CompatibilityDashboardFactDetailV1, CompatibilityDashboardFactEntityLinkV1,
    CompatibilityDashboardFactSummaryV1, CompatibilityDashboardGrowthPointV1,
    CompatibilityDashboardHrrCoverageV1, CompatibilityDashboardHrrStateV1,
    CompatibilityDashboardMemoryBankV1, CompatibilityDashboardMemoryOverviewQueryV1,
    CompatibilityDashboardMemoryOverviewV1, CompatibilityDashboardNamedCountV1,
    CompatibilityDashboardOplogDetailsV1, CompatibilityDashboardOplogEntryV1,
    CompatibilityDashboardOplogQueryV1, CompatibilityDashboardVectorPointV1,
    CompatibilityDashboardVectorPointsQueryV1, CompatibilityFactAddAliasV1,
    CompatibilityFactAddCommandV1, CompatibilityFactAddDispositionV1,
    CompatibilityFactAddOutcomeV1, CompatibilityFactAvailabilityV1,
    CompatibilityFactContentDigestQueryV1, CompatibilityFactContradictionPageV1,
    CompatibilityFactContradictionQueryV1, CompatibilityFactContradictionV1,
    CompatibilityFactCurationBatchV1, CompatibilityFactCurationOperationV1,
    CompatibilityFactCurationReceiptV1, CompatibilityFactFeedbackActionV1,
    CompatibilityFactFeedbackCommandV1, CompatibilityFactFeedbackDetailsAvailabilityV1,
    CompatibilityFactFeedbackHistoryEntryV1, CompatibilityFactFeedbackHistoryQueryV1,
    CompatibilityFactFeedbackHistoryV1, CompatibilityFactFeedbackOutcomeV1,
    CompatibilityFactHistoryQueryV1, CompatibilityFactHistoryV1, CompatibilityFactIdV1,
    CompatibilityFactInspectionV1, CompatibilityFactLinkV1, CompatibilityFactListQueryV1,
    CompatibilityFactMappingV1, CompatibilityFactMergeCommandV1, CompatibilityFactMergeEntitiesV1,
    CompatibilityFactMergeOutcomeV1, CompatibilityFactNormalizeTagsV1, CompatibilityFactPageV1,
    CompatibilityFactProjectionV1, CompatibilityFactProposalImportReceiptV1,
    CompatibilityFactProposalImportV1, CompatibilityFactProposalLegacyRecordV1,
    CompatibilityFactProposalPageV1, CompatibilityFactProposalPromotionDispositionV1,
    CompatibilityFactProposalPromotionResultV1, CompatibilityFactProposalPromotionV1,
    CompatibilityFactProposalRecordV1, CompatibilityFactProposalRevisionV1,
    CompatibilityFactProposalStateV1, CompatibilityFactRelationV1,
    CompatibilityFactRemoveCommandV1, CompatibilityFactRemoveOutcomeV1,
    CompatibilityFactRepairVectorV1, CompatibilityFactRetrievalCommandV1,
    CompatibilityFactSearchCursorV1, CompatibilityFactSearchFilterV1, CompatibilityFactSearchHitV1,
    CompatibilityFactSearchKindV1, CompatibilityFactSearchPageV1, CompatibilityFactSearchQuery,
    CompatibilityFactSearchScoresV1, CompatibilityFactSourceV1, CompatibilityFactStatusV1,
    CompatibilityFactTargetV1, CompatibilityFactTelemetryV1, CompatibilityFactUnavailableV1,
    CompatibilityFactUpdateCommandV1, CompatibilityFactUpdateOutcomeV1,
    CompatibilityFactUpdatePatchV1, CompatibilityFactV1, CompatibilityFeedbackRepairProgressV1,
    CompatibilityLegacyEntityTargetV1, CompatibilityLegacyMemoryCutoverCommandV1,
    CompatibilityLegacyMemoryCutoverProgressV1, CompatibilityMemoryAlgebraV1,
    CompatibilityMemoryFeedbackFunnelV1, CompatibilityMemoryRepairCommandV1,
    CompatibilityMemoryRepairStatsV1, CompatibilityMemoryStatusV1, CompatibilityProjectionStateV1,
    CurrentFactsQuery, FactAsOfQuery, FactCommitConflict, FactCommitOutcome, FactCommitReceipt,
    FactCompatibilityResult, FactCompatibilityStore, FactCompatibilityStoreError, FactCurrentQuery,
    FactLineageCursor, FactLineageQuery, FactProposalPromotionStateV1, FactProposalStore,
    FactProposalStoreError, FactStore, FactStoreError, FactStoreResult, FactWriteBatch,
    LegacyFactQuery, PromoteFactProposal, PromoteFactProposalOutcome, RetrievalAnchorQuery,
    StoredFactV1,
};
pub use observation::{
    AnchoredObservationWrite, ObservationCommitReceipt, ObservationPersistOutcome,
    ObservationProjectionStatus, ObservationReplayRequest, ObservationStore, ObservationStoreError,
    ObservationStoreResult, ObservationWrite, ObservedEvidenceAnchorResolution,
    RepositoryProvenanceAttachmentV1, StoredObservation,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
    build_scope_resolution_authorization_v1,
};
pub use projection::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ClaudeObservationProjection,
    ClaudeSessionMessageProjection, ObservationProjection, ObservationProjectionStore,
    ProjectedObservation, ProjectionCheckpoint, ProjectionPersistOutcome, ProjectionProvenance,
    ProjectionRebuildOutcome, ProjectionSkipReason, ProjectionStoreError, ProjectionStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION, SESSION_MESSAGE_PROJECTOR_VERSION_V1,
    SESSION_MESSAGE_PROJECTOR_VERSION_V2, SESSION_MESSAGE_PROJECTOR_VERSION_V3,
    SESSION_MESSAGE_PROJECTOR_VERSION_V4, SessionMessageProjection, WorkflowFactProjection,
    WorkflowFactRecord,
};
pub use transcript::{
    ParseOffset, SessionMessageRecord, SessionRecord, TranscriptStore, TranscriptStoreError,
    TranscriptStoreResult, TranscriptWriteBatch, TranscriptWriteKind,
};

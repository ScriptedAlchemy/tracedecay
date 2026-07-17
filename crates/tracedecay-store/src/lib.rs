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
    CompatibilityFactAddCommandV1, CompatibilityFactAddDispositionV1,
    CompatibilityFactAddOutcomeV1, CompatibilityFactAvailabilityV1,
    CompatibilityFactContradictionPageV1, CompatibilityFactContradictionQueryV1,
    CompatibilityFactContradictionV1, CompatibilityFactFeedbackActionV1,
    CompatibilityFactFeedbackCommandV1, CompatibilityFactFeedbackOutcomeV1,
    CompatibilityFactHistoryQueryV1, CompatibilityFactHistoryV1, CompatibilityFactIdV1,
    CompatibilityFactInspectionV1, CompatibilityFactListQueryV1, CompatibilityFactMappingV1,
    CompatibilityFactPageV1, CompatibilityFactProjectionV1,
    CompatibilityFactProposalImportReceiptV1, CompatibilityFactProposalImportV1,
    CompatibilityFactProposalLegacyRecordV1, CompatibilityFactProposalPageV1,
    CompatibilityFactProposalPromotionV1, CompatibilityFactProposalRecordV1,
    CompatibilityFactProposalRevisionV1, CompatibilityFactProposalStateV1,
    CompatibilityFactRemoveCommandV1, CompatibilityFactRemoveOutcomeV1,
    CompatibilityFactRetrievalCommandV1, CompatibilityFactSearchCursorV1,
    CompatibilityFactSearchFilterV1, CompatibilityFactSearchHitV1, CompatibilityFactSearchKindV1,
    CompatibilityFactSearchPageV1, CompatibilityFactSearchQuery, CompatibilityFactSearchScoresV1,
    CompatibilityFactSourceV1, CompatibilityFactStatusV1, CompatibilityFactTargetV1,
    CompatibilityFactTelemetryV1, CompatibilityFactUnavailableV1, CompatibilityFactUpdateCommandV1,
    CompatibilityFactUpdateOutcomeV1, CompatibilityFactUpdatePatchV1, CompatibilityFactV1,
    CompatibilityMemoryAlgebraV1, CompatibilityMemoryFeedbackFunnelV1,
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
    ObservationStoreResult, ObservationWrite, RepositoryProvenanceAttachmentV1, StoredObservation,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
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

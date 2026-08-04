//! Store-facing persistence contracts for TraceDecay.
//!
//! This crate owns only persistence contracts and their data transfer objects.
//! Connection ownership, transaction boundaries, recovery policy, and storage
//! resolution remain with the application crate's authoritative store adapter.

mod canonical_projection;
pub mod configuration;
pub mod cursor_dispatch;
pub mod diagnostics;
pub mod evidence_assembly;
pub mod external_source;
// The crash harness has to hold a live daemon inside a persistence boundary, so
// it needs the filesystem and thread authority that these contracts refuse.
// Keeping it outside `src/` is what makes that split structural rather than a
// guard exception, while the cfg keeps it out of every ordinary build.
#[cfg(tracedecay_observation_fault_harness)]
#[path = "../test-support/fault_harness.rs"]
pub mod fault_harness;
pub mod git_index_transactions;
pub mod memory;
pub mod observation;
pub mod projection;
pub mod retrieval_anchor;
pub mod runtime;
pub mod schema;
pub mod session;
pub mod transcript;

pub use canonical_projection::{
    canonical_fact_text, derive_canonical_projection, workflow_semantic_kind,
};
pub use configuration::{
    ConfigurationCommitV1, ConfigurationMutationReceiptV1, ConfigurationRevisionRecordV1,
    ConfigurationRevisionStore, ConfigurationStoreError, ConfigurationStoreResult,
};
pub use diagnostics::{
    DIAGNOSTIC_STATE_CLEARED, DIAGNOSTIC_STATE_CURRENT, DIAGNOSTIC_STATE_SUPERSEDED,
    DiagnosticGenerationSupersessionV1, DiagnosticPublicationDispositionV1,
    DiagnosticPublicationReceiptV1, DiagnosticRecordStateKindV1, DiagnosticStore,
    DiagnosticStoreError, DiagnosticStoreResult, SanitizedCleanDiagnosticSnapshotV1,
    diagnostic_evidence_class_name, diagnostic_producer_kind_name, diagnostic_severity_name,
    diagnostic_state_columns, parse_diagnostic_evidence_class, parse_diagnostic_producer_kind,
    parse_diagnostic_severity,
};
pub use evidence_assembly::{
    CanonicalSourceOccurrenceSetIdentityProjectionV1, CanonicalSourceOccurrenceSetRecordV1,
    EvidenceAssemblyDrilldownPageV1, EvidenceAssemblyIdempotencyKeyV1, EvidenceAssemblyOwnerV1,
    EvidenceAssemblyPublicationIdentityProjectionV1, EvidenceAssemblyPublicationOutcomeV1,
    EvidenceAssemblyPublicationReceiptV1, EvidenceAssemblyReadOperationV1,
    EvidenceAssemblyReadResultV1, EvidenceAssemblyStore, EvidenceAssemblyStoreError,
    EvidenceAssemblyStoreResult, EvidenceAssemblyWriteV1, EvidenceSourceOccurrenceRecordV1,
    EvidenceSourceTimelineV1, EvidenceSpanCatalogBindingV1, EvidenceSpanHorizonV1,
    EvidenceSpanIdentityProjectionV1, EvidenceSpanMemberReceiptBindingV1,
    EvidenceSpanProjectionReceiptIdentityProjectionV1, EvidenceSpanProjectionReceiptV1,
    EvidenceSpanRecordV1, EvidenceSpanRunV1, MAX_EVIDENCE_ASSEMBLY_MEMBERS_V1,
    PrivacyBoundRequestDigestV1, PrivacyBoundRequestEnvelopeV1,
    RetrieverContributionIdentityProjectionV1, RetrieverContributionRecordV1, RetrieverIdentityV1,
    RetrieverWatermarkBindingV1, SanitizedObservationByteRangeV1, SourceCapabilityCatalogBindingV1,
    SourceOccurrenceCoordinateV1, SourceOccurrenceIdentityProjectionV1, SourceOccurrenceKindV1,
    SourceOccurrenceRelationV1, SourceOccurrenceSanitizationV1, SourceTimelineKeyV1,
    VerifiedSourceOrderingProofV1, derive_canonical_source_occurrence_set_id_v1,
    derive_evidence_assembly_publication_receipt_id_v1, derive_evidence_span_id_v1,
    derive_evidence_span_projection_receipt_id_v1, derive_retriever_contribution_id_v1,
    derive_source_occurrence_id_v1,
};
pub use external_source::{
    MAX_SOURCE_COMMIT_OBSERVATIONS_V1, SourceAuthorityPublicationReceiptV1,
    SourceAuthorityPublicationV1, SourceCommitApplyOutcomeV1, SourceCommitReceiptV1,
    SourceCommitV1, SourceObjectLineageV1, SourceObjectMutationV1, SourceObjectTransitionV1,
    SourceObservationEvidenceV1, SourceProjectionCommitV1, SourceProjectionEffectV1,
    SourceStoreErrorV1, SourceStoreResult, SourceStoreStateV1, apply_source_authority_publication,
    apply_source_commit,
};
pub use git_index_transactions::{
    GitIndexTransactionBeginRequestV1, GitIndexTransactionBeginResultV1,
    GitIndexTransactionRecordV1, GitIndexTransactionStore, GitIndexTransactionStoreError,
    GitIndexTransactionStoreResult, GitIndexTransactionTerminalWriteV1,
};
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
    CompatibilityLegacyEntityTargetV1, CompatibilityMemoryAlgebraV1,
    CompatibilityMemoryFeedbackFunnelV1, CompatibilityMemoryRepairCommandV1,
    CompatibilityMemoryRepairStatsV1, CompatibilityMemoryStatusV1, CompatibilityProjectionStateV1,
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactCommitConflict, FactCommitOutcome,
    FactCommitReceipt, FactCompatibilityResult, FactCompatibilityStore,
    FactCompatibilityStoreError, FactContradictionStateV1, FactCurrentQuery, FactCurrentResponseV1,
    FactLineageCursor, FactLineageQuery, FactLineageResponseV1, FactProposalPromotionStateV1,
    FactProposalStore, FactProposalStoreError, FactQueryCoverageV1, FactStore, FactStoreError,
    FactStoreResult, FactWriteBatch, LegacyFactQuery, MAX_FACT_QUERY_CONTRADICTIONS,
    MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1, MemoryV2ArchiveConflictV1, MemoryV2ArchiveError,
    MemoryV2ArchiveFamilyV1, MemoryV2ArchiveRecordV1, MemoryV2ArchiveReferenceV1,
    MemoryV2ArchiveScalarV1, MemoryV2OwnerArchiveV1, MemoryV2OwnerMergePlanV1, PromoteFactProposal,
    PromoteFactProposalOutcome, RetrievalAnchorQuery, StoredFactV1,
    authoritative_memory_v2_archive_families, plan_memory_v2_owner_merge,
};
pub use observation::{
    AnchoredObservationWrite, CursorAdvanceOutcome, ObservationAdmissionPort,
    ObservationCaptureSink, ObservationCommitReceipt, ObservationCoverageReason,
    ObservationCoverageV1, ObservationCursorAdvance, ObservationCursorPort,
    ObservationPersistOutcome, ObservationProjectionStatus, ObservationReplayRequest,
    ObservationStore, ObservationStoreError, ObservationStoreResult, ObservationWrite,
    ObservedEvidenceAnchorResolution, RepositoryProvenanceAttachmentV1, StoredObservation,
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
pub use retrieval_anchor::{
    AnchorDerivativeKindV1, AnchorDispositionAppendOutcomeV1, AnchorDispositionReasonClassV1,
    AnchorDispositionStateV1, RetrievalAnchorDerivativeV1, RetrievalAnchorDispositionRecordV1,
    RetrievalAnchorDispositionStore, RetrievalAnchorOwnerV1, RetrievalAnchorStoreError,
    RetrievalAnchorStoreResult, RetrievalAnchorTombstoneV1, StoredRetrievalAnchorRecordV1,
};
pub use runtime::*;
pub use schema::{GENERATION_DIAGNOSTICS_SCHEMA_DDL, RETRIEVAL_ANCHORS_SCHEMA_DDL};
pub use session::{
    MAX_SESSION_SUMMARY_SOURCE_ANCHORS, MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS,
    MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE, SessionFrozenWatermarksV1,
    SessionGenerationActivateOperation, SessionGenerationActivatePermit,
    SessionGenerationActivationReceiptV1, SessionGenerationActivationRequestV1,
    SessionGenerationRebuildBeginOperation, SessionGenerationRebuildBeginPermit,
    SessionGenerationRebuildDispositionV1, SessionGenerationRebuildReceiptV1,
    SessionGenerationRebuildRequestV1, SessionProjectionBatchPersistOperation,
    SessionProjectionBatchPersistPermit, SessionRefreshBeginOrJoinOperation,
    SessionRefreshBeginOrJoinPermit, SessionRefreshBeginOrJoinReceiptV1,
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCancelOperation, SessionRefreshCancelPermit,
    SessionRefreshCancellationRequestV1, SessionRefreshCompleteOperation,
    SessionRefreshCompletePermit, SessionRefreshCompletionRequestV1, SessionRefreshDispositionV1,
    SessionRefreshFailOperation, SessionRefreshFailPermit,
    SessionRefreshFailureCodeInvalidReasonV1, SessionRefreshFailureCodeV1,
    SessionRefreshFailureRequestV1, SessionRefreshFrontierV1,
    SessionRefreshProgressPersistOperation, SessionRefreshProgressPersistPermit,
    SessionRefreshProgressReadOperation, SessionRefreshProgressReadPermit,
    SessionRefreshProgressRequestV1, SessionRefreshProgressV1, SessionRefreshReceiptReadOperation,
    SessionRefreshReceiptReadPermit, SessionRefreshReceiptRequestV1, SessionRefreshReceiptV1,
    SessionRefreshStateV1, SessionRefreshStore, SessionRefreshTerminalStateV1,
    SessionRetrievalPageV1, SessionRetrievalStore, SessionSnapshotFreezeOperation,
    SessionSnapshotFreezePermit, SessionStoreError, SessionStoreResult,
    SessionSummaryPublicationRequestV1, SessionTemporalCapabilitiesV1,
    SessionTemporalCapabilityProvider, SessionTemporalCapabilityV1,
    SessionTemporalDigestInvalidReasonV1, SessionTemporalDigestV1, SessionTemporalOperationPermit,
    SessionTemporalPageRetrieveOperation, SessionTemporalPageRetrievePermit,
    SessionTemporalProjectionBatchDispositionV1, SessionTemporalProjectionBatchReceiptV1,
    SessionTemporalProjectionBatchV1, SessionTemporalProjectionStore,
    SessionTemporalRetrievalRequestV1, SessionTemporalSnapshotRequestV1, SessionTemporalSnapshotV1,
};
pub use transcript::{
    ParseOffset, SessionMessageRecord, SessionRecord, TranscriptStore, TranscriptStoreError,
    TranscriptStoreResult, TranscriptWriteBatch, TranscriptWriteKind,
};

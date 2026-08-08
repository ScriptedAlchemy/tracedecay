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
pub mod native_integration;
pub mod observation;
pub mod projection;
pub mod remote;
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
    MAX_SOURCE_ACQUISITION_ATTEMPTS_V1, MAX_SOURCE_ACQUISITION_RECEIPTS_V1,
    MAX_SOURCE_COMMIT_OBSERVATIONS_V1, SourceAcquisitionQueueCasV1,
    SourceAcquisitionQueueContractErrorV1, SourceAcquisitionQueueResultV1,
    SourceAcquisitionQueueStateV1, SourceAcquisitionRequestV1,
    SourceAuthorityPublicationApplyOutcomeV1, SourceAuthorityPublicationReceiptV1,
    SourceAuthorityPublicationV1, SourceCommitApplyOutcomeV1, SourceCommitReceiptV1,
    SourceCommitV1, SourceObjectLineageV1, SourceObjectMutationV1, SourceObjectTransitionV1,
    SourceObservationEvidenceV1, SourcePendingProjectionV1, SourceProjectionApplyOutcomeV1,
    SourceProjectionCommitV1, SourceProjectionEffectV1, SourceScheduledRefetchV1,
    SourceStoreErrorV1, SourceStoreResult, SourceStoreStateV1, apply_source_authority_publication,
    apply_source_commit, apply_source_projection, build_source_projection,
};
pub use git_index_transactions::{
    GitIndexPreviewInputReadV1, GitIndexTransactionBeginRequestV1,
    GitIndexTransactionBeginResultV1, GitIndexTransactionRecordV1, GitIndexTransactionStore,
    GitIndexTransactionStoreError, GitIndexTransactionStoreResult,
    GitIndexTransactionTerminalWriteV1, MAX_GIT_INDEX_PREVIEW_INPUT_BYTES,
    MAX_GIT_INDEX_PREVIEW_INPUT_GC_BATCH,
};
pub use memory::{
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactCommitConflict, FactCommitOutcome,
    FactCommitReceipt, FactContradictionStateV1, FactCurrentQuery, FactCurrentResponseV1,
    FactLineageCursor, FactLineageQuery, FactLineageResponseV1, FactProposalPromotionStateV1,
    FactProposalStore, FactProposalStoreError, FactQueryCoverageV1, FactStore, FactStoreError,
    FactStoreResult, FactWriteBatch, LegacyFactQuery, MAX_FACT_QUERY_CONTRADICTIONS,
    ProjectMemoryDashboardEntityV1, ProjectMemoryDashboardFactDetailQueryV1,
    ProjectMemoryDashboardFactDetailV1, ProjectMemoryDashboardFactEntityLinkV1,
    ProjectMemoryDashboardFactSummaryV1, ProjectMemoryDashboardGrowthPointV1,
    ProjectMemoryDashboardHrrCoverageV1, ProjectMemoryDashboardHrrStateV1,
    ProjectMemoryDashboardMemoryBankV1, ProjectMemoryDashboardMemoryOverviewQueryV1,
    ProjectMemoryDashboardMemoryOverviewV1, ProjectMemoryDashboardNamedCountV1,
    ProjectMemoryDashboardOplogDetailsV1, ProjectMemoryDashboardOplogEntryV1,
    ProjectMemoryDashboardOplogQueryV1, ProjectMemoryDashboardVectorPointV1,
    ProjectMemoryDashboardVectorPointsQueryV1, ProjectMemoryFactAddAliasV1,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddDispositionV1,
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactAvailabilityV1,
    ProjectMemoryFactContentDigestQueryV1, ProjectMemoryFactContradictionPageV1,
    ProjectMemoryFactContradictionQueryV1, ProjectMemoryFactContradictionV1,
    ProjectMemoryFactCurationBatchV1, ProjectMemoryFactCurationOperationV1,
    ProjectMemoryFactCurationReceiptV1, ProjectMemoryFactFeedbackActionV1,
    ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactFeedbackDetailsAvailabilityV1,
    ProjectMemoryFactFeedbackHistoryEntryV1, ProjectMemoryFactFeedbackHistoryQueryV1,
    ProjectMemoryFactFeedbackHistoryV1, ProjectMemoryFactFeedbackOutcomeV1,
    ProjectMemoryFactHistoryQueryV1, ProjectMemoryFactHistoryV1, ProjectMemoryFactIdV1,
    ProjectMemoryFactInspectionV1, ProjectMemoryFactLinkV1, ProjectMemoryFactListQueryV1,
    ProjectMemoryFactMappingV1, ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeEntitiesV1,
    ProjectMemoryFactMergeOutcomeV1, ProjectMemoryFactNormalizeTagsV1, ProjectMemoryFactPageV1,
    ProjectMemoryFactProjectionV1, ProjectMemoryFactProposalEvidenceV1,
    ProjectMemoryFactProposalPageV1, ProjectMemoryFactProposalPromotionDispositionV1,
    ProjectMemoryFactProposalPromotionResultV1, ProjectMemoryFactProposalPromotionV1,
    ProjectMemoryFactProposalRecordV1, ProjectMemoryFactProposalRevisionV1,
    ProjectMemoryFactProposalStateV1, ProjectMemoryFactRelationV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1,
    ProjectMemoryFactRepairVectorV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactSearchCursorV1, ProjectMemoryFactSearchFilterV1, ProjectMemoryFactSearchHitV1,
    ProjectMemoryFactSearchKindV1, ProjectMemoryFactSearchPageV1, ProjectMemoryFactSearchQuery,
    ProjectMemoryFactSearchScoresV1, ProjectMemoryFactSourceV1, ProjectMemoryFactStatusV1,
    ProjectMemoryFactStore, ProjectMemoryFactTargetV1, ProjectMemoryFactTelemetryV1,
    ProjectMemoryFactUnavailableV1, ProjectMemoryFactUpdateCommandV1,
    ProjectMemoryFactUpdateOutcomeV1, ProjectMemoryFactUpdatePatchV1, ProjectMemoryFactV1,
    ProjectMemoryFeedbackRepairProgressV1, ProjectMemoryLegacyEntityTargetV1,
    ProjectMemoryMemoryAlgebraV1, ProjectMemoryMemoryFeedbackFunnelV1,
    ProjectMemoryMemoryRepairCommandV1, ProjectMemoryMemoryRepairStatsV1,
    ProjectMemoryMemoryStatusV1, ProjectMemoryProjectionStateV1, ProjectMemoryRelationProvenanceV1,
    ProjectMemoryResult, ProjectMemoryStoreError, PromoteFactProposal, PromoteFactProposalOutcome,
    RetrievalAnchorQuery, StoredFactV1,
};
pub use native_integration::{
    NativeIntegrationBeginResultV1, NativeIntegrationRecordV1, NativeIntegrationStore,
    NativeIntegrationStoreError, NativeIntegrationStoreResult, NativeWorktreeCleanupBeginResultV1,
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
    PROVIDER_USAGE_PROJECTOR_VERSION, ProjectedObservation, ProjectionCheckpoint,
    ProjectionPersistOutcome, ProjectionProvenance, ProjectionRebuildOutcome, ProjectionSkipReason,
    ProjectionStoreError, ProjectionStoreResult, SESSION_MESSAGE_PROJECTOR_VERSION,
    SESSION_MESSAGE_PROJECTOR_VERSION_V1, SESSION_MESSAGE_PROJECTOR_VERSION_V2,
    SESSION_MESSAGE_PROJECTOR_VERSION_V3, SESSION_MESSAGE_PROJECTOR_VERSION_V4,
    SessionMessageProjection, WorkflowFactProjection, WorkflowFactRecord,
};
pub use remote::{RemoteObservationReplayWriteV1, RemoteWriterFenceInstallV1};
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

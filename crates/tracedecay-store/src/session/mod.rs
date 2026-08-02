//! Transport-neutral contracts for session temporal projection and retrieval.
//!
//! These modules define bounded DTOs and ports only. Connection ownership,
//! transactions, SQL, transport, daemon scheduling, and runtime ownership stay
//! with downstream adapters.

mod common;
mod content;
mod projection;
mod refresh;
mod retrieval;
mod summary;

pub use common::{
    SessionFrozenWatermarksV1, SessionGenerationActivateOperation, SessionGenerationActivatePermit,
    SessionGenerationRebuildBeginOperation, SessionGenerationRebuildBeginPermit,
    SessionProjectionBatchPersistOperation, SessionProjectionBatchPersistPermit,
    SessionRefreshBeginOrJoinOperation, SessionRefreshBeginOrJoinPermit,
    SessionRefreshCancelOperation, SessionRefreshCancelPermit, SessionRefreshCompleteOperation,
    SessionRefreshCompletePermit, SessionRefreshFailOperation, SessionRefreshFailPermit,
    SessionRefreshFailureCodeInvalidReasonV1, SessionRefreshProgressPersistOperation,
    SessionRefreshProgressPersistPermit, SessionRefreshProgressReadOperation,
    SessionRefreshProgressReadPermit, SessionRefreshReceiptReadOperation,
    SessionRefreshReceiptReadPermit, SessionRefreshStateV1, SessionSnapshotFreezeOperation,
    SessionSnapshotFreezePermit, SessionStoreError, SessionStoreResult,
    SessionTemporalCapabilitiesV1, SessionTemporalCapabilityProvider, SessionTemporalCapabilityV1,
    SessionTemporalDigestInvalidReasonV1, SessionTemporalDigestV1, SessionTemporalOperationPermit,
    SessionTemporalPageRetrieveOperation, SessionTemporalPageRetrievePermit,
    SessionTemporalSnapshotRequestV1, SessionTemporalSnapshotV1,
};
pub use content::{
    MAX_SESSION_CONTENT_FTS_CHARS, MAX_SESSION_CONTENT_GC_PAGE_SIZE, SessionContentErrorV1,
    SessionContentFileLocatorV1, SessionContentGcRequestV1, SessionContentKeyV1,
    SessionContentKindV1, SessionContentObjectV1, SessionContentOccurrenceOwnerV1,
    SessionContentOwnerV1, SessionContentProjectionOwnerV1, SessionContentReadRequestV1,
    SessionContentReferenceV1, SessionContentResult, SessionContentSearchTextV1,
    SessionContentStorageV1, SessionContentSummaryOwnerV1, SessionContentValueV1,
};
pub use projection::{
    MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS, SessionGenerationActivationReceiptV1,
    SessionGenerationActivationRequestV1, SessionGenerationRebuildDispositionV1,
    SessionGenerationRebuildReceiptV1, SessionGenerationRebuildRequestV1,
    SessionTemporalProjectionBatchDispositionV1, SessionTemporalProjectionBatchReceiptV1,
    SessionTemporalProjectionBatchV1, SessionTemporalProjectionStore,
};
pub use refresh::{
    SessionRefreshBeginOrJoinReceiptV1, SessionRefreshBeginOrJoinRequestV1,
    SessionRefreshCancellationRequestV1, SessionRefreshCompletionRequestV1,
    SessionRefreshDispositionV1, SessionRefreshFailureCodeV1, SessionRefreshFailureRequestV1,
    SessionRefreshFrontierV1, SessionRefreshProgressRequestV1, SessionRefreshProgressV1,
    SessionRefreshReceiptRequestV1, SessionRefreshReceiptV1, SessionRefreshStore,
    SessionRefreshTerminalStateV1,
};
pub use retrieval::{
    MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE, SessionRetrievalPageV1, SessionRetrievalStore,
    SessionTemporalRetrievalRequestV1,
};
pub use summary::{MAX_SESSION_SUMMARY_SOURCE_ANCHORS, SessionSummaryPublicationRequestV1};

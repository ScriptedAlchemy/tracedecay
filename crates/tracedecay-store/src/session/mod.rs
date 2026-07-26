//! Transport-neutral contracts for session temporal projection and retrieval.
//!
//! These modules define bounded DTOs and ports only. Connection ownership,
//! transactions, SQL, transport, daemon scheduling, and runtime ownership stay
//! with downstream adapters.

mod common;
mod migration;
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
    SessionSummaryPublishOrReplayOperation, SessionSummaryPublishOrReplayPermit,
    SessionTemporalCapabilitiesV1, SessionTemporalCapabilityProvider, SessionTemporalCapabilityV1,
    SessionTemporalDigestInvalidReasonV1, SessionTemporalDigestV1,
    SessionTemporalMigrationBatchApplyOperation, SessionTemporalMigrationBatchApplyPermit,
    SessionTemporalMigrationReceiptReadOperation, SessionTemporalMigrationReceiptReadPermit,
    SessionTemporalOperationPermit, SessionTemporalPageRetrieveOperation,
    SessionTemporalPageRetrievePermit, SessionTemporalSnapshotRequestV1, SessionTemporalSnapshotV1,
};
pub use migration::{
    MAX_SESSION_TEMPORAL_MIGRATION_BATCH_ITEMS, SessionTemporalMigrationBatchV1,
    SessionTemporalMigrationDispositionV1, SessionTemporalMigrationReceiptRequestV1,
    SessionTemporalMigrationReceiptV1, SessionTemporalMigrationStore,
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
pub use summary::{
    MAX_SESSION_SUMMARY_SOURCE_ANCHORS, SessionSummaryPublicationDispositionV1,
    SessionSummaryPublicationReceiptV1, SessionSummaryPublicationRequestV1, SessionSummaryStore,
};

use std::future::Future;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

use serde_json::json;
use tracedecay_domain::{
    CanonicalObservationIdV1, CopyProofV1, LogicalCopyRecordV1, MessageOccurrenceIdV1,
    MessageOccurrenceRecordV1, ObservationId, ProjectionOutputOrdinalV1, RetrievalAnchorId,
    RetrievalGrainV1, SessionContractError, SessionId, SessionProjectionGenerationV1,
    SessionRefreshOperationIdV1, SessionSummaryIdV1, SessionSummaryRecordV1,
    SummarySourceHorizonV1, TemporalAssertionRecordV1, TemporalCoverageCountsV1, TemporalModeV1,
    TemporalValidityV1, UtcMicros,
};
use tracedecay_store::{
    MAX_SESSION_SUMMARY_SOURCE_ANCHORS, MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS,
    MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE, SessionFrozenWatermarksV1,
    SessionGenerationActivatePermit, SessionGenerationActivationReceiptV1,
    SessionGenerationActivationRequestV1, SessionGenerationRebuildBeginPermit,
    SessionGenerationRebuildDispositionV1, SessionGenerationRebuildReceiptV1,
    SessionGenerationRebuildRequestV1, SessionProjectionBatchPersistPermit,
    SessionRefreshBeginOrJoinPermit, SessionRefreshBeginOrJoinReceiptV1,
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCancelPermit,
    SessionRefreshCancellationRequestV1, SessionRefreshCompletePermit,
    SessionRefreshCompletionRequestV1, SessionRefreshDispositionV1, SessionRefreshFailPermit,
    SessionRefreshFailureCodeInvalidReasonV1, SessionRefreshFailureCodeV1,
    SessionRefreshFailureRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressPersistPermit,
    SessionRefreshProgressReadPermit, SessionRefreshProgressRequestV1, SessionRefreshProgressV1,
    SessionRefreshReceiptReadPermit, SessionRefreshReceiptRequestV1, SessionRefreshReceiptV1,
    SessionRefreshStateV1, SessionRefreshStore, SessionRefreshTerminalStateV1,
    SessionRetrievalPageV1, SessionRetrievalStore, SessionSnapshotFreezePermit, SessionStoreError,
    SessionStoreResult, SessionSummaryPublicationRequestV1, SessionTemporalCapabilitiesV1,
    SessionTemporalCapabilityProvider, SessionTemporalCapabilityV1,
    SessionTemporalDigestInvalidReasonV1, SessionTemporalDigestV1,
    SessionTemporalPageRetrievePermit, SessionTemporalProjectionBatchDispositionV1,
    SessionTemporalProjectionBatchReceiptV1, SessionTemporalProjectionBatchV1,
    SessionTemporalProjectionStore, SessionTemporalRetrievalRequestV1,
    SessionTemporalSnapshotRequestV1, SessionTemporalSnapshotV1,
};

#[path = "session_contract/capabilities.rs"]
mod capabilities;
#[path = "session_contract/common.rs"]
mod common;
#[path = "session_contract/projection.rs"]
mod projection;
#[path = "session_contract/refresh.rs"]
mod refresh;
#[path = "session_contract/retrieval.rs"]
mod retrieval;
#[path = "session_contract/summary.rs"]
mod summary;

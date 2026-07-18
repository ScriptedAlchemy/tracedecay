use std::future::Future;

use tracedecay_domain::{SessionSummaryIdV1, SessionSummaryRecordV1, UtcMicros};

use super::common::{
    SessionFrozenWatermarksV1, SessionStoreError, SessionStoreResult,
    SessionSummaryPublishOrReplayPermit, SessionTemporalCapabilityProvider,
    SessionTemporalCapabilityV1, SessionTemporalSnapshotV1, require_capability,
    require_snapshot_session,
};

/// Maximum source anchors accepted in one immutable summary publication.
pub const MAX_SESSION_SUMMARY_SOURCE_ANCHORS: usize = 1_000;

/// Immutable publication request carrying the exact frozen source snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummaryPublicationRequestV1 {
    summary: SessionSummaryRecordV1,
    snapshot: SessionTemporalSnapshotV1,
}

impl SessionSummaryPublicationRequestV1 {
    pub fn new(
        summary: SessionSummaryRecordV1,
        snapshot: SessionTemporalSnapshotV1,
    ) -> SessionStoreResult<Self> {
        require_snapshot_session(summary.session_id(), &snapshot, "summary publication")?;
        require_capability(
            &snapshot,
            SessionTemporalCapabilityV1::ImmutableSummaryPublication,
        )?;
        if summary.source_anchors().len() > MAX_SESSION_SUMMARY_SOURCE_ANCHORS {
            return Err(SessionStoreError::BatchLimitExceeded {
                field: "session summary source anchors",
                count: summary.source_anchors().len(),
                max: MAX_SESSION_SUMMARY_SOURCE_ANCHORS,
            });
        }
        Ok(Self { summary, snapshot })
    }

    pub fn summary(&self) -> &SessionSummaryRecordV1 {
        &self.summary
    }

    pub fn snapshot(&self) -> &SessionTemporalSnapshotV1 {
        &self.snapshot
    }

    pub fn watermarks(&self) -> &SessionFrozenWatermarksV1 {
        self.snapshot.watermarks()
    }

    pub fn replay_disposition(
        &self,
        existing: &SessionSummaryRecordV1,
    ) -> SessionStoreResult<SessionSummaryPublicationDispositionV1> {
        if self.summary.summary_id() != existing.summary_id() {
            return Ok(SessionSummaryPublicationDispositionV1::Published);
        }
        if &self.summary == existing {
            return Ok(SessionSummaryPublicationDispositionV1::ExactReplay);
        }
        Err(SessionStoreError::ImmutableSummaryConflict {
            summary_id: self.summary.summary_id().clone(),
        })
    }
}

/// Outcome for an immutable summary publication request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionSummaryPublicationDispositionV1 {
    Published,
    ExactReplay,
}

/// Receipt for immutable session-summary publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummaryPublicationReceiptV1 {
    summary_id: SessionSummaryIdV1,
    watermarks: SessionFrozenWatermarksV1,
    disposition: SessionSummaryPublicationDispositionV1,
    published_at: UtcMicros,
}

impl SessionSummaryPublicationReceiptV1 {
    pub fn published(
        request: &SessionSummaryPublicationRequestV1,
        published_at: UtcMicros,
    ) -> Self {
        Self::build(
            request,
            SessionSummaryPublicationDispositionV1::Published,
            published_at,
        )
    }

    pub fn exact_replay(
        request: &SessionSummaryPublicationRequestV1,
        existing: &SessionSummaryRecordV1,
        published_at: UtcMicros,
    ) -> SessionStoreResult<Self> {
        if request.replay_disposition(existing)?
            != SessionSummaryPublicationDispositionV1::ExactReplay
        {
            return Err(SessionStoreError::ReceiptIdentityMismatch {
                context: "summary exact replay",
            });
        }
        Ok(Self::build(
            request,
            SessionSummaryPublicationDispositionV1::ExactReplay,
            published_at,
        ))
    }

    fn build(
        request: &SessionSummaryPublicationRequestV1,
        disposition: SessionSummaryPublicationDispositionV1,
        published_at: UtcMicros,
    ) -> Self {
        Self {
            summary_id: request.summary().summary_id().clone(),
            watermarks: request.watermarks().clone(),
            disposition,
            published_at,
        }
    }

    pub fn summary_id(&self) -> &SessionSummaryIdV1 {
        &self.summary_id
    }

    pub fn watermarks(&self) -> &SessionFrozenWatermarksV1 {
        &self.watermarks
    }

    pub const fn disposition(&self) -> SessionSummaryPublicationDispositionV1 {
        self.disposition
    }

    pub const fn published_at(&self) -> UtcMicros {
        self.published_at
    }
}

/// Immutable summary publication. Exact replay never mutates existing content.
///
/// The adapter's capability declaration is authoritative; snapshot
/// capabilities are descriptive and can only further restrict a request.
pub trait SessionSummaryStore: SessionTemporalCapabilityProvider + Send + Sync {
    fn publish_immutable_session_summary(
        &self,
        request: SessionSummaryPublicationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionSummaryPublicationReceiptV1>> + Send {
        async move {
            let permit =
                SessionSummaryPublishOrReplayPermit::grant(self.session_temporal_capabilities())?;
            self.publish_immutable_session_summary_supported(permit, request)
                .await
        }
    }

    fn publish_immutable_session_summary_supported(
        &self,
        permit: SessionSummaryPublishOrReplayPermit,
        request: SessionSummaryPublicationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionSummaryPublicationReceiptV1>> + Send;
}

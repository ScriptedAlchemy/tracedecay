use tracedecay_domain::SessionSummaryRecordV1;

use super::common::{
    SessionFrozenWatermarksV1, SessionStoreError, SessionStoreResult, SessionTemporalCapabilityV1,
    SessionTemporalSnapshotV1, require_capability, require_snapshot_session,
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
}

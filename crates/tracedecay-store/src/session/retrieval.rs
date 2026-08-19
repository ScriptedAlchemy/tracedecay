use std::future::Future;

use tracedecay_domain::{
    LogicalCopyRecordV1, MessageOccurrenceIdV1, MessageOccurrenceRecordV1, RetrievalGrainV1,
    SessionId, SessionSummaryRecordV1, TemporalAssertionRecordV1, TemporalCoverageCountsV1,
    TemporalModeV1,
};
use tracedecay_temporal_query::ports::ExecutionControl;

use super::common::{
    SessionSnapshotFreezePermit, SessionStoreError, SessionStoreResult,
    SessionTemporalCapabilityProvider, SessionTemporalCapabilityV1,
    SessionTemporalPageRetrievePermit, SessionTemporalSnapshotRequestV1, SessionTemporalSnapshotV1,
    require_capability, require_snapshot_session,
};

/// Maximum primary and nested records returned by one temporal retrieval page.
pub const MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE: usize = 100;

/// Bounded request for records from an immutable temporal snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTemporalRetrievalRequestV1 {
    session_id: SessionId,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    snapshot: SessionTemporalSnapshotV1,
    page_size: usize,
    after_occurrence_id: Option<MessageOccurrenceIdV1>,
    execution_control: ExecutionControl,
}

impl SessionTemporalRetrievalRequestV1 {
    pub fn new(
        session_id: SessionId,
        temporal_mode: TemporalModeV1,
        grain: RetrievalGrainV1,
        snapshot: SessionTemporalSnapshotV1,
        page_size: usize,
        after_occurrence_id: Option<MessageOccurrenceIdV1>,
        execution_control: ExecutionControl,
    ) -> SessionStoreResult<Self> {
        require_snapshot_session(&session_id, &snapshot, "temporal retrieval request")?;
        require_capability(&snapshot, SessionTemporalCapabilityV1::FrozenWatermarks)?;
        if !(1..=MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE).contains(&page_size) {
            return Err(SessionStoreError::InvalidPageLimit {
                limit: page_size,
                max: MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE,
            });
        }
        if after_occurrence_id.is_some() && snapshot.watermarks().cursor_key().is_none() {
            return Err(SessionStoreError::CursorKeyRequired);
        }
        Ok(Self {
            session_id,
            temporal_mode,
            grain,
            snapshot,
            page_size,
            after_occurrence_id,
            execution_control,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn temporal_mode(&self) -> TemporalModeV1 {
        self.temporal_mode
    }

    pub const fn grain(&self) -> RetrievalGrainV1 {
        self.grain
    }

    pub fn snapshot(&self) -> &SessionTemporalSnapshotV1 {
        &self.snapshot
    }

    pub const fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn after_occurrence_id(&self) -> Option<&MessageOccurrenceIdV1> {
        self.after_occurrence_id.as_ref()
    }

    pub fn execution_control(&self) -> &ExecutionControl {
        &self.execution_control
    }
}

/// Bounded temporal records plus explicit coverage for one retrieval page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRetrievalPageV1 {
    snapshot: SessionTemporalSnapshotV1,
    occurrences: Vec<MessageOccurrenceRecordV1>,
    copies: Vec<LogicalCopyRecordV1>,
    assertions: Vec<TemporalAssertionRecordV1>,
    summaries: Vec<SessionSummaryRecordV1>,
    coverage: TemporalCoverageCountsV1,
    next_after_occurrence_id: Option<MessageOccurrenceIdV1>,
}

impl SessionRetrievalPageV1 {
    pub fn new(
        snapshot: SessionTemporalSnapshotV1,
        occurrences: Vec<MessageOccurrenceRecordV1>,
        copies: Vec<LogicalCopyRecordV1>,
        assertions: Vec<TemporalAssertionRecordV1>,
        summaries: Vec<SessionSummaryRecordV1>,
        coverage: TemporalCoverageCountsV1,
        next_after_occurrence_id: Option<MessageOccurrenceIdV1>,
    ) -> SessionStoreResult<Self> {
        let record_count = deep_record_count(&occurrences, &copies, &assertions, &summaries);
        if record_count > MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE {
            return Err(SessionStoreError::BatchLimitExceeded {
                field: "session temporal retrieval page",
                count: record_count,
                max: MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE,
            });
        }
        if next_after_occurrence_id.is_some() && snapshot.watermarks().cursor_key().is_none() {
            return Err(SessionStoreError::CursorKeyRequired);
        }

        for occurrence in &occurrences {
            occurrence.validate()?;
            if &occurrence.session_id != snapshot.session_id() {
                return Err(SessionStoreError::SessionMismatch {
                    context: "retrieval occurrence",
                });
            }
        }
        for summary in &summaries {
            if summary.session_id() != snapshot.session_id() {
                return Err(SessionStoreError::SessionMismatch {
                    context: "retrieval summary",
                });
            }
        }

        for copy in &copies {
            copy.validate()?;
        }
        for assertion in &assertions {
            assertion.validate()?;
        }

        Ok(Self {
            snapshot,
            occurrences,
            copies,
            assertions,
            summaries,
            coverage,
            next_after_occurrence_id,
        })
    }

    pub fn snapshot(&self) -> &SessionTemporalSnapshotV1 {
        &self.snapshot
    }

    pub fn occurrences(&self) -> &[MessageOccurrenceRecordV1] {
        &self.occurrences
    }

    pub fn copies(&self) -> &[LogicalCopyRecordV1] {
        &self.copies
    }

    pub fn assertions(&self) -> &[TemporalAssertionRecordV1] {
        &self.assertions
    }

    pub fn summaries(&self) -> &[SessionSummaryRecordV1] {
        &self.summaries
    }

    pub fn coverage(&self) -> &TemporalCoverageCountsV1 {
        &self.coverage
    }

    pub fn next_after_occurrence_id(&self) -> Option<&MessageOccurrenceIdV1> {
        self.next_after_occurrence_id.as_ref()
    }

    pub fn record_count(&self) -> usize {
        deep_record_count(
            &self.occurrences,
            &self.copies,
            &self.assertions,
            &self.summaries,
        )
    }
}

fn deep_record_count(
    occurrences: &[MessageOccurrenceRecordV1],
    copies: &[LogicalCopyRecordV1],
    assertions: &[TemporalAssertionRecordV1],
    summaries: &[SessionSummaryRecordV1],
) -> usize {
    summaries.iter().fold(
        occurrences
            .len()
            .saturating_add(copies.len())
            .saturating_add(assertions.len())
            .saturating_add(summaries.len()),
        |count, summary| count.saturating_add(summary.source_anchors().len()),
    )
}

/// Frozen, side-effect-free temporal reads.
///
/// `Send + Sync` is required because daemon adapters are shared across
/// concurrent request tasks. Snapshot capabilities describe what was frozen;
/// only the adapter capability provider authorizes dispatch.
pub trait SessionRetrievalStore: SessionTemporalCapabilityProvider + Send + Sync {
    fn freeze_session_temporal_snapshot(
        &self,
        request: SessionTemporalSnapshotRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalSnapshotV1>> + Send {
        async move {
            let permit = SessionSnapshotFreezePermit::grant(self.session_temporal_capabilities())?;
            self.freeze_session_temporal_snapshot_supported(permit, request)
                .await
        }
    }

    fn freeze_session_temporal_snapshot_supported(
        &self,
        permit: SessionSnapshotFreezePermit,
        request: SessionTemporalSnapshotRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalSnapshotV1>> + Send;

    fn retrieve_session_temporal_page(
        &self,
        request: SessionTemporalRetrievalRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRetrievalPageV1>> + Send {
        async move {
            let permit =
                SessionTemporalPageRetrievePermit::grant(self.session_temporal_capabilities())?;
            self.retrieve_session_temporal_page_supported(permit, request)
                .await
        }
    }

    fn retrieve_session_temporal_page_supported(
        &self,
        permit: SessionTemporalPageRetrievePermit,
        request: SessionTemporalRetrievalRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRetrievalPageV1>> + Send;
}

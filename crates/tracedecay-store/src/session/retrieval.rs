use std::future::Future;

use tracedecay_domain::{
    DerivedEvidenceIdV1, DerivedEvidenceKindV1, DerivedEvidenceMemberRoleV1, HydrationStateV1,
    LogicalCopyRecordV1, MessageOccurrenceIdV1, MessageOccurrenceRecordV1, RetrievalGrainV1,
    SessionId, SessionSummaryRecordV1, TemporalAssertionRecordV1, TemporalCoverageCountsV1,
    TemporalModeV1,
};

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
}

impl SessionTemporalRetrievalRequestV1 {
    pub fn new(
        session_id: SessionId,
        temporal_mode: TemporalModeV1,
        grain: RetrievalGrainV1,
        snapshot: SessionTemporalSnapshotV1,
        page_size: usize,
        after_occurrence_id: Option<MessageOccurrenceIdV1>,
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

/// One paged member of a generation-bound derived evidence record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedEvidenceMemberPageItemV1 {
    pub ordinal: u32,
    pub occurrence_id: Option<MessageOccurrenceIdV1>,
    pub member_role: DerivedEvidenceMemberRoleV1,
    pub availability: HydrationStateV1,
}

/// Bounded lossless expansion of derived evidence members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedEvidenceMemberPageV1 {
    evidence_id: DerivedEvidenceIdV1,
    evidence_kind: DerivedEvidenceKindV1,
    members: Vec<DerivedEvidenceMemberPageItemV1>,
    next_after_ordinal: Option<u32>,
}

impl DerivedEvidenceMemberPageV1 {
    pub fn new(
        evidence_id: DerivedEvidenceIdV1,
        evidence_kind: DerivedEvidenceKindV1,
        members: Vec<DerivedEvidenceMemberPageItemV1>,
        next_after_ordinal: Option<u32>,
    ) -> SessionStoreResult<Self> {
        if members.len() > MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE {
            return Err(SessionStoreError::BatchLimitExceeded {
                field: "derived evidence member page",
                count: members.len(),
                max: MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE,
            });
        }
        if members
            .windows(2)
            .any(|pair| pair[0].ordinal >= pair[1].ordinal)
        {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "derived evidence member ordinal order",
            });
        }
        if members.iter().any(|member| {
            (member.availability == HydrationStateV1::Available) != member.occurrence_id.is_some()
        }) {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "derived evidence member availability",
            });
        }
        if next_after_ordinal.is_some()
            && next_after_ordinal != members.last().map(|member| member.ordinal)
        {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "derived evidence member continuation",
            });
        }
        Ok(Self {
            evidence_id,
            evidence_kind,
            members,
            next_after_ordinal,
        })
    }

    pub fn evidence_id(&self) -> &DerivedEvidenceIdV1 {
        &self.evidence_id
    }

    pub const fn evidence_kind(&self) -> DerivedEvidenceKindV1 {
        self.evidence_kind
    }

    pub fn members(&self) -> &[DerivedEvidenceMemberPageItemV1] {
        &self.members
    }

    pub const fn next_after_ordinal(&self) -> Option<u32> {
        self.next_after_ordinal
    }
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

    fn expand_derived_members(
        &self,
        snapshot: SessionTemporalSnapshotV1,
        evidence_kind: DerivedEvidenceKindV1,
        evidence_id: DerivedEvidenceIdV1,
        after_ordinal: Option<u32>,
        limit: usize,
    ) -> impl Future<Output = SessionStoreResult<DerivedEvidenceMemberPageV1>> + Send {
        async move {
            let permit =
                SessionTemporalPageRetrievePermit::grant(self.session_temporal_capabilities())?;
            self.expand_derived_members_supported(
                permit,
                snapshot,
                evidence_kind,
                evidence_id,
                after_ordinal,
                limit,
            )
            .await
        }
    }

    fn expand_derived_members_supported(
        &self,
        permit: SessionTemporalPageRetrievePermit,
        snapshot: SessionTemporalSnapshotV1,
        evidence_kind: DerivedEvidenceKindV1,
        evidence_id: DerivedEvidenceIdV1,
        after_ordinal: Option<u32>,
        limit: usize,
    ) -> impl Future<Output = SessionStoreResult<DerivedEvidenceMemberPageV1>> + Send;
}

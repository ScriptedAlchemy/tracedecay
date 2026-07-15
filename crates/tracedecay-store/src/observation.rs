use std::error::Error;
use std::future::Future;

use tracedecay_domain::{
    CanonicalObservationIdV1, DurableObservationV1, ObservationCollisionOutcomeV1,
    ObservationContractError, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadDigestV1, SanitizationReceiptV1, SanitizerDispositionV1,
};

const MAX_REPLAY_LIMIT: usize = 1_000;

fn cursor_transition_covers(
    expected: Option<&ObservationSourceCursorV1>,
    next: &ObservationSourceCursorV1,
    covered: ObservationSourceRangeV1,
) -> bool {
    if next.position() != covered.end() {
        return false;
    }
    if next.ordering_domain() == ObservationOrderingDomainV1::FileBytes
        && expected.is_none_or(|cursor| cursor.generation() != next.generation())
        && covered.start() != 0
    {
        return false;
    }
    let Some(expected) = expected else {
        return true;
    };
    if expected.source() != next.source() || expected.scope() != next.scope() {
        return false;
    }
    expected.generation() != next.generation()
        || (expected.ordering_domain() == next.ordering_domain()
            && expected.position() == covered.start())
}

/// Validated request to persist one sanitized observation and advance its source cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationWrite {
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    next_cursor: ObservationSourceCursorV1,
}

impl ObservationWrite {
    pub fn new(
        observation: DurableObservationV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        next_cursor: ObservationSourceCursorV1,
    ) -> ObservationStoreResult<Self> {
        if observation.source() != next_cursor.source()
            || observation.scope() != next_cursor.scope()
            || observation.identity().generation() != next_cursor.generation()
            || observation.identity().ordering_domain() != next_cursor.ordering_domain()
            || observation.identity().position().end() != next_cursor.position()
            || !cursor_transition_covers(
                expected_cursor.as_ref(),
                &next_cursor,
                observation.identity().position(),
            )
        {
            return Err(ObservationStoreError::CursorObservationMismatch);
        }
        Ok(Self {
            observation,
            expected_cursor,
            next_cursor,
        })
    }

    pub fn observation(&self) -> &DurableObservationV1 {
        &self.observation
    }

    pub fn expected_cursor(&self) -> Option<&ObservationSourceCursorV1> {
        self.expected_cursor.as_ref()
    }

    pub fn next_cursor(&self) -> &ObservationSourceCursorV1 {
        &self.next_cursor
    }

    pub fn into_parts(
        self,
    ) -> (
        DurableObservationV1,
        Option<ObservationSourceCursorV1>,
        ObservationSourceCursorV1,
    ) {
        (self.observation, self.expected_cursor, self.next_cursor)
    }
}

/// Fully processed provider evidence that intentionally produces no durable observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ObservationCoverageReason {
    BlankFrame,
    OutOfScope,
    MalformedFrame,
    OversizedFrame,
    UnknownVersion,
    UnsupportedFact,
    DuplicateObservation,
    SanitizerRejected,
    SanitizerQuarantined,
}

impl ObservationCoverageReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BlankFrame => "blank_frame",
            Self::OutOfScope => "out_of_scope",
            Self::MalformedFrame => "malformed_frame",
            Self::OversizedFrame => "oversized_frame",
            Self::UnknownVersion => "unknown_version",
            Self::UnsupportedFact => "unsupported_fact",
            Self::DuplicateObservation => "duplicate_observation",
            Self::SanitizerRejected => "sanitizer_rejected",
            Self::SanitizerQuarantined => "sanitizer_quarantined",
        }
    }
}

pub type NonDurableFrameReason = ObservationCoverageReason;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObservationCoverageV1 {
    generation: ObservationSourceGenerationV1,
    ordering_domain: ObservationOrderingDomainV1,
    range: ObservationSourceRangeV1,
}

impl ObservationCoverageV1 {
    pub fn new(
        generation: ObservationSourceGenerationV1,
        ordering_domain: ObservationOrderingDomainV1,
        range: ObservationSourceRangeV1,
    ) -> Self {
        Self {
            generation,
            ordering_domain,
            range,
        }
    }

    pub fn generation(self) -> ObservationSourceGenerationV1 {
        self.generation
    }

    pub fn ordering_domain(self) -> ObservationOrderingDomainV1 {
        self.ordering_domain
    }

    pub fn range(self) -> ObservationSourceRangeV1 {
        self.range
    }
}

/// Validated exact-CAS cursor advance over fully processed non-durable evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationCursorAdvance {
    expected_cursor: Option<ObservationSourceCursorV1>,
    next_cursor: ObservationSourceCursorV1,
    covered: ObservationSourceRangeV1,
    reason: ObservationCoverageReason,
    sanitization_receipt: Option<SanitizationReceiptV1>,
}

impl ObservationCursorAdvance {
    pub fn new(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        covered: ObservationSourceRangeV1,
        reason: ObservationCoverageReason,
    ) -> ObservationStoreResult<Self> {
        Self::build(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::FileBytes,
            expected_cursor,
            covered,
            reason,
            None,
        )
    }

    pub fn for_ordering(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        ordering_domain: ObservationOrderingDomainV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        covered: ObservationSourceRangeV1,
        reason: ObservationCoverageReason,
    ) -> ObservationStoreResult<Self> {
        Self::build(
            source,
            scope,
            generation,
            ordering_domain,
            expected_cursor,
            covered,
            reason,
            None,
        )
    }

    pub fn new_with_sanitization_receipt(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        covered: ObservationSourceRangeV1,
        reason: ObservationCoverageReason,
        sanitization_receipt: SanitizationReceiptV1,
    ) -> ObservationStoreResult<Self> {
        Self::build(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::FileBytes,
            expected_cursor,
            covered,
            reason,
            Some(sanitization_receipt),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn for_ordering_with_sanitization_receipt(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        ordering_domain: ObservationOrderingDomainV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        covered: ObservationSourceRangeV1,
        reason: ObservationCoverageReason,
        sanitization_receipt: SanitizationReceiptV1,
    ) -> ObservationStoreResult<Self> {
        Self::build(
            source,
            scope,
            generation,
            ordering_domain,
            expected_cursor,
            covered,
            reason,
            Some(sanitization_receipt),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        ordering_domain: ObservationOrderingDomainV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        covered: ObservationSourceRangeV1,
        reason: ObservationCoverageReason,
        sanitization_receipt: Option<SanitizationReceiptV1>,
    ) -> ObservationStoreResult<Self> {
        let receipt_matches_reason = matches!(
            (
                reason,
                sanitization_receipt
                    .as_ref()
                    .map(SanitizationReceiptV1::disposition)
            ),
            (
                ObservationCoverageReason::SanitizerRejected,
                Some(SanitizerDispositionV1::Rejected)
            ) | (
                ObservationCoverageReason::SanitizerQuarantined,
                Some(SanitizerDispositionV1::Quarantined)
            ) | (
                ObservationCoverageReason::DuplicateObservation,
                Some(SanitizerDispositionV1::Accepted | SanitizerDispositionV1::Redacted)
            ) | (
                ObservationCoverageReason::BlankFrame
                    | ObservationCoverageReason::OutOfScope
                    | ObservationCoverageReason::MalformedFrame
                    | ObservationCoverageReason::OversizedFrame
                    | ObservationCoverageReason::UnknownVersion
                    | ObservationCoverageReason::UnsupportedFact,
                None
            )
        );
        if !receipt_matches_reason {
            return Err(ObservationStoreError::CursorSanitizationReceiptMismatch);
        }
        let next_cursor = ObservationSourceCursorV1::for_ordering(
            source,
            scope,
            generation,
            ordering_domain,
            covered.end(),
        )
        .map_err(ObservationStoreError::Contract)?;
        if !cursor_transition_covers(expected_cursor.as_ref(), &next_cursor, covered) {
            return Err(ObservationStoreError::CursorCoverageMismatch);
        }
        Ok(Self {
            expected_cursor,
            next_cursor,
            covered,
            reason,
            sanitization_receipt,
        })
    }

    pub fn expected_cursor(&self) -> Option<&ObservationSourceCursorV1> {
        self.expected_cursor.as_ref()
    }

    pub fn next_cursor(&self) -> &ObservationSourceCursorV1 {
        &self.next_cursor
    }

    pub fn covered(&self) -> ObservationSourceRangeV1 {
        self.covered
    }

    pub fn coverage(&self) -> ObservationCoverageV1 {
        ObservationCoverageV1::new(
            self.next_cursor.generation(),
            self.next_cursor.ordering_domain(),
            self.covered,
        )
    }

    pub fn reason(&self) -> ObservationCoverageReason {
        self.reason
    }

    pub fn sanitization_receipt(&self) -> Option<&SanitizationReceiptV1> {
        self.sanitization_receipt.as_ref()
    }

    #[must_use]
    pub fn with_resume_checkpoint(mut self, file_identity: u64, resume_fingerprint: u64) -> Self {
        self.next_cursor = self
            .next_cursor
            .with_resume_checkpoint(file_identity, resume_fingerprint);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorAdvanceOutcome {
    Committed,
    ExactDuplicate,
}

/// Stable receipt for committed observation evidence and its authoritative cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationCommitReceipt {
    sequence: u64,
    observation: Box<DurableObservationV1>,
    committed_cursor: ObservationSourceCursorV1,
}

impl ObservationCommitReceipt {
    pub fn new(
        sequence: u64,
        observation: DurableObservationV1,
        committed_cursor: ObservationSourceCursorV1,
    ) -> Self {
        Self {
            sequence,
            observation: Box::new(observation),
            committed_cursor,
        }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn observation(&self) -> &DurableObservationV1 {
        self.observation.as_ref()
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        self.observation.receipt()
    }

    pub fn committed_cursor(&self) -> &ObservationSourceCursorV1 {
        &self.committed_cursor
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationPersistOutcome {
    Committed(ObservationCommitReceipt),
    ExactDuplicate(ObservationCommitReceipt),
    CoveredDuplicate(ObservationCommitReceipt),
}

impl ObservationPersistOutcome {
    pub fn receipt(&self) -> &ObservationCommitReceipt {
        match self {
            Self::Committed(receipt)
            | Self::ExactDuplicate(receipt)
            | Self::CoveredDuplicate(receipt) => receipt,
        }
    }
}

/// One immutable observation in authoritative ingestion order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredObservation {
    commit_receipt: ObservationCommitReceipt,
    projection_status: ObservationProjectionStatus,
}

impl StoredObservation {
    pub fn new(
        sequence: u64,
        observation: DurableObservationV1,
        committed_cursor: ObservationSourceCursorV1,
        projection_status: ObservationProjectionStatus,
    ) -> Self {
        Self::from_commit_receipt(
            ObservationCommitReceipt::new(sequence, observation, committed_cursor),
            projection_status,
        )
    }

    pub fn from_commit_receipt(
        commit_receipt: ObservationCommitReceipt,
        projection_status: ObservationProjectionStatus,
    ) -> Self {
        Self {
            commit_receipt,
            projection_status,
        }
    }

    pub fn commit_receipt(&self) -> &ObservationCommitReceipt {
        &self.commit_receipt
    }

    pub fn sequence(&self) -> u64 {
        self.commit_receipt.sequence()
    }

    pub fn observation(&self) -> &DurableObservationV1 {
        self.commit_receipt.observation()
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        self.commit_receipt.sanitization_receipt()
    }

    pub fn committed_cursor(&self) -> &ObservationSourceCursorV1 {
        self.commit_receipt.committed_cursor()
    }

    pub fn projection_status(&self) -> ObservationProjectionStatus {
        self.projection_status
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationProjectionStatus {
    Queued,
    NotQueued,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObservationReplayRequest {
    after_sequence: u64,
    limit: usize,
}

impl ObservationReplayRequest {
    pub fn new(after_sequence: u64, limit: usize) -> ObservationStoreResult<Self> {
        if limit == 0 || limit > MAX_REPLAY_LIMIT {
            return Err(ObservationStoreError::InvalidReplayLimit {
                limit,
                max: MAX_REPLAY_LIMIT,
            });
        }
        Ok(Self {
            after_sequence,
            limit,
        })
    }

    pub fn after_sequence(self) -> u64 {
        self.after_sequence
    }

    pub fn limit(self) -> usize {
        self.limit
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ObservationStoreError {
    #[error("observation cursor does not match its source evidence")]
    CursorObservationMismatch,
    #[error("covered source evidence is not contiguous with the expected cursor")]
    CursorCoverageMismatch,
    #[error("source cursor conflict: expected {expected:?}, found {actual:?}")]
    CursorConflict {
        expected: Box<Option<ObservationSourceCursorV1>>,
        actual: Box<Option<ObservationSourceCursorV1>>,
    },
    #[error("source cursor advance receipt collided with different contents")]
    CursorAdvanceCollision,
    #[error("source cursor advance reason disagrees with its sanitization receipt")]
    CursorSanitizationReceiptMismatch,
    #[error(
        "observation {observation_id:?} collided: existing digest {existing_digest:?}, candidate digest {candidate_digest:?}"
    )]
    ObservationCollision {
        observation_id: Box<CanonicalObservationIdV1>,
        existing_digest: Box<PayloadDigestV1>,
        candidate_digest: Box<PayloadDigestV1>,
        outcome: ObservationCollisionOutcomeV1,
    },
    #[error("sanitization receipt identifier collided with different contents")]
    SanitizationReceiptCollision,
    #[error("replay limit {limit} must be between 1 and {max}")]
    InvalidReplayLimit { limit: usize, max: usize },
    #[error("observation contract validation failed")]
    Contract(#[source] ObservationContractError),
    #[error("observation storage operation {operation} failed")]
    Storage {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
}

pub type ObservationStoreResult<T> = Result<T, ObservationStoreError>;

/// Authoritative persistence boundary for sanitized observations.
pub trait ObservationStore: Send + Sync {
    fn persist_observation(
        &self,
        write: ObservationWrite,
    ) -> impl Future<Output = ObservationStoreResult<ObservationPersistOutcome>> + Send;

    fn get_source_cursor(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> impl Future<Output = ObservationStoreResult<Option<ObservationSourceCursorV1>>> + Send;

    fn advance_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
    ) -> impl Future<Output = ObservationStoreResult<CursorAdvanceOutcome>> + Send;

    fn get_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> impl Future<Output = ObservationStoreResult<Option<StoredObservation>>> + Send;

    fn replay_observations(
        &self,
        request: ObservationReplayRequest,
    ) -> impl Future<Output = ObservationStoreResult<Vec<StoredObservation>>> + Send;
}

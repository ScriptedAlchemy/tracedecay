use std::error::Error;
use std::future::Future;

use tracedecay_domain::{
    CanonicalObservationIdV1, ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeSourceCursorV1,
    ClaudeSourceIdentityV1, DurableClaudeObservationV1, ObservationCollisionOutcomeV1,
    ObservationContractError, ObservationScopeV1, PayloadDigestV1, SanitizationReceiptV1,
};

const MAX_REPLAY_LIMIT: usize = 1_000;

/// Validated request to persist one sanitized observation and advance its source cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationWrite {
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
    next_cursor: ClaudeSourceCursorV1,
}

impl ObservationWrite {
    pub fn new(
        observation: DurableClaudeObservationV1,
        expected_cursor: Option<ClaudeSourceCursorV1>,
        next_cursor: ClaudeSourceCursorV1,
    ) -> ObservationStoreResult<Self> {
        if observation.source() != next_cursor.source()
            || observation.scope() != next_cursor.scope()
            || observation.identity().generation() != next_cursor.generation()
            || observation.identity().position().end() != next_cursor.byte_offset()
        {
            return Err(ObservationStoreError::CursorObservationMismatch);
        }
        if let Some(expected) = &expected_cursor {
            if expected.source() != next_cursor.source() || expected.scope() != next_cursor.scope()
            {
                return Err(ObservationStoreError::CursorObservationMismatch);
            }
            if expected.generation() == next_cursor.generation()
                && expected.byte_offset() > observation.identity().position().start()
            {
                return Err(ObservationStoreError::CursorObservationMismatch);
            }
        }
        Ok(Self {
            observation,
            expected_cursor,
            next_cursor,
        })
    }

    pub fn observation(&self) -> &DurableClaudeObservationV1 {
        &self.observation
    }

    pub fn expected_cursor(&self) -> Option<&ClaudeSourceCursorV1> {
        self.expected_cursor.as_ref()
    }

    pub fn next_cursor(&self) -> &ClaudeSourceCursorV1 {
        &self.next_cursor
    }

    pub fn into_parts(
        self,
    ) -> (
        DurableClaudeObservationV1,
        Option<ClaudeSourceCursorV1>,
        ClaudeSourceCursorV1,
    ) {
        (self.observation, self.expected_cursor, self.next_cursor)
    }
}

/// Fully processed source bytes that intentionally produce no durable observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NonDurableFrameReason {
    BlankFrame,
    OutOfScope,
    SanitizerRejected,
    SanitizerQuarantined,
}

/// Validated exact-CAS cursor advance over fully processed non-durable bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationCursorAdvance {
    expected_cursor: Option<ClaudeSourceCursorV1>,
    next_cursor: ClaudeSourceCursorV1,
    covered: ClaudeByteRangeV1,
    reason: NonDurableFrameReason,
}

impl ObservationCursorAdvance {
    pub fn new(
        source: ClaudeSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ClaudeFileGenerationV1,
        expected_cursor: Option<ClaudeSourceCursorV1>,
        covered: ClaudeByteRangeV1,
        reason: NonDurableFrameReason,
    ) -> ObservationStoreResult<Self> {
        let next_cursor = ClaudeSourceCursorV1::new(source, scope, generation, covered.end())
            .map_err(ObservationStoreError::Contract)?;
        let coverage_starts_at_expected = expected_cursor.as_ref().map_or_else(
            || covered.start() == 0,
            |expected| {
                if expected.generation() == next_cursor.generation() {
                    expected.byte_offset() == covered.start()
                } else {
                    covered.start() == 0
                }
            },
        );
        if !coverage_starts_at_expected
            || expected_cursor.as_ref().is_some_and(|expected| {
                expected.source() != next_cursor.source() || expected.scope() != next_cursor.scope()
            })
        {
            return Err(ObservationStoreError::CursorCoverageMismatch);
        }
        Ok(Self {
            expected_cursor,
            next_cursor,
            covered,
            reason,
        })
    }

    pub fn expected_cursor(&self) -> Option<&ClaudeSourceCursorV1> {
        self.expected_cursor.as_ref()
    }

    pub fn next_cursor(&self) -> &ClaudeSourceCursorV1 {
        &self.next_cursor
    }

    pub fn covered(&self) -> ClaudeByteRangeV1 {
        self.covered
    }

    pub fn reason(&self) -> NonDurableFrameReason {
        self.reason
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorAdvanceOutcome {
    Committed,
    ExactDuplicate,
}

/// Stable receipt for either a newly committed observation or an exact duplicate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationCommitReceipt {
    sequence: u64,
    observation: Box<DurableClaudeObservationV1>,
    committed_cursor: ClaudeSourceCursorV1,
}

impl ObservationCommitReceipt {
    pub fn new(
        sequence: u64,
        observation: DurableClaudeObservationV1,
        committed_cursor: ClaudeSourceCursorV1,
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

    pub fn observation(&self) -> &DurableClaudeObservationV1 {
        self.observation.as_ref()
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        self.observation.receipt()
    }

    pub fn committed_cursor(&self) -> &ClaudeSourceCursorV1 {
        &self.committed_cursor
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationPersistOutcome {
    Committed(ObservationCommitReceipt),
    ExactDuplicate(ObservationCommitReceipt),
}

impl ObservationPersistOutcome {
    pub fn receipt(&self) -> &ObservationCommitReceipt {
        match self {
            Self::Committed(receipt) | Self::ExactDuplicate(receipt) => receipt,
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
        observation: DurableClaudeObservationV1,
        committed_cursor: ClaudeSourceCursorV1,
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

    pub fn observation(&self) -> &DurableClaudeObservationV1 {
        self.commit_receipt.observation()
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        self.commit_receipt.sanitization_receipt()
    }

    pub fn committed_cursor(&self) -> &ClaudeSourceCursorV1 {
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
pub enum ObservationStoreError {
    #[error("observation cursor does not match its source evidence")]
    CursorObservationMismatch,
    #[error("covered source bytes are not contiguous with the expected cursor")]
    CursorCoverageMismatch,
    #[error("source cursor conflict: expected {expected:?}, found {actual:?}")]
    CursorConflict {
        expected: Box<Option<ClaudeSourceCursorV1>>,
        actual: Box<Option<ClaudeSourceCursorV1>>,
    },
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
        source: &ClaudeSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> impl Future<Output = ObservationStoreResult<Option<ClaudeSourceCursorV1>>> + Send;

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

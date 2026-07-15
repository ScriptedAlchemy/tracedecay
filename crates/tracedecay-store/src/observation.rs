use std::error::Error;
use std::future::Future;

use tracedecay_domain::{
    CanonicalObservationIdV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1,
    DurableClaudeObservationV1, ObservationCollisionOutcomeV1, ObservationContractError,
    ObservationScopeV1, PayloadDigestV1, SanitizationReceiptV1,
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
        if !observation
            .receipt()
            .disposition()
            .permits_durable_payload()
        {
            return Err(ObservationStoreError::Contract(
                ObservationContractError::ReceiptPayloadForbidden,
            ));
        }
        if observation.receipt().payload() != Some(observation.payload_reference()) {
            return Err(ObservationStoreError::Contract(
                ObservationContractError::ReceiptPayloadMismatch,
            ));
        }
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
                && expected.byte_offset() != observation.identity().position().start()
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

/// Stable receipt for either a newly committed observation or an exact duplicate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationCommitReceipt {
    sequence: u64,
    observation: DurableClaudeObservationV1,
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
            observation,
            committed_cursor,
        }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn observation(&self) -> &DurableClaudeObservationV1 {
        &self.observation
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
    sequence: u64,
    observation: DurableClaudeObservationV1,
    committed_cursor: ClaudeSourceCursorV1,
    projection_status: ObservationProjectionStatus,
}

impl StoredObservation {
    pub fn new(
        sequence: u64,
        observation: DurableClaudeObservationV1,
        committed_cursor: ClaudeSourceCursorV1,
        projection_status: ObservationProjectionStatus,
    ) -> Self {
        Self {
            sequence,
            observation,
            committed_cursor,
            projection_status,
        }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn observation(&self) -> &DurableClaudeObservationV1 {
        &self.observation
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        self.observation.receipt()
    }

    pub fn committed_cursor(&self) -> &ClaudeSourceCursorV1 {
        &self.committed_cursor
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
    #[error("source cursor conflict: expected {expected:?}, found {actual:?}")]
    CursorConflict {
        expected: Option<ClaudeSourceCursorV1>,
        actual: Option<ClaudeSourceCursorV1>,
    },
    #[error(
        "observation {observation_id:?} collided: existing digest {existing_digest:?}, candidate digest {candidate_digest:?}"
    )]
    ObservationCollision {
        observation_id: CanonicalObservationIdV1,
        existing_digest: PayloadDigestV1,
        candidate_digest: PayloadDigestV1,
        outcome: ObservationCollisionOutcomeV1,
    },
    #[error("idempotency key collided with another observation")]
    IdempotencyCollision,
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

    fn get_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> impl Future<Output = ObservationStoreResult<Option<StoredObservation>>> + Send;

    fn replay_observations(
        &self,
        request: ObservationReplayRequest,
    ) -> impl Future<Output = ObservationStoreResult<Vec<StoredObservation>>> + Send;
}

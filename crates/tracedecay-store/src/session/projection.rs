use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    LogicalCopyRecordV1, MessageOccurrenceRecordV1, SessionId, SessionProjectionGenerationV1,
    TemporalAssertionRecordV1, UtcMicros,
};

use super::common::{
    SessionFrozenWatermarksV1, SessionStoreError, SessionStoreResult, SessionTemporalDigestV1,
};

/// Maximum records accepted by one temporal projection batch.
pub const MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS: usize = 1_000;

/// One bounded candidate-generation write for a single session generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionTemporalProjectionBatchV1 {
    session_id: SessionId,
    generation: SessionProjectionGenerationV1,
    watermarks: SessionFrozenWatermarksV1,
    batch_ordinal: u64,
    source_through: u64,
    projection_through: u64,
    occurrences: Vec<MessageOccurrenceRecordV1>,
    copies: Vec<LogicalCopyRecordV1>,
    assertions: Vec<TemporalAssertionRecordV1>,
}

impl SessionTemporalProjectionBatchV1 {
    #[tracing::instrument(
        name = "store.session.build_projection_batch",
        level = "trace",
        skip_all
    )]
    pub fn new(
        session_id: SessionId,
        generation: SessionProjectionGenerationV1,
        watermarks: SessionFrozenWatermarksV1,
        occurrences: Vec<MessageOccurrenceRecordV1>,
        copies: Vec<LogicalCopyRecordV1>,
        assertions: Vec<TemporalAssertionRecordV1>,
    ) -> SessionStoreResult<Self> {
        let item_count = occurrences
            .len()
            .saturating_add(copies.len())
            .saturating_add(assertions.len());
        if item_count > MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS {
            return Err(SessionStoreError::BatchLimitExceeded {
                field: "session temporal projection batch",
                count: item_count,
                max: MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS,
            });
        }

        for occurrence in &occurrences {
            occurrence.validate()?;
            if occurrence.session_id != session_id {
                return Err(SessionStoreError::SessionMismatch {
                    context: "projection occurrence",
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
            session_id,
            generation,
            batch_ordinal: 0,
            source_through: watermarks.source_frontier(),
            projection_through: watermarks.projection_frontier(),
            watermarks,
            occurrences,
            copies,
            assertions,
        })
    }

    pub fn with_checkpoint(
        mut self,
        batch_ordinal: u64,
        source_through: u64,
        projection_through: u64,
    ) -> SessionStoreResult<Self> {
        if source_through > self.watermarks.source_frontier()
            || projection_through > self.watermarks.projection_frontier()
        {
            return Err(SessionStoreError::FrozenWatermarkMismatch);
        }
        self.batch_ordinal = batch_ordinal;
        self.source_through = source_through;
        self.projection_through = projection_through;
        Ok(self)
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn generation(&self) -> SessionProjectionGenerationV1 {
        self.generation
    }

    pub fn watermarks(&self) -> &SessionFrozenWatermarksV1 {
        &self.watermarks
    }

    pub const fn batch_ordinal(&self) -> u64 {
        self.batch_ordinal
    }

    pub const fn source_through(&self) -> u64 {
        self.source_through
    }

    pub const fn projection_through(&self) -> u64 {
        self.projection_through
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

    pub fn item_count(&self) -> usize {
        self.occurrences
            .len()
            .saturating_add(self.copies.len())
            .saturating_add(self.assertions.len())
    }

    pub fn replay_disposition(
        &self,
        batch_digest: &SessionTemporalDigestV1,
        existing: &SessionTemporalProjectionBatchReceiptV1,
    ) -> SessionStoreResult<SessionTemporalProjectionBatchDispositionV1> {
        if existing.session_id() != self.session_id()
            || existing.generation() != self.generation()
            || existing.batch_ordinal() != self.batch_ordinal()
        {
            return Err(SessionStoreError::ReceiptIdentityMismatch {
                context: "projection batch replay",
            });
        }
        if existing.batch_digest() != batch_digest
            || existing.watermarks() != self.watermarks()
            || existing.source_through() != self.source_through()
            || existing.projection_through() != self.projection_through()
            || existing.persisted_occurrences() != self.occurrences().len()
            || existing.persisted_copies() != self.copies().len()
            || existing.persisted_assertions() != self.assertions().len()
        {
            return Err(SessionStoreError::IdempotencyConflict {
                context: "projection batch replay",
            });
        }
        Ok(SessionTemporalProjectionBatchDispositionV1::ExactReplay)
    }
}

/// Durable acknowledgement for one projection batch write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTemporalProjectionBatchReceiptV1 {
    session_id: SessionId,
    generation: SessionProjectionGenerationV1,
    watermarks: SessionFrozenWatermarksV1,
    batch_ordinal: u64,
    batch_digest: SessionTemporalDigestV1,
    source_through: u64,
    projection_through: u64,
    persisted_occurrences: usize,
    persisted_copies: usize,
    persisted_assertions: usize,
    disposition: SessionTemporalProjectionBatchDispositionV1,
    committed_at: UtcMicros,
}

impl SessionTemporalProjectionBatchReceiptV1 {
    pub fn applied(
        batch: &SessionTemporalProjectionBatchV1,
        batch_digest: SessionTemporalDigestV1,
        persisted_occurrences: usize,
        persisted_copies: usize,
        persisted_assertions: usize,
        committed_at: UtcMicros,
    ) -> SessionStoreResult<Self> {
        Self::build(
            batch,
            batch_digest,
            persisted_occurrences,
            persisted_copies,
            persisted_assertions,
            SessionTemporalProjectionBatchDispositionV1::Applied,
            committed_at,
        )
    }

    pub fn exact_replay(
        batch: &SessionTemporalProjectionBatchV1,
        batch_digest: SessionTemporalDigestV1,
        existing: &Self,
        committed_at: UtcMicros,
    ) -> SessionStoreResult<Self> {
        batch.replay_disposition(&batch_digest, existing)?;
        Self::build(
            batch,
            batch_digest,
            batch.occurrences().len(),
            batch.copies().len(),
            batch.assertions().len(),
            SessionTemporalProjectionBatchDispositionV1::ExactReplay,
            committed_at,
        )
    }

    fn build(
        batch: &SessionTemporalProjectionBatchV1,
        batch_digest: SessionTemporalDigestV1,
        persisted_occurrences: usize,
        persisted_copies: usize,
        persisted_assertions: usize,
        disposition: SessionTemporalProjectionBatchDispositionV1,
        committed_at: UtcMicros,
    ) -> SessionStoreResult<Self> {
        for (field, expected, actual) in [
            (
                "projection occurrences",
                batch.occurrences().len(),
                persisted_occurrences,
            ),
            ("projection copies", batch.copies().len(), persisted_copies),
            (
                "projection assertions",
                batch.assertions().len(),
                persisted_assertions,
            ),
        ] {
            if expected != actual {
                return Err(SessionStoreError::ReceiptCountMismatch {
                    field,
                    expected,
                    actual,
                });
            }
        }
        Ok(Self {
            session_id: batch.session_id().clone(),
            generation: batch.generation(),
            watermarks: batch.watermarks().clone(),
            batch_ordinal: batch.batch_ordinal(),
            batch_digest,
            source_through: batch.source_through(),
            projection_through: batch.projection_through(),
            persisted_occurrences,
            persisted_copies,
            persisted_assertions,
            disposition,
            committed_at,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn generation(&self) -> SessionProjectionGenerationV1 {
        self.generation
    }

    pub fn watermarks(&self) -> &SessionFrozenWatermarksV1 {
        &self.watermarks
    }

    pub const fn batch_ordinal(&self) -> u64 {
        self.batch_ordinal
    }

    pub fn batch_digest(&self) -> &SessionTemporalDigestV1 {
        &self.batch_digest
    }

    pub const fn source_through(&self) -> u64 {
        self.source_through
    }

    pub const fn projection_through(&self) -> u64 {
        self.projection_through
    }

    pub const fn persisted_occurrences(&self) -> usize {
        self.persisted_occurrences
    }

    pub const fn persisted_copies(&self) -> usize {
        self.persisted_copies
    }

    pub const fn persisted_assertions(&self) -> usize {
        self.persisted_assertions
    }

    pub const fn disposition(&self) -> SessionTemporalProjectionBatchDispositionV1 {
        self.disposition
    }

    pub const fn committed_at(&self) -> UtcMicros {
        self.committed_at
    }
}

/// Idempotent outcome for a candidate projection batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTemporalProjectionBatchDispositionV1 {
    Applied,
    ExactReplay,
}

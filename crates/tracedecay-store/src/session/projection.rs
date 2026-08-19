use std::future::Future;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    LogicalCopyRecordV1, MessageOccurrenceRecordV1, SessionId, SessionProjectionGenerationV1,
    TemporalAssertionRecordV1, UtcMicros,
};
use tracedecay_temporal_query::ports::ExecutionControl;

use super::common::{
    SessionFrozenWatermarksV1, SessionGenerationActivatePermit,
    SessionGenerationRebuildBeginPermit, SessionProjectionBatchPersistPermit, SessionStoreError,
    SessionStoreResult, SessionTemporalCapabilityProvider, SessionTemporalCapabilityV1,
    SessionTemporalDigestV1, SessionTemporalSnapshotV1, require_capability,
    require_newer_generation, require_snapshot_session,
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

/// Request to build a candidate generation from an already-frozen snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGenerationRebuildRequestV1 {
    session_id: SessionId,
    candidate_generation: SessionProjectionGenerationV1,
    snapshot: SessionTemporalSnapshotV1,
}

impl SessionGenerationRebuildRequestV1 {
    pub fn new(
        session_id: SessionId,
        candidate_generation: SessionProjectionGenerationV1,
        snapshot: SessionTemporalSnapshotV1,
    ) -> SessionStoreResult<Self> {
        require_snapshot_session(&session_id, &snapshot, "generation rebuild request")?;
        require_capability(&snapshot, SessionTemporalCapabilityV1::GenerationRebuild)?;
        require_newer_generation(
            candidate_generation,
            snapshot.watermarks().active_generation(),
        )?;
        Ok(Self {
            session_id,
            candidate_generation,
            snapshot,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn candidate_generation(&self) -> SessionProjectionGenerationV1 {
        self.candidate_generation
    }

    pub fn snapshot(&self) -> &SessionTemporalSnapshotV1 {
        &self.snapshot
    }
}

/// State of an explicit candidate-generation rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionGenerationRebuildDispositionV1 {
    Started,
    Resumed,
    Complete,
}

impl SessionGenerationRebuildDispositionV1 {
    /// Valid durable transitions: started/resumed may resume or complete;
    /// complete is terminal and may only be observed again as complete.
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Started | Self::Resumed,
                Self::Resumed | Self::Complete
            ) | (Self::Complete, Self::Complete)
        )
    }
}

/// Receipt for beginning, resuming, or completing a generation rebuild.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGenerationRebuildReceiptV1 {
    session_id: SessionId,
    generation: SessionProjectionGenerationV1,
    snapshot: SessionTemporalSnapshotV1,
    disposition: SessionGenerationRebuildDispositionV1,
    recorded_at: UtcMicros,
}

impl SessionGenerationRebuildReceiptV1 {
    pub fn new(
        request: &SessionGenerationRebuildRequestV1,
        disposition: SessionGenerationRebuildDispositionV1,
        recorded_at: UtcMicros,
    ) -> SessionStoreResult<Self> {
        Ok(Self {
            session_id: request.session_id().clone(),
            generation: request.candidate_generation(),
            snapshot: request.snapshot().clone(),
            disposition,
            recorded_at,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn generation(&self) -> SessionProjectionGenerationV1 {
        self.generation
    }

    pub fn snapshot(&self) -> &SessionTemporalSnapshotV1 {
        &self.snapshot
    }

    pub const fn disposition(&self) -> SessionGenerationRebuildDispositionV1 {
        self.disposition
    }

    pub const fn recorded_at(&self) -> UtcMicros {
        self.recorded_at
    }

    pub fn validate_successor(&self, next: &Self) -> SessionStoreResult<()> {
        if self.session_id != next.session_id
            || self.generation != next.generation
            || self.snapshot != next.snapshot
        {
            return Err(SessionStoreError::ReceiptIdentityMismatch {
                context: "generation rebuild successor",
            });
        }
        if !self.disposition.can_transition_to(next.disposition)
            || next.recorded_at < self.recorded_at
        {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "generation rebuild successor",
            });
        }
        Ok(())
    }
}

/// Request to publish a fully-built candidate generation as active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGenerationActivationRequestV1 {
    session_id: SessionId,
    generation: SessionProjectionGenerationV1,
    snapshot: SessionTemporalSnapshotV1,
    execution_control: ExecutionControl,
}

impl SessionGenerationActivationRequestV1 {
    pub fn new(
        session_id: SessionId,
        generation: SessionProjectionGenerationV1,
        snapshot: SessionTemporalSnapshotV1,
        execution_control: ExecutionControl,
    ) -> SessionStoreResult<Self> {
        require_snapshot_session(&session_id, &snapshot, "generation activation request")?;
        require_capability(&snapshot, SessionTemporalCapabilityV1::GenerationRebuild)?;
        require_newer_generation(generation, snapshot.watermarks().active_generation())?;
        Ok(Self {
            session_id,
            generation,
            snapshot,
            execution_control,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn generation(&self) -> SessionProjectionGenerationV1 {
        self.generation
    }

    pub fn snapshot(&self) -> &SessionTemporalSnapshotV1 {
        &self.snapshot
    }

    pub fn execution_control(&self) -> &ExecutionControl {
        &self.execution_control
    }
}

/// Receipt that atomically switched the active temporal generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGenerationActivationReceiptV1 {
    session_id: SessionId,
    generation: SessionProjectionGenerationV1,
    previous_generation: Option<SessionProjectionGenerationV1>,
    watermarks: SessionFrozenWatermarksV1,
    activated_at: UtcMicros,
}

impl SessionGenerationActivationReceiptV1 {
    pub fn new(
        request: &SessionGenerationActivationRequestV1,
        watermarks: SessionFrozenWatermarksV1,
        activated_at: UtcMicros,
    ) -> SessionStoreResult<Self> {
        if watermarks.active_generation() != request.generation()
            || !watermarks.has_same_frontiers_and_cursor(request.snapshot().watermarks())
        {
            return Err(SessionStoreError::ReceiptIdentityMismatch {
                context: "generation activation",
            });
        }
        Ok(Self {
            session_id: request.session_id().clone(),
            generation: request.generation(),
            previous_generation: Some(request.snapshot().watermarks().active_generation()),
            watermarks,
            activated_at,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn generation(&self) -> SessionProjectionGenerationV1 {
        self.generation
    }

    pub const fn previous_generation(&self) -> Option<SessionProjectionGenerationV1> {
        self.previous_generation
    }

    pub fn watermarks(&self) -> &SessionFrozenWatermarksV1 {
        &self.watermarks
    }

    pub const fn activated_at(&self) -> UtcMicros {
        self.activated_at
    }
}

/// Candidate-generation writes. Implementations own no caller runtime or connection opening.
///
/// `Send + Sync` is retained for daemon sharing. Every public operation checks
/// the adapter's capabilities before entering permit-requiring dispatch.
pub trait SessionTemporalProjectionStore: SessionTemporalCapabilityProvider + Send + Sync {
    fn begin_session_generation_rebuild(
        &self,
        request: SessionGenerationRebuildRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionGenerationRebuildReceiptV1>> + Send {
        async move {
            let permit =
                SessionGenerationRebuildBeginPermit::grant(self.session_temporal_capabilities())?;
            self.begin_session_generation_rebuild_supported(permit, request)
                .await
        }
    }

    fn begin_session_generation_rebuild_supported(
        &self,
        permit: SessionGenerationRebuildBeginPermit,
        request: SessionGenerationRebuildRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionGenerationRebuildReceiptV1>> + Send;

    fn persist_session_temporal_projection_batch(
        &self,
        batch: SessionTemporalProjectionBatchV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalProjectionBatchReceiptV1>> + Send
    {
        async move {
            let permit =
                SessionProjectionBatchPersistPermit::grant(self.session_temporal_capabilities())?;
            self.persist_session_temporal_projection_batch_supported(permit, batch)
                .await
        }
    }

    fn persist_session_temporal_projection_batch_supported(
        &self,
        permit: SessionProjectionBatchPersistPermit,
        batch: SessionTemporalProjectionBatchV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalProjectionBatchReceiptV1>> + Send;

    fn activate_session_temporal_generation(
        &self,
        request: SessionGenerationActivationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionGenerationActivationReceiptV1>> + Send {
        async move {
            let permit =
                SessionGenerationActivatePermit::grant(self.session_temporal_capabilities())?;
            self.activate_session_temporal_generation_supported(permit, request)
                .await
        }
    }

    fn activate_session_temporal_generation_supported(
        &self,
        permit: SessionGenerationActivatePermit,
        request: SessionGenerationActivationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionGenerationActivationReceiptV1>> + Send;
}

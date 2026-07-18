use std::future::Future;

use tracedecay_domain::{
    SessionId, SessionProjectionGenerationV1, SessionSummaryRecordV1, UtcMicros,
};

use super::common::{
    SessionFrozenWatermarksV1, SessionStoreError, SessionStoreResult,
    SessionTemporalCapabilityProvider, SessionTemporalDigestV1,
    SessionTemporalMigrationBatchApplyPermit, SessionTemporalMigrationReceiptReadPermit,
};
use super::projection::SessionTemporalProjectionBatchV1;

/// Maximum primary and nested records accepted by one temporal migration batch.
pub const MAX_SESSION_TEMPORAL_MIGRATION_BATCH_ITEMS: usize = 1_000;

/// Bounded idempotent import into one candidate temporal generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTemporalMigrationBatchV1 {
    session_id: SessionId,
    source_digest: SessionTemporalDigestV1,
    generation: SessionProjectionGenerationV1,
    batch_ordinal: u64,
    watermarks: SessionFrozenWatermarksV1,
    projection_batch: SessionTemporalProjectionBatchV1,
    summaries: Vec<SessionSummaryRecordV1>,
}

impl SessionTemporalMigrationBatchV1 {
    pub fn new(
        session_id: SessionId,
        source_digest: SessionTemporalDigestV1,
        generation: SessionProjectionGenerationV1,
        batch_ordinal: u64,
        watermarks: SessionFrozenWatermarksV1,
        projection_batch: SessionTemporalProjectionBatchV1,
        summaries: Vec<SessionSummaryRecordV1>,
    ) -> SessionStoreResult<Self> {
        projection_batch.validate()?;
        if projection_batch.session_id() != &session_id {
            return Err(SessionStoreError::SessionMismatch {
                context: "migration projection batch",
            });
        }
        if projection_batch.generation() != generation {
            return Err(SessionStoreError::ProjectionBatchGenerationMismatch);
        }
        if projection_batch.batch_ordinal() != batch_ordinal {
            return Err(SessionStoreError::ReceiptIdentityMismatch {
                context: "migration projection batch ordinal",
            });
        }
        if projection_batch.watermarks() != &watermarks {
            return Err(SessionStoreError::FrozenWatermarkMismatch);
        }
        if summaries
            .iter()
            .any(|summary| summary.session_id() != &session_id)
        {
            return Err(SessionStoreError::SessionMismatch {
                context: "migration summary",
            });
        }

        let item_count = deep_item_count(&projection_batch, &summaries);
        if item_count > MAX_SESSION_TEMPORAL_MIGRATION_BATCH_ITEMS {
            return Err(SessionStoreError::BatchLimitExceeded {
                field: "session temporal migration batch",
                count: item_count,
                max: MAX_SESSION_TEMPORAL_MIGRATION_BATCH_ITEMS,
            });
        }

        Ok(Self {
            session_id,
            source_digest,
            generation,
            batch_ordinal,
            watermarks,
            projection_batch,
            summaries,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn source_digest(&self) -> &SessionTemporalDigestV1 {
        &self.source_digest
    }

    pub const fn generation(&self) -> SessionProjectionGenerationV1 {
        self.generation
    }

    pub const fn batch_ordinal(&self) -> u64 {
        self.batch_ordinal
    }

    pub fn watermarks(&self) -> &SessionFrozenWatermarksV1 {
        &self.watermarks
    }

    pub fn projection_batch(&self) -> &SessionTemporalProjectionBatchV1 {
        &self.projection_batch
    }

    pub fn summaries(&self) -> &[SessionSummaryRecordV1] {
        &self.summaries
    }

    pub fn item_count(&self) -> usize {
        deep_item_count(&self.projection_batch, &self.summaries)
    }

    pub fn replay_disposition(
        &self,
        existing: &SessionTemporalMigrationReceiptV1,
    ) -> SessionStoreResult<SessionTemporalMigrationDispositionV1> {
        if existing.session_id() != self.session_id()
            || existing.generation() != self.generation()
            || existing.batch_ordinal() != self.batch_ordinal()
        {
            return Err(SessionStoreError::ReceiptIdentityMismatch {
                context: "migration batch replay",
            });
        }
        if existing.source_digest() != self.source_digest()
            || existing.watermarks() != self.watermarks()
            || existing.imported_items() != self.item_count()
        {
            return Err(SessionStoreError::IdempotencyConflict {
                context: "migration batch replay",
            });
        }
        Ok(SessionTemporalMigrationDispositionV1::AlreadyApplied)
    }
}

fn deep_item_count(
    projection_batch: &SessionTemporalProjectionBatchV1,
    summaries: &[SessionSummaryRecordV1],
) -> usize {
    summaries.iter().fold(
        projection_batch
            .item_count()
            .saturating_add(summaries.len()),
        |count, summary| count.saturating_add(summary.source_anchors().len()),
    )
}

/// Request to look up one idempotent migration receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTemporalMigrationReceiptRequestV1 {
    session_id: SessionId,
    generation: SessionProjectionGenerationV1,
    batch_ordinal: u64,
}

impl SessionTemporalMigrationReceiptRequestV1 {
    pub fn new(
        session_id: SessionId,
        generation: SessionProjectionGenerationV1,
        batch_ordinal: u64,
    ) -> Self {
        Self {
            session_id,
            generation,
            batch_ordinal,
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub const fn generation(&self) -> SessionProjectionGenerationV1 {
        self.generation
    }

    pub const fn batch_ordinal(&self) -> u64 {
        self.batch_ordinal
    }
}

/// Whether a migration batch introduced data or repeated an existing receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTemporalMigrationDispositionV1 {
    Applied,
    AlreadyApplied,
}

/// Idempotent receipt for one temporal migration batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTemporalMigrationReceiptV1 {
    session_id: SessionId,
    source_digest: SessionTemporalDigestV1,
    generation: SessionProjectionGenerationV1,
    batch_ordinal: u64,
    watermarks: SessionFrozenWatermarksV1,
    imported_items: usize,
    disposition: SessionTemporalMigrationDispositionV1,
    committed_at: UtcMicros,
}

impl SessionTemporalMigrationReceiptV1 {
    pub fn applied(
        batch: &SessionTemporalMigrationBatchV1,
        imported_items: usize,
        committed_at: UtcMicros,
    ) -> SessionStoreResult<Self> {
        Self::build(
            batch,
            imported_items,
            SessionTemporalMigrationDispositionV1::Applied,
            committed_at,
        )
    }

    pub fn already_applied(
        batch: &SessionTemporalMigrationBatchV1,
        existing: &Self,
        committed_at: UtcMicros,
    ) -> SessionStoreResult<Self> {
        batch.replay_disposition(existing)?;
        Self::build(
            batch,
            batch.item_count(),
            SessionTemporalMigrationDispositionV1::AlreadyApplied,
            committed_at,
        )
    }

    fn build(
        batch: &SessionTemporalMigrationBatchV1,
        imported_items: usize,
        disposition: SessionTemporalMigrationDispositionV1,
        committed_at: UtcMicros,
    ) -> SessionStoreResult<Self> {
        if imported_items != batch.item_count() {
            return Err(SessionStoreError::ReceiptCountMismatch {
                field: "migration imported items",
                expected: batch.item_count(),
                actual: imported_items,
            });
        }
        Ok(Self {
            session_id: batch.session_id().clone(),
            source_digest: batch.source_digest().clone(),
            generation: batch.generation(),
            batch_ordinal: batch.batch_ordinal(),
            watermarks: batch.watermarks().clone(),
            imported_items,
            disposition,
            committed_at,
        })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn source_digest(&self) -> &SessionTemporalDigestV1 {
        &self.source_digest
    }

    pub const fn generation(&self) -> SessionProjectionGenerationV1 {
        self.generation
    }

    pub const fn batch_ordinal(&self) -> u64 {
        self.batch_ordinal
    }

    pub fn watermarks(&self) -> &SessionFrozenWatermarksV1 {
        &self.watermarks
    }

    pub const fn imported_items(&self) -> usize {
        self.imported_items
    }

    pub const fn disposition(&self) -> SessionTemporalMigrationDispositionV1 {
        self.disposition
    }

    pub const fn committed_at(&self) -> UtcMicros {
        self.committed_at
    }
}

/// Bounded, receipt-backed migration batches.
///
/// Public caller entrypoints grant an operation-specific permit before
/// dispatch. Low-level `*_supported` methods require their exact unforgeable
/// permit and are therefore unreachable without the matching capability guard.
pub trait SessionTemporalMigrationStore: SessionTemporalCapabilityProvider + Send + Sync {
    fn apply_session_temporal_migration_batch(
        &self,
        batch: SessionTemporalMigrationBatchV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalMigrationReceiptV1>> + Send {
        async move {
            let permit = SessionTemporalMigrationBatchApplyPermit::grant(
                self.session_temporal_capabilities(),
            )?;
            self.apply_session_temporal_migration_batch_supported(permit, batch)
                .await
        }
    }

    fn apply_session_temporal_migration_batch_supported(
        &self,
        permit: SessionTemporalMigrationBatchApplyPermit,
        batch: SessionTemporalMigrationBatchV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalMigrationReceiptV1>> + Send;

    fn session_temporal_migration_receipt(
        &self,
        request: SessionTemporalMigrationReceiptRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionTemporalMigrationReceiptV1>>> + Send
    {
        async move {
            let permit = SessionTemporalMigrationReceiptReadPermit::grant(
                self.session_temporal_capabilities(),
            )?;
            self.session_temporal_migration_receipt_supported(permit, request)
                .await
        }
    }

    fn session_temporal_migration_receipt_supported(
        &self,
        permit: SessionTemporalMigrationReceiptReadPermit,
        request: SessionTemporalMigrationReceiptRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionTemporalMigrationReceiptV1>>> + Send;
}

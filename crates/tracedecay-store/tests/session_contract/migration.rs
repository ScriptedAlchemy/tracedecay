// Contract-test adapters keep the trait's `impl Future` signature shape
// explicit; the bodies are the async implementation.
#![allow(clippy::manual_async_fn)]
use super::common::*;
use super::*;

#[test]
fn migration_revalidates_projection_records_and_summary_session_ownership() {
    let session_id = session("session.fixture");
    let projection = projection_batch(&session_id);
    assert!(matches!(
        SessionTemporalMigrationBatchV1::new(
            session_id.clone(),
            digest(),
            generation(8),
            0,
            projection.watermarks().clone(),
            projection.clone(),
            vec![summary(&session("session.other"), "summary.other", 1)],
        ),
        Err(SessionStoreError::SessionMismatch {
            context: "migration summary"
        })
    ));

    let receipt_request =
        SessionTemporalMigrationReceiptRequestV1::new(session_id, generation(8), 0);
    assert_eq!(receipt_request.generation(), generation(8));
}

#[test]
fn projection_and_migration_receipts_bind_counts_and_exact_replay_identity() {
    let session_id = session("session.receipts");
    let projection = projection_batch(&session_id);
    assert!(matches!(
        SessionTemporalProjectionBatchReceiptV1::applied(
            &projection,
            temporal_digest('a'),
            projection.occurrences().len() + 1,
            projection.copies().len(),
            projection.assertions().len(),
            UtcMicros(100),
        ),
        Err(SessionStoreError::ReceiptCountMismatch {
            field: "projection occurrences",
            ..
        })
    ));
    let projection_receipt = SessionTemporalProjectionBatchReceiptV1::applied(
        &projection,
        temporal_digest('a'),
        projection.occurrences().len(),
        projection.copies().len(),
        projection.assertions().len(),
        UtcMicros(100),
    )
    .unwrap();
    assert_eq!(
        projection
            .replay_disposition(&temporal_digest('a'), &projection_receipt)
            .unwrap(),
        SessionTemporalProjectionBatchDispositionV1::ExactReplay
    );
    assert!(matches!(
        projection.replay_disposition(&temporal_digest('b'), &projection_receipt),
        Err(SessionStoreError::IdempotencyConflict {
            context: "projection batch replay"
        })
    ));

    let migration = SessionTemporalMigrationBatchV1::new(
        session_id,
        digest(),
        generation(8),
        0,
        projection.watermarks().clone(),
        projection,
        vec![],
    )
    .unwrap();
    assert!(matches!(
        SessionTemporalMigrationReceiptV1::applied(
            &migration,
            migration.item_count() + 1,
            UtcMicros(101),
        ),
        Err(SessionStoreError::ReceiptCountMismatch {
            field: "migration imported items",
            ..
        })
    ));
    let migration_receipt = SessionTemporalMigrationReceiptV1::applied(
        &migration,
        migration.item_count(),
        UtcMicros(101),
    )
    .unwrap();
    assert_eq!(
        migration.replay_disposition(&migration_receipt).unwrap(),
        SessionTemporalMigrationDispositionV1::AlreadyApplied
    );
    let conflicting = SessionTemporalMigrationBatchV1::new(
        migration.session_id().clone(),
        temporal_digest('c'),
        migration.generation(),
        migration.batch_ordinal(),
        migration.watermarks().clone(),
        migration.projection_batch().clone(),
        vec![],
    )
    .unwrap();
    assert!(matches!(
        conflicting.replay_disposition(&migration_receipt),
        Err(SessionStoreError::IdempotencyConflict {
            context: "migration batch replay"
        })
    ));
}

#[test]
fn migration_rejects_generation_watermark_and_ordinal_mismatches() {
    let session_id = session("session.migration-identity");
    let projection = projection_batch(&session_id);
    assert!(matches!(
        SessionTemporalMigrationBatchV1::new(
            session_id.clone(),
            digest(),
            generation(9),
            0,
            projection.watermarks().clone(),
            projection.clone(),
            vec![],
        ),
        Err(SessionStoreError::ProjectionBatchGenerationMismatch)
    ));
    assert!(matches!(
        SessionTemporalMigrationBatchV1::new(
            session_id.clone(),
            digest(),
            generation(8),
            0,
            SessionFrozenWatermarksV1::new(generation(7), 52, 47, 43),
            projection.clone(),
            vec![],
        ),
        Err(SessionStoreError::FrozenWatermarkMismatch)
    ));
    assert!(matches!(
        SessionTemporalMigrationBatchV1::new(
            session_id,
            digest(),
            generation(8),
            1,
            projection.watermarks().clone(),
            projection,
            vec![],
        ),
        Err(SessionStoreError::ReceiptIdentityMismatch {
            context: "migration projection batch ordinal"
        })
    ));
}

impl SessionTemporalMigrationStore for InMemorySessionPorts {
    fn apply_session_temporal_migration_batch_supported(
        &self,
        _permit: SessionTemporalMigrationBatchApplyPermit,
        batch: SessionTemporalMigrationBatchV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalMigrationReceiptV1>> + Send {
        async move {
            yield_once().await;
            let mut state = self.state.lock().unwrap();
            let receipt = match &state.migration {
                Some(existing) => SessionTemporalMigrationReceiptV1::already_applied(
                    &batch,
                    existing,
                    UtcMicros(111),
                )?,
                None => SessionTemporalMigrationReceiptV1::applied(
                    &batch,
                    batch.item_count(),
                    UtcMicros(111),
                )?,
            };
            state.migration = Some(receipt.clone());
            Ok(receipt)
        }
    }

    fn session_temporal_migration_receipt_supported(
        &self,
        _permit: SessionTemporalMigrationReceiptReadPermit,
        request: SessionTemporalMigrationReceiptRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionTemporalMigrationReceiptV1>>> + Send
    {
        async move {
            yield_once().await;
            Ok(self
                .state
                .lock()
                .unwrap()
                .migration
                .clone()
                .filter(|receipt| {
                    receipt.session_id() == request.session_id()
                        && receipt.generation() == request.generation()
                        && receipt.batch_ordinal() == request.batch_ordinal()
                }))
        }
    }
}

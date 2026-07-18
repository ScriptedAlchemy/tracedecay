use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::store::{GlobalDbSessionTemporalStore, SessionRefreshRestartStateV1};
use tracedecay_domain::{SessionId, TemporalCoverageCountsV1, UtcMicros};
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCancellationRequestV1,
    SessionRefreshCompletionRequestV1, SessionRefreshDispositionV1, SessionRefreshFailureRequestV1,
    SessionRefreshFrontierV1, SessionRefreshProgressRequestV1, SessionRefreshProgressV1,
    SessionRefreshReceiptRequestV1, SessionRefreshStore, SessionRefreshTerminalStateV1,
    SessionStoreError, SessionTemporalProjectionBatchV1,
};

use crate::common::{isolated_lcm_db_path, open_lcm_db};

fn session(value: &str) -> SessionId {
    SessionId::new(value).unwrap()
}

fn frontier(observed: u64, committed: u64) -> SessionRefreshFrontierV1 {
    SessionRefreshFrontierV1::new(observed, committed).unwrap()
}

fn coverage(visible: u64) -> TemporalCoverageCountsV1 {
    TemporalCoverageCountsV1 {
        visible,
        hidden: 0,
        unknown: 0,
        redacted: 0,
    }
}

fn now() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros(),
        )
        .unwrap(),
    )
}

async fn begin(
    store: &GlobalDbSessionTemporalStore<'_>,
    session_id: &SessionId,
    target: SessionRefreshFrontierV1,
) -> tracedecay_store::SessionRefreshBeginOrJoinReceiptV1 {
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            target,
        ))
        .await
        .unwrap()
}

async fn scalar(path: &std::path::Path, sql: &str) -> i64 {
    let raw_db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn batch_for(
    recovery: &tracedecay::store::SessionRefreshRecoveryV1,
    batch_ordinal: u64,
    source_through: u64,
    projection_through: u64,
) -> SessionTemporalProjectionBatchV1 {
    SessionTemporalProjectionBatchV1::new(
        recovery.session_id().clone(),
        recovery.candidate_generation(),
        recovery.frozen_watermarks().clone(),
        vec![],
        vec![],
        vec![],
    )
    .unwrap()
    .with_checkpoint(batch_ordinal, source_through, projection_through)
    .unwrap()
}

fn progress_for(
    recovery: &tracedecay::store::SessionRefreshRecoveryV1,
    committed_through: u64,
    committed_batches: u64,
) -> SessionRefreshProgressV1 {
    SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        frontier(
            recovery.target_frontier().observed_through(),
            committed_through,
        ),
        coverage(committed_through),
        committed_batches,
        committed_through,
        now(),
    )
}

#[tokio::test]
async fn begin_joins_exact_requests_and_rejects_conflicting_running_targets() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.join");

    let started = begin(&store, &session_id, frontier(4, 0)).await;
    assert_eq!(started.disposition(), SessionRefreshDispositionV1::Started);
    let joined = begin(&store, &session_id, frontier(4, 0)).await;
    assert_eq!(joined.disposition(), SessionRefreshDispositionV1::Joined);
    assert_eq!(joined.operation_id(), started.operation_id());

    assert!(matches!(
        store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                frontier(5, 0),
            ))
            .await,
        Err(SessionStoreError::IdempotencyConflict { .. })
    ));

    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovery.operation_id(), started.operation_id());
    assert_eq!(recovery.source_frontier(), 0);
    assert_eq!(recovery.target_frontier(), frontier(4, 0));
    assert_eq!(
        recovery.projector_version(),
        "session-temporal-projector.v1"
    );
    assert!(recovery.config_digest().starts_with("sha256:"));
    assert!(recovery.binding_digest().starts_with("sha256:"));
    assert_eq!(
        recovery.restart_state(),
        SessionRefreshRestartStateV1::BeginProjection
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_generations
             WHERE session_id = 'session.refresh.join' AND state = 'building'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn projection_batch_and_progress_commit_atomically_and_replay_exactly() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.progress");
    begin(&store, &session_id, frontier(0, 0)).await;
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let progress = progress_for(&recovery, 0, 1);
    let batch = batch_for(&recovery, 0, 0, 0);

    let (persisted, first_receipt) = store
        .persist_session_refresh_projection_batch(progress.clone(), batch.clone())
        .await
        .unwrap();
    assert_eq!(persisted, progress);
    let (replayed, replay_receipt) = store
        .persist_session_refresh_projection_batch(progress.clone(), batch)
        .await
        .unwrap();
    assert_eq!(replayed, progress);
    assert_eq!(replay_receipt.batch_digest(), first_receipt.batch_digest());
    assert_eq!(
        store
            .persist_session_refresh_progress(progress.clone())
            .await
            .unwrap(),
        progress
    );
    assert_eq!(
        store
            .session_refresh_progress(SessionRefreshProgressRequestV1::new(
                recovery.operation_id().clone(),
                session_id.clone(),
            ))
            .await
            .unwrap(),
        Some(progress)
    );

    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_refresh_progress
             WHERE session_id = 'session.refresh.progress'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_refresh_batch_bindings
             WHERE session_id = 'session.refresh.progress'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn invalid_progress_rolls_back_the_projection_receipt_without_a_crash_gap() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.rollback");
    begin(&store, &session_id, frontier(0, 0)).await;
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let invalid = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        session_id.clone(),
        frontier(0, 0),
        coverage(0),
        2,
        0,
        now(),
    );

    assert!(
        store
            .persist_session_refresh_projection_batch(invalid, batch_for(&recovery, 0, 0, 0),)
            .await
            .is_err()
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_projection_receipts
             WHERE session_id = 'session.refresh.rollback'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_refresh_progress
             WHERE session_id = 'session.refresh.rollback'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn complete_activates_the_bound_generation_and_terminal_retry_is_exact() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.complete");
    begin(&store, &session_id, frontier(0, 0)).await;
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let progress = progress_for(&recovery, 0, 1);
    store
        .persist_session_refresh_projection_batch(progress.clone(), batch_for(&recovery, 0, 0, 0))
        .await
        .unwrap();
    let request = SessionRefreshCompletionRequestV1::new(
        recovery.operation_id().clone(),
        session_id.clone(),
        progress.frontier(),
        *progress.coverage(),
    )
    .unwrap();

    let receipt = store
        .complete_session_refresh(request.clone())
        .await
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert_eq!(
        store.complete_session_refresh(request).await.unwrap(),
        receipt
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT generation FROM session_temporal_generations
             WHERE session_id = 'session.refresh.complete' AND state = 'active'"
        )
        .await,
        i64::try_from(recovery.candidate_generation().value()).unwrap()
    );
    assert!(
        store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .is_none()
    );
    let retry = begin(&store, &session_id, frontier(0, 0)).await;
    assert_eq!(retry.disposition(), SessionRefreshDispositionV1::Joined);
    assert_eq!(retry.operation_id(), receipt.operation_id());
}

#[tokio::test]
async fn failure_and_cancellation_preserve_last_durable_progress() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);

    let failed_session = session("session.refresh.failed");
    begin(&store, &failed_session, frontier(0, 0)).await;
    let failed = store
        .session_refresh_recovery(&failed_session)
        .await
        .unwrap()
        .unwrap();
    let failed_progress = progress_for(&failed, 0, 1);
    store
        .persist_session_refresh_projection_batch(
            failed_progress.clone(),
            batch_for(&failed, 0, 0, 0),
        )
        .await
        .unwrap();
    let failure = SessionRefreshFailureRequestV1::new(
        failed.operation_id().clone(),
        failed_session.clone(),
        failed_progress.frontier(),
        *failed_progress.coverage(),
        "projector_failed",
    )
    .unwrap();
    let failed_receipt = store.fail_session_refresh(failure.clone()).await.unwrap();
    assert_eq!(
        failed_receipt.state(),
        SessionRefreshTerminalStateV1::Failed
    );
    assert_eq!(
        store.fail_session_refresh(failure).await.unwrap(),
        failed_receipt
    );

    let cancelled_session = session("session.refresh.cancelled");
    begin(&store, &cancelled_session, frontier(0, 0)).await;
    let cancelled = store
        .session_refresh_recovery(&cancelled_session)
        .await
        .unwrap()
        .unwrap();
    let cancelled_progress = progress_for(&cancelled, 0, 1);
    store
        .persist_session_refresh_projection_batch(
            cancelled_progress.clone(),
            batch_for(&cancelled, 0, 0, 0),
        )
        .await
        .unwrap();
    let cancellation = SessionRefreshCancellationRequestV1::new(
        cancelled.operation_id().clone(),
        cancelled_session.clone(),
        cancelled_progress.frontier(),
        *cancelled_progress.coverage(),
    );
    let cancelled_receipt = store
        .cancel_session_refresh(cancellation.clone())
        .await
        .unwrap();
    assert_eq!(
        cancelled_receipt.state(),
        SessionRefreshTerminalStateV1::Cancelled
    );
    assert_eq!(
        store.cancel_session_refresh(cancellation).await.unwrap(),
        cancelled_receipt
    );

    for (session_id, operation_id, expected) in [
        (
            failed_session,
            failed.operation_id().clone(),
            SessionRefreshTerminalStateV1::Failed,
        ),
        (
            cancelled_session,
            cancelled.operation_id().clone(),
            SessionRefreshTerminalStateV1::Cancelled,
        ),
    ] {
        assert_eq!(
            store
                .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
                    operation_id,
                    session_id,
                ))
                .await
                .unwrap()
                .unwrap()
                .state(),
            expected
        );
    }
}

#[tokio::test]
async fn fail_or_cancel_with_no_progress_terminates_and_releases_session() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);

    let failed_session = session("session.refresh.empty.fail");
    begin(&store, &failed_session, frontier(0, 0)).await;
    let failed = store
        .session_refresh_recovery(&failed_session)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        store
            .fail_session_refresh(
                SessionRefreshFailureRequestV1::new(
                    failed.operation_id().clone(),
                    failed_session.clone(),
                    frontier(0, 0),
                    coverage(1),
                    "projector_failed",
                )
                .unwrap()
            )
            .await,
        Err(SessionStoreError::InvalidStateTransition { .. })
    ));
    let failed_receipt = store
        .fail_session_refresh(
            SessionRefreshFailureRequestV1::new(
                failed.operation_id().clone(),
                failed_session.clone(),
                frontier(0, 0),
                coverage(0),
                "projector_failed",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        failed_receipt.state(),
        SessionRefreshTerminalStateV1::Failed
    );
    assert!(
        store
            .session_refresh_recovery(&failed_session)
            .await
            .unwrap()
            .is_none()
    );

    let cancelled_session = session("session.refresh.empty.cancel");
    begin(&store, &cancelled_session, frontier(0, 0)).await;
    let cancelled = store
        .session_refresh_recovery(&cancelled_session)
        .await
        .unwrap()
        .unwrap();
    let cancelled_receipt = store
        .cancel_session_refresh(SessionRefreshCancellationRequestV1::new(
            cancelled.operation_id().clone(),
            cancelled_session.clone(),
            frontier(0, 0),
            coverage(0),
        ))
        .await
        .unwrap();
    assert_eq!(
        cancelled_receipt.state(),
        SessionRefreshTerminalStateV1::Cancelled
    );
    assert!(
        store
            .session_refresh_recovery(&cancelled_session)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn begin_after_failure_or_cancellation_starts_new_operation_for_same_frontier() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);

    let failed_session = session("session.refresh.retry.failed");
    let first = begin(&store, &failed_session, frontier(0, 0)).await;
    store
        .fail_session_refresh(
            SessionRefreshFailureRequestV1::new(
                first.operation_id().clone(),
                failed_session.clone(),
                frontier(0, 0),
                coverage(0),
                "projector_failed",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let restarted = begin(&store, &failed_session, frontier(0, 0)).await;
    assert_eq!(
        restarted.disposition(),
        SessionRefreshDispositionV1::Started
    );
    assert_ne!(restarted.operation_id(), first.operation_id());
    assert_eq!(
        store
            .session_refresh_recovery(&failed_session)
            .await
            .unwrap()
            .unwrap()
            .operation_id(),
        restarted.operation_id()
    );

    let cancelled_session = session("session.refresh.retry.cancelled");
    let cancelled = begin(&store, &cancelled_session, frontier(0, 0)).await;
    store
        .cancel_session_refresh(SessionRefreshCancellationRequestV1::new(
            cancelled.operation_id().clone(),
            cancelled_session.clone(),
            frontier(0, 0),
            coverage(0),
        ))
        .await
        .unwrap();
    let restarted_cancel = begin(&store, &cancelled_session, frontier(0, 0)).await;
    assert_eq!(
        restarted_cancel.disposition(),
        SessionRefreshDispositionV1::Started
    );
    assert_ne!(restarted_cancel.operation_id(), cancelled.operation_id());
}

#[tokio::test]
async fn cross_terminal_retry_is_idempotency_conflict() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.cross.terminal");
    begin(&store, &session_id, frontier(0, 0)).await;
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let progress = progress_for(&recovery, 0, 1);
    store
        .persist_session_refresh_projection_batch(progress.clone(), batch_for(&recovery, 0, 0, 0))
        .await
        .unwrap();
    let complete = SessionRefreshCompletionRequestV1::new(
        recovery.operation_id().clone(),
        session_id.clone(),
        progress.frontier(),
        *progress.coverage(),
    )
    .unwrap();
    store.complete_session_refresh(complete).await.unwrap();

    assert!(matches!(
        store
            .fail_session_refresh(
                SessionRefreshFailureRequestV1::new(
                    recovery.operation_id().clone(),
                    session_id.clone(),
                    progress.frontier(),
                    *progress.coverage(),
                    "projector_failed",
                )
                .unwrap()
            )
            .await,
        Err(SessionStoreError::IdempotencyConflict { .. })
    ));
    assert!(matches!(
        store
            .cancel_session_refresh(SessionRefreshCancellationRequestV1::new(
                recovery.operation_id().clone(),
                session_id,
                progress.frontier(),
                *progress.coverage(),
            ))
            .await,
        Err(SessionStoreError::IdempotencyConflict { .. })
    ));
}

#[tokio::test]
async fn progress_replay_ignores_updated_at_clock_skew() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.progress.clock");
    begin(&store, &session_id, frontier(0, 0)).await;
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let first = progress_for(&recovery, 0, 1);
    store
        .persist_session_refresh_projection_batch(first.clone(), batch_for(&recovery, 0, 0, 0))
        .await
        .unwrap();
    let skewed = SessionRefreshProgressV1::new(
        first.operation_id().clone(),
        first.session_id().clone(),
        first.frontier(),
        *first.coverage(),
        first.committed_batches(),
        first.committed_records(),
        UtcMicros(first.updated_at().0 + 5_000),
    );
    let replayed = store
        .persist_session_refresh_progress(skewed)
        .await
        .unwrap();
    assert_eq!(replayed, first);
}

#[tokio::test]
async fn progress_rejects_observed_frontier_mismatch() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.progress.binding");
    begin(&store, &session_id, frontier(4, 0)).await;
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let mismatched = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        session_id,
        frontier(5, 0),
        coverage(0),
        1,
        0,
        now(),
    );
    assert!(matches!(
        store
            .persist_session_refresh_projection_batch(mismatched, batch_for(&recovery, 0, 0, 0),)
            .await,
        Err(SessionStoreError::InvalidStateTransition { .. })
    ));
}

#[tokio::test]
async fn concurrent_same_digest_begins_both_join_or_start_one_operation() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let first = GlobalDbSessionTemporalStore::new(&db);
    let second = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.same.digest");
    let request = SessionRefreshBeginOrJoinRequestV1::new(session_id.clone(), frontier(1, 0));

    let (left, right) = tokio::join!(
        first.begin_or_join_session_refresh(request.clone()),
        second.begin_or_join_session_refresh(request),
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_eq!(left.operation_id(), right.operation_id());
    assert_eq!(
        usize::from(left.disposition() == SessionRefreshDispositionV1::Started)
            + usize::from(right.disposition() == SessionRefreshDispositionV1::Started),
        1
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_refresh_operations WHERE state = 'running'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_generations WHERE state = 'building'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn running_session_refreshes_are_bounded_and_ordered() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    for name in [
        "session.refresh.bound.a",
        "session.refresh.bound.b",
        "session.refresh.bound.c",
    ] {
        begin(&store, &session(name), frontier(0, 0)).await;
    }
    let running = store.running_session_refreshes().await.unwrap();
    assert_eq!(running.len(), 3);
    let ids: Vec<_> = running
        .iter()
        .map(|recovery| recovery.session_id().as_str().to_string())
        .collect();
    assert_eq!(
        ids,
        vec![
            "session.refresh.bound.a",
            "session.refresh.bound.b",
            "session.refresh.bound.c",
        ]
    );
}

#[tokio::test]
async fn recovery_is_read_only_and_deterministic_across_reopen() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let session_id = session("session.refresh.restart");
    let operation_id;
    {
        let db = open_lcm_db(&tmp).await;
        let store = GlobalDbSessionTemporalStore::new(&db);
        operation_id = begin(&store, &session_id, frontier(0, 0))
            .await
            .operation_id()
            .clone();
        let recovery = store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap();
        store
            .persist_session_refresh_projection_batch(
                progress_for(&recovery, 0, 1),
                batch_for(&recovery, 0, 0, 0),
            )
            .await
            .unwrap();
    }

    let before = scalar(
        &path,
        "SELECT
            (SELECT COUNT(*) FROM session_refresh_operations)
          + (SELECT COUNT(*) FROM session_refresh_bindings)
          + (SELECT COUNT(*) FROM session_refresh_progress)
          + (SELECT COUNT(*) FROM session_refresh_batch_bindings)
          + (SELECT COUNT(*) FROM session_refresh_receipts)",
    )
    .await;
    let reopened = GlobalDb::try_open_at(&path)
        .await
        .unwrap()
        .expect("database should reopen");
    let store = GlobalDbSessionTemporalStore::new(&reopened);
    let running = store.running_session_refreshes().await.unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].operation_id(), &operation_id);
    assert_eq!(
        running[0].restart_state(),
        SessionRefreshRestartStateV1::ReadyToComplete
    );
    let after = scalar(
        &path,
        "SELECT
            (SELECT COUNT(*) FROM session_refresh_operations)
          + (SELECT COUNT(*) FROM session_refresh_bindings)
          + (SELECT COUNT(*) FROM session_refresh_progress)
          + (SELECT COUNT(*) FROM session_refresh_batch_bindings)
          + (SELECT COUNT(*) FROM session_refresh_receipts)",
    )
    .await;
    assert_eq!(before, after);
}

#[tokio::test]
async fn stale_batch_replay_against_newer_progress_is_idempotency_conflict() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.stale.batch");
    // Non-zero observed target gives distinct projection_through values so empty
    // batches do not collide on content digest (ordinal is not part of the digest).
    begin(&store, &session_id, frontier(2, 0)).await;
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let first = progress_for(&recovery, 0, 1);
    store
        .persist_session_refresh_projection_batch(first, batch_for(&recovery, 0, 0, 0))
        .await
        .unwrap();
    let second = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        session_id.clone(),
        frontier(2, 1),
        coverage(0),
        2,
        0,
        now(),
    );
    store
        .persist_session_refresh_projection_batch(second.clone(), batch_for(&recovery, 1, 0, 1))
        .await
        .unwrap();
    assert!(matches!(
        store
            .persist_session_refresh_projection_batch(second, batch_for(&recovery, 0, 0, 0))
            .await,
        Err(SessionStoreError::IdempotencyConflict { .. })
    ));
}

#[tokio::test]
async fn future_progress_timestamp_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.future.ts");
    begin(&store, &session_id, frontier(0, 0)).await;
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let future = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        session_id,
        frontier(0, 0),
        coverage(0),
        1,
        0,
        UtcMicros(now().0 + 3_600_000_000),
    );
    assert!(matches!(
        store
            .persist_session_refresh_projection_batch(future, batch_for(&recovery, 0, 0, 0))
            .await,
        Err(SessionStoreError::InvalidStateTransition { .. })
    ));
}

#[tokio::test]
async fn concurrent_begins_leave_one_running_owner_and_one_candidate() {
    let tmp = TempDir::new().unwrap();
    let path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let first = GlobalDbSessionTemporalStore::new(&db);
    let second = GlobalDbSessionTemporalStore::new(&db);
    let session_id = session("session.refresh.concurrent");
    let first_request = SessionRefreshBeginOrJoinRequestV1::new(session_id.clone(), frontier(1, 0));
    let second_request =
        SessionRefreshBeginOrJoinRequestV1::new(session_id.clone(), frontier(2, 0));

    let (left, right) = tokio::join!(
        first.begin_or_join_session_refresh(first_request),
        second.begin_or_join_session_refresh(second_request),
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let error = left.err().or_else(|| right.err()).unwrap();
    assert!(matches!(
        error,
        SessionStoreError::IdempotencyConflict { .. }
    ));
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_refresh_operations WHERE state = 'running'"
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &path,
            "SELECT COUNT(*) FROM session_temporal_generations WHERE state = 'building'"
        )
        .await,
        1
    );
}

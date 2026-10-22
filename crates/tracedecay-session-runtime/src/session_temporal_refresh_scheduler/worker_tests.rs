use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracedecay_domain::{SessionId, UtcMicros};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
use tracedecay_session_temporal_store::{SessionRefreshRecoveryV1, SessionTemporalStore};
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressV1,
    SessionRefreshReceiptRequestV1, SessionRefreshStore, SessionRefreshTerminalStateV1,
    SessionTemporalProjectionBatchV1,
};

use super::history::{
    SessionHistoricalCapacityRelease, SessionHistoricalIngestOutcome, SessionHistoricalIngestPass,
    SessionHistoricalIngestor, SharedSessionHistoricalIngestor,
};
use super::projector::{
    CanonicalSessionTemporalProjector, SessionTemporalRefreshEffect, SessionTemporalRefreshPolicy,
    SessionTemporalRefreshProjectionFuture, SessionTemporalRefreshProjector,
    SessionTemporalRefreshProjectorError, zero_refresh_coverage,
};
use super::registry::SessionTemporalRefreshPassReport;
use super::wake::{
    SessionTemporalRefreshRetryClass, SessionTemporalRefreshWakeState, enabled_idle_notification,
};
use super::worker::{
    apply_refresh_effect, run_session_temporal_refresh_pass, run_session_temporal_refresh_scheduler,
};

async fn begin_empty_refreshes(
    store: &SessionTemporalStore<'_, tracedecay_global_db::RegisteredGlobalDb>,
    names: impl IntoIterator<Item = &'static str>,
) {
    for name in names {
        store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                SessionId::new(name).expect("session id"),
                SessionRefreshFrontierV1::new(0, 0).expect("empty frontier"),
            ))
            .await
            .expect("running recovery");
    }
}

#[tokio::test]
async fn durable_recovery_progresses_before_another_discovery_scan() {
    let harness = RegisteredGlobalDbHarness::open("refresh-durable-recovery-first").await;
    let store = SessionTemporalStore::new(harness.registered.as_ref());
    begin_empty_refreshes(&store, ["durable-recovery-first"]).await;
    let state = Arc::new(SessionTemporalRefreshWakeState::default());

    let report = run_session_temporal_refresh_pass(
        &harness.registered,
        &state,
        &CanonicalSessionTemporalProjector,
        SessionTemporalRefreshPolicy {
            max_operations_per_pass: 1,
            ..SessionTemporalRefreshPolicy::default()
        },
    )
    .await;

    assert_eq!(report.projected_batches, 1);
    assert_eq!(report.retryable_errors, 0);
    assert!(
        report.saturated,
        "deferring discovery must schedule a follow-up pass"
    );
}

/// Never finishes a projection, so every operation outlives its deadline.
struct StalledProjector;

impl SessionTemporalRefreshProjector for StalledProjector {
    fn project<'a>(
        &'a self,
        _database: &'a RegisteredGlobalDbLeaseV1,
        _recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn retryable_recovery_stops_the_pass_and_preserves_unattempted_work() {
    let harness = RegisteredGlobalDbHarness::open("refresh-retry-stops-pass").await;
    let store = SessionTemporalStore::new(harness.registered.as_ref());
    begin_empty_refreshes(&store, ["retry-first", "retry-second"]).await;
    let state = Arc::new(SessionTemporalRefreshWakeState::default());

    let report = run_session_temporal_refresh_pass(
        &harness.registered,
        &state,
        &StalledProjector,
        SessionTemporalRefreshPolicy {
            max_operations_per_pass: 2,
            operation_deadline: Duration::from_millis(10),
            ..SessionTemporalRefreshPolicy::default()
        },
    )
    .await;

    assert_eq!(report.deadline_errors, 1);
    assert_eq!(
        report.retry_class,
        Some(SessionTemporalRefreshRetryClass::Deadline)
    );
    assert_eq!(report.backlog, Some(2));
    assert_eq!(state.pending_recovery_operations().len(), 1);
}

#[tokio::test]
async fn refused_projection_progress_retires_the_refresh_instead_of_retrying() {
    let harness = RegisteredGlobalDbHarness::open("refresh-refused-progress-retires").await;
    let store = SessionTemporalStore::new(harness.registered.as_ref());
    begin_empty_refreshes(&store, ["refused-progress"]).await;
    let recovery = store
        .running_session_refreshes()
        .await
        .expect("recoveries")
        .pop()
        .expect("one running recovery");
    let state = SessionTemporalRefreshWakeState::default();
    let mut report = SessionTemporalRefreshPassReport::default();

    // Progress that claims a second committed batch while submitting the
    // first one. The durable contract refuses it, and every later pass would
    // hand the projector the same state and rebuild the same refused row.
    let progress = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        SessionRefreshFrontierV1::new(0, 0).expect("empty frontier"),
        zero_refresh_coverage(),
        2,
        0,
        UtcMicros(1),
    );
    let batch = SessionTemporalProjectionBatchV1::new(
        recovery.session_id().clone(),
        recovery.candidate_generation(),
        recovery.frozen_watermarks().clone(),
        vec![],
        vec![],
        vec![],
    )
    .expect("batch")
    .with_checkpoint(0, 0, 0)
    .expect("checkpoint");

    apply_refresh_effect(
        &store,
        &state,
        &recovery,
        SessionTemporalRefreshEffect::Projection { progress, batch },
        &mut report,
    )
    .await;

    assert_eq!(
        report.failed, 1,
        "a refused progress row must retire the refresh, not stay running"
    );
    assert_eq!(report.retryable_errors, 0);
    assert_eq!(report.terminal_errors, 0);
    assert!(
        store
            .running_session_refreshes()
            .await
            .expect("recoveries")
            .is_empty(),
        "the retired refresh must not be rediscovered"
    );
}

/// Holds its history pass open until the test releases it.
struct GatedHistory {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl SessionHistoricalIngestor for GatedHistory {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            SessionHistoricalIngestOutcome::Complete
        })
    }

    fn capacity_release(&self) -> SessionHistoricalCapacityRelease {
        SessionHistoricalCapacityRelease::new(Vec::new())
    }

    fn cancel(&self) {}
}

/// A hook ingest joins the refresh worker with a wake. When that wake lands
/// while a history pass is running, the pass's projection serves it and no
/// further pass starts, so the join must end with that pass rather than wait
/// out its deadline.
#[tokio::test]
async fn a_wake_served_by_the_running_pass_joins_that_pass() {
    let harness = RegisteredGlobalDbHarness::open("refresh-wake-served-mid-pass").await;
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let history = Arc::new(GatedHistory {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let ingestor: SharedSessionHistoricalIngestor = history.clone();
    state.wake_history();
    let worker = tokio::spawn(run_session_temporal_refresh_scheduler(
        harness.registered.clone(),
        Arc::clone(&state),
        Arc::new(CanonicalSessionTemporalProjector),
        Arc::new(std::sync::RwLock::new(Some(ingestor))),
        Arc::new(tokio::sync::Semaphore::new(1)),
        SessionTemporalRefreshPolicy::default(),
    ));
    history.entered.notified().await;

    let handle = state.handle();
    let join = handle.wake_and_wait_for_projection(Duration::from_secs(5));
    tokio::pin!(join);
    // The first poll registers the join and delivers the wake.
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(join.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    history.release.notify_one();

    assert!(
        join.await,
        "the pass that served the wake must end the join"
    );
    state.cancel();
    worker.await.expect("refresh worker exits on cancellation");
}

/// Every history window advances but waits for the test before returning.
/// This leaves the next window running after the preceding projection drains.
struct ContinuingHistory {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl SessionHistoricalIngestor for ContinuingHistory {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            SessionHistoricalIngestOutcome::Pending {
                made_progress: true,
            }
        })
    }

    fn capacity_release(&self) -> SessionHistoricalCapacityRelease {
        SessionHistoricalCapacityRelease::new(Vec::new())
    }

    fn cancel(&self) {
        self.release.notify_one();
    }
}

#[tokio::test]
async fn a_published_wake_does_not_wait_for_the_next_history_window() {
    let harness = RegisteredGlobalDbHarness::open("refresh-join-before-next-history").await;
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let history = Arc::new(ContinuingHistory {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let ingestor: SharedSessionHistoricalIngestor = history.clone();
    state.wake_history();
    let worker = tokio::spawn(run_session_temporal_refresh_scheduler(
        harness.registered.clone(),
        Arc::clone(&state),
        Arc::new(CanonicalSessionTemporalProjector),
        Arc::new(std::sync::RwLock::new(Some(ingestor))),
        Arc::new(tokio::sync::Semaphore::new(1)),
        SessionTemporalRefreshPolicy::default(),
    ));
    history.entered.notified().await;

    let handle = state.handle();
    let join = handle.wake_and_wait_for_projection(Duration::from_secs(1));
    tokio::pin!(join);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(join.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    history.release.notify_one();
    history.entered.notified().await;

    assert!(
        join.await,
        "the published wake must join while the next history window is still running"
    );
    state.cancel();
    history.release.notify_one();
    worker.await.expect("refresh worker exits on cancellation");
}

#[tokio::test]
async fn a_live_wake_publishes_before_waiting_for_summary_admission() {
    let harness = RegisteredGlobalDbHarness::open("refresh-join-before-summary-admission").await;
    let store = SessionTemporalStore::new(harness.registered.as_ref());
    let begun = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            SessionId::new("refresh-before-summary").expect("session id"),
            SessionRefreshFrontierV1::new(0, 0).expect("empty frontier"),
        ))
        .await
        .expect("running refresh");
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let handle = state.handle();
    let join = handle.wake_and_wait_for_projection(Duration::from_secs(5));
    tokio::pin!(join);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(join.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    let worker = tokio::spawn(run_session_temporal_refresh_scheduler(
        harness.registered.clone(),
        Arc::clone(&state),
        Arc::new(CanonicalSessionTemporalProjector),
        Arc::new(std::sync::RwLock::new(None)),
        Arc::new(tokio::sync::Semaphore::new(0)),
        SessionTemporalRefreshPolicy::default(),
    ));

    assert!(
        join.await,
        "summary admission must not hold a live projection"
    );
    let receipt = store
        .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
            begun.operation_id().clone(),
            begun.session_id().clone(),
        ))
        .await
        .expect("read terminal receipt")
        .expect("joining the wake must publish a terminal receipt");
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);

    state.cancel();
    worker.await.expect("refresh worker exits on cancellation");
}

#[tokio::test]
async fn a_live_wake_during_summary_admission_preserves_the_queued_turn() {
    let harness = RegisteredGlobalDbHarness::open("refresh-join-during-summary-admission").await;
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let admission = Arc::new(tokio::sync::Semaphore::new(0));
    state.wake();
    let worker = tokio::spawn(run_session_temporal_refresh_scheduler(
        harness.registered.clone(),
        Arc::clone(&state),
        Arc::new(CanonicalSessionTemporalProjector),
        Arc::new(std::sync::RwLock::new(None)),
        Arc::clone(&admission),
        SessionTemporalRefreshPolicy::default(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let idle = enabled_idle_notification(&state);
            if state.is_idle() {
                break;
            }
            idle.await;
        }
    })
    .await
    .expect("worker queues for summary admission before advertising idle");

    // Register another profile behind this worker before the live wake. A
    // projection must not surrender the existing FIFO turn and queue anew.
    let later = admission.acquire();
    tokio::pin!(later);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(later.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    let store = SessionTemporalStore::new(harness.registered.as_ref());
    let begun = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            SessionId::new("refresh-during-summary").expect("session id"),
            SessionRefreshFrontierV1::new(0, 0).expect("empty frontier"),
        ))
        .await
        .expect("running refresh");
    assert!(
        state
            .handle()
            .wake_and_wait_for_projection(Duration::from_secs(5))
            .await,
        "a new wake must publish while summary admission has no permits"
    );
    let receipt = store
        .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
            begun.operation_id().clone(),
            begun.session_id().clone(),
        ))
        .await
        .expect("read terminal receipt")
        .expect("joining the wake must publish a terminal receipt");
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert_eq!(admission.available_permits(), 0);

    admission.add_permits(1);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(later.as_mut().poll(cx)))
            .await
            .is_pending(),
        "the worker's original summary admission must retain the first turn"
    );
    state.cancel();
    worker.await.expect("refresh worker exits on cancellation");
}

struct ControlledProjector {
    reject: AtomicBool,
}

/// Fails one session while every other session uses the production projector.
struct SessionRefusingProjector {
    refused: SessionId,
}

impl SessionTemporalRefreshProjector for SessionRefusingProjector {
    fn project<'a>(
        &'a self,
        database: &'a RegisteredGlobalDbLeaseV1,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(async move {
            if recovery.session_id() == &self.refused {
                Err(SessionTemporalRefreshProjectorError::terminal(
                    "fixture_session_projection_refused",
                ))
            } else {
                CanonicalSessionTemporalProjector
                    .project(database, recovery)
                    .await
            }
        })
    }
}

#[tokio::test]
async fn named_projection_join_survives_an_unrelated_session_failure() {
    let harness = RegisteredGlobalDbHarness::open("refresh-join-session-isolation").await;
    let store = SessionTemporalStore::new(harness.registered.as_ref());
    let successful = SessionId::new("projection-accepted").expect("successful session");
    let refused = SessionId::new("projection-refused").expect("refused session");
    begin_empty_refreshes(&store, ["projection-accepted", "projection-refused"]).await;
    let recoveries = store.running_session_refreshes().await.expect("recoveries");
    assert_eq!(recoveries.len(), 2);
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let handle = state.handle();
    let successful_join =
        handle.wake_and_wait_for_session_projection(&successful, Duration::from_secs(5));
    let refused_join =
        handle.wake_and_wait_for_session_projection(&refused, Duration::from_secs(5));
    let aggregate_join = handle.wake_and_wait_for_projection(Duration::from_secs(5));
    tokio::pin!(successful_join, refused_join, aggregate_join);
    // Put all three obligations in the same pass before starting the worker.
    // The unnamed caller still owns the aggregate failure contract.
    let pending = std::future::poll_fn(|cx| {
        std::task::Poll::Ready((
            successful_join.as_mut().poll(cx).is_pending(),
            refused_join.as_mut().poll(cx).is_pending(),
            aggregate_join.as_mut().poll(cx).is_pending(),
        ))
    })
    .await;
    assert_eq!(pending, (true, true, true));
    let worker = tokio::spawn(run_session_temporal_refresh_scheduler(
        harness.registered.clone(),
        Arc::clone(&state),
        Arc::new(SessionRefusingProjector {
            refused: refused.clone(),
        }),
        Arc::new(std::sync::RwLock::new(None)),
        Arc::new(tokio::sync::Semaphore::new(0)),
        SessionTemporalRefreshPolicy::default(),
    ));

    let (successful_result, refused_result, aggregate_result) =
        tokio::join!(successful_join, refused_join, aggregate_join);
    state.cancel();
    worker.await.expect("worker exits on cancellation");
    assert!(
        successful_result,
        "another session cannot veto a committed publication"
    );
    assert!(
        !refused_result,
        "the failed session must refuse its own join"
    );
    assert!(
        !aggregate_result,
        "unnamed sweeps must retain aggregate failure"
    );
    for recovery in recoveries {
        let receipt = store
            .session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
                recovery.operation_id().clone(),
                recovery.session_id().clone(),
            ))
            .await
            .expect("read durable receipt")
            .expect("both sessions must settle durably");
        let expected = if recovery.session_id() == &successful {
            SessionRefreshTerminalStateV1::Complete
        } else {
            SessionRefreshTerminalStateV1::Failed
        };
        assert_eq!(receipt.state(), expected);
    }
}

impl SessionTemporalRefreshProjector for ControlledProjector {
    fn project<'a>(
        &'a self,
        database: &'a RegisteredGlobalDbLeaseV1,
        recovery: SessionRefreshRecoveryV1,
    ) -> SessionTemporalRefreshProjectionFuture<'a> {
        Box::pin(async move {
            if self.reject.load(Ordering::Acquire) {
                Err(SessionTemporalRefreshProjectorError::terminal(
                    "fixture_projection_refused",
                ))
            } else {
                CanonicalSessionTemporalProjector
                    .project(database, recovery)
                    .await
            }
        })
    }
}

#[tokio::test]
async fn projection_joins_keep_their_result_when_later_work_settles_differently() {
    for reject_first in [false, true] {
        let label = format!("refresh-join-result-reject-first-{reject_first}");
        let harness = RegisteredGlobalDbHarness::open(&label).await;
        let store = SessionTemporalStore::new(harness.registered.as_ref());
        begin_empty_refreshes(&store, ["first-projection"]).await;
        let state = Arc::new(SessionTemporalRefreshWakeState::default());
        let history = Arc::new(ContinuingHistory {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let ingestor: SharedSessionHistoricalIngestor = history.clone();
        let projector = Arc::new(ControlledProjector {
            reject: AtomicBool::new(reject_first),
        });
        state.wake_history();
        let worker = tokio::spawn(run_session_temporal_refresh_scheduler(
            harness.registered.clone(),
            Arc::clone(&state),
            projector.clone(),
            Arc::new(std::sync::RwLock::new(Some(ingestor))),
            Arc::new(tokio::sync::Semaphore::new(1)),
            SessionTemporalRefreshPolicy::default(),
        ));
        history.entered.notified().await;

        let handle = state.handle();
        let first = handle.wake_and_wait_for_projection(Duration::from_secs(5));
        tokio::pin!(first);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(first.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        history.release.notify_one();
        history.entered.notified().await;

        // Do not poll the first join again until a different projection has
        // settled with the opposite result. Neither result may overwrite the
        // one already delivered to the earlier request.
        projector.reject.store(!reject_first, Ordering::Release);
        begin_empty_refreshes(&store, ["second-projection"]).await;
        let second = handle.wake_and_wait_for_projection(Duration::from_secs(5));
        tokio::pin!(second);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(second.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        history.release.notify_one();
        assert_eq!(second.await, reject_first);
        assert_eq!(first.await, !reject_first);

        state.cancel();
        history.release.notify_one();
        worker.await.expect("refresh worker exits on cancellation");
    }
}

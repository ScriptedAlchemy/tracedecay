use std::sync::Arc;
use std::time::Duration;

use tracedecay_domain::{SessionId, UtcMicros};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
use tracedecay_session_temporal_store::{SessionRefreshRecoveryV1, SessionTemporalStore};
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressV1,
    SessionRefreshStore, SessionTemporalProjectionBatchV1,
};

use super::history::{
    SessionHistoricalCapacityRelease, SessionHistoricalIngestOutcome, SessionHistoricalIngestPass,
    SessionHistoricalIngestor, SessionHistoricalPass, SharedSessionHistoricalIngestor,
};
use super::projector::{
    CanonicalSessionTemporalProjector, SessionTemporalRefreshEffect, SessionTemporalRefreshPolicy,
    SessionTemporalRefreshProjectionFuture, SessionTemporalRefreshProjector, zero_refresh_coverage,
};
use super::registry::SessionTemporalRefreshPassReport;
use super::wake::{SessionTemporalRefreshRetryClass, SessionTemporalRefreshWakeState};
use super::worker::{
    apply_refresh_effect, run_session_temporal_refresh_pass, run_session_temporal_refresh_scheduler,
};
use tracedecay_sessions::ProviderPassUnits;
use tracedecay_sessions::serving::{
    SessionHistoryProgressState, SessionProjectionServingStatusPort,
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
            SessionHistoricalIngestOutcome::Complete.into()
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
    let join = handle.wake_and_wait_until_idle(Duration::from_secs(5));
    tokio::pin!(join);
    // The first poll records the pass count and delivers the wake.
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

/// Replays scripted passes, each with its own provider unit tallies.
struct TalliedHistory {
    passes: std::sync::Mutex<std::collections::VecDeque<SessionHistoricalPass>>,
}

impl SessionHistoricalIngestor for TalliedHistory {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        let pass = self
            .passes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| SessionHistoricalIngestOutcome::Complete.into());
        Box::pin(async move { pass })
    }

    fn capacity_release(&self) -> SessionHistoricalCapacityRelease {
        SessionHistoricalCapacityRelease::new(Vec::new())
    }

    fn cancel(&self) {}
}

/// Status reads catch-up progress from the worker that ran the passes: each
/// provider's dispositions sum across passes, and a pass that commits work
/// keeps the stall count at zero.
#[tokio::test]
async fn the_worker_reports_provider_units_summed_over_its_history_passes() {
    let harness = RegisteredGlobalDbHarness::open("refresh-history-progress").await;
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    let units = |units_projected, units_parked, messages_upserted| ProviderPassUnits {
        units_projected,
        units_parked,
        messages_upserted,
        ..ProviderPassUnits::default()
    };
    let history = Arc::new(TalliedHistory {
        passes: std::sync::Mutex::new(
            [
                SessionHistoricalPass {
                    outcome: SessionHistoricalIngestOutcome::Pending {
                        made_progress: true,
                    },
                    provider_units: [("codex", units(1, 0, 3)), ("cursor", units(0, 1, 0))].into(),
                },
                SessionHistoricalPass {
                    outcome: SessionHistoricalIngestOutcome::Complete,
                    provider_units: [("cursor", units(1, 0, 2))].into(),
                },
            ]
            .into(),
        ),
    });
    let ingestor: SharedSessionHistoricalIngestor = history;
    let handle = state.handle();
    assert_eq!(
        handle
            .serving_status()
            .history
            .expect("a mounted worker reports catch-up progress")
            .state(),
        SessionHistoryProgressState::AwaitingFirstPass
    );
    state.wake_history();
    let worker = tokio::spawn(run_session_temporal_refresh_scheduler(
        harness.registered.clone(),
        Arc::clone(&state),
        Arc::new(CanonicalSessionTemporalProjector),
        Arc::new(std::sync::RwLock::new(Some(ingestor))),
        Arc::new(tokio::sync::Semaphore::new(1)),
        SessionTemporalRefreshPolicy::default(),
    ));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while handle
        .serving_status()
        .history
        .is_none_or(|history| history.passes < 2)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the worker must record both history passes, saw {:?}",
            handle.serving_status().history
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let progress = handle
        .serving_status()
        .history
        .expect("a mounted worker reports catch-up progress");
    assert_eq!(progress.passes, 2);
    assert_eq!(progress.passes_without_progress, 0);
    assert!(progress.last_progress_at_unix_micros.is_some());
    assert_eq!(progress.state(), SessionHistoryProgressState::Advancing);
    assert_eq!(
        progress.provider_units.into_iter().collect::<Vec<_>>(),
        vec![("codex", units(1, 0, 3)), ("cursor", units(1, 1, 2))]
    );
    state.cancel();
    worker.await.expect("refresh worker exits on cancellation");
}

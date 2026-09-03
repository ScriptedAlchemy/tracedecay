use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::TempDir;

use super::SessionTemporalRefreshTestAuthority;
use tracedecay_session_runtime::session_temporal_refresh_scheduler::history::{
    SessionHistoricalIngestOutcome, SessionHistoricalIngestPass, SessionHistoricalIngestor,
};
use tracedecay_session_runtime::session_temporal_refresh_scheduler::registry::SessionTemporalRefreshSchedulerRegistry;

use crate::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::serving::{
    SessionProjectionServingState, SessionProjectionServingStatusPort, SessionProjectionStaleReason,
};

struct ScriptedHistoricalIngestor {
    outcomes: std::sync::Mutex<VecDeque<SessionHistoricalIngestOutcome>>,
    passes: AtomicUsize,
}

impl ScriptedHistoricalIngestor {
    fn new(outcomes: impl IntoIterator<Item = SessionHistoricalIngestOutcome>) -> Self {
        Self {
            outcomes: std::sync::Mutex::new(outcomes.into_iter().collect()),
            passes: AtomicUsize::new(0),
        }
    }
}

impl SessionHistoricalIngestor for ScriptedHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            self.passes.fetch_add(1, Ordering::AcqRel);
            self.outcomes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or(SessionHistoricalIngestOutcome::Complete)
        })
    }

    fn cancel(&self) {}
}

struct CancelAwareHistoricalIngestor {
    entered: AtomicBool,
    exited: AtomicBool,
    cancelled: AtomicBool,
    wake: tokio::sync::Notify,
}

struct RetryThenBlockHistoricalIngestor {
    passes: AtomicUsize,
    cancelled: AtomicBool,
    wake: tokio::sync::Notify,
}

struct BlockThirdHistoricalIngestor {
    passes: AtomicUsize,
    third_entered: AtomicBool,
    release_third: tokio::sync::Notify,
}

struct PanicOnceHistoricalIngestor {
    passes: AtomicUsize,
}

impl SessionHistoricalIngestor for PanicOnceHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            assert!(
                self.passes.fetch_add(1, Ordering::AcqRel) != 0,
                "historical ingest panic fixture"
            );
            SessionHistoricalIngestOutcome::Complete
        })
    }

    fn cancel(&self) {}
}

impl RetryThenBlockHistoricalIngestor {
    fn new() -> Self {
        Self {
            passes: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
        }
    }
}

impl BlockThirdHistoricalIngestor {
    fn new() -> Self {
        Self {
            passes: AtomicUsize::new(0),
            third_entered: AtomicBool::new(false),
            release_third: tokio::sync::Notify::new(),
        }
    }
}

impl SessionHistoricalIngestor for BlockThirdHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            match self.passes.fetch_add(1, Ordering::AcqRel) {
                0 => SessionHistoricalIngestOutcome::Complete,
                1 => SessionHistoricalIngestOutcome::Pending {
                    made_progress: false,
                },
                _ => {
                    self.third_entered.store(true, Ordering::Release);
                    self.release_third.notified().await;
                    SessionHistoricalIngestOutcome::Blocked {
                        reason_code: "fixture_complete",
                    }
                }
            }
        })
    }

    fn cancel(&self) {
        self.release_third.notify_waiters();
    }
}

impl SessionHistoricalIngestor for RetryThenBlockHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            let pass = self.passes.fetch_add(1, Ordering::AcqRel);
            if pass == 0 {
                return SessionHistoricalIngestOutcome::Retryable {
                    reason_code: "provider_busy",
                    made_progress: false,
                };
            }
            while !self.cancelled.load(Ordering::Acquire) {
                self.wake.notified().await;
            }
            SessionHistoricalIngestOutcome::Cancelled
        })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }
}

impl CancelAwareHistoricalIngestor {
    fn new() -> Self {
        Self {
            entered: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
        }
    }
}

impl SessionHistoricalIngestor for CancelAwareHistoricalIngestor {
    fn run_pass(
        &self,
    ) -> Pin<Box<dyn Future<Output = SessionHistoricalIngestOutcome> + Send + '_>> {
        Box::pin(async move {
            self.entered.store(true, Ordering::Release);
            while !self.cancelled.load(Ordering::Acquire) {
                self.wake.notified().await;
            }
            self.exited.store(true, Ordering::Release);
            SessionHistoricalIngestOutcome::Cancelled
        })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }
}

async fn profile_authority(temp: &TempDir, label: &str) -> SessionTemporalRefreshTestAuthority {
    let runtime = HostAdmissionTestRuntimeV1::profile(temp.path().join(label))
        .await
        .unwrap();
    runtime
        .into_session_temporal_refresh_test_authority(HostAdmissionScope::Profile)
        .expect("registered profile session authority")
}

async fn wait_until(predicate: impl Fn() -> bool, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while !predicate() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::task::yield_now().await;
    }
    true
}

#[tokio::test]
async fn retained_history_worker_wakes_again_after_idle() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-rewake").await;
    let ingestor = Arc::new(ScriptedHistoricalIngestor::new([
        SessionHistoricalIngestOutcome::Complete,
        SessionHistoricalIngestOutcome::Complete,
    ]));
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    let first = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );
    assert_eq!(ingestor.passes.load(Ordering::Acquire), 1);

    let second = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(first.same_route(&second));
    assert!(
        wait_until(
            || ingestor.passes.load(Ordering::Acquire) == 2,
            Duration::from_secs(2),
        )
        .await
    );
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn history_only_retry_does_not_admit_projection_snapshots() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-only-retry").await;
    let ingestor = Arc::new(BlockThirdHistoricalIngestor::new());
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );
    let baseline_admissions = authority
        .database()
        .read_connection()
        .reader_pool_occupancy()
        .expect("registered reader pool")
        .snapshot_admissions;

    registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        wait_until(
            || ingestor.third_entered.load(Ordering::Acquire),
            Duration::from_secs(2),
        )
        .await
    );
    let after_explicit_wake = authority
        .database()
        .read_connection()
        .reader_pool_occupancy()
        .expect("registered reader pool")
        .snapshot_admissions;
    assert!(
        after_explicit_wake > baseline_admissions,
        "the explicit ensure wake must retain its projection discovery",
    );
    ingestor.release_third.notify_waiters();
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );
    assert_eq!(
        authority
            .database()
            .read_connection()
            .reader_pool_occupancy()
            .expect("registered reader pool")
            .snapshot_admissions,
        after_explicit_wake,
        "history-only no-progress retries must not rediscover temporal projections",
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn profile_history_has_one_retained_owner() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-single-owner").await;
    let ingestor = Arc::new(ScriptedHistoricalIngestor::new([
        SessionHistoricalIngestOutcome::Complete,
        SessionHistoricalIngestOutcome::Complete,
    ]));
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    let first = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    let second = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor,
        )
        .await;

    assert!(first.same_route(&second));
    assert_eq!(registry.profile_worker_count().await, 1);
    registry.shutdown().await;
}

#[tokio::test]
async fn worker_restart_retains_historical_ingest_owner() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-worker-restart").await;
    let ingestor = Arc::new(PanicOnceHistoricalIngestor {
        passes: AtomicUsize::new(0),
    });
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;

    assert!(
        wait_until(
            || ingestor.passes.load(Ordering::Acquire) == 2,
            Duration::from_secs(2),
        )
        .await
    );
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn shutdown_cancels_and_joins_in_flight_history_pass() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-shutdown").await;
    let ingestor = Arc::new(CancelAwareHistoricalIngestor::new());
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        wait_until(
            || ingestor.entered.load(Ordering::Acquire),
            Duration::from_secs(2),
        )
        .await
    );

    tokio::time::timeout(Duration::from_secs(2), registry.shutdown())
        .await
        .expect("retained history shutdown should join");
    assert!(ingestor.cancelled.load(Ordering::Acquire));
    assert!(ingestor.exited.load(Ordering::Acquire));
}

/// A saturated daemon-wide historical-ingest admission defers the pass as
/// typed retryable state — the ingestor never runs, the worker keeps cycling
/// (serving stays available) — and the deferred pass runs once a permit
/// frees, so a huge backlog on other stores cannot wedge this one.
#[tokio::test]
async fn saturated_history_admission_defers_the_pass_and_resumes() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-admission-saturated").await;
    let ingestor = Arc::new(ScriptedHistoricalIngestor::new([
        SessionHistoricalIngestOutcome::Complete,
    ]));
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let admission = registry.historical_ingest_admission();
    let permits = u32::try_from(admission.available_permits()).unwrap();
    assert!(permits > 0, "the historical ingest admission is bounded");
    let held = admission
        .try_acquire_many(permits)
        .expect("occupy every historical ingest permit");

    let wake = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;

    let saturated = SessionProjectionServingState::Stale {
        reason: SessionProjectionStaleReason::HistoricalRetry {
            reason_code: "history_admission_saturated".to_owned(),
        },
    };
    assert!(
        wait_until(
            || wake.serving_status().state == saturated,
            Duration::from_secs(2),
        )
        .await,
        "the deferred pass must surface as typed retryable staleness"
    );
    assert_eq!(
        ingestor.passes.load(Ordering::Acquire),
        0,
        "a saturated admission must not run the historical pass"
    );
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await,
        "the worker keeps serving between deferred history retries"
    );
    assert_eq!(ingestor.passes.load(Ordering::Acquire), 0);

    drop(held);
    assert!(
        wait_until(
            || ingestor.passes.load(Ordering::Acquire) >= 1,
            Duration::from_secs(2),
        )
        .await,
        "the deferred pass must run once a permit frees"
    );
    assert!(
        registry
            .wait_profile_idle(authority.database().db_path(), Duration::from_secs(2))
            .await
    );

    registry.shutdown().await;
}

#[tokio::test]
async fn retrying_history_is_typed_stale() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-stale-status").await;
    let ingestor = Arc::new(RetryThenBlockHistoricalIngestor::new());
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    let wake = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            authority.database.clone(),
            ingestor.clone(),
        )
        .await;
    assert!(
        wait_until(
            || ingestor.passes.load(Ordering::Acquire) >= 2,
            Duration::from_secs(2),
        )
        .await
    );

    let status = wake.serving_status();
    assert_eq!(
        status.state,
        SessionProjectionServingState::Stale {
            reason: SessionProjectionStaleReason::HistoricalRetry {
                reason_code: "provider_busy".to_owned(),
            },
        }
    );

    registry.shutdown().await;
}

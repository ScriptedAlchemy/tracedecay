use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::TempDir;

use super::SessionTemporalRefreshTestAuthority;
use super::history::{
    SessionHistoricalIngestOutcome, SessionHistoricalIngestPass, SessionHistoricalIngestor,
};
use super::registry::SessionTemporalRefreshSchedulerRegistry;
use crate::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_usecases::host_admission::HostAdmissionScope;
use tracedecay_usecases::session::{
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
            Arc::clone(&authority.database),
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
            Arc::clone(&authority.database),
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
            Arc::clone(&authority.database),
            ingestor.clone(),
        )
        .await;
    let second = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            Arc::clone(&authority.database),
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
            Arc::clone(&authority.database),
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
            Arc::clone(&authority.database),
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

#[tokio::test]
async fn retrying_history_is_typed_stale() {
    let temp = TempDir::new().unwrap();
    let authority = profile_authority(&temp, "history-stale-status").await;
    let ingestor = Arc::new(RetryThenBlockHistoricalIngestor::new());
    let registry = SessionTemporalRefreshSchedulerRegistry::default();

    let wake = registry
        .ensure_profile_with_history(
            authority.database().db_path().to_path_buf(),
            Arc::clone(&authority.database),
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

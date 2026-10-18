//! Constructor-injected seams for root-owned types this crate must not name.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_runtime_core::native_runtime;

/// Watcher knobs the git-metadata watcher needs from resolved sync config.
///
/// Root maps `tracedecay_configuration::SyncConfig` into this type at construction.
/// The usecases `SyncConfig` is a different, smaller PR-autotrack type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitWatchSyncConfigV1 {
    pub auto_watch: bool,
    pub watch_linked_worktrees: bool,
    pub watch_debounce_ms: u64,
    pub watch_max_delay_ms: u64,
    pub watch_max_projects: usize,
    pub backstop_interval_mins: u64,
}

impl Default for GitWatchSyncConfigV1 {
    fn default() -> Self {
        Self {
            auto_watch: false,
            watch_linked_worktrees: false,
            watch_debounce_ms: 2000,
            watch_max_delay_ms: 30_000,
            watch_max_projects: 32,
            backstop_interval_mins: 15,
        }
    }
}

/// Wake handle for the daemon maintenance owner.
///
/// Git watch only calls [`Self::wake`]. Root wraps
/// `MaintenanceCoordinator::wake` at construction.
#[derive(Clone)]
pub struct GitWatchMaintenanceWakeV1 {
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl GitWatchMaintenanceWakeV1 {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            wake: Arc::new(wake),
        }
    }

    pub fn wake(&self) {
        (self.wake)();
    }
}

impl Default for GitWatchMaintenanceWakeV1 {
    fn default() -> Self {
        Self::new(|| {})
    }
}

/// Connection-admission lease the scheduler parks behind on blocking work.
pub trait AdmissionParkLeaseV1: Send + Sync {
    fn release(&self) -> bool;
    fn reacquire(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

tokio::task_local! {
    pub static CONNECTION_ADMISSION: Arc<dyn AdmissionParkLeaseV1>;
}

/// How long a park may keep its admission permit before surrendering it.
///
/// A request that finishes inside this grace never touches the semaphore.
/// Only a request that is genuinely parked, waiting on a project open, on the
/// writer gate, or on a single-flight generation decode, gives its slot back.
pub const ADMISSION_PARK_GRACE: Duration = Duration::from_millis(50);

/// Park a future without holding a connection admission slot across a long wait.
///
/// Reads [`CONNECTION_ADMISSION`]. Outside a connection scope (tests, background
/// reconcile, reserved-control clients) this is a transparent passthrough.
/// Daemon callers install that task-local beside their concrete lease; this
/// function is the only park algorithm.
#[tracing::instrument(name = "daemon.engine.admission.park", level = "trace", skip_all)]
pub async fn park_admission<F>(future: F) -> F::Output
where
    F: Future,
{
    let mut future = std::pin::pin!(future);
    if let Ok(output) = native_runtime::timeout(ADMISSION_PARK_GRACE, &mut future).await {
        return output;
    }
    let Ok(lease) = CONNECTION_ADMISSION.try_with(Arc::clone) else {
        return future.await;
    };
    if !lease.release() {
        return future.await;
    }
    let output = future.await;
    lease.reacquire().await;
    output
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct RecordingLease {
        released: AtomicUsize,
        reacquired: AtomicUsize,
        on_release: tokio::sync::Notify,
    }

    impl AdmissionParkLeaseV1 for RecordingLease {
        fn release(&self) -> bool {
            self.released.fetch_add(1, Ordering::SeqCst);
            self.on_release.notify_one();
            true
        }

        fn reacquire(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async {
                self.reacquired.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    #[test]
    fn a_park_off_tokio_keeps_its_slot_through_the_grace_and_returns_it_past_it() {
        let lease = Arc::new(RecordingLease::default());
        let scoped: Arc<dyn AdmissionParkLeaseV1> = lease.clone();
        let (quick, parked) = native_runtime::block_on(CONNECTION_ADMISSION.scope(scoped, async {
            let quick = park_admission(async { lease.released.load(Ordering::SeqCst) }).await;
            let parked = native_runtime::timeout(
                Duration::from_secs(10),
                park_admission(async {
                    lease.on_release.notified().await;
                    lease.reacquired.load(Ordering::SeqCst)
                }),
            )
            .await;
            (quick, parked)
        }))
        .expect("process runtime");

        assert_eq!(
            quick, 0,
            "a park inside the grace never gives its slot back"
        );
        assert_eq!(
            parked,
            Ok(0),
            "a park past the grace gives its slot back while it waits"
        );
        assert_eq!(lease.released.load(Ordering::SeqCst), 1);
        assert_eq!(
            lease.reacquired.load(Ordering::SeqCst),
            1,
            "the slot is taken back before the park returns"
        );
    }
}

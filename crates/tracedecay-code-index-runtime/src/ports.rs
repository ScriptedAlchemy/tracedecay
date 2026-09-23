//! Constructor-injected seams for root-owned types this crate must not name.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::time::{Duration, timeout};

/// Watcher knobs the git-metadata watcher needs from resolved sync config.
///
/// Root maps `tracedecay::config::SyncConfig` into this type at construction.
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
#[hotpath::measure(label = "daemon.engine.admission.park", future = true)]
pub async fn park_admission<F>(future: F) -> F::Output
where
    F: Future,
{
    let mut future = std::pin::pin!(future);
    if let Ok(output) = timeout(ADMISSION_PARK_GRACE, &mut future).await {
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

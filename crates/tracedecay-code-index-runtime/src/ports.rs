//! Constructor-injected seams for root-owned types this crate must not name.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use tokio::time::{Duration, timeout};
use tracedecay_application::ResolvedScope;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_query::retrieval::QueryAuthorityV1;
use tracedecay_tool_catalog::CatalogSnapshotV1;

/// Scheduler-facing view of a prepared query activation.
pub struct PreparedQueryActivationViewV1 {
    pub scope: ResolvedScope,
    pub configuration_revision: ConfigurationRevisionId,
    pub query_authority: Arc<QueryAuthorityV1>,
}

impl PreparedQueryActivationViewV1 {
    pub fn scope(&self) -> &ResolvedScope {
        &self.scope
    }

    pub fn configuration_revision(&self) -> &ConfigurationRevisionId {
        &self.configuration_revision
    }

    pub fn query_authority(&self) -> &Arc<QueryAuthorityV1> {
        &self.query_authority
    }
}

/// Commit port for a prepared query activation. Root's provider implements this.
pub trait QueryActivationCommitPortV1: Send + Sync {
    fn commit_prepared_activation(
        &self,
        prepared: &PreparedQueryActivationViewV1,
    ) -> Result<(), String>;
}

/// Watcher knobs the git-metadata watcher needs from resolved sync config.
///
/// Root maps `tracedecay::config::SyncConfig` into this type at construction.
/// The usecases `SyncConfig` is a different, smaller PR-autotrack type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitWatchSyncConfigV1 {
    pub auto_watch: bool,
    pub watch_debounce_ms: u64,
    pub watch_max_delay_ms: u64,
    pub watch_max_projects: usize,
    pub backstop_interval_mins: u64,
}

impl Default for GitWatchSyncConfigV1 {
    fn default() -> Self {
        Self {
            auto_watch: false,
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

/// Catalog snapshot the git-transaction owner consults for capability manifests.
pub type ApplicationCatalogSnapshotFn =
    fn() -> Result<CatalogSnapshotV1, ApplicationCatalogSnapshotErrorV1>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationCatalogSnapshotErrorV1 {
    pub message: String,
}

impl ApplicationCatalogSnapshotErrorV1 {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

static APPLICATION_CATALOG_SNAPSHOT: OnceLock<ApplicationCatalogSnapshotFn> = OnceLock::new();

/// Register the root catalog composer. Idempotent: the first install wins.
pub fn install_application_catalog_snapshot(composer: ApplicationCatalogSnapshotFn) {
    let _ = APPLICATION_CATALOG_SNAPSHOT.set(composer);
}

pub(crate) fn build_application_catalog_snapshot()
-> Result<CatalogSnapshotV1, ApplicationCatalogSnapshotErrorV1> {
    let Some(composer) = APPLICATION_CATALOG_SNAPSHOT.get().copied() else {
        return Err(ApplicationCatalogSnapshotErrorV1::new(
            "application catalog snapshot composer is not installed",
        ));
    };
    composer()
}

/// Connection-admission lease the scheduler parks behind on blocking work.
pub trait AdmissionParkLeaseV1: Send + Sync {
    fn release(&self) -> bool;
    fn reacquire(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

tokio::task_local! {
    pub static CONNECTION_ADMISSION: Arc<dyn AdmissionParkLeaseV1>;
}

/// Same grace the daemon admission park uses: 50ms before surrendering a slot.
pub const ADMISSION_PARK_GRACE: Duration = Duration::from_millis(50);

/// Park a future the way daemon connection admission does.
///
/// Outside a connection scope (tests, background reconcile) this is a
/// transparent passthrough — matching the pre-extract helper.
#[hotpath::measure(label = "code_index_runtime.admission.park", future = true)]
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

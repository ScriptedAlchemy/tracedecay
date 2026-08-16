use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport};

#[cfg(any(unix, test))]
use super::ProjectServerKey;
use super::StoreOwnerKey;
use super::git_transactions::DaemonGitIndexTransactionServiceRegistry;
use super::profile_host_admission_replay::{
    ProfileHostAdmissionBootstrapOperation, ProfileHostAdmissionBootstrapStatus,
    ProfileHostAdmissionReplayRegistry,
};
#[cfg(unix)]
use super::scheduler::{AutomationSchedulerHandle, MaintenanceTaskTermination};
use super::session_temporal_refresh_scheduler::SessionTemporalRefreshSchedulerRegistry;
use super::store_writer_gate::StoreWriterGates;
pub(super) use super::store_writer_gate::{StoreWriterClass, WriterScope};
use super::{DaemonHandshake, DatabaseOwnerRegistry, authority, write_json_rpc_response};

const BRANCH_ADMIN_TOOL_NAME: &str = "tracedecay_admin_branch";
const DEFAULT_RETAINED_PROFILE_RUNTIME_CAPACITY: usize = 8;
mod project_retirement;
mod remote_deletion_lifecycle;
pub(in crate::daemon) mod remote_recovery_lifecycle;
mod session_runtime_shutdown;

/// Resolves the writer scope for one store family.
///
/// The key is the canonical `data_root` — the exact value
/// [`StoreOwnerKey::store_root`](super::StoreOwnerKey) carries — so every lane
/// naming the same store lands on the same gate. A path that cannot be
/// canonicalized degrades to daemon-wide, which is strictly *more* exclusive and
/// therefore can never split one store's gate into two.
pub(super) fn store_writer_scope(data_root: &Path, class: StoreWriterClass) -> WriterScope {
    match authority::canonical_identity_path(data_root) {
        Ok(canonical) => WriterScope::store(canonical, class),
        Err(_) => WriterScope::Daemon,
    }
}

/// Owner-lane scope for one registered database owner.
///
/// [`StoreOwnerKey::store_root`](super::StoreOwnerKey) is already the
/// canonicalized `data_root`, so this agrees by construction with
/// [`store_writer_scope`] and [`graph_writer_scope`] for the same store.
#[cfg(any(unix, test))]
pub(super) fn owner_writer_scope(key: &ProjectServerKey) -> WriterScope {
    WriterScope::store(key.owner.store_root.clone(), StoreWriterClass::Owner)
}

/// [`store_writer_scope`] for the store an open graph is serving.
pub(super) fn graph_writer_scope(
    cg: &crate::tracedecay::TraceDecay,
    class: StoreWriterClass,
) -> WriterScope {
    store_writer_scope(&cg.store_layout().data_root, class)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum MaintenanceReaperKind {
    Automation,
}

#[cfg(unix)]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MaintenanceReaperKey {
    kind: MaintenanceReaperKind,
    owner: ProjectServerKey,
    generation: u64,
}

#[cfg(unix)]
struct MaintenanceReaperHandle {
    retired_task: tokio::task::AbortHandle,
    termination: Arc<MaintenanceTaskTermination>,
    _task: tokio::task::JoinHandle<()>,
}

#[cfg(unix)]
struct MaintenanceReaperRegistryState {
    accepting: bool,
    pending: HashMap<StoreOwnerKey, usize>,
    next_generation: u64,
    reapers: HashMap<MaintenanceReaperKey, MaintenanceReaperHandle>,
}

#[cfg(unix)]
struct MaintenanceReaperRegistry {
    state: std::sync::Mutex<MaintenanceReaperRegistryState>,
    changed: tokio::sync::Notify,
    #[cfg(test)]
    registration_barrier: std::sync::Mutex<Option<Arc<RetirementReaperRegistrationBarrier>>>,
    #[cfg(test)]
    shutdown_passes: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    shutdown_changed: tokio::sync::Notify,
}

#[cfg(unix)]
impl Default for MaintenanceReaperRegistry {
    fn default() -> Self {
        Self {
            state: std::sync::Mutex::new(MaintenanceReaperRegistryState {
                accepting: true,
                pending: HashMap::new(),
                next_generation: 1,
                reapers: HashMap::new(),
            }),
            changed: tokio::sync::Notify::new(),
            #[cfg(test)]
            registration_barrier: std::sync::Mutex::new(None),
            #[cfg(test)]
            shutdown_passes: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            shutdown_changed: tokio::sync::Notify::new(),
        }
    }
}

#[cfg(unix)]
impl MaintenanceReaperRegistry {
    fn state(&self) -> std::sync::MutexGuard<'_, MaintenanceReaperRegistryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn reserve(self: &Arc<Self>, owner: &ProjectServerKey) -> Option<MaintenanceReaperReservation> {
        let mut state = self.state();
        if !state.accepting {
            return None;
        }
        *state.pending.entry(owner.owner.clone()).or_default() += 1;
        drop(state);
        self.changed.notify_waiters();
        Some(MaintenanceReaperReservation {
            registry: Arc::clone(self),
            owner: owner.owner.clone(),
            active: true,
        })
    }

    fn release_reservation(&self, owner: &StoreOwnerKey) {
        let mut state = self.state();
        let mut remove = false;
        if let Some(pending) = state.pending.get_mut(owner) {
            debug_assert!(*pending > 0);
            *pending = pending.saturating_sub(1);
            remove = *pending == 0;
        }
        if remove {
            state.pending.remove(owner);
        }
        drop(state);
        self.changed.notify_waiters();
    }

    fn next_key(
        state: &mut MaintenanceReaperRegistryState,
        kind: MaintenanceReaperKind,
        owner: &ProjectServerKey,
    ) -> MaintenanceReaperKey {
        loop {
            let generation = state.next_generation;
            state.next_generation = state.next_generation.wrapping_add(1).max(1);
            let key = MaintenanceReaperKey {
                kind,
                owner: owner.clone(),
                generation,
            };
            if !state.reapers.contains_key(&key) {
                return key;
            }
        }
    }

    fn finish(&self, key: &MaintenanceReaperKey) {
        self.state().reapers.remove(key);
        self.changed.notify_waiters();
    }
}

#[cfg(unix)]
pub(super) struct MaintenanceReaperReservation {
    registry: Arc<MaintenanceReaperRegistry>,
    owner: StoreOwnerKey,
    active: bool,
}

#[cfg(unix)]
impl Drop for MaintenanceReaperReservation {
    fn drop(&mut self) {
        if self.active {
            self.registry.release_reservation(&self.owner);
        }
    }
}

#[cfg(unix)]
struct MaintenanceReaperFinalizer {
    registry: Arc<MaintenanceReaperRegistry>,
    key: MaintenanceReaperKey,
    termination: Arc<MaintenanceTaskTermination>,
}

#[cfg(unix)]
impl Drop for MaintenanceReaperFinalizer {
    fn drop(&mut self) {
        self.termination.finish();
        self.registry.finish(&self.key);
    }
}

#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))] // exercised only by unix-only daemon tests
pub(super) struct RetirementReaperRegistrationBarrier {
    reached: tokio::sync::watch::Sender<bool>,
    released: std::sync::Mutex<bool>,
    released_changed: std::sync::Condvar,
}

#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))] // exercised only by unix-only daemon tests
impl RetirementReaperRegistrationBarrier {
    fn new() -> Self {
        let (reached, _) = tokio::sync::watch::channel(false);
        Self {
            reached,
            released: std::sync::Mutex::new(false),
            released_changed: std::sync::Condvar::new(),
        }
    }

    fn block(&self) {
        self.reached.send_replace(true);
        let mut released = self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*released {
            released = self
                .released_changed
                .wait(released)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub(super) async fn wait_until_reached(&self) {
        let mut reached = self.reached.subscribe();
        while !*reached.borrow_and_update() {
            if reached.changed().await.is_err() {
                return;
            }
        }
    }

    pub(super) fn release(&self) {
        *self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.released_changed.notify_all();
    }
}

#[cfg(test)]
impl Drop for RetirementReaperRegistrationBarrier {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Clone)]
pub(super) struct SessionRuntimeRegistryEntryV1 {
    pub(super) identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    pub(super) registry: Arc<
        tokio::sync::OnceCell<
            Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>,
        >,
    >,
    pub(super) retiring: Arc<AtomicBool>,
    pub(super) opening: Arc<AtomicUsize>,
}

impl SessionRuntimeRegistryEntryV1 {
    pub(super) fn new(
        identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    ) -> Self {
        Self {
            identity,
            registry: Arc::new(tokio::sync::OnceCell::new()),
            retiring: Arc::new(AtomicBool::new(false)),
            opening: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct SessionRuntimeRegistryCapacityV1 {
    max_profiles: usize,
}

impl Default for SessionRuntimeRegistryCapacityV1 {
    fn default() -> Self {
        Self {
            max_profiles: DEFAULT_RETAINED_PROFILE_RUNTIME_CAPACITY,
        }
    }
}

impl SessionRuntimeRegistryCapacityV1 {
    #[cfg(test)]
    pub(super) fn for_test(max_profiles: usize) -> Result<Self> {
        if max_profiles == 0 {
            return Err(TraceDecayError::Config {
                message: "session runtime registry capacity must be greater than zero".to_owned(),
            });
        }
        Ok(Self { max_profiles })
    }

    pub(super) const fn max_profiles(self) -> usize {
        self.max_profiles
    }
}

type SessionRuntimeRegistries = HashMap<PathBuf, SessionRuntimeRegistryEntryV1>;
pub(super) type SharedSessionRuntimeRegistries = Arc<tokio::sync::Mutex<SessionRuntimeRegistries>>;

#[derive(Clone)]
struct ProfileHostAdmissionBootstrapContext {
    profile_root: PathBuf,
    profile_identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    session_runtime_registry: Arc<
        tokio::sync::OnceCell<
            Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>,
        >,
    >,
    retiring: Arc<AtomicBool>,
    opening: Arc<AtomicUsize>,
    host_admission_brokers: Arc<
        tokio::sync::Mutex<
            HashMap<PathBuf, tracedecay_usecases::host_admission::SharedHostAdmissionBroker>,
        >,
    >,
    host_admission_broker_gate: Arc<tokio::sync::Mutex<()>>,
    profile_host_admission_replay: Weak<ProfileHostAdmissionReplayRegistry>,
}

impl ProfileHostAdmissionBootstrapContext {
    async fn ensure(&self) -> Result<()> {
        if self.retiring.load(Ordering::Acquire) {
            return Err(TraceDecayError::Config {
                message: "session runtime registry is retiring under bounded capacity".to_owned(),
            });
        }
        let profile_identity = self.profile_identity.clone();
        self.opening.fetch_add(1, Ordering::AcqRel);
        let retiring = Arc::clone(&self.retiring);
        let session_runtime_registry = self
            .session_runtime_registry
            .get_or_try_init(|| async move {
                if retiring.load(Ordering::Acquire) {
                    return Err(TraceDecayError::Config {
                        message: "session runtime registry is retiring under bounded capacity"
                            .to_owned(),
                    });
                }
                crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                    profile_identity,
                )
                .await
                .map(Arc::new)
            })
            .await;
        self.opening.fetch_sub(1, Ordering::AcqRel);
        let session_runtime_registry =
            session_runtime_registry.map(Arc::clone).map_err(|error| {
                TraceDecayError::project_route(
                    "registered_authority_unavailable",
                    true,
                    error.to_string(),
                )
            })?;
        let user_session_db =
            session_runtime_registry
                .profile_sessions()
                .await
                .map_err(|error| {
                    TraceDecayError::project_route(
                        "registered_authority_unavailable",
                        true,
                        error.to_string(),
                    )
                })?;
        let broker_path =
            authority::canonical_identity_path(user_session_db.db_path()).map_err(|error| {
                TraceDecayError::project_route(
                    "host_admission_broker_unavailable",
                    true,
                    error.to_string(),
                )
            })?;
        let broker = self.open_broker(&broker_path).await?;
        let Some(replay) = self.profile_host_admission_replay.upgrade() else {
            return Err(TraceDecayError::project_route(
                "daemon_shutting_down",
                false,
                "profile host-admission replay registry is unavailable",
            ));
        };
        replay
            .ensure(&broker_path, &self.profile_root, &broker)
            .await;
        Ok(())
    }

    async fn open_broker(
        &self,
        path: &Path,
    ) -> Result<tracedecay_usecases::host_admission::SharedHostAdmissionBroker> {
        if let Some(broker) = self.host_admission_brokers.lock().await.get(path).cloned() {
            return Ok(broker);
        }

        let _open = self.host_admission_broker_gate.lock().await;
        let brokers = self.host_admission_brokers.lock().await;
        if let Some(broker) = brokers.get(path) {
            return Ok(Arc::clone(broker));
        }
        drop(brokers);
        let open_path = path.to_path_buf();
        let (runtime, _) = tokio::task::spawn_blocking(move || {
            tracedecay_usecases::host_admission::HostAdmissionRuntime::open_for_database(&open_path)
        })
        .await
        .map_err(|_| {
            TraceDecayError::project_route(
                "spool_runtime_unavailable",
                true,
                "host-admission spool runtime task failed",
            )
        })??;
        let broker =
            Arc::new(tracedecay_usecases::host_admission::HostAdmissionBroker::new(runtime));
        self.host_admission_brokers
            .lock()
            .await
            .insert(path.to_path_buf(), Arc::clone(&broker));
        Ok(broker)
    }
}

/// Coordinates every daemon operation that can create, rekey, or remove a
/// database owner. There is one copy of each shared registry so branch
/// administration cannot prove ownership against stale daemon state.
///
/// Writer admission itself is *per store* — see
/// [`store_writer_gate`](super::store_writer_gate) for the hierarchy and the
/// exclusivity argument. The proof branch administration performs is computed
/// from one store family's database paths, so a writer on another store can
/// never invalidate it; a single daemon-wide gate only meant a sync of project
/// A parked the first request for project B behind it, unbounded.
#[derive(Clone)]
pub(super) struct StoreAdministration {
    profile_identity: Option<crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
    authenticated_profile_database_scopes:
        Arc<tokio::sync::Mutex<HashMap<PathBuf, crate::db::DaemonDatabaseScope>>>,
    session_runtime_registries: SharedSessionRuntimeRegistries,
    session_runtime_registry_capacity: SessionRuntimeRegistryCapacityV1,
    session_runtime_registry_admission_closed: Arc<AtomicBool>,
    gate: Arc<StoreWriterGates>,
    project_servers: Arc<tokio::sync::Mutex<DatabaseOwnerRegistry>>,
    project_server_retirements:
        Arc<tokio::sync::Mutex<Vec<project_retirement::ProjectServerRetirement>>>,
    pub(super) retained_project_shutdown_owners:
        Arc<tokio::sync::Mutex<Vec<RetainedProjectShutdownOwner>>>,
    project_routes: crate::mcp::project_route::SharedHookProjectRouteCache,
    host_admission_brokers: Arc<
        tokio::sync::Mutex<
            HashMap<PathBuf, tracedecay_usecases::host_admission::SharedHostAdmissionBroker>,
        >,
    >,
    host_admission_broker_gate: Arc<tokio::sync::Mutex<()>>,
    profile_host_admission_replay: Arc<ProfileHostAdmissionReplayRegistry>,
    session_sync_service: Arc<crate::daemon::session_sync::DaemonSessionSyncService>,
    store_telemetry_sampling: super::maintenance::StoreTelemetrySamplingRegistry,
    #[cfg(unix)]
    automation_schedulers:
        Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, AutomationSchedulerHandle>>>,
    session_temporal_refresh_schedulers: Arc<SessionTemporalRefreshSchedulerRegistry>,
    git_index_transaction_services: Arc<DaemonGitIndexTransactionServiceRegistry>,
    native_integration_services:
        Arc<crate::daemon::native_integration::DaemonNativeIntegrationServiceRegistry>,
    remote_recovery_project_lifecycles:
        remote_recovery_lifecycle::SharedRemoteRecoveryProjectLifecyclesV1,
    #[cfg(unix)]
    retirement_reapers: Arc<MaintenanceReaperRegistry>,
}

/// Retry ownership for a timed-out server, or a terminal failure receipt that
/// must remain visible without retaining the server and its daemon callbacks.
pub(super) enum RetainedProjectShutdownOwner {
    TimedOut { server: Arc<crate::mcp::McpServer> },
    Failed { error: String },
}

impl Default for StoreAdministration {
    fn default() -> Self {
        Self {
            profile_identity: None,
            authenticated_profile_database_scopes: Arc::new(
                tokio::sync::Mutex::new(HashMap::new()),
            ),
            session_runtime_registries: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            session_runtime_registry_capacity: SessionRuntimeRegistryCapacityV1::default(),
            session_runtime_registry_admission_closed: Arc::new(AtomicBool::new(false)),
            gate: Arc::new(StoreWriterGates::default()),
            project_servers: Arc::new(tokio::sync::Mutex::new(DatabaseOwnerRegistry::default())),
            project_server_retirements: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            retained_project_shutdown_owners: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            project_routes: crate::mcp::project_route::SharedHookProjectRouteCache::default(),
            host_admission_brokers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            host_admission_broker_gate: Arc::new(tokio::sync::Mutex::new(())),
            profile_host_admission_replay: Arc::new(ProfileHostAdmissionReplayRegistry::default()),
            session_sync_service: Arc::new(
                crate::daemon::session_sync::DaemonSessionSyncService::default(),
            ),
            store_telemetry_sampling: super::maintenance::StoreTelemetrySamplingRegistry::default(),
            #[cfg(unix)]
            automation_schedulers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            session_temporal_refresh_schedulers: Arc::new(
                SessionTemporalRefreshSchedulerRegistry::default(),
            ),
            git_index_transaction_services: Arc::new(
                DaemonGitIndexTransactionServiceRegistry::default(),
            ),
            native_integration_services: Arc::new(
                crate::daemon::native_integration::DaemonNativeIntegrationServiceRegistry::default(
                ),
            ),
            remote_recovery_project_lifecycles: Default::default(),
            #[cfg(unix)]
            retirement_reapers: Arc::new(MaintenanceReaperRegistry::default()),
        }
    }
}

impl StoreAdministration {
    pub(super) fn store_telemetry_sampling(
        &self,
    ) -> super::maintenance::StoreTelemetrySamplingRegistry {
        self.store_telemetry_sampling.clone()
    }

    pub(super) fn project_routes(&self) -> crate::mcp::project_route::SharedHookProjectRouteCache {
        self.project_routes.clone()
    }

    #[cfg(unix)]
    pub(super) async fn for_retained_project_graph(
        graph: &crate::tracedecay::TraceDecay,
    ) -> Result<Self> {
        let profile_root = graph.retained_profile_root()?;
        let profile_identity = crate::daemon::profile_identity::load_or_create(&profile_root)?;
        let administration = Self::default().with_profile_identity(profile_identity.clone());
        let registry = Arc::new(tokio::sync::OnceCell::new());
        registry
            .set(graph.retained_store_runtime_registry())
            .map_err(|_| TraceDecayError::Config {
                message: "retained project runtime registry was already initialized".to_owned(),
            })?;
        administration
            .session_runtime_registries
            .lock()
            .await
            .insert(
                authority::canonical_identity_path(&profile_root)?,
                SessionRuntimeRegistryEntryV1 {
                    identity: profile_identity,
                    registry,
                    retiring: Arc::new(AtomicBool::new(false)),
                    opening: Arc::new(AtomicUsize::new(0)),
                },
            );
        Ok(administration)
    }

    pub(super) fn with_profile_identity(
        mut self,
        profile_identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    ) -> Self {
        self.profile_identity = Some(profile_identity);
        self
    }

    #[cfg(test)]
    pub(super) fn with_session_runtime_registry_capacity_for_test(
        mut self,
        capacity: SessionRuntimeRegistryCapacityV1,
    ) -> Self {
        self.session_runtime_registry_capacity = capacity;
        self
    }

    pub(super) fn profile_identity(
        &self,
    ) -> Result<&crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1> {
        self.profile_identity
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "daemon profile identity authority is unavailable".to_string(),
            })
    }

    /// Retains daemon database authority for an authenticated client profile
    /// that differs from the daemon process's startup profile.
    ///
    /// The map is shared by every `StoreAdministration` clone, so cached
    /// project/runtime owners keep the authority after the admitting socket
    /// closes. Concurrent first requests for one profile reuse exactly one
    /// process-stable election scope.
    pub(super) async fn retain_authenticated_profile_database_scope(
        &self,
        profile_root: &Path,
    ) -> Result<()> {
        let profile_root = authority::canonical_identity_path(profile_root)?;
        let mut scopes = self.authenticated_profile_database_scopes.lock().await;
        if scopes.contains_key(&profile_root) {
            return Ok(());
        }
        let scope = crate::db::enter_daemon_database_scope(
            &profile_root,
            0,
            crate::runtime_identity::process_run_id(),
        )?;
        scopes.insert(profile_root, scope);
        Ok(())
    }

    pub(super) async fn registered_profile_session_database(
        &self,
    ) -> Result<Arc<crate::global_db::RegisteredGlobalDb>> {
        self.ensure_account_active().await?;
        self.session_runtime_registry()
            .await?
            .profile_sessions()
            .await
    }

    pub(super) async fn registered_profile_database(
        &self,
    ) -> Result<Arc<crate::global_db::RegisteredGlobalDb>> {
        let database = self.raw_registered_profile_database().await?;
        let profile_id = self.profile_identity()?.profile_id().as_str();
        if database
            .remote_account_deletion_tombstone(profile_id)
            .await?
            .is_some()
        {
            return Err(TraceDecayError::project_route(
                "remote_deleted",
                false,
                "authenticated profile was remotely deleted",
            ));
        }
        Ok(database)
    }

    async fn raw_registered_profile_database(
        &self,
    ) -> Result<Arc<crate::global_db::RegisteredGlobalDb>> {
        self.session_runtime_registry()
            .await?
            .profile_database()
            .await
    }

    pub(super) async fn ensure_account_active(&self) -> Result<()> {
        let database = self.raw_registered_profile_database().await?;
        let profile_id = self.profile_identity()?.profile_id().as_str();
        if database
            .remote_account_deletion_tombstone(profile_id)
            .await?
            .is_some()
        {
            return Err(TraceDecayError::project_route(
                "remote_deleted",
                false,
                "authenticated profile was remotely deleted",
            ));
        }
        Ok(())
    }

    pub(super) async fn remote_account_deletion_tombstone(
        &self,
    ) -> Result<Option<crate::global_db::RemoteDeletionTombstone>> {
        let database = self.raw_registered_profile_database().await?;
        database
            .remote_account_deletion_tombstone(self.profile_identity()?.profile_id().as_str())
            .await
    }

    pub(super) async fn mounted_registered_session_databases(
        &self,
    ) -> Vec<Arc<crate::global_db::RegisteredGlobalDb>> {
        let Ok(profile_root) = self
            .profile_identity()
            .and_then(|identity| authority::canonical_identity_path(identity.profile_root()))
        else {
            return Vec::new();
        };
        let registry = {
            let registries = self.session_runtime_registries.lock().await;
            registries
                .get(&profile_root)
                .map(|entry| Arc::clone(&entry.registry))
        };
        let Some(registry) = registry.and_then(|registry| registry.get().cloned()) else {
            return Vec::new();
        };
        registry.mounted_session_databases().await
    }

    pub(super) async fn mounted_project_servers(&self) -> Vec<Arc<crate::mcp::McpServer>> {
        let Ok(profile_root) = self
            .profile_identity()
            .and_then(|identity| authority::canonical_identity_path(identity.profile_root()))
        else {
            return Vec::new();
        };
        let servers = {
            let servers = self.project_servers.lock().await;
            servers
                .servers
                .iter()
                .filter(|(key, _)| key.owner.profile_root == profile_root)
                .map(|(_, entry)| Arc::clone(&entry.server))
                .collect::<Vec<_>>()
        };
        servers
    }

    pub(super) async fn mounted_project_graphs(&self) -> Vec<Arc<crate::tracedecay::TraceDecay>> {
        let servers = self.mounted_project_servers().await;
        let mut graphs = Vec::with_capacity(servers.len());
        for server in &servers {
            graphs.push(server.cg().await);
        }
        graphs
    }

    pub(super) async fn registered_project_session_database(
        &self,
        project_root: &Path,
        store_layout: &crate::storage::StoreLayout,
    ) -> Result<Arc<crate::global_db::RegisteredGlobalDb>> {
        let project_id = store_layout
            .identity
            .project_id
            .as_deref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project session runtime requires an authoritative project identity"
                    .to_owned(),
            })
            .and_then(|project_id| {
                tracedecay_store::ProjectId::new(project_id.to_owned()).map_err(|error| {
                    TraceDecayError::Config {
                        message: format!(
                            "invalid authoritative project identity for session runtime: {error}"
                        ),
                    }
                })
            })?;
        let registry = self.session_runtime_registry().await?;
        let profile_database = self.registered_profile_database().await?;
        let profile_id = self.profile_identity()?.profile_id().as_str();
        if profile_database
            .remote_deletion_tombstone_for_project(profile_id, project_id.as_str())
            .await?
            .is_some()
        {
            return Err(TraceDecayError::project_route(
                "remote_deleted",
                false,
                format!("project '{}' was remotely deleted", project_id.as_str()),
            ));
        }
        // A mounted shard already carries the exact typed enrollment authority
        // that opened it. Later client routes may be linked-worktree aliases;
        // reuse by ProjectId instead of treating those paths as new authority.
        if let Some(database) = registry.mounted_project_sessions(&project_id).await {
            return Ok(database);
        }
        let enrollment_roots = crate::tracedecay::TraceDecay::registered_enrollment_roots(
            project_root,
            store_layout,
            &project_id,
            profile_database.as_ref(),
        )
        .await?;
        registry
            .project_sessions(project_id, enrollment_roots)
            .await
    }

    #[cfg(test)]
    pub(super) fn with_project_servers(
        project_servers: Arc<tokio::sync::Mutex<DatabaseOwnerRegistry>>,
    ) -> Self {
        Self {
            project_servers,
            ..Self::default()
        }
    }

    pub(super) fn project_servers(&self) -> &Arc<tokio::sync::Mutex<DatabaseOwnerRegistry>> {
        &self.project_servers
    }

    pub(super) async fn host_admission_broker(
        &self,
        database: &Arc<crate::global_db::RegisteredGlobalDb>,
    ) -> Result<tracedecay_usecases::host_admission::SharedHostAdmissionBroker> {
        let profile_id = self.profile_identity()?.profile_id().as_str();
        if database
            .remote_account_deletion_tombstone(profile_id)
            .await?
            .is_some()
        {
            return Err(TraceDecayError::project_route(
                "remote_deleted",
                false,
                "authenticated profile was remotely deleted",
            ));
        }
        self.host_admission_broker_for_path(database.db_path())
            .await
    }

    async fn host_admission_broker_for_path(
        &self,
        database_path: &Path,
    ) -> Result<tracedecay_usecases::host_admission::SharedHostAdmissionBroker> {
        let path = authority::canonical_identity_path(database_path)?;
        if let Some(broker) = self.host_admission_brokers.lock().await.get(&path).cloned() {
            self.maybe_ensure_user_profile_host_admission_replay(&path, &broker)
                .await;
            return Ok(broker);
        }

        // Serialize first-open publication without retaining the broker map
        // lock across blocking filesystem work.
        let _open = self.host_admission_broker_gate.lock().await;
        let state = {
            let brokers = self.host_admission_brokers.lock().await;
            if let Some(broker) = brokers.get(&path) {
                Arc::clone(broker)
            } else {
                drop(brokers);
                let open_path = path.clone();
                let (runtime, _) = tokio::task::spawn_blocking(move || {
                    tracedecay_usecases::host_admission::HostAdmissionRuntime::open_for_database(
                        &open_path,
                    )
                })
                .await
                .map_err(|_| {
                    TraceDecayError::project_route(
                        "spool_runtime_unavailable",
                        true,
                        "host-admission spool runtime task failed",
                    )
                })??;
                let broker = Arc::new(
                    tracedecay_usecases::host_admission::HostAdmissionBroker::new(runtime),
                );
                self.host_admission_brokers
                    .lock()
                    .await
                    .insert(path.clone(), Arc::clone(&broker));
                broker
            }
        };
        self.maybe_ensure_user_profile_host_admission_replay(&path, &state)
            .await;
        Ok(state)
    }

    /// Kick the coalesced user-profile replay worker. Never awaits a replay pass.
    pub(super) async fn ensure_user_profile_host_admission_replay(
        &self,
        profile_root: &Path,
        broker: &tracedecay_usecases::host_admission::SharedHostAdmissionBroker,
        broker_path: &Path,
    ) {
        self.profile_host_admission_replay
            .ensure(broker_path, profile_root, broker)
            .await;
    }

    pub(super) async fn ensure_profile_host_admission_bootstrap(
        &self,
        profile_root: &Path,
    ) -> Result<()> {
        if self
            .session_runtime_registry_admission_closed
            .load(Ordering::Acquire)
        {
            return Err(TraceDecayError::Config {
                message: "session runtime registry admission is closed for daemon shutdown"
                    .to_owned(),
            });
        }
        let profile_root = authority::canonical_identity_path(profile_root)?;
        let profile_identity = self.profile_identity()?.clone();
        let authority_profile_root =
            authority::canonical_identity_path(profile_identity.profile_root())?;
        if profile_root != authority_profile_root {
            return Err(TraceDecayError::Config {
                message: "profile host-admission bootstrap identity mismatch".to_owned(),
            });
        }
        let (session_runtime_registry, retiring, opening) = {
            let mut registries = self.session_runtime_registries.lock().await;
            if self
                .session_runtime_registry_admission_closed
                .load(Ordering::Acquire)
            {
                return Err(TraceDecayError::Config {
                    message: "session runtime registry admission is closed for daemon shutdown"
                        .to_owned(),
                });
            }
            let entry = registries
                .entry(profile_root.clone())
                .or_insert_with(|| SessionRuntimeRegistryEntryV1::new(profile_identity.clone()));
            (
                Arc::clone(&entry.registry),
                Arc::clone(&entry.retiring),
                Arc::clone(&entry.opening),
            )
        };
        let context = ProfileHostAdmissionBootstrapContext {
            profile_root: profile_root.clone(),
            profile_identity,
            session_runtime_registry,
            retiring,
            opening,
            host_admission_brokers: Arc::clone(&self.host_admission_brokers),
            host_admission_broker_gate: Arc::clone(&self.host_admission_broker_gate),
            profile_host_admission_replay: Arc::downgrade(&self.profile_host_admission_replay),
        };
        let operation: ProfileHostAdmissionBootstrapOperation = Arc::new(move || {
            let context = context.clone();
            Box::pin(async move { context.ensure().await })
        });
        self.profile_host_admission_replay
            .ensure_bootstrap(&profile_root, operation)
            .await;
        Ok(())
    }

    pub(super) async fn profile_host_admission_bootstrap_status(
        &self,
        profile_root: &Path,
    ) -> Result<Option<ProfileHostAdmissionBootstrapStatus>> {
        let profile_root = authority::canonical_identity_path(profile_root)?;
        let authority_profile_root =
            authority::canonical_identity_path(self.profile_identity()?.profile_root())?;
        if profile_root != authority_profile_root {
            return Err(TraceDecayError::Config {
                message: "profile host-admission bootstrap identity mismatch".to_owned(),
            });
        }
        Ok(self
            .profile_host_admission_replay
            .bootstrap_status(&profile_root)
            .await)
    }

    async fn maybe_ensure_user_profile_host_admission_replay(
        &self,
        broker_path: &Path,
        broker: &tracedecay_usecases::host_admission::SharedHostAdmissionBroker,
    ) {
        let is_user_sessions = broker_path.file_name().and_then(|name| name.to_str())
            == Some(tracedecay_sessions::runtime::USER_SESSIONS_DB_FILENAME);
        if !is_user_sessions {
            return;
        }
        let Some(profile_root) = broker_path.parent() else {
            return;
        };
        self.ensure_user_profile_host_admission_replay(profile_root, broker, broker_path)
            .await;
    }

    pub(super) async fn wait_user_profile_host_admission_replay_idle(
        &self,
        broker_path: &Path,
        timeout: std::time::Duration,
    ) -> bool {
        self.profile_host_admission_replay
            .wait_idle(broker_path, timeout)
            .await
    }

    pub(super) async fn shutdown_host_admission_replay(&self) {
        self.profile_host_admission_replay.shutdown().await;
    }

    pub(super) fn session_sync_service(
        &self,
    ) -> Arc<crate::daemon::session_sync::DaemonSessionSyncService> {
        Arc::clone(&self.session_sync_service)
    }

    pub(super) async fn shutdown_session_sync(&self) {
        tracedecay_application::session_sync::SessionSyncServicePort::shutdown(
            self.session_sync_service.as_ref(),
        )
        .await;
    }

    #[cfg(unix)]
    pub(super) fn automation_schedulers(
        &self,
    ) -> &Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, AutomationSchedulerHandle>>> {
        &self.automation_schedulers
    }

    pub(super) fn session_temporal_refresh_schedulers(
        &self,
    ) -> &Arc<SessionTemporalRefreshSchedulerRegistry> {
        &self.session_temporal_refresh_schedulers
    }

    pub(super) fn git_index_transaction_services(
        &self,
    ) -> &Arc<DaemonGitIndexTransactionServiceRegistry> {
        &self.git_index_transaction_services
    }

    pub(super) fn native_integration_services(
        &self,
    ) -> &Arc<crate::daemon::native_integration::DaemonNativeIntegrationServiceRegistry> {
        &self.native_integration_services
    }

    #[cfg(unix)]
    pub(super) fn reserve_retirement_reaper(
        &self,
        owner: &ProjectServerKey,
    ) -> Option<MaintenanceReaperReservation> {
        self.retirement_reapers.reserve(owner)
    }

    #[cfg(unix)]
    pub(super) fn spawn_retirement_reaper<F>(
        &self,
        mut reservation: MaintenanceReaperReservation,
        kind: MaintenanceReaperKind,
        owner: ProjectServerKey,
        task: tokio::task::JoinHandle<()>,
        termination: Arc<MaintenanceTaskTermination>,
        cleanup: F,
    ) where
        F: Future<Output = ()> + Send + 'static,
    {
        debug_assert!(Arc::ptr_eq(&reservation.registry, &self.retirement_reapers));
        debug_assert_eq!(reservation.owner, owner.owner);
        #[cfg(test)]
        if let Some(barrier) = self
            .retirement_reapers
            .registration_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            barrier.block();
        }

        let retired_task = task.abort_handle();
        task.abort();
        let mut state = self.retirement_reapers.state();
        let key = MaintenanceReaperRegistry::next_key(&mut state, kind, &owner);
        let finalizer = MaintenanceReaperFinalizer {
            registry: Arc::clone(&self.retirement_reapers),
            key: key.clone(),
            termination: Arc::clone(&termination),
        };
        let (start, registered) = tokio::sync::oneshot::channel();
        let reaper = tokio::spawn(async move {
            let _finalizer = finalizer;
            let _ = registered.await;
            let _ = task.await;
            cleanup.await;
        });
        let replaced = state.reapers.insert(
            key,
            MaintenanceReaperHandle {
                retired_task,
                termination,
                _task: reaper,
            },
        );
        debug_assert!(replaced.is_none());
        let mut remove_pending = false;
        if let Some(pending) = state.pending.get_mut(&reservation.owner) {
            debug_assert!(*pending > 0);
            *pending = pending.saturating_sub(1);
            remove_pending = *pending == 0;
        }
        if remove_pending {
            state.pending.remove(&reservation.owner);
        }
        reservation.active = false;
        drop(state);
        self.retirement_reapers.changed.notify_waiters();
        let _ = start.send(());
    }

    #[cfg(unix)]
    pub(super) async fn shutdown_retirement_reapers(&self) {
        loop {
            let changed = self.retirement_reapers.changed.notified();
            let (pending, reapers) = {
                let mut state = self.retirement_reapers.state();
                state.accepting = false;
                (
                    state.pending.values().copied().sum::<usize>(),
                    state
                        .reapers
                        .values()
                        .map(|handle| {
                            (handle.retired_task.clone(), Arc::clone(&handle.termination))
                        })
                        .collect::<Vec<_>>(),
                )
            };
            #[cfg(test)]
            if pending > 0 || !reapers.is_empty() {
                self.retirement_reapers
                    .shutdown_passes
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                self.retirement_reapers.shutdown_changed.notify_waiters();
            }
            if pending == 0 && reapers.is_empty() {
                return;
            }
            for (retired_task, _) in &reapers {
                retired_task.abort();
            }
            if reapers.is_empty() {
                changed.await;
                continue;
            }
            for (_, termination) in reapers {
                termination.wait().await;
            }
        }
    }

    #[cfg(unix)]
    pub(super) async fn settle_retirement_reapers(&self, timeout: std::time::Duration) -> bool {
        self.settle_retirement_reapers_for_owner(None, timeout, true)
            .await
    }

    #[cfg(unix)]
    pub(super) async fn settle_retirement_reapers_for_project(
        &self,
        profile_root: &Path,
        project_id: &str,
        timeout: std::time::Duration,
    ) -> bool {
        self.settle_retirement_reapers_for_owner(Some((profile_root, project_id)), timeout, false)
            .await
    }

    #[cfg(unix)]
    async fn settle_retirement_reapers_for_owner(
        &self,
        owner: Option<(&Path, &str)>,
        timeout: std::time::Duration,
        stop_accepting: bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let changed = self.retirement_reapers.changed.notified();
            let (pending, matching) = {
                let mut state = self.retirement_reapers.state();
                if stop_accepting {
                    state.accepting = false;
                }
                let pending = state
                    .pending
                    .iter()
                    .filter(|(key, _)| {
                        owner.is_none_or(|(profile_root, project_id)| {
                            key.profile_root == profile_root
                                && key.project_id.as_deref() == Some(project_id)
                        })
                    })
                    .map(|(_, pending)| *pending)
                    .sum::<usize>();
                let matching = state
                    .reapers
                    .iter()
                    .filter(|(key, _)| {
                        owner.is_none_or(|(profile_root, project_id)| {
                            key.owner.owner.profile_root == profile_root
                                && key.owner.owner.project_id.as_deref() == Some(project_id)
                        })
                    })
                    .map(|(_, handle)| {
                        (handle.retired_task.clone(), Arc::clone(&handle.termination))
                    })
                    .collect::<Vec<_>>();
                (pending, matching)
            };
            if pending == 0 && matching.is_empty() {
                return true;
            }
            for (task, _) in &matching {
                task.abort();
            }
            if matching.is_empty() {
                if tokio::time::timeout_at(deadline, changed).await.is_err() {
                    return false;
                }
                continue;
            }
            for (_, termination) in matching {
                if tokio::time::timeout_at(deadline, termination.wait())
                    .await
                    .is_err()
                {
                    return false;
                }
            }
        }
    }

    #[cfg(all(test, unix))]
    pub(super) async fn retirement_reaper_count(&self) -> usize {
        self.retirement_reapers.state().reapers.len()
    }

    #[cfg(all(test, unix))]
    pub(super) async fn wait_for_retirement_reaper_count_for_test(&self, expected: usize) {
        loop {
            let changed = self.retirement_reapers.changed.notified();
            if self.retirement_reapers.state().reapers.len() == expected {
                return;
            }
            changed.await;
        }
    }

    #[cfg(all(test, unix))]
    pub(super) fn install_retirement_reaper_registration_barrier_for_test(
        &self,
    ) -> Arc<RetirementReaperRegistrationBarrier> {
        let barrier = Arc::new(RetirementReaperRegistrationBarrier::new());
        *self
            .retirement_reapers
            .registration_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&barrier));
        barrier
    }

    #[cfg(all(test, unix))]
    pub(super) fn retirement_reaper_shutdown_passes_for_test(&self) -> u64 {
        self.retirement_reapers
            .shutdown_passes
            .load(std::sync::atomic::Ordering::Acquire)
    }

    #[cfg(all(test, unix))]
    pub(super) async fn wait_for_retirement_reaper_shutdown_pass_for_test(&self, after: u64) {
        loop {
            let changed = self.retirement_reapers.shutdown_changed.notified();
            if self
                .retirement_reapers
                .shutdown_passes
                .load(std::sync::atomic::Ordering::Acquire)
                > after
            {
                return;
            }
            changed.await;
        }
    }

    pub(super) async fn reconcile_cached_automation_for_profile(
        &self,
        profile_root: &Path,
    ) -> Result<Vec<crate::dashboard::AutomationSchedulerOwnerReconcileOutcome>> {
        self.ensure_account_active().await?;
        let profile_root = authority::canonical_identity_path(profile_root)?;
        let servers = {
            let registry = self.project_servers.lock().await;
            registry
                .servers
                .iter()
                .filter(|(key, _)| key.owner.profile_root == profile_root)
                .map(|(key, entry)| (key.clone(), Arc::clone(&entry.server)))
                .collect::<Vec<_>>()
        };
        let mut outcomes = Vec::with_capacity(servers.len());
        for (key, server) in servers {
            outcomes.push(crate::dashboard::AutomationSchedulerOwnerReconcileOutcome {
                project_id: key.owner.project_id,
                store_root: key.owner.store_root,
                graph_db_path: key.owner.graph_db_path,
                scope_prefix: key.scope_prefix,
                outcome: server.reconcile_automation_scheduler().await,
            });
        }
        Ok(outcomes)
    }

    /// Acquires daemon-wide writer administration before constructing the
    /// supplied future and holds it until that future completes.
    ///
    /// Prefer [`Self::with_writer_in`] with a store scope. This lane excludes
    /// every store and is reserved for operations that sweep all of them.
    pub(super) async fn with_writer<Operation, OperationFuture, Output>(
        &self,
        operation: Operation,
    ) -> Output
    where
        Operation: FnOnce() -> OperationFuture,
        OperationFuture: Future<Output = Output>,
    {
        self.with_writer_in(WriterScope::Daemon, operation).await
    }

    /// Acquires writer administration for `scope` before constructing the
    /// supplied future and holds it until that future completes.
    pub(super) async fn with_writer_in<Operation, OperationFuture, Output>(
        &self,
        scope: WriterScope,
        operation: Operation,
    ) -> Output
    where
        Operation: FnOnce() -> OperationFuture,
        OperationFuture: Future<Output = Output>,
    {
        // Queueing for the writer is a park, not work: a background refresh or a
        // generation rebuild can hold a store's gate for minutes. Surrender the
        // admission slot while queued and take it back before running.
        let _writer = super::park_admission(self.gate.acquire(&scope)).await;
        operation().await
    }

    pub(super) async fn try_with_writer<Operation, OperationFuture, Output>(
        &self,
        operation: Operation,
    ) -> Option<Output>
    where
        Operation: FnOnce() -> OperationFuture,
        OperationFuture: Future<Output = Output>,
    {
        let writer = self.gate.try_acquire(&WriterScope::Daemon)?;
        let output = operation().await;
        drop(writer);
        Some(output)
    }

    /// Resolves the authenticated client's project layout and runs destructive
    /// branch administration against that exact profile-owned store.
    pub(super) async fn execute_branch_admin_for_handshake(
        &self,
        handshake: &DaemonHandshake,
        action: crate::branch::BranchAdminAction,
    ) -> Result<crate::branch::BranchAdminReport> {
        let project_root =
            handshake
                .project_path
                .as_deref()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "branch administration requires a project path".to_string(),
                })?;
        let layout = crate::storage::resolve_persisted_layout(
            project_root,
            &handshake.client_identity.profile_root,
        )?
        .ok_or_else(|| TraceDecayError::Config {
            message: "branch administration requires an enrolled project session authority"
                .to_owned(),
        })?;
        let Some(_) = layout.identity.project_id.as_deref() else {
            return Err(TraceDecayError::Config {
                message:
                    "branch administration requires an authoritative registered project identity"
                        .to_owned(),
            });
        };
        let configuration_database = self
            .registered_project_session_database(project_root, &layout)
            .await?;
        // Branch administration runs inside the daemon, which owns the durable
        // configuration store. Resolve the pinned snapshot on demand when this
        // process has not yet opened the project (first operation, or the first
        // after a daemon restart) instead of failing closed. The resolver reads
        // only durable authority; it never consults legacy config input and a
        // genuinely unresolvable store still fails before any destructive store
        // action.
        let config = crate::config::resolve_runtime_configuration_for_registered_database(
            project_root,
            &layout,
            configuration_database,
        )
        .await?
        .config
        .sync;
        self.execute_branch_admin_in_layout(
            project_root,
            &layout.data_root,
            action,
            config.branch_gc_days,
            config.orphan_db_gc_days,
        )
        .await
    }

    /// Prepares, proves, and commits one destructive branch-store mutation under
    /// the physical runtime registry's exact path reservation.
    pub(super) async fn execute_branch_admin_in_layout(
        &self,
        project_root: &Path,
        data_root: &Path,
        action: crate::branch::BranchAdminAction,
        branch_gc_days: u64,
        orphan_db_gc_days: u64,
    ) -> Result<crate::branch::BranchAdminReport> {
        let prepared = crate::branch::prepare_branch_admin_mutation(
            project_root,
            data_root,
            action,
            branch_gc_days,
            orphan_db_gc_days,
        )?;
        let database_paths = canonical_branch_database_paths(prepared.database_paths())?;
        if database_paths.is_empty() {
            return prepared.finish_without_database_deletion();
        }

        {
            let project_servers = self.project_servers.lock().await;
            let refresh_scheduler_busy = self
                .session_temporal_refresh_schedulers
                .owns_project_database_paths(&database_paths)
                .await;
            #[cfg(unix)]
            let scheduler_busy = cached_scheduler_owns_selected(
                &*self.automation_schedulers.lock().await,
                &database_paths,
            ) || refresh_scheduler_busy;
            #[cfg(not(unix))]
            let scheduler_busy = refresh_scheduler_busy;
            ensure_no_cached_store_owners(&project_servers, scheduler_busy, &database_paths)?;
        }

        let mut canonical_paths = database_paths.iter().cloned().collect::<Vec<_>>();
        canonical_paths.sort();
        let reservation = self
            .session_runtime_registry()
            .await?
            .begin_destructive_code_maintenance(data_root, canonical_paths.iter().cloned())
            .await?;
        let report = match prepared.commit_destructive() {
            Ok(report) => report,
            Err(error) => {
                reservation
                    .abort_preserved()
                    .map_err(destructive_reservation_error)?;
                return Err(error);
            }
        };
        reservation
            .finish_deleted()
            .map_err(destructive_reservation_error)?;
        Ok(report)
    }
}

pub(super) struct BranchAdminRequest {
    pub(super) id: serde_json::Value,
    pub(super) action: std::result::Result<crate::branch::BranchAdminAction, String>,
}

pub(super) fn parse_branch_admin_request(line: &str) -> Option<BranchAdminRequest> {
    let request = serde_json::from_str::<JsonRpcRequest>(line.trim()).ok()?;
    if request.method != "tools/call" {
        return None;
    }
    let params = request.params.as_ref()?;
    if params.get("name").and_then(serde_json::Value::as_str) != Some(BRANCH_ADMIN_TOOL_NAME) {
        return None;
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Some(BranchAdminRequest {
        id: request.id.unwrap_or(serde_json::Value::Null),
        action: serde_json::from_value(arguments)
            .map_err(|error| format!("invalid branch administration arguments: {error}")),
    })
}

fn canonical_branch_database_paths(paths: &[PathBuf]) -> Result<HashSet<PathBuf>> {
    paths
        .iter()
        .map(|path| authority::canonical_identity_path(path))
        .collect()
}

fn branch_administration_busy(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route("branch_administration_busy", true, detail)
}

#[cfg(any(unix, test))]
fn cached_scheduler_owns_selected<Scheduler>(
    automation_schedulers: &HashMap<ProjectServerKey, Scheduler>,
    database_paths: &HashSet<PathBuf>,
) -> bool {
    automation_schedulers
        .keys()
        .any(|key| database_paths.contains(&key.owner.graph_db_path))
}

fn ensure_no_cached_store_owners<Server>(
    project_servers: &DatabaseOwnerRegistry<Server>,
    scheduler_busy: bool,
    database_paths: &HashSet<PathBuf>,
) -> Result<()> {
    let server_busy = project_servers
        .servers
        .keys()
        .any(|key| database_paths.contains(&key.owner.graph_db_path));
    if !server_busy && !scheduler_busy {
        return Ok(());
    }

    let cached_as = match (server_busy, scheduler_busy) {
        (true, true) => "a project server and a background scheduler",
        (true, false) => "a project server",
        (false, true) => "a background scheduler",
        (false, false) => return Ok(()),
    };
    let mut paths = database_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    paths.sort();
    Err(branch_administration_busy(format!(
        "branch store administration is busy: selected database(s) {} are still cached by the daemon as {cached_as}; restart the TraceDecay daemon before retrying",
        paths.join(", ")
    )))
}

fn destructive_reservation_error(
    error: crate::daemon::store_runtime::registry::StoreRuntimeRegistryFailure,
) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("destructive store runtime reservation failed: {error:?}"),
    }
}

fn branch_admin_tool_result(
    report: &crate::branch::BranchAdminReport,
) -> Result<serde_json::Value> {
    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(report)?,
        }]
    }))
}

fn branch_admin_error_response(id: serde_json::Value, error: &TraceDecayError) -> JsonRpcResponse {
    if let Some((reason_code, retryable, detail)) = error.project_route_context() {
        return JsonRpcResponse::error_with_data(
            id,
            ErrorCode::InternalError,
            format!("branch administration unavailable: {detail}"),
            Some(json!({
                "tool": BRANCH_ADMIN_TOOL_NAME,
                "reason_code": reason_code,
                "retryable": retryable,
                "detail": detail,
            })),
        );
    }
    JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string())
}

pub(super) async fn write_branch_admin_response(
    transport: &mut impl McpTransport,
    request: BranchAdminRequest,
    result: Result<crate::branch::BranchAdminReport>,
) -> Result<()> {
    let response = match (request.action, result) {
        (Err(message), _) => JsonRpcResponse::error(request.id, ErrorCode::InvalidParams, message),
        (Ok(_), Ok(report)) => {
            JsonRpcResponse::success(request.id, branch_admin_tool_result(&report)?)
        }
        (Ok(_), Err(error)) => branch_admin_error_response(request.id, &error),
    };
    write_json_rpc_response(transport, &response).await
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::super::{ProjectRouteKey, StoreOwnerKey};
    use super::*;
    use std::time::Duration;

    fn write_future_spool_metadata(database_path: &Path) -> (PathBuf, Vec<u8>) {
        let file_name = database_path.file_name().unwrap().to_str().unwrap();
        let spool_path = database_path
            .parent()
            .unwrap()
            .join(format!(".{file_name}.host-admission"));
        std::fs::create_dir_all(&spool_path).unwrap();
        let bytes =
            br#"{"version":2,"committed_through":0,"next_seq":1,"integrity":"healthy"}"#.to_vec();
        let meta_path = spool_path.join("meta.json");
        std::fs::write(&meta_path, &bytes).unwrap();
        (meta_path, bytes)
    }

    #[test]
    fn branch_administration_busy_is_retryable_and_typed() {
        let error = branch_administration_busy("another daemon writer is active");

        assert_eq!(
            error.project_route_context(),
            Some((
                "branch_administration_busy",
                true,
                "another daemon writer is active",
            ))
        );

        let response = branch_admin_error_response(json!(7), &error);
        let response_error = response
            .error
            .expect("branch busy response must be an error");
        assert_eq!(response_error.code, ErrorCode::InternalError.as_i32());
        assert_eq!(
            response_error.data,
            Some(json!({
                "tool": BRANCH_ADMIN_TOOL_NAME,
                "reason_code": "branch_administration_busy",
                "retryable": true,
                "detail": "another daemon writer is active",
            }))
        );
    }

    /// Rank 1 regression: a git-watch sync of project A used to hold the one
    /// daemon-wide gate across a full `cg.sync()`, so the first request for
    /// project B parked behind it for as long as the sync ran.
    #[tokio::test]
    async fn one_projects_sync_does_not_delay_another_projects_open() {
        let administration = StoreAdministration::default();
        let syncing = PathBuf::from("/stores/project-a");
        let opening = PathBuf::from("/stores/project-b");
        let (holding_tx, holding_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();

        let sync_administration = administration.clone();
        let sync = tokio::spawn(async move {
            sync_administration
                .with_writer_in(
                    WriterScope::store(&syncing, StoreWriterClass::Content),
                    || async {
                        holding_tx.send(()).expect("publish sync gate acquisition");
                        release_rx.await.expect("release the sync");
                    },
                )
                .await;
        });
        holding_rx.await.expect("project A's sync holds its lane");

        let started = std::time::Instant::now();
        administration
            .with_writer_in(
                WriterScope::store(&opening, StoreWriterClass::Owner),
                || async {},
            )
            .await;
        let waited = started.elapsed();

        assert!(
            waited < std::time::Duration::from_millis(500),
            "project B's open waited {waited:?} on project A's sync"
        );
        release_tx.send(()).expect("release project A's sync");
        sync.await.expect("sync task joins");
    }

    #[tokio::test]
    async fn unavailable_spool_does_not_block_unrelated_database_authority() {
        let temp = tempfile::tempdir().unwrap();
        let blocked_path = temp.path().join("blocked.db");
        std::fs::File::create(&blocked_path).unwrap();
        let administration = StoreAdministration::default();
        std::fs::write(
            temp.path().join(".blocked.db.host-admission"),
            "not a directory",
        )
        .unwrap();

        let blocked = administration
            .host_admission_broker_for_path(&blocked_path)
            .await;
        let error = match blocked {
            Err(error) => error,
            Ok(_) => panic!("spool open failure must remain typed"),
        };
        assert_eq!(
            error.hook_runtime_context(),
            Some(("spool_io_failed", true, "host-admission spool open failed"))
        );
        assert!(error.reset_required_context().is_none());

        let healthy_path = temp.path().join("healthy.db");
        std::fs::File::create(&healthy_path).unwrap();
        administration
            .host_admission_broker_for_path(&healthy_path)
            .await
            .unwrap();
        administration.shutdown_host_admission_replay().await;
    }

    #[tokio::test]
    async fn future_spool_version_reaches_branch_admin_as_typed_reset_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("future.db");
        std::fs::File::create(&database_path).unwrap();
        let (meta_path, bytes_before) = write_future_spool_metadata(&database_path);
        let administration = StoreAdministration::default();

        let result = administration
            .host_admission_broker_for_path(&database_path)
            .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("future spool version must not become an unavailable outcome"),
        };
        assert_eq!(
            error
                .reset_required_context()
                .map(|(authority, _)| authority),
            Some("host-admission spool")
        );
        assert!(error.project_route_context().is_none());
        assert_eq!(std::fs::read(meta_path).unwrap(), bytes_before);
    }

    #[tokio::test]
    async fn profile_bootstrap_preserves_future_spool_reset_without_retry_mapping() {
        let temp = tempfile::tempdir().unwrap();
        let profile_identity =
            crate::daemon::profile_identity::load_or_create(temp.path()).unwrap();
        let database_path = tracedecay_sessions::runtime::user_sessions_db_path(temp.path());
        let (meta_path, bytes_before) = write_future_spool_metadata(&database_path);
        let administration = StoreAdministration::default().with_profile_identity(profile_identity);

        administration
            .ensure_profile_host_admission_bootstrap(temp.path())
            .await
            .unwrap();
        assert!(
            administration
                .profile_host_admission_replay
                .wait_bootstrap_completed(temp.path(), Duration::from_secs(1))
                .await,
            "production bootstrap worker must publish its terminal state"
        );
        assert_eq!(
            administration
                .profile_host_admission_replay
                .bootstrap_attempt_count(temp.path())
                .await,
            1
        );
        assert_eq!(
            administration
                .profile_host_admission_replay
                .bootstrap_backoff_count(temp.path())
                .await,
            0
        );
        let Some(ProfileHostAdmissionBootstrapStatus::Terminal(error)) = administration
            .profile_host_admission_bootstrap_status(temp.path())
            .await
            .unwrap()
        else {
            panic!("future spool version must remain a typed bootstrap terminal");
        };
        assert_eq!(
            error
                .reset_required_context()
                .map(|(authority, _)| authority),
            Some("host-admission spool")
        );
        assert!(error.project_route_context().is_none());
        assert_eq!(std::fs::read(meta_path).unwrap(), bytes_before);

        let client_identity = crate::client_identity::DaemonClientIdentity {
            profile_root: temp.path().to_path_buf(),
            global_db_path: temp.path().join("global.db"),
        };
        let Some(ProfileHostAdmissionBootstrapStatus::Terminal(observed_error)) =
            super::super::project_server_lifecycle::schedule_user_profile_host_admission_replay_for_identity(
                &administration,
                &client_identity,
            )
            .await
        else {
            panic!("connection-serving status reader must retrieve the typed terminal");
        };
        assert_eq!(
            observed_error.reset_required_context(),
            error.reset_required_context()
        );
        assert_eq!(
            administration
                .profile_host_admission_replay
                .bootstrap_attempt_count(temp.path())
                .await,
            1,
            "reading cached terminal status must not start another attempt"
        );

        let unrelated_path = temp.path().join("unrelated.db");
        std::fs::File::create(&unrelated_path).unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            administration.host_admission_broker_for_path(&unrelated_path),
        )
        .await
        .expect("typed bootstrap terminal must not block unrelated broker opens")
        .unwrap();
        administration.shutdown_host_admission_replay().await;
    }

    fn owner(graph_db_path: &str) -> StoreOwnerKey {
        StoreOwnerKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_id: Some("project".to_string()),
            store_root: PathBuf::from("/profile/projects/project"),
            graph_db_path: PathBuf::from(graph_db_path),
        }
    }

    fn server_key(graph_db_path: &str, scope_prefix: Option<&str>) -> ProjectServerKey {
        ProjectServerKey {
            owner: owner(graph_db_path),
            project_root: PathBuf::from("/project"),
            scope_prefix: scope_prefix.map(str::to_string),
        }
    }

    fn route(project_path: &str, scope_prefix: Option<&str>) -> ProjectRouteKey {
        ProjectRouteKey {
            profile_root: PathBuf::from("/profile"),
            global_db_path: PathBuf::from("/profile/global.db"),
            project_path: PathBuf::from(project_path),
            scope_prefix: scope_prefix.map(str::to_string),
        }
    }

    #[test]
    fn matching_cached_server_and_scheduler_fail_busy_without_mutation() {
        let target_a = server_key("/profile/projects/project/branches/feature.db", None);
        let target_b = server_key("/profile/projects/project/branches/feature.db", Some("src"));
        let survivor = server_key("/profile/projects/project/tracedecay.db", None);
        let target_route_a = route("/repo", None);
        let target_route_b = route("/repo", Some("src"));
        let survivor_route = route("/repo-main", None);
        let target_server_a = Arc::new("target-a");
        let target_server_b = Arc::new("target-b");
        let survivor_server = Arc::new("survivor");
        let mut registry = DatabaseOwnerRegistry::default();
        registry.insert_route(
            target_route_a.clone(),
            target_a.clone(),
            Arc::clone(&target_server_a),
        );
        registry.insert_route(
            target_route_b.clone(),
            target_b.clone(),
            Arc::clone(&target_server_b),
        );
        registry.insert_route(
            survivor_route.clone(),
            survivor.clone(),
            Arc::clone(&survivor_server),
        );
        let scheduler = Arc::new("scheduler");
        let mut schedulers = HashMap::from([(target_b.clone(), Arc::clone(&scheduler))]);
        let selected = HashSet::from([PathBuf::from(
            "/profile/projects/project/branches/feature.db",
        )]);

        let error = ensure_no_cached_store_owners(
            &registry,
            cached_scheduler_owns_selected(&schedulers, &selected),
            &selected,
        )
        .expect_err("matching daemon owners must fail closed");

        let message = error.to_string();
        assert!(message.contains("busy"), "{message}");
        assert!(
            message.contains("restart the TraceDecay daemon"),
            "{message}"
        );
        ensure_no_cached_store_owners(&registry, false, &selected)
            .expect_err("a matching project server alone must fail closed");
        let no_servers: DatabaseOwnerRegistry<Arc<&str>> = DatabaseOwnerRegistry::default();
        ensure_no_cached_store_owners(
            &no_servers,
            cached_scheduler_owns_selected(&schedulers, &selected),
            &selected,
        )
        .expect_err("a matching scheduler alone must fail closed");
        assert!(Arc::ptr_eq(
            registry
                .get_route(&target_route_a)
                .expect("target a route")
                .1,
            &target_server_a
        ));
        assert!(Arc::ptr_eq(
            registry
                .get_route(&target_route_b)
                .expect("target b route")
                .1,
            &target_server_b
        ));
        assert!(Arc::ptr_eq(
            registry
                .get_route(&survivor_route)
                .expect("survivor route")
                .1,
            &survivor_server
        ));
        assert!(Arc::ptr_eq(
            schedulers.get(&target_b).expect("scheduler entry"),
            &scheduler
        ));
        assert_eq!(registry.servers.len(), 3);
        assert_eq!(registry.aliases.len(), 3);
        assert_eq!(schedulers.len(), 1);

        // Keep the maps mutable in this regression test so accidental eviction
        // implementations cannot hide behind immutable test fixtures.
        assert!(schedulers.remove(&survivor).is_none());
    }

    #[test]
    fn unmatched_cached_owners_allow_administration_to_continue() {
        let survivor = server_key("/profile/projects/project/tracedecay.db", None);
        let survivor_route = route("/repo-main", None);
        let survivor_server = Arc::new("survivor");
        let mut registry = DatabaseOwnerRegistry::default();
        registry.insert_route(
            survivor_route.clone(),
            survivor.clone(),
            Arc::clone(&survivor_server),
        );
        let scheduler = Arc::new("scheduler");
        let schedulers = HashMap::from([(survivor.clone(), Arc::clone(&scheduler))]);
        let selected = HashSet::from([PathBuf::from(
            "/profile/projects/project/branches/feature.db",
        )]);

        ensure_no_cached_store_owners(
            &registry,
            cached_scheduler_owns_selected(&schedulers, &selected),
            &selected,
        )
        .expect("unmatched owners must proceed to holder proof and commit");

        assert!(Arc::ptr_eq(
            registry
                .get_route(&survivor_route)
                .expect("survivor route")
                .1,
            &survivor_server
        ));
        assert!(Arc::ptr_eq(
            schedulers.get(&survivor).expect("scheduler entry"),
            &scheduler
        ));
    }

    #[test]
    fn branch_admin_parser_accepts_only_the_hidden_destructive_tool() {
        let request = parse_branch_admin_request(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": BRANCH_ADMIN_TOOL_NAME,
                    "arguments": { "action": "remove", "branch": "feature/a" }
                }
            })
            .to_string(),
        )
        .expect("branch admin request");
        assert_eq!(request.id, json!(7));
        assert_eq!(
            request.action.expect("valid action"),
            crate::branch::BranchAdminAction::Remove {
                branch: "feature/a".to_string()
            }
        );

        assert!(
            parse_branch_admin_request(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 8,
                    "method": "tools/call",
                    "params": { "name": "tracedecay_status", "arguments": {} }
                })
                .to_string()
            )
            .is_none()
        );
    }

    #[test]
    fn branch_admin_parser_preserves_invalid_arguments_for_error_response() {
        let request = parse_branch_admin_request(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "bad",
                "method": "tools/call",
                "params": {
                    "name": BRANCH_ADMIN_TOOL_NAME,
                    "arguments": { "action": "remove" }
                }
            })
            .to_string(),
        )
        .expect("recognized hidden tool");
        assert!(
            request
                .action
                .unwrap_err()
                .contains("invalid branch administration arguments")
        );
    }
}

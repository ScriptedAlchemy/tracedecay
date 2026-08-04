use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use crate::errors::{Result, TraceDecayError};

#[cfg(any(unix, test))]
use super::ProjectServerKey;
use super::git_transactions::DaemonGitIndexTransactionServiceRegistry;
#[cfg(unix)]
use super::memory_repair_scheduler::MemoryRepairSchedulerHandle;
use super::profile_host_admission_replay::{
    ProfileHostAdmissionBootstrapOperation, ProfileHostAdmissionReplayRegistry,
};
#[cfg(unix)]
use super::scheduler::{AutomationSchedulerHandle, MaintenanceTaskTermination};
use super::session_temporal_refresh_scheduler::SessionTemporalRefreshSchedulerRegistry;
use super::store_writer_gate::StoreWriterGates;
pub(super) use super::store_writer_gate::{StoreWriterClass, WriterScope};
use super::{DatabaseOwnerRegistry, authority};

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

#[derive(Clone)]
pub(super) enum HostAdmissionBrokerState {
    Available(crate::application::host_admission::SharedHostAdmissionBroker),
    Unavailable(crate::application::host_admission::HostAdmissionOutcome),
}

impl HostAdmissionBrokerState {
    pub(super) fn broker(
        &self,
    ) -> Option<&crate::application::host_admission::SharedHostAdmissionBroker> {
        match self {
            Self::Available(broker) => Some(broker),
            Self::Unavailable(_) => None,
        }
    }

    pub(super) fn unavailable_outcome(
        &self,
    ) -> Option<crate::application::host_admission::HostAdmissionOutcome> {
        match self {
            Self::Available(_) => None,
            Self::Unavailable(outcome) => Some(*outcome),
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum MaintenanceReaperKind {
    Automation,
    MemoryRepair,
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
    pending: usize,
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
                pending: 0,
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

    fn reserve(self: &Arc<Self>) -> Option<MaintenanceReaperReservation> {
        let mut state = self.state();
        if !state.accepting {
            return None;
        }
        state.pending += 1;
        drop(state);
        self.changed.notify_waiters();
        Some(MaintenanceReaperReservation {
            registry: Arc::clone(self),
            active: true,
        })
    }

    fn release_reservation(&self) {
        let mut state = self.state();
        debug_assert!(state.pending > 0);
        state.pending = state.pending.saturating_sub(1);
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
    active: bool,
}

#[cfg(unix)]
impl Drop for MaintenanceReaperReservation {
    fn drop(&mut self) {
        if self.active {
            self.registry.release_reservation();
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

type SessionRuntimeRegistries = HashMap<
    PathBuf,
    Arc<
        tokio::sync::OnceCell<
            Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>,
        >,
    >,
>;
type SharedSessionRuntimeRegistries = Arc<tokio::sync::Mutex<SessionRuntimeRegistries>>;

#[derive(Clone)]
struct ProfileHostAdmissionBootstrapContext {
    profile_root: PathBuf,
    profile_identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    session_runtime_registry: Arc<
        tokio::sync::OnceCell<
            Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>,
        >,
    >,
    host_admission_brokers: Arc<
        tokio::sync::Mutex<
            HashMap<PathBuf, crate::application::host_admission::SharedHostAdmissionBroker>,
        >,
    >,
    host_admission_broker_gate: Arc<tokio::sync::Mutex<()>>,
    profile_host_admission_replay: Weak<ProfileHostAdmissionReplayRegistry>,
}

impl ProfileHostAdmissionBootstrapContext {
    async fn ensure(&self) -> Result<()> {
        let profile_identity = self.profile_identity.clone();
        let session_runtime_registry = self
            .session_runtime_registry
            .get_or_try_init(|| async move {
                crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                    profile_identity,
                )
                .await
                .map(Arc::new)
            })
            .await
            .map(Arc::clone)
            .map_err(|error| {
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
        let state = self.open_broker(&broker_path).await;
        let broker = match state {
            HostAdmissionBrokerState::Available(broker) => broker,
            HostAdmissionBrokerState::Unavailable(outcome) => {
                return Err(TraceDecayError::project_route(
                    outcome.reason_code.unwrap_or("spool_unavailable"),
                    outcome.retryable,
                    "user-profile host admission spool is unavailable",
                ));
            }
        };
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

    async fn open_broker(&self, path: &Path) -> HostAdmissionBrokerState {
        if let Some(broker) = self.host_admission_brokers.lock().await.get(path).cloned() {
            return HostAdmissionBrokerState::Available(broker);
        }

        let _open = self.host_admission_broker_gate.lock().await;
        let brokers = self.host_admission_brokers.lock().await;
        if let Some(broker) = brokers.get(path) {
            return HostAdmissionBrokerState::Available(Arc::clone(broker));
        }
        drop(brokers);
        let open_path = path.to_path_buf();
        let opened = tokio::task::spawn_blocking(move || {
            crate::application::host_admission::HostAdmissionRuntime::open_for_database(&open_path)
        })
        .await;
        let state = match opened {
            Ok(Ok((runtime, _))) => HostAdmissionBrokerState::Available(Arc::new(
                crate::application::host_admission::HostAdmissionBroker::new(runtime),
            )),
            Ok(Err(outcome)) => HostAdmissionBrokerState::Unavailable(outcome),
            Err(_) => HostAdmissionBrokerState::Unavailable(
                crate::application::host_admission::HostAdmissionOutcome::retained_unavailable(
                    "spool_runtime_unavailable",
                ),
            ),
        };
        if let HostAdmissionBrokerState::Available(broker) = &state {
            self.host_admission_brokers
                .lock()
                .await
                .insert(path.to_path_buf(), Arc::clone(broker));
        }
        state
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
    session_runtime_registries: SharedSessionRuntimeRegistries,
    gate: Arc<StoreWriterGates>,
    project_servers: Arc<tokio::sync::Mutex<DatabaseOwnerRegistry>>,
    project_server_retirements: Arc<tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    project_routes: crate::mcp::project_route::SharedHookProjectRouteCache,
    host_admission_brokers: Arc<
        tokio::sync::Mutex<
            HashMap<PathBuf, crate::application::host_admission::SharedHostAdmissionBroker>,
        >,
    >,
    host_admission_broker_gate: Arc<tokio::sync::Mutex<()>>,
    profile_host_admission_replay: Arc<ProfileHostAdmissionReplayRegistry>,
    #[cfg(unix)]
    automation_schedulers:
        Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, AutomationSchedulerHandle>>>,
    #[cfg(unix)]
    memory_repair_schedulers:
        Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, MemoryRepairSchedulerHandle>>>,
    session_temporal_refresh_schedulers: Arc<SessionTemporalRefreshSchedulerRegistry>,
    git_index_transaction_services: Arc<DaemonGitIndexTransactionServiceRegistry>,
    #[cfg(unix)]
    retirement_reapers: Arc<MaintenanceReaperRegistry>,
}

impl Default for StoreAdministration {
    fn default() -> Self {
        Self {
            profile_identity: None,
            session_runtime_registries: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            gate: Arc::new(StoreWriterGates::default()),
            project_servers: Arc::new(tokio::sync::Mutex::new(DatabaseOwnerRegistry::default())),
            project_server_retirements: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            project_routes: crate::mcp::project_route::SharedHookProjectRouteCache::default(),
            host_admission_brokers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            host_admission_broker_gate: Arc::new(tokio::sync::Mutex::new(())),
            profile_host_admission_replay: Arc::new(ProfileHostAdmissionReplayRegistry::default()),
            #[cfg(unix)]
            automation_schedulers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            #[cfg(unix)]
            memory_repair_schedulers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            session_temporal_refresh_schedulers: Arc::new(
                SessionTemporalRefreshSchedulerRegistry::default(),
            ),
            git_index_transaction_services: Arc::new(
                DaemonGitIndexTransactionServiceRegistry::default(),
            ),
            #[cfg(unix)]
            retirement_reapers: Arc::new(MaintenanceReaperRegistry::default()),
        }
    }
}

impl StoreAdministration {
    pub(super) fn project_routes(&self) -> crate::mcp::project_route::SharedHookProjectRouteCache {
        self.project_routes.clone()
    }

    #[cfg(unix)]
    pub(super) async fn for_retained_project_graph(
        graph: &crate::tracedecay::TraceDecay,
    ) -> Result<Self> {
        let profile_root = graph.retained_profile_root()?;
        let profile_identity = crate::daemon::profile_identity::load_or_create(&profile_root)?;
        let administration = Self::default().with_profile_identity(profile_identity);
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
            .insert(authority::canonical_identity_path(&profile_root)?, registry);
        Ok(administration)
    }

    pub(super) fn with_profile_identity(
        mut self,
        profile_identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    ) -> Self {
        self.profile_identity = Some(profile_identity);
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

    async fn session_runtime_registry(
        &self,
    ) -> Result<Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>>
    {
        let identity = self.profile_identity()?.clone();
        let profile_root = authority::canonical_identity_path(identity.profile_root())?;
        let registry = {
            let mut registries = self.session_runtime_registries.lock().await;
            Arc::clone(
                registries
                    .entry(profile_root)
                    .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())),
            )
        };
        registry
            .get_or_try_init(|| async move {
                crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                    identity,
                )
                .await
                .map(Arc::new)
            })
            .await
            .map(Arc::clone)
    }

    pub(super) async fn retained_runtime_registry(
        &self,
    ) -> Result<Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>>
    {
        self.session_runtime_registry().await
    }

    pub(super) async fn registered_runtime_registry(
        &self,
    ) -> Result<Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>>
    {
        self.session_runtime_registry().await
    }

    pub(super) async fn registered_profile_session_database(
        &self,
    ) -> Result<Arc<crate::global_db::RegisteredGlobalDb>> {
        self.session_runtime_registry()
            .await?
            .profile_sessions()
            .await
    }

    pub(super) async fn registered_profile_database(
        &self,
    ) -> Result<Arc<crate::global_db::RegisteredGlobalDb>> {
        self.session_runtime_registry()
            .await?
            .profile_database()
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
            registries.get(&profile_root).cloned()
        };
        let Some(registry) = registry.and_then(|registry| registry.get().cloned()) else {
            return Vec::new();
        };
        registry.mounted_session_databases().await
    }

    pub(super) async fn mounted_project_graphs(&self) -> Vec<Arc<crate::tracedecay::TraceDecay>> {
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
        let mut graphs = Vec::with_capacity(servers.len());
        for server in servers {
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
        // A mounted shard already carries the exact typed enrollment authority
        // that opened it. Later client routes may be linked-worktree aliases;
        // reuse by ProjectId instead of treating those paths as new authority.
        if let Some(database) = registry.mounted_project_sessions(&project_id).await {
            return Ok(database);
        }
        let profile_database = self.registered_profile_database().await?;
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

    pub(super) async fn track_project_server_retirement(&self, task: tokio::task::JoinHandle<()>) {
        let mut retirements = self.project_server_retirements.lock().await;
        retirements.retain(|retirement| !retirement.is_finished());
        retirements.push(task);
    }

    pub(super) async fn join_project_server_retirements(&self) {
        let retirements = std::mem::take(&mut *self.project_server_retirements.lock().await);
        for retirement in retirements {
            let _ = retirement.await;
        }
    }

    pub(super) async fn host_admission_broker(
        &self,
        database: &Arc<crate::global_db::RegisteredGlobalDb>,
    ) -> Result<HostAdmissionBrokerState> {
        self.host_admission_broker_for_path(database.db_path())
            .await
    }

    async fn host_admission_broker_for_path(
        &self,
        database_path: &Path,
    ) -> Result<HostAdmissionBrokerState> {
        let path = authority::canonical_identity_path(database_path)?;
        if let Some(broker) = self.host_admission_brokers.lock().await.get(&path).cloned() {
            self.maybe_ensure_user_profile_host_admission_replay(&path, &broker)
                .await;
            return Ok(HostAdmissionBrokerState::Available(broker));
        }

        // Serialize first-open publication without retaining the broker map
        // lock across blocking filesystem work.
        let _open = self.host_admission_broker_gate.lock().await;
        let state = {
            let brokers = self.host_admission_brokers.lock().await;
            if let Some(broker) = brokers.get(&path) {
                HostAdmissionBrokerState::Available(Arc::clone(broker))
            } else {
                drop(brokers);
                let open_path = path.clone();
                let opened = tokio::task::spawn_blocking(move || {
                    crate::application::host_admission::HostAdmissionRuntime::open_for_database(
                        &open_path,
                    )
                })
                .await;
                let state = match opened {
                    Ok(Ok((runtime, _))) => HostAdmissionBrokerState::Available(Arc::new(
                        crate::application::host_admission::HostAdmissionBroker::new(runtime),
                    )),
                    Ok(Err(outcome)) => HostAdmissionBrokerState::Unavailable(outcome),
                    Err(_) => HostAdmissionBrokerState::Unavailable(
                        crate::application::host_admission::HostAdmissionOutcome::retained_unavailable(
                            "spool_runtime_unavailable",
                        ),
                    ),
                };
                if let HostAdmissionBrokerState::Available(broker) = &state {
                    self.host_admission_brokers
                        .lock()
                        .await
                        .insert(path.clone(), Arc::clone(broker));
                }
                state
            }
        };
        if let Some(broker) = state.broker() {
            self.maybe_ensure_user_profile_host_admission_replay(&path, broker)
                .await;
        }
        Ok(state)
    }

    /// Kick the coalesced user-profile replay worker. Never awaits a replay pass.
    pub(super) async fn ensure_user_profile_host_admission_replay(
        &self,
        profile_root: &Path,
        broker: &crate::application::host_admission::SharedHostAdmissionBroker,
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
        let profile_root = authority::canonical_identity_path(profile_root)?;
        let profile_identity = self.profile_identity()?.clone();
        let authority_profile_root =
            authority::canonical_identity_path(profile_identity.profile_root())?;
        if profile_root != authority_profile_root {
            return Err(TraceDecayError::Config {
                message: "profile host-admission bootstrap identity mismatch".to_owned(),
            });
        }
        let session_runtime_registry = {
            let mut registries = self.session_runtime_registries.lock().await;
            Arc::clone(
                registries
                    .entry(profile_root.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())),
            )
        };
        let context = ProfileHostAdmissionBootstrapContext {
            profile_root: profile_root.clone(),
            profile_identity,
            session_runtime_registry,
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

    async fn maybe_ensure_user_profile_host_admission_replay(
        &self,
        broker_path: &Path,
        broker: &crate::application::host_admission::SharedHostAdmissionBroker,
    ) {
        let is_user_sessions = broker_path.file_name().and_then(|name| name.to_str())
            == Some(crate::sessions::USER_SESSIONS_DB_FILENAME);
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

    #[cfg(unix)]
    pub(super) fn automation_schedulers(
        &self,
    ) -> &Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, AutomationSchedulerHandle>>> {
        &self.automation_schedulers
    }

    #[cfg(unix)]
    pub(super) fn memory_repair_schedulers(
        &self,
    ) -> &Arc<tokio::sync::Mutex<HashMap<ProjectServerKey, MemoryRepairSchedulerHandle>>> {
        &self.memory_repair_schedulers
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

    #[cfg(unix)]
    pub(super) fn reserve_retirement_reaper(&self) -> Option<MaintenanceReaperReservation> {
        self.retirement_reapers.reserve()
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
        debug_assert!(state.pending > 0);
        state.pending = state.pending.saturating_sub(1);
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
                    state.pending,
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
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

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
            .await
            .unwrap();
        let outcome = blocked
            .unavailable_outcome()
            .expect("spool open failure must be represented as typed unavailability");
        assert_eq!(
            outcome.status,
            crate::application::host_admission::HostAdmissionStatus::Unavailable
        );
        assert!(blocked.broker().is_none());

        let healthy_path = temp.path().join("healthy.db");
        std::fs::File::create(&healthy_path).unwrap();
        let healthy = administration
            .host_admission_broker_for_path(&healthy_path)
            .await
            .unwrap();
        assert!(healthy.broker().is_some());
        administration.shutdown_host_admission_replay().await;
    }
}

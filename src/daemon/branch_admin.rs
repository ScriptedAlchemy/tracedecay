use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use serde_json::json;

use crate::application::context::CancellationToken;
use crate::errors::{Result, TraceDecayError};
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport};

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
pub(super) use super::store_writer_gate::{StoreWriterClass, WriterScope};
use super::store_writer_gate::StoreWriterGates;
use super::{DaemonHandshake, DatabaseOwnerRegistry, authority, write_json_rpc_response};

const BRANCH_ADMIN_TOOL_NAME: &str = "tracedecay_admin_branch";

/// How long a *request-side* caller queues for writer administration before it
/// gives up and answers with a typed retryable busy error.
///
/// The gate is per store now, so the only thing a request can queue behind is
/// another writer on the store it asked for. Ten seconds is long enough to
/// absorb ordinary owner bookkeeping and short enough that a stuck writer
/// surfaces as a retryable error rather than a 900-second hang.
pub(super) const REQUEST_WRITER_ADMISSION_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(10);

/// Outcome of one attempt to run an operation under writer administration.
pub(super) enum WriterAdmission<Output> {
    /// Admitted; the operation ran to completion.
    Completed(Output),
    /// The caller's cancellation token fired while queued.
    Cancelled,
    /// The wait outlived the caller's deadline. Retryable.
    Busy,
}

/// Typed, retryable error for a request that could not be admitted to a store's
/// writer lane inside its deadline.
pub(super) fn store_writer_busy(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route("store_writer_busy", true, detail)
}

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

/// Owner-lane scope for a project that is about to be opened.
///
/// A project already enrolled in this profile resolves to its persisted
/// `data_root`, so its open contends only with writers on its own store — the
/// case that used to park the first request after a daemon start behind an
/// unrelated project's sync. A project with no persisted layout yet (first
/// `init`) names no store, so it falls back to the daemon-wide lane, which is
/// exactly what the single gate always did.
pub(super) fn project_open_writer_scope(project_root: &Path, profile_root: &Path) -> WriterScope {
    match crate::storage::resolve_persisted_layout(project_root, profile_root) {
        Ok(Some(layout)) => store_writer_scope(&layout.data_root, StoreWriterClass::Owner),
        Ok(None) | Err(_) => WriterScope::Daemon,
    }
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

#[cfg(test)]
type ExternalHolderVerifier = fn(&[PathBuf]) -> Result<()>;

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
    #[cfg(test)]
    external_holder_verifier: Option<ExternalHolderVerifier>,
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
            #[cfg(test)]
            external_holder_verifier: None,
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

    #[cfg(all(test, unix))]
    pub(super) fn with_external_holder_verifier(
        external_holder_verifier: ExternalHolderVerifier,
    ) -> Self {
        Self {
            external_holder_verifier: Some(external_holder_verifier),
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

    #[cfg_attr(not(test), allow(clippy::unused_self))]
    fn prove_no_external_branch_store_holders(&self, database_paths: &[PathBuf]) -> Result<()> {
        #[cfg(test)]
        if let Some(external_holder_verifier) = self.external_holder_verifier {
            return external_holder_verifier(database_paths);
        }
        ensure_no_external_branch_store_holders(database_paths)
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

    /// Runs an operator-facing daemon-wide writer operation only when it can
    /// acquire the administration lane immediately. Destructive commands must
    /// report busy instead of silently queuing behind project warm-up.
    pub(super) async fn try_with_writer<Operation, OperationFuture, Output>(
        &self,
        operation: Operation,
    ) -> Option<Output>
    where
        Operation: FnOnce() -> OperationFuture,
        OperationFuture: Future<Output = Output>,
    {
        self.try_with_writer_in(WriterScope::Daemon, operation)
            .await
    }

    /// [`Self::try_with_writer`] scoped to one store.
    pub(super) async fn try_with_writer_in<Operation, OperationFuture, Output>(
        &self,
        scope: WriterScope,
        operation: Operation,
    ) -> Option<Output>
    where
        Operation: FnOnce() -> OperationFuture,
        OperationFuture: Future<Output = Output>,
    {
        let writer = self.gate.try_acquire(&scope)?;
        let output = operation().await;
        drop(writer);
        Some(output)
    }

    /// Cancels while queued for daemon-wide writer administration, then lets an
    /// admitted operation finish its transactionally safe unit before releasing
    /// it. Production request paths use [`Self::with_writer_admission`] with a
    /// store scope and a deadline instead.
    #[cfg(test)]
    pub(super) async fn with_writer_until_cancelled<Operation, OperationFuture, Output>(
        &self,
        cancellation: &CancellationToken,
        operation: Operation,
    ) -> Option<Output>
    where
        Operation: FnOnce() -> OperationFuture,
        OperationFuture: Future<Output = Output>,
    {
        match self
            .with_writer_admission(WriterScope::Daemon, cancellation, None, operation)
            .await
        {
            WriterAdmission::Completed(output) => Some(output),
            WriterAdmission::Cancelled | WriterAdmission::Busy => None,
        }
    }

    /// Queues for writer administration on `scope` under both a cancellation
    /// token and an optional deadline.
    ///
    /// A request-side caller passes `Some(deadline)`: a gate wait that outlives
    /// it reports [`WriterAdmission::Busy`] so the request answers with a typed
    /// retryable error instead of parking without bound. The deadline bounds
    /// only the *wait*; an admitted operation always runs to completion so no
    /// transactional unit is torn.
    pub(super) async fn with_writer_admission<Operation, OperationFuture, Output>(
        &self,
        scope: WriterScope,
        cancellation: &CancellationToken,
        deadline: Option<std::time::Duration>,
        operation: Operation,
    ) -> WriterAdmission<Output>
    where
        Operation: FnOnce() -> OperationFuture,
        OperationFuture: Future<Output = Output>,
    {
        let admitted = super::park_admission(async {
            let acquire = self.gate.acquire(&scope);
            let cancellable = async {
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => None,
                    writer = acquire => Some(writer),
                }
            };
            match deadline {
                Some(deadline) => match tokio::time::timeout(deadline, cancellable).await {
                    Ok(Some(writer)) => WriterAdmission::Completed(writer),
                    Ok(None) => WriterAdmission::Cancelled,
                    Err(_) => WriterAdmission::Busy,
                },
                None => match cancellable.await {
                    Some(writer) => WriterAdmission::Completed(writer),
                    None => WriterAdmission::Cancelled,
                },
            }
        })
        .await;
        let _writer = match admitted {
            WriterAdmission::Completed(writer) => writer,
            WriterAdmission::Cancelled => return WriterAdmission::Cancelled,
            WriterAdmission::Busy => return WriterAdmission::Busy,
        };
        WriterAdmission::Completed(operation().await)
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

    /// Prepares, proves, and commits one destructive branch-store mutation while
    /// excluding every writer on *this* store. Cached owners fail closed and are
    /// left completely untouched; operators must restart the daemon to release
    /// them.
    ///
    /// The destructive lane is store-scoped, not daemon-wide, because the holder
    /// proof below is computed entirely from this `data_root`'s database paths:
    /// a writer on another store cannot invalidate it. Within this store the
    /// lane is still totally exclusive — see
    /// [`store_writer_gate`](super::store_writer_gate).
    pub(super) async fn execute_branch_admin_in_layout(
        &self,
        project_root: &Path,
        data_root: &Path,
        action: crate::branch::BranchAdminAction,
        branch_gc_days: u64,
        orphan_db_gc_days: u64,
    ) -> Result<crate::branch::BranchAdminReport> {
        let scope = store_writer_scope(data_root, StoreWriterClass::Destructive);
        self.try_with_writer_in(scope, || async {
            if let Some(recovery) =
                crate::branch::prepare_pending_branch_admin_recovery(data_root)?
            {
                let database_paths =
                    canonical_branch_database_paths(recovery.database_paths())?;
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
                    ) || cached_scheduler_owns_selected(
                        &*self.memory_repair_schedulers.lock().await,
                        &database_paths,
                    ) || refresh_scheduler_busy;
                    #[cfg(not(unix))]
                    let scheduler_busy = refresh_scheduler_busy;
                    ensure_no_cached_store_owners(
                        &project_servers,
                        scheduler_busy,
                        &database_paths,
                    )?;
                }

                let mut canonical_paths = database_paths.iter().cloned().collect::<Vec<_>>();
                canonical_paths.sort();
                let (fence, states) = crate::db::DatabaseDeletionFence::reacquire(
                    &canonical_paths,
                    recovery.transaction_id(),
                    "recover branch SQLite family deletion",
                )?;
                ensure_recovery_tombstone_states(recovery.disposition(), states)?;
                let fenced_paths = fence.database_paths().collect::<Vec<_>>();
                if fenced_paths.len() != database_paths.len()
                    || fenced_paths
                        .iter()
                        .any(|path| !database_paths.contains(*path))
                {
                    return Err(TraceDecayError::Config {
                        message: "database deletion recovery fence resolved a different branch-store identity set"
                            .to_string(),
                    });
                }

                recovery.recover(
                    |paths| {
                        self.prove_no_external_branch_store_holders(paths)?;
                        crate::migrate::memory_cutover::verify_branch_removal_receipts(
                            data_root,
                            &canonical_paths,
                            paths,
                        )
                    },
                    |disposition| match disposition {
                        crate::branch::BranchAdminRecoveryDisposition::PreCommitRollback => {
                            fence.rollback_deleting()
                        }
                        crate::branch::BranchAdminRecoveryDisposition::CommittedCleanup => {
                            fence.promote_deleted()
                        }
                    },
                )?;
            }

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

            self.session_runtime_registry()
                .await?
                .close_code_graph_paths(database_paths.iter().cloned())
                .await?;

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
                ) || cached_scheduler_owns_selected(
                    &*self.memory_repair_schedulers.lock().await,
                    &database_paths,
                ) || refresh_scheduler_busy;
                #[cfg(not(unix))]
                let scheduler_busy = refresh_scheduler_busy;
                ensure_no_cached_store_owners(
                    &project_servers,
                    scheduler_busy,
                    &database_paths,
                )?;
            }

            let mut canonical_paths = database_paths.iter().cloned().collect::<Vec<_>>();
            canonical_paths.sort();
            let fence = crate::db::DatabaseDeletionFence::acquire(
                &canonical_paths,
                "delete branch SQLite families",
            )?;
            let fenced_paths = fence.database_paths().collect::<Vec<_>>();
            if fenced_paths.len() != database_paths.len()
                || fenced_paths
                    .iter()
                    .any(|path| !database_paths.contains(*path))
            {
                return Err(TraceDecayError::Config {
                    message:
                        "database deletion fence resolved a different branch-store identity set"
                            .to_string(),
                });
            }
            prepared.commit_with_transaction(
                fence.transaction_id(),
                || fence.publish_deleting(),
                |paths| {
                    self.prove_no_external_branch_store_holders(paths)?;
                    crate::migrate::memory_cutover::verify_branch_removal_receipts(
                        data_root,
                        &canonical_paths,
                        paths,
                    )
                },
                || fence.rollback_deleting(),
                || fence.promote_deleted(),
            )
        })
        .await
        .unwrap_or_else(|| {
            Err(branch_administration_busy(
                "branch store administration is busy: another daemon writer is active; retry after current project maintenance finishes",
            ))
        })
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

fn ensure_recovery_tombstone_states(
    disposition: crate::branch::BranchAdminRecoveryDisposition,
    states: crate::db::DatabaseDeletionStates,
) -> Result<()> {
    let valid = match disposition {
        crate::branch::BranchAdminRecoveryDisposition::PreCommitRollback => !states.has_deleted(),
        crate::branch::BranchAdminRecoveryDisposition::CommittedCleanup => !states.has_missing(),
    };
    if valid {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!(
            "branch deletion recovery found incompatible tombstone states for {disposition:?}: missing={}, deleting={}, deleted={}",
            states.missing(),
            states.deleting(),
            states.deleted(),
        ),
    })
}

fn ensure_no_external_branch_store_holders(database_paths: &[PathBuf]) -> Result<()> {
    let options = crate::open_store_holders::OpenStoreHolderScanOptions {
        include_current_process: true,
        excluded_current_process_fds: BTreeSet::new(),
    };
    let scan = crate::open_store_holders::scan_with_options(database_paths, &options).map_err(
        |error| TraceDecayError::Config {
            message: format!("failed to inspect open branch stores: {error}"),
        },
    )?;
    match scan {
        crate::open_store_holders::OpenStoreHolderScan::Supported(holders)
            if holders.is_empty() =>
        {
            Ok(())
        }
        crate::open_store_holders::OpenStoreHolderScan::Supported(holders) => {
            let details = holders
                .into_iter()
                .map(|holder| format!("pid {} ({})", holder.pid, holder.command))
                .collect::<Vec<_>>()
                .join(", ");
            Err(TraceDecayError::Config {
                message: format!(
                    "cannot delete branch stores while external processes still hold them: {details}"
                ),
            })
        }
        crate::open_store_holders::OpenStoreHolderScan::Unsupported { reason } => {
            Err(TraceDecayError::Config {
                message: format!(
                    "cannot prove branch stores are closed: {reason}; destructive branch operation refused"
                ),
            })
        }
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

    #[tokio::test]
    async fn destructive_writer_operation_fails_fast_while_lane_is_busy() {
        let administration = StoreAdministration::default();
        let _writer = administration.gate.acquire(&WriterScope::Daemon).await;

        let outcome = administration
            .try_with_writer(|| async { "unexpected admission" })
            .await;

        assert!(outcome.is_none());
    }

    /// The store-scoped lane must keep the same fail-closed answer for a
    /// destructive command whose own store already has a writer, while leaving
    /// a *different* store admissible.
    #[tokio::test]
    async fn destructive_writer_is_refused_only_on_the_busy_store() {
        let administration = StoreAdministration::default();
        let busy = PathBuf::from("/stores/busy");
        let idle = PathBuf::from("/stores/idle");
        let _writer = administration
            .gate
            .acquire(&WriterScope::store(&busy, StoreWriterClass::Content))
            .await;

        assert!(
            administration
                .try_with_writer_in(
                    WriterScope::store(&busy, StoreWriterClass::Destructive),
                    || async { "unexpected admission" },
                )
                .await
                .is_none(),
            "destructive administration must not select a store a writer owns"
        );
        assert!(
            administration
                .try_with_writer_in(
                    WriterScope::store(&idle, StoreWriterClass::Destructive),
                    || async { "admitted" },
                )
                .await
                .is_some(),
            "an unrelated store must stay admissible"
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
    async fn request_side_gate_wait_reports_typed_busy_at_its_deadline() {
        let administration = StoreAdministration::default();
        let store = PathBuf::from("/stores/held");
        let _held = administration
            .gate
            .acquire(&WriterScope::store(&store, StoreWriterClass::Owner))
            .await;

        let cancellation = CancellationToken::new();
        let outcome = administration
            .with_writer_admission(
                WriterScope::store(&store, StoreWriterClass::Owner),
                &cancellation,
                Some(std::time::Duration::from_millis(50)),
                || async { "unexpected admission" },
            )
            .await;

        assert!(matches!(outcome, WriterAdmission::Busy));
        let error = store_writer_busy("project open could not acquire the store writer lane");
        assert_eq!(
            error.project_route_context(),
            Some((
                "store_writer_busy",
                true,
                "project open could not acquire the store writer lane",
            ))
        );
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
    fn recovery_phase_validation_accepts_only_phase_compatible_mixed_states() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("a.db");
        let second = temp.path().join("b.db");

        let fence = crate::db::DatabaseDeletionFence::acquire(
            &[first.clone(), second.clone()],
            "test partial publication",
        )
        .unwrap();
        fence.publish_deleting().unwrap();
        let transaction_id = fence.transaction_id().to_string();
        let first_tombstone = fence.tombstone_paths().next().unwrap().to_path_buf();
        std::fs::remove_file(first_tombstone).unwrap();
        drop(fence);
        let (fence, states) = crate::db::DatabaseDeletionFence::reacquire(
            &[second.clone(), first.clone()],
            &transaction_id,
            "test partial publication recovery",
        )
        .unwrap();
        ensure_recovery_tombstone_states(
            crate::branch::BranchAdminRecoveryDisposition::PreCommitRollback,
            states,
        )
        .unwrap();
        ensure_recovery_tombstone_states(
            crate::branch::BranchAdminRecoveryDisposition::CommittedCleanup,
            states,
        )
        .unwrap_err();
        fence.rollback_deleting().unwrap();
        drop(fence);

        let fence = crate::db::DatabaseDeletionFence::acquire(
            &[first.clone(), second.clone()],
            "test partial promotion",
        )
        .unwrap();
        fence.publish_deleting().unwrap();
        fence.promote_deleted().unwrap();
        let transaction_id = fence.transaction_id().to_string();
        let first_tombstone = fence.tombstone_paths().next().unwrap().to_path_buf();
        let deleted = std::fs::read_to_string(&first_tombstone).unwrap();
        std::fs::write(
            &first_tombstone,
            deleted.replace("state=deleted", "state=deleting"),
        )
        .unwrap();
        drop(fence);
        let (fence, states) = crate::db::DatabaseDeletionFence::reacquire(
            &[second, first],
            &transaction_id,
            "test partial promotion recovery",
        )
        .unwrap();
        ensure_recovery_tombstone_states(
            crate::branch::BranchAdminRecoveryDisposition::CommittedCleanup,
            states,
        )
        .unwrap();
        ensure_recovery_tombstone_states(
            crate::branch::BranchAdminRecoveryDisposition::PreCommitRollback,
            states,
        )
        .unwrap_err();
        fence.promote_deleted().unwrap();
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

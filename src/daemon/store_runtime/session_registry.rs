//! Daemon-owned registry assembly for profile and project session shards.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::Mutex;
#[cfg(test)]
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;
use tracedecay_agent_hosts::ports::project_runtime::{
    MemoryCurateOptions as AgentMemoryCurateOptions, ProfileRuntime, RuntimeFuture,
};
use tracedecay_domain::RefId;
use tracedecay_store::{
    CodeShardScopeV1, ProjectId, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
    StoreSnapshotIdV1,
};

use super::registry::{
    LifecycleShardRuntimePublisher, ProfileAuthorityPin, ProfileAuthorityPinResult,
    StoreRuntimeHandle, StoreRuntimeKey, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
    StoreRuntimeRegistry, StoreRuntimeRegistryFailure,
};
use super::resolver::{
    LocalCodeStoreAuthorityV1, LocalProfileStoreAuthorityV1, LocalProjectEnrollmentAuthorityV1,
    LocalStoreLocatorResolutionV1, LocalStoreRuntimeResolverV1,
};
use crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1;
use crate::db::{Database, DatabaseAccessMode, DatabaseAuthority};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;

static LONG_LIVED_SESSION_MAINTENANCE: AtomicBool = AtomicBool::new(false);

pub(crate) fn mark_process_long_lived_for_session_maintenance() {
    LONG_LIVED_SESSION_MAINTENANCE.store(true, Ordering::Relaxed);
}

pub(crate) fn release_process_allocator_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `malloc_trim` is a process-wide, thread-safe glibc allocator
        // maintenance operation. It does not invalidate live allocations.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

/// Root wiring for `tracedecay_global_db::host_ports::profile_sessions`.
///
/// The moved global-db crate cannot reach the daemon's profile identity or
/// session-runtime registry (they sit above it), so it exposes a
/// composition-root port instead. This installs the daemon-backed opener —
/// profile identity creation followed by registry open, with `mount` serving
/// the registered profile-sessions database. Idempotent (first call wins);
/// called at daemon startup and by root test harnesses before they open a
/// `RegisteredGlobalDbHarness`.
pub(crate) fn register_profile_sessions_port() {
    use tracedecay_global_db::host_ports::profile_sessions;

    struct DaemonProfileSessions {
        registry: DaemonSessionRuntimeRegistryV1,
    }

    impl profile_sessions::ProfileSessionsRuntime for DaemonProfileSessions {
        fn mount(&self) -> profile_sessions::MountFuture<'_> {
            Box::pin(async {
                self.registry
                    .profile_sessions()
                    .await
                    .expect("registered profile sessions")
            })
        }
    }

    fn open_runtime(profile_root: PathBuf) -> profile_sessions::OpenFuture {
        Box::pin(async move {
            // The caller has already entered the daemon database scope; the
            // identity is therefore created inside it, not ahead of it.
            let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
                .expect("profile identity for the profile-sessions port");
            let registry = DaemonSessionRuntimeRegistryV1::open(identity)
                .await
                .expect("session runtime registry for the profile-sessions port");
            Box::new(DaemonProfileSessions { registry })
                as Box<dyn profile_sessions::ProfileSessionsRuntime>
        })
    }

    profile_sessions::register(open_runtime);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RegisteredSchemaConvergenceStatus {
    Pending,
    Complete,
    Degraded { message: String },
}

struct RegisteredSchemaConvergenceMaintenance {
    statuses: Arc<StdMutex<BTreeMap<StoreShardIdV1, RegisteredSchemaConvergenceStatus>>>,
    tasks: StdMutex<BTreeMap<StoreShardIdV1, JoinHandle<()>>>,
    #[cfg(test)]
    schedule_count: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    gate: StdMutex<Option<Arc<RegisteredSchemaConvergenceTestGateState>>>,
}

impl RegisteredSchemaConvergenceMaintenance {
    fn new() -> Self {
        Self {
            statuses: Arc::new(StdMutex::new(BTreeMap::new())),
            tasks: StdMutex::new(BTreeMap::new()),
            #[cfg(test)]
            schedule_count: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            gate: StdMutex::new(None),
        }
    }

    fn status(&self, shard_id: &StoreShardIdV1) -> Option<RegisteredSchemaConvergenceStatus> {
        self.statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(shard_id)
            .cloned()
    }

    fn defer(&self, shard_id: StoreShardIdV1) {
        self.statuses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(shard_id)
            .or_insert(RegisteredSchemaConvergenceStatus::Pending);
    }

    fn schedule(
        &self,
        database: Arc<RegisteredGlobalDb>,
        convergence: Option<crate::global_db::schema_stages::RegisteredSchemaConvergence>,
    ) {
        let shard_id = database.binding().shard_id.clone();
        {
            let mut statuses = self
                .statuses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if statuses.contains_key(&shard_id) {
                return;
            }
            statuses.insert(shard_id.clone(), RegisteredSchemaConvergenceStatus::Pending);
        }
        #[cfg(test)]
        self.schedule_count.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        let gate = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let statuses = Arc::clone(&self.statuses);
        let task_shard_id = shard_id.clone();
        let task = tokio::spawn(async move {
            #[cfg(test)]
            if let Some(gate) = gate {
                gate.block().await;
            }
            let result = match convergence {
                Some(convergence) => database.converge_schema(convergence).await,
                None => Ok(()),
            };
            if let Err(error) = database.release_connection_memory().await {
                crate::daemon::log_daemon_event(
                    "registered_schema_convergence_memory_release",
                    &[
                        ("outcome", "degraded".to_owned()),
                        ("database", database.db_path().display().to_string()),
                        ("shard", format!("{task_shard_id:?}")),
                        ("error", error.to_string()),
                    ],
                );
            }
            release_process_allocator_memory();
            let status = match result {
                Ok(()) => {
                    crate::daemon::log_daemon_event(
                        "registered_schema_convergence",
                        &[
                            ("outcome", "complete".to_owned()),
                            ("database", database.db_path().display().to_string()),
                            ("shard", format!("{task_shard_id:?}")),
                        ],
                    );
                    RegisteredSchemaConvergenceStatus::Complete
                }
                Err(error) => {
                    let message = error.to_string();
                    crate::daemon::log_daemon_event(
                        "registered_schema_convergence",
                        &[
                            ("outcome", "degraded".to_owned()),
                            ("database", database.db_path().display().to_string()),
                            ("shard", format!("{task_shard_id:?}")),
                            ("error", message.clone()),
                        ],
                    );
                    RegisteredSchemaConvergenceStatus::Degraded { message }
                }
            };
            statuses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(task_shard_id, status);
        });
        self.tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(shard_id, task);
    }

    #[cfg(test)]
    fn install_gate(&self) -> RegisteredSchemaConvergenceTestGate {
        let state = Arc::new(RegisteredSchemaConvergenceTestGateState {
            started: AtomicBool::new(false),
            started_notify: Notify::new(),
            release: Semaphore::new(0),
        });
        *self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&state));
        RegisteredSchemaConvergenceTestGate { state }
    }
}

impl Drop for RegisteredSchemaConvergenceMaintenance {
    fn drop(&mut self) {
        let tasks = self
            .tasks
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, task) in std::mem::take(tasks) {
            task.abort();
        }
    }
}

#[cfg(test)]
struct RegisteredSchemaConvergenceTestGateState {
    started: AtomicBool,
    started_notify: Notify,
    release: Semaphore,
}

#[cfg(test)]
impl RegisteredSchemaConvergenceTestGateState {
    async fn block(&self) {
        self.started.store(true, Ordering::Release);
        self.started_notify.notify_waiters();
        self.release
            .acquire()
            .await
            .expect("registered schema convergence test gate remains open")
            .forget();
    }
}

#[cfg(test)]
struct RegisteredSchemaConvergenceTestGate {
    state: Arc<RegisteredSchemaConvergenceTestGateState>,
}

#[cfg(test)]
impl RegisteredSchemaConvergenceTestGate {
    async fn wait_until_blocked(&self) {
        while !self.state.started.load(Ordering::Acquire) {
            self.state.started_notify.notified().await;
        }
    }

    fn release(&self) {
        self.state.release.add_permits(1);
    }
}

/// One canonical registry and profile pin shared by every daemon session shard.
pub(crate) struct DaemonSessionRuntimeRegistryV1 {
    identity: LocalProfileIdentityAuthorityV1,
    incarnation: StoreIncarnationV1,
    resolver: Arc<LocalStoreRuntimeResolverV1>,
    registry: StoreRuntimeRegistry,
    profile_pin: ProfileAuthorityPin,
    profile_runtime: StoreRuntimeHandle,
    profile_database: Mutex<Option<Arc<RegisteredGlobalDb>>>,
    profile_memory: Mutex<Option<Arc<Database>>>,
    profile_sessions: Mutex<Option<Arc<RegisteredGlobalDb>>>,
    project_memory: Mutex<BTreeMap<ProjectId, Arc<Database>>>,
    project_sessions: Mutex<BTreeMap<ProjectId, Arc<RegisteredGlobalDb>>>,
    registered_schema_convergence: RegisteredSchemaConvergenceMaintenance,
    #[cfg(test)]
    long_lived_session_maintenance_for_test: AtomicBool,
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) async fn close_code_graph_paths(
        &self,
        database_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<()> {
        for database_path in database_paths {
            let closed = self
                .registry
                .close_path(&database_path)
                .await
                .map_err(|error| {
                    session_registry_error(
                        "close registered code-shard runtime",
                        format!("{error:?}"),
                    )
                })?;
            if let Some(closed) = closed {
                self.resolver
                    .retire_code_authority(&closed.binding().shard_id, closed.path())
                    .map_err(|error| {
                        session_registry_error(
                            "retire registered code-shard authority",
                            format!("{error:?}"),
                        )
                    })?;
            }
        }
        Ok(())
    }

    pub(crate) async fn open(identity: LocalProfileIdentityAuthorityV1) -> Result<Self> {
        // The kernel's registry initialises profile- and session-scoped shards
        // through a fail-closed port, because the registered schema lives in
        // `tracedecay-global-db` (which depends on the kernel transitively).
        // This is the sole constructor of the production registry, so it is the
        // one place that must supply the installer.
        super::register_registered_schema_installer();
        crate::automation::register_runtime_ports();
        let incarnation = runtime_incarnation(&identity)?;
        let resolver = Arc::new(LocalStoreRuntimeResolverV1::new(
            LocalProfileStoreAuthorityV1::new(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
                identity.profile_root().to_path_buf(),
            ),
        ));
        let registry_resolver: Arc<dyn super::registry::StoreRuntimeResolver> = resolver.clone();
        let registry =
            StoreRuntimeRegistry::new(registry_resolver, Arc::new(LifecycleShardRuntimePublisher));
        let profile_shard =
            StoreShardIdV1::profile(identity.brain_id().clone(), identity.profile_id().clone());
        let profile_runtime = open_runtime(
            &registry,
            resolver.as_ref(),
            profile_shard.clone(),
            incarnation,
            None,
            None,
            true,
            "mount profile authority store",
        )
        .await?;
        let profile_pin = match registry.profile_authority_pin(&profile_shard) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            outcome => {
                return Err(session_registry_error(
                    "pin profile authority",
                    format!("{outcome:?}"),
                ));
            }
        };
        Ok(Self {
            identity,
            incarnation,
            resolver,
            registry,
            profile_pin,
            profile_runtime,
            profile_database: Mutex::new(None),
            profile_memory: Mutex::new(None),
            profile_sessions: Mutex::new(None),
            project_memory: Mutex::new(BTreeMap::new()),
            project_sessions: Mutex::new(BTreeMap::new()),
            registered_schema_convergence: RegisteredSchemaConvergenceMaintenance::new(),
            #[cfg(test)]
            long_lived_session_maintenance_for_test: AtomicBool::new(false),
        })
    }

    pub(crate) async fn profile_database(&self) -> Result<Arc<RegisteredGlobalDb>> {
        let mut mounted = self.profile_database.lock().await;
        if let Some(database) = mounted.as_ref() {
            return Ok(Arc::clone(database));
        }
        let database = self
            .attach_registered(
                self.profile_runtime.clone(),
                "attach profile authority store",
            )
            .await?;
        *mounted = Some(Arc::clone(&database));
        Ok(database)
    }

    pub(crate) async fn profile_sessions(&self) -> Result<Arc<RegisteredGlobalDb>> {
        let mut mounted = self.profile_sessions.lock().await;
        if let Some(database) = mounted.as_ref() {
            return Ok(Arc::clone(database));
        }
        let shard_id = StoreShardIdV1::profile_sessions(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
        );
        let runtime = open_runtime(
            &self.registry,
            self.resolver.as_ref(),
            shard_id,
            self.incarnation,
            Some(self.profile_pin.clone()),
            None,
            true,
            "mount profile session store",
        )
        .await?;
        let database = self
            .attach_registered(runtime, "mount profile session store")
            .await?;
        *mounted = Some(Arc::clone(&database));
        Ok(database)
    }

    /// Mounts the distinct profile-memory shard through this daemon's pinned
    /// profile registry. `ProfileMemory` never aliases the profile/global
    /// shard, and publication never reopens a filesystem path.
    pub(crate) async fn profile_memory(&self) -> Result<Arc<Database>> {
        let mut mounted = self.profile_memory.lock().await;
        if let Some(database) = mounted.as_ref() {
            return Ok(Arc::clone(database));
        }
        let shard_id = StoreShardIdV1::profile_memory(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
        );
        let runtime = open_runtime(
            &self.registry,
            self.resolver.as_ref(),
            shard_id,
            self.incarnation,
            Some(self.profile_pin.clone()),
            None,
            true,
            "mount profile memory store",
        )
        .await?;
        let database =
            Arc::new(Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await?);
        crate::db::migrations::migrate(database.as_ref()).await?;
        *mounted = Some(Arc::clone(&database));
        Ok(database)
    }

    pub(crate) async fn mounted_session_databases(&self) -> Vec<Arc<RegisteredGlobalDb>> {
        let mut databases = Vec::new();
        if let Some(database) = self.profile_sessions.lock().await.as_ref() {
            databases.push(Arc::clone(database));
        }
        databases.extend(self.project_sessions.lock().await.values().cloned());
        databases
    }

    pub(crate) async fn mounted_project_sessions(
        &self,
        project_id: &ProjectId,
    ) -> Option<Arc<RegisteredGlobalDb>> {
        self.project_sessions.lock().await.get(project_id).cloned()
    }

    pub(crate) async fn project_sessions(
        &self,
        project_id: ProjectId,
        enrollment_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Arc<RegisteredGlobalDb>> {
        self.resolver
            .register_project_authority(LocalProjectEnrollmentAuthorityV1::new(
                project_id.clone(),
                enrollment_roots,
            ))
            .map_err(|error| {
                session_registry_error("register project session authority", format!("{error:?}"))
            })?;
        let mut mounted = self.project_sessions.lock().await;
        if let Some(database) = mounted.get(&project_id) {
            return Ok(Arc::clone(database));
        }
        let shard_id = StoreShardIdV1::project_sessions(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id.clone(),
        );
        let runtime = open_runtime(
            &self.registry,
            self.resolver.as_ref(),
            shard_id,
            self.incarnation,
            Some(self.profile_pin.clone()),
            None,
            true,
            "mount project session store",
        )
        .await?;
        let database = self
            .attach_registered(runtime, "mount project session store")
            .await?;
        mounted.insert(project_id, Arc::clone(&database));
        Ok(database)
    }

    fn long_lived_session_maintenance(&self) -> bool {
        if LONG_LIVED_SESSION_MAINTENANCE.load(Ordering::Relaxed) {
            return true;
        }
        #[cfg(test)]
        if self
            .long_lived_session_maintenance_for_test
            .load(Ordering::Relaxed)
        {
            return true;
        }
        false
    }

    async fn attach_registered(
        &self,
        runtime: StoreRuntimeHandle,
        operation: &'static str,
    ) -> Result<Arc<RegisteredGlobalDb>> {
        let expected_binding: StoreRuntimeBindingV1 = runtime.binding().clone();
        let expected_locator = runtime.locator().verified().clone();
        let authority = runtime
            .database_authority(operation)
            .map_err(|failure| registry_open_error(operation, failure))?;
        let long_lived = self.long_lived_session_maintenance();
        let (database, convergence) = if long_lived {
            RegisteredGlobalDb::migrate_and_attach_for_daemon(
                runtime,
                expected_binding,
                expected_locator,
                authority,
            )
            .await?
        } else {
            (
                RegisteredGlobalDb::migrate_and_attach(
                    runtime,
                    expected_binding,
                    expected_locator,
                    authority,
                )
                .await?,
                None,
            )
        };
        let database = Arc::new(database);
        if long_lived {
            #[cfg(test)]
            if self
                .long_lived_session_maintenance_for_test
                .load(Ordering::Relaxed)
            {
                self.registered_schema_convergence
                    .schedule(Arc::clone(&database), convergence);
                return Ok(database);
            }
            let _ = convergence;
            if let Err(error) = database.release_connection_memory().await {
                crate::daemon::log_daemon_event(
                    "registered_schema_admission_memory_release",
                    &[
                        ("outcome", "degraded".to_owned()),
                        ("database", database.db_path().display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
            }
            release_process_allocator_memory();
            self.registered_schema_convergence
                .defer(database.binding().shard_id.clone());
        }
        Ok(database)
    }

    pub(crate) fn registered_schema_convergence_status(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> Option<RegisteredSchemaConvergenceStatus> {
        self.registered_schema_convergence.status(shard_id)
    }

    #[cfg(test)]
    fn enable_long_lived_session_maintenance_for_test(&self) {
        self.long_lived_session_maintenance_for_test
            .store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn block_registered_schema_convergence_for_test(&self) -> RegisteredSchemaConvergenceTestGate {
        self.registered_schema_convergence.install_gate()
    }

    #[cfg(test)]
    fn registered_schema_convergence_schedule_count_for_test(&self) -> usize {
        self.registered_schema_convergence
            .schedule_count
            .load(Ordering::Relaxed)
    }

    /// Mounts one project graph/memory database through the retained registry.
    ///
    /// The typed project id and enrollment roots authorize the resolver; the
    /// returned database remains cached so migration and live use share one
    /// writer authority.
    pub(crate) async fn project_memory(
        &self,
        project_id: ProjectId,
        enrollment_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Arc<Database>> {
        self.resolver
            .register_project_authority(LocalProjectEnrollmentAuthorityV1::new(
                project_id.clone(),
                enrollment_roots,
            ))
            .map_err(|error| {
                session_registry_error("register project memory authority", format!("{error:?}"))
            })?;
        let mut mounted = self.project_memory.lock().await;
        if let Some(database) = mounted.get(&project_id) {
            return Ok(Arc::clone(database));
        }
        let shard_id = StoreShardIdV1::project(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id.clone(),
        );
        let runtime = open_runtime(
            &self.registry,
            self.resolver.as_ref(),
            shard_id,
            self.incarnation,
            Some(self.profile_pin.clone()),
            None,
            true,
            "mount project memory store",
        )
        .await?;
        let database =
            Arc::new(Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await?);
        crate::db::migrations::migrate(database.as_ref()).await?;
        mounted.insert(project_id, Arc::clone(&database));
        Ok(database)
    }

    /// Mounts an existing project-memory shard without initializing or
    /// migrating it, and exposes only a read-only database facade.
    pub(crate) async fn project_memory_read_only(
        &self,
        project_id: ProjectId,
        enrollment_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Database> {
        self.resolver
            .register_project_authority(LocalProjectEnrollmentAuthorityV1::new(
                project_id.clone(),
                enrollment_roots,
            ))
            .map_err(|error| {
                session_registry_error("register project memory authority", format!("{error:?}"))
            })?;
        if let Some(database) = self.project_memory.lock().await.get(&project_id).cloned() {
            return Database::publish_runtime(
                database.retained_runtime().clone(),
                DatabaseAccessMode::ReadOnly,
            )
            .await;
        }
        let shard_id = StoreShardIdV1::project(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
        );
        let runtime = open_runtime(
            &self.registry,
            self.resolver.as_ref(),
            shard_id,
            self.incarnation,
            Some(self.profile_pin.clone()),
            None,
            false,
            "mount project memory store read-only",
        )
        .await?;
        Database::publish_runtime(runtime, DatabaseAccessMode::ReadOnly).await
    }

    pub(crate) async fn code_graph(
        &self,
        shard_id: StoreShardIdV1,
        database_path: PathBuf,
        database_authority: DatabaseAuthority,
    ) -> Result<StoreRuntimeHandle> {
        let initialize_if_missing = !matches!(
            &shard_id.scope,
            tracedecay_store::StoreShardScopeV1::Code {
                scope: CodeShardScopeV1::Snapshot { .. },
                ..
            }
        );
        self.code_graph_with_authority(
            shard_id,
            database_path,
            Some(database_authority),
            initialize_if_missing,
        )
        .await
    }

    async fn code_graph_with_authority(
        &self,
        shard_id: StoreShardIdV1,
        database_path: PathBuf,
        mut database_authority: Option<DatabaseAuthority>,
        initialize_if_missing: bool,
    ) -> Result<StoreRuntimeHandle> {
        self.resolver
            .register_code_authority(
                LocalCodeStoreAuthorityV1::new(shard_id.clone(), database_path.clone()).map_err(
                    |error| {
                        session_registry_error(
                            "construct code-shard authority",
                            format!("{error:?}"),
                        )
                    },
                )?,
            )
            .map_err(|error| {
                session_registry_error("register code-shard authority", format!("{error:?}"))
            })?;
        if database_authority.is_none() && !initialize_if_missing {
            let key = StoreRuntimeKey::new(shard_id.clone(), self.incarnation);
            if let Some(runtime) = self.registry.retained_runtime_for_read(&key) {
                if runtime.locator().path() != database_path {
                    return Err(session_registry_error(
                        "reuse read-only code-shard runtime",
                        "retained runtime locator differs from the registered database path"
                            .to_owned(),
                    ));
                }
                return Ok(runtime);
            }
            database_authority = Some(DatabaseAuthority::for_owned_runtime(
                &database_path,
                "publish registered read-only code-shard runtime",
            )?);
        }
        open_runtime(
            &self.registry,
            self.resolver.as_ref(),
            shard_id,
            self.incarnation,
            Some(self.profile_pin.clone()),
            database_authority,
            initialize_if_missing,
            "mount code-shard store",
        )
        .await
    }

    /// Mounts the mutable graph for this exact project/repository/worktree
    /// identity. The checkout path is used only by the Git identity authority;
    /// it is never itself the shard identity.
    pub(crate) async fn code_graph_worktree(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        database_path: PathBuf,
        database_authority: DatabaseAuthority,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        // Mutable graph storage exists for non-Git projects too. Its structural
        // shard identity needs only the stable repository/worktree components;
        // resolving HEAD here would incorrectly make ordinary project open
        // depend on a Git repository being present.
        let repository_id =
            crate::daemon::code_index_scheduler::identity::repository_id_for(project_root)
                .map_err(|error| {
                    session_registry_error("resolve code-shard repository", error.to_string())
                })?;
        let worktree_id =
            crate::daemon::code_index_scheduler::identity::worktree_id_for(project_root).map_err(
                |error| session_registry_error("resolve code-shard worktree", error.to_string()),
            )?;
        let shard_id = StoreShardIdV1::code(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
            repository_id,
            CodeShardScopeV1::Worktree { worktree_id },
        );
        let runtime = self
            .code_graph_with_authority(
                shard_id,
                database_path,
                Some(database_authority),
                matches!(access, DatabaseAccessMode::ReadWrite),
            )
            .await?;
        Database::publish_runtime(runtime, access).await
    }

    /// Mounts the mutable graph for an exact named Git ref in this worktree.
    /// The ref is normalized to its full `refs/heads/*` identity before it
    /// enters the shard key.
    pub(crate) async fn code_graph_branch(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        branch_name: &str,
        database_path: PathBuf,
        database_authority: DatabaseAuthority,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        self.code_graph_branch_with_authority(
            project_root,
            project_id,
            branch_name,
            database_path,
            Some(database_authority),
            access,
        )
        .await
    }

    pub(crate) async fn code_graph_branch_registered(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        branch_name: &str,
        database_path: PathBuf,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        self.code_graph_branch_with_authority(
            project_root,
            project_id,
            branch_name,
            database_path,
            None,
            access,
        )
        .await
    }

    async fn code_graph_branch_with_authority(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        branch_name: &str,
        database_path: PathBuf,
        database_authority: Option<DatabaseAuthority>,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        let identity = crate::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(
            project_root,
        )
        .map_err(|error| {
            session_registry_error("resolve code-branch identity", error.to_string())
        })?;
        let ref_name = if branch_name.starts_with("refs/heads/") {
            branch_name.to_owned()
        } else if branch_name.starts_with("refs/") {
            return Err(session_registry_error(
                "construct code-branch ref identity",
                "branch ref must be under refs/heads/".to_owned(),
            ));
        } else {
            format!("refs/heads/{branch_name}")
        };
        let ref_id = RefId::new(ref_name).map_err(|error| {
            session_registry_error("construct code-branch ref identity", error.to_string())
        })?;
        let shard_id = StoreShardIdV1::code(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
            identity.repository_id().clone(),
            CodeShardScopeV1::Branch {
                worktree_id: identity.worktree_id().clone(),
                ref_id,
            },
        );
        let runtime = self
            .code_graph_with_authority(
                shard_id,
                database_path,
                database_authority,
                matches!(access, DatabaseAccessMode::ReadWrite),
            )
            .await?;
        Database::publish_runtime(runtime, access).await
    }

    /// Mounts an immutable graph generation for cross-branch comparison. A
    /// snapshot identity is caller-supplied from durable branch/generation
    /// truth; the current worktree identity is still resolved and bound here.
    pub(crate) async fn code_graph_snapshot(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        snapshot_id: StoreSnapshotIdV1,
        database_path: PathBuf,
        database_authority: DatabaseAuthority,
    ) -> Result<Database> {
        let identity = crate::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(
            project_root,
        )
        .map_err(|error| {
            session_registry_error("resolve code-snapshot identity", error.to_string())
        })?;
        let shard_id = StoreShardIdV1::code(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
            identity.repository_id().clone(),
            CodeShardScopeV1::Snapshot {
                worktree_id: Some(identity.worktree_id().clone()),
                snapshot_id,
            },
        );
        let runtime = self
            .code_graph(shard_id, database_path, database_authority)
            .await?;
        Database::publish_runtime(runtime, DatabaseAccessMode::ReadOnly).await
    }
}

impl ProfileRuntime for DaemonSessionRuntimeRegistryV1 {
    fn profile_sessions(&self) -> RuntimeFuture<'_, Arc<RegisteredGlobalDb>> {
        Box::pin(DaemonSessionRuntimeRegistryV1::profile_sessions(self))
    }

    fn open_user_memory_db(&self) -> RuntimeFuture<'_, Database> {
        Box::pin(crate::memory::user::open_user_memory_db(self))
    }

    fn curate_user_memory<'a>(
        &'a self,
        profile_root: &'a Path,
        automation_root: &'a Path,
        options: &'a AgentMemoryCurateOptions,
    ) -> RuntimeFuture<'a, serde_json::Value> {
        Box::pin(async move {
            let memory_db = crate::memory::user::open_user_memory_db(self).await?;
            let options = crate::dashboard::memory_curate::MemoryCurateOptions {
                apply: options.apply,
                llm: options.llm,
                llm_ops: options.llm_ops.clone(),
                max_clusters: options.max_clusters,
                min_confidence: options.min_confidence,
            };
            crate::dashboard::memory_curate::run_user_memory_curate(
                &memory_db,
                memory_db.database_path(),
                profile_root,
                automation_root,
                &options,
            )
            .await
        })
    }
}

fn runtime_incarnation(identity: &LocalProfileIdentityAuthorityV1) -> Result<StoreIncarnationV1> {
    let process_run_id = crate::runtime_identity::process_run_id();
    let daemon_generation = crate::daemon::authority::current_record(identity.profile_root())?
        .filter(|record| {
            record.process_run_id == process_run_id
                && record.profile_root == identity.profile_root()
                && record.brain_id.as_ref() == Some(identity.brain_id())
                && record.profile_id.as_ref() == Some(identity.profile_id())
        })
        .map(|record| record.epoch);
    let generation = match daemon_generation {
        Some(generation) => generation,
        None => process_runtime_generation(process_run_id).ok_or_else(|| {
            session_registry_error(
                "create store incarnation",
                "process runtime generation has an unsupported format".to_owned(),
            )
        })?,
    };
    StoreIncarnationV1::new(generation)
        .map_err(|error| session_registry_error("create store incarnation", error.to_string()))
}

fn process_runtime_generation(process_run_id: &str) -> Option<u64> {
    let raw = process_run_id
        .get(..16)
        .and_then(|prefix| u64::from_str_radix(prefix, 16).ok())
        .or_else(|| {
            process_run_id
                .strip_prefix("mcp-")
                .and_then(|value| value.parse::<u64>().ok())
                .map(|timestamp| timestamp ^ u64::from(std::process::id()))
        })?;
    Some((raw & i64::MAX as u64).max(1))
}

async fn open_runtime(
    registry: &StoreRuntimeRegistry,
    resolver: &LocalStoreRuntimeResolverV1,
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    profile_pin: Option<ProfileAuthorityPin>,
    database_authority: Option<DatabaseAuthority>,
    initialize_if_missing: bool,
    operation: &'static str,
) -> Result<StoreRuntimeHandle> {
    let key = StoreRuntimeKey::new(shard_id.clone(), incarnation);
    let locator = match resolver.resolve_key(&key) {
        LocalStoreLocatorResolutionV1::Resolved(locator) => locator,
        LocalStoreLocatorResolutionV1::Unavailable(unavailable) => {
            return Err(session_registry_error(
                operation,
                format!(
                    "registered store locator unavailable: {:?}",
                    unavailable.reason
                ),
            ));
        }
    };
    let authority = match database_authority {
        Some(authority) => authority,
        None => DatabaseAuthority::for_runtime(locator.locator().path(), operation)?,
    };
    if authority.canonical_database_path() != locator.locator().path() {
        return Err(session_registry_error(
            operation,
            format!(
                "registered locator {} does not match originating database authority {}",
                locator.locator().path().display(),
                authority.canonical_database_path().display()
            ),
        ));
    }
    let exists = locator
        .locator()
        .path()
        .try_exists()
        .map_err(|error| session_registry_error(operation, error.to_string()))?;
    let request = if initialize_if_missing && !exists {
        StoreRuntimeOpenRequest::new_initialize_authorized(
            shard_id,
            incarnation,
            profile_pin,
            authority,
        )
    } else {
        StoreRuntimeOpenRequest::new_authorized(shard_id, incarnation, profile_pin, authority)
    };
    match registry.open(request).await {
        StoreRuntimeOpenResult::Published(runtime) => Ok(runtime),
        StoreRuntimeOpenResult::Failed(failure) => Err(registry_open_error(
            "open registered session runtime",
            failure,
        )),
    }
}

fn registry_open_error(
    operation: &'static str,
    failure: StoreRuntimeRegistryFailure,
) -> TraceDecayError {
    session_registry_error(operation, format!("{failure:?}"))
}

fn session_registry_error(operation: &'static str, message: String) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::engine::{Executor, TestConnection};

    async fn project_sessions_pending_convergence(
        project_name: &str,
    ) -> (
        tempfile::TempDir,
        LocalProfileIdentityAuthorityV1,
        ProjectId,
        PathBuf,
        PathBuf,
    ) {
        let temporary = tempfile::tempdir().expect("temporary project parent");
        let root = temporary
            .path()
            .canonicalize()
            .expect("canonical fixture root");
        let profile_root = root.join("profile");
        let project_root = root.join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let project_id = ProjectId::new(project_name).expect("typed project identity");
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .expect("project enrollment");
        let sessions_path =
            crate::storage::profile_sharded_data_root(identity.profile_root(), project_id.as_str())
                .join(crate::storage::SESSIONS_DB_FILENAME);
        std::fs::create_dir_all(sessions_path.parent().expect("session database parent"))
            .expect("session database directory");
        let connection = TestConnection::open(&sessions_path);
        crate::global_db::ensure_registered_schema(&connection)
            .await
            .expect("seed complete registered schema");
        connection
            .execute("DELETE FROM authority_audit_checkpoints", ())
            .await
            .expect("remove durable convergence checkpoint");
        drop(connection);
        (temporary, identity, project_id, project_root, sessions_path)
    }

    async fn wait_for_schema_convergence(
        registry: &DaemonSessionRuntimeRegistryV1,
        shard_id: &StoreShardIdV1,
    ) -> RegisteredSchemaConvergenceStatus {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(status) = registry.registered_schema_convergence_status(shard_id)
                    && !matches!(status, RegisteredSchemaConvergenceStatus::Pending)
                {
                    return status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("registered schema convergence must reach a terminal state")
    }

    #[test]
    fn fallback_runtime_generation_always_fits_sqlite_integer() {
        assert_eq!(
            process_runtime_generation("ffffffffffffffff0000000000000000"),
            Some(i64::MAX as u64)
        );
        assert_eq!(
            process_runtime_generation("00000000000000000000000000000000"),
            Some(1)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn daemon_restart_fences_the_previous_session_runtime_binding() {
        let temporary = tempfile::tempdir().expect("temporary profile parent");
        let profile_root = temporary.path().join("profile");
        #[cfg(unix)]
        let endpoint = crate::daemon::transport::DaemonEndpoint::Unix(
            profile_root.join("session-runtime.sock"),
        );
        #[cfg(not(unix))]
        let endpoint = crate::daemon::transport::default_loopback_endpoint();

        let first_authority =
            crate::daemon::authority::DaemonAuthority::acquire(&profile_root, &endpoint, "test")
                .expect("first daemon authority");
        let first_database_scope = crate::db::enter_daemon_database_scope(
            &profile_root,
            first_authority.record().epoch,
            "first session runtime registry",
        )
        .expect("first daemon database scope");
        let identity = first_authority.profile_identity().clone();
        let first_registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
            .await
            .expect("first session runtime registry");
        let stale = first_registry.profile_runtime.binding().clone();
        assert_eq!(
            stale.incarnation.get(),
            first_authority.record().epoch,
            "the durable daemon generation must own the store incarnation"
        );
        drop(first_registry);
        drop(first_database_scope);
        drop(first_authority);

        let second_authority =
            crate::daemon::authority::DaemonAuthority::acquire(&profile_root, &endpoint, "test")
                .expect("successor daemon authority");
        let _second_database_scope = crate::db::enter_daemon_database_scope(
            &profile_root,
            second_authority.record().epoch,
            "successor session runtime registry",
        )
        .expect("successor daemon database scope");
        let second_registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("successor session runtime registry");
        let current = second_registry.profile_runtime.binding();

        assert_eq!(current.incarnation.get(), second_authority.record().epoch);
        assert!(current.incarnation > stale.incarnation);
        assert!(matches!(
            second_registry.registry.lookup(&stale),
            super::super::registry::StoreRuntimeLookup::WrongIncarnation {
                expected,
                actual,
            } if *expected == stale && actual.as_ref() == current
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn existing_profile_memory_is_migrated_before_exposure() {
        let temporary = tempfile::tempdir().expect("temporary profile parent");
        let profile_root = temporary.path().join("profile");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let memory_path = crate::memory::user::user_memory_db_path(identity.profile_root());
        let seed = TestConnection::open(&memory_path);
        crate::db::migrations::migrate_test_connection_to_version(&seed, 22)
            .await
            .expect("migrate profile memory fixture through production v22");
        drop(seed);

        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        let database = registry
            .profile_memory()
            .await
            .expect("migrated profile memory");
        let mut rows = database
            .conn()
            .query(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'memory_v2_fact_relations'",
                (),
            )
            .await
            .expect("query migrated profile memory schema");
        let table_count: i64 = rows
            .next()
            .await
            .expect("read schema row")
            .expect("schema count row")
            .get(0)
            .expect("decode schema count");

        assert_eq!(table_count, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn profile_sessions_mount_uses_the_durable_profile_identity_and_profile_pin() {
        let temporary = tempfile::tempdir().expect("temporary profile parent");
        let profile_root = temporary.path().join("profile");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let user_sessions_path = crate::sessions::user_sessions_db_path(identity.profile_root());

        let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
            .await
            .expect("session runtime registry");
        let registered = registry
            .profile_sessions()
            .await
            .expect("registered profile sessions");

        assert_eq!(
            &registered.binding().shard_id,
            &StoreShardIdV1::profile_sessions(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
            )
        );
        assert_eq!(registered.db_path(), user_sessions_path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn profile_sessions_mount_rejects_incompatible_schema_through_registered_runtime() {
        let temporary = tempfile::tempdir().expect("temporary profile parent");
        let profile_root = temporary.path().join("profile");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let seed_registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
            .await
            .expect("schema seed runtime registry");
        let seeded_sessions = seed_registry
            .profile_sessions()
            .await
            .expect("seed registered profile sessions");
        seeded_sessions
            .writer_connection()
            .expect("schema corruption writer")
            .execute_batch(
                "DROP TABLE sessions;
                 CREATE TABLE sessions(provider TEXT NOT NULL);",
            )
            .await
            .expect("replace required session table with an incompatible shape");
        drop(seeded_sessions);
        drop(seed_registry);

        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        let error = match registry.profile_sessions().await {
            Ok(_) => panic!("incompatible registered schema must fail closed"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("no such column: project_key")
                && error.to_string().contains("initialize transcript schema"),
            "unexpected mount error: {error}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn project_sessions_mount_uses_typed_enrollment_and_is_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary project parent");
        let root = temporary
            .path()
            .canonicalize()
            .expect("canonical fixture root");
        let profile_root = root.join("profile");
        let project_root = root.join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let project_id = ProjectId::new("project.session-runtime").expect("typed project identity");
        crate::storage::write_enrollment_marker(
            &project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .expect("project enrollment");
        let sessions_path =
            crate::storage::profile_sharded_data_root(identity.profile_root(), project_id.as_str())
                .join(crate::storage::SESSIONS_DB_FILENAME);

        let registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
            .await
            .expect("session runtime registry");
        let first = registry
            .project_sessions(project_id.clone(), [project_root.clone()])
            .await
            .expect("registered project sessions");
        let second = registry
            .project_sessions(project_id.clone(), [project_root])
            .await
            .expect("idempotent project sessions");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            &first.binding().shard_id,
            &StoreShardIdV1::project_sessions(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
                project_id,
            )
        );
        assert_eq!(first.db_path(), sessions_path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn daemon_admission_returns_while_historical_convergence_is_blocked() {
        let (_temporary, identity, project_id, project_root, _sessions_path) =
            project_sessions_pending_convergence("project.schema-admission").await;
        let shard_id = StoreShardIdV1::project_sessions(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            project_id.clone(),
        );
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        registry.enable_long_lived_session_maintenance_for_test();
        let convergence_gate = registry.block_registered_schema_convergence_for_test();

        let database = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            registry.project_sessions(project_id, [project_root]),
        )
        .await
        .expect("daemon admission must not wait for historical convergence")
        .expect("registered project sessions");
        convergence_gate.wait_until_blocked().await;

        assert_eq!(
            registry.registered_schema_convergence_status(&shard_id),
            Some(RegisteredSchemaConvergenceStatus::Pending)
        );
        let snapshot = database
            .read_snapshot()
            .await
            .expect("ordinary read snapshot while convergence is pending");
        let mut rows = snapshot
            .query("SELECT COUNT(*) FROM sessions", ())
            .await
            .expect("ordinary read while convergence is pending");
        assert_eq!(
            rows.next()
                .await
                .expect("read session count")
                .expect("session count row")
                .get::<i64>(0)
                .expect("decode session count"),
            0
        );
        convergence_gate.release();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplicate_project_attaches_schedule_one_historical_convergence() {
        let (_temporary, identity, project_id, project_root, _sessions_path) =
            project_sessions_pending_convergence("project.schema-deduplication").await;
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        registry.enable_long_lived_session_maintenance_for_test();
        let convergence_gate = registry.block_registered_schema_convergence_for_test();

        let first = registry
            .project_sessions(project_id.clone(), [project_root.clone()])
            .await
            .expect("first project session attach");
        convergence_gate.wait_until_blocked().await;
        let second = registry
            .project_sessions(project_id, [project_root])
            .await
            .expect("duplicate project session attach");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            registry.registered_schema_convergence_schedule_count_for_test(),
            1,
            "the retained registry must deduplicate convergence tasks"
        );
        convergence_gate.release();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_convergence_commits_the_durable_authority_checkpoint() {
        let (_temporary, identity, project_id, project_root, _sessions_path) =
            project_sessions_pending_convergence("project.schema-checkpoint").await;
        let shard_id = StoreShardIdV1::project_sessions(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            project_id.clone(),
        );
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        registry.enable_long_lived_session_maintenance_for_test();
        let database = registry
            .project_sessions(project_id, [project_root])
            .await
            .expect("registered project sessions");

        assert_eq!(
            wait_for_schema_convergence(&registry, &shard_id).await,
            RegisteredSchemaConvergenceStatus::Complete
        );
        let snapshot = database
            .read_snapshot()
            .await
            .expect("checkpoint read snapshot");
        let mut rows = snapshot
            .query(
                "SELECT bounded_passes_since_exhaustive
                 FROM authority_audit_checkpoints
                 WHERE audit_name = 'observation-authority'",
                (),
            )
            .await
            .expect("read durable authority checkpoint");
        assert_eq!(
            rows.next()
                .await
                .expect("read checkpoint")
                .expect("durable checkpoint row")
                .get::<i64>(0)
                .expect("decode checkpoint"),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_convergence_failure_remains_observable_as_degraded() {
        let (_temporary, identity, project_id, project_root, sessions_path) =
            project_sessions_pending_convergence("project.schema-degraded").await;
        rusqlite::Connection::open(&sessions_path)
            .expect("open corruption fixture")
            .execute_batch(
                "DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;
                 DROP TRIGGER IF EXISTS session_query_cursor_keys_retire_update_v1;
                 DROP TRIGGER IF EXISTS session_query_cursor_keys_rotate_insert_v1;
                 INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES
                    ('cursor-a', 1, X'01', 100, NULL),
                    ('cursor-b', 2, X'02', 200, NULL);",
            )
            .expect("seed corruption behind missing guards");
        let shard_id = StoreShardIdV1::project_sessions(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            project_id.clone(),
        );
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        registry.enable_long_lived_session_maintenance_for_test();

        registry
            .project_sessions(project_id, [project_root])
            .await
            .expect("minimum schema admission remains available");
        let status = wait_for_schema_convergence(&registry, &shard_id).await;

        assert!(
            matches!(
                status,
                RegisteredSchemaConvergenceStatus::Degraded { ref message }
                    if message.contains("session cursor key rotation state is invalid")
            ),
            "unexpected convergence status: {status:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cached_project_sessions_reject_conflicting_enrollment_authority() {
        let temporary = tempfile::tempdir().expect("temporary project parent");
        let root = temporary
            .path()
            .canonicalize()
            .expect("canonical fixture root");
        let profile_root = root.join("profile");
        let first_project_root = root.join("project");
        let conflicting_project_root = root.join("conflicting-project");
        std::fs::create_dir_all(&first_project_root).expect("project root");
        std::fs::create_dir_all(&conflicting_project_root).expect("conflicting project root");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let project_id = ProjectId::new("project.session-runtime").expect("typed project identity");
        crate::storage::write_enrollment_marker(
            &first_project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .expect("project enrollment");

        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        registry
            .project_sessions(project_id.clone(), [first_project_root])
            .await
            .expect("registered project sessions");
        let error = match registry
            .project_sessions(project_id, [conflicting_project_root])
            .await
        {
            Ok(_) => panic!("conflicting project enrollment authority must fail closed"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("DuplicateProjectAuthority"),
            "unexpected authority error: {error}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worktree_graph_mount_does_not_require_git() {
        let temporary = tempfile::tempdir().expect("temporary project parent");
        let root = temporary
            .path()
            .canonicalize()
            .expect("canonical fixture root");
        let profile_root = root.join("profile");
        let project_root = root.join("project");
        std::fs::create_dir_all(&project_root).expect("non-git project root");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        let database_path = profile_root.join("stores/non-git-worktree.db");
        std::fs::create_dir_all(database_path.parent().expect("database parent"))
            .expect("database directory");
        let authority =
            DatabaseAuthority::acquire_test(&database_path, "non-git worktree graph mount")
                .expect("database authority");

        let database = registry
            .code_graph_worktree(
                &project_root,
                ProjectId::new("project.non-git-worktree").expect("project id"),
                database_path.clone(),
                authority,
                DatabaseAccessMode::ReadWrite,
            )
            .await
            .expect("non-git graph runtime");

        assert_eq!(database.database_path(), database_path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn code_database_replacement_rebinds_after_runtime_retirement() {
        let temporary = tempfile::tempdir().expect("temporary project parent");
        let root = temporary
            .path()
            .canonicalize()
            .expect("canonical fixture root");
        let profile_root = root.join("profile");
        let project_root = root.join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let _database_scope =
            crate::db::enter_daemon_database_scope(&profile_root, 12, "code replacement")
                .expect("daemon database scope");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        let database_path = profile_root.join("stores/replaced-worktree.db");
        std::fs::create_dir_all(database_path.parent().expect("database parent"))
            .expect("database directory");
        let project_id = ProjectId::new("project.replaced-worktree").expect("project id");
        let authority =
            DatabaseAuthority::for_runtime(&database_path, "publish original code database")
                .expect("original database authority");
        let database = registry
            .code_graph_worktree(
                &project_root,
                project_id.clone(),
                database_path.clone(),
                authority,
                DatabaseAccessMode::ReadWrite,
            )
            .await
            .expect("original code database");
        database
            .checkpoint()
            .await
            .expect("checkpoint original database");
        drop(database);

        registry
            .close_code_graph_paths([database_path.clone()])
            .await
            .expect("retire original code runtime before replacement");
        let preserved_path = database_path.with_extension("db.preserved");
        std::fs::rename(&database_path, &preserved_path).expect("preserve original database");
        rusqlite::Connection::open(&database_path)
            .expect("create replacement database")
            .execute_batch("CREATE TABLE replacement(value INTEGER);")
            .expect("seed replacement database");

        let rebound = registry
            .code_graph_worktree(
                &project_root,
                project_id,
                database_path.clone(),
                DatabaseAuthority::for_runtime(
                    &database_path,
                    "publish replacement after retirement",
                )
                .expect("rebound database authority"),
                DatabaseAccessMode::ReadWrite,
            )
            .await
            .expect("replacement code database must publish after retirement");
        assert_eq!(rebound.database_path(), database_path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_only_branch_reuses_daemon_publication_without_write_authority() {
        let temporary = tempfile::tempdir().expect("temporary project parent");
        let root = temporary
            .path()
            .canonicalize()
            .expect("canonical fixture root");
        let profile_root = root.join("profile");
        let project_root = root.join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        gix::init(&project_root).expect("initialize project repository");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let database_scope =
            crate::db::enter_daemon_database_scope(&profile_root, 11, "branch publication")
                .expect("daemon database scope");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        let project_id = ProjectId::new("project.branch-publication").expect("project id");
        let branch_root = profile_root.join("projects/project.branch-publication/branches");
        std::fs::create_dir_all(&branch_root).expect("branch database directory");
        let main_path = branch_root.join("main.db");
        let unpublished_path = branch_root.join("unpublished.db");
        rusqlite::Connection::open(&unpublished_path)
            .expect("seed unpublished branch database")
            .execute_batch("CREATE TABLE seed(value INTEGER);")
            .expect("seed unpublished branch schema");

        let main_authority =
            DatabaseAuthority::for_runtime(&main_path, "publish daemon-owned branch")
                .expect("daemon branch authority");
        assert_eq!(
            main_authority.role(),
            crate::db::DatabaseAuthorityRole::Daemon
        );
        let main = registry
            .code_graph_branch(
                &project_root,
                project_id.clone(),
                "main",
                main_path.clone(),
                main_authority,
                DatabaseAccessMode::ReadWrite,
            )
            .await
            .expect("daemon-owned branch publication");
        let publication_id = main.retained_runtime().publication().publication_id.clone();
        let unpublished_authority =
            DatabaseAuthority::for_runtime(&unpublished_path, "reserve unpublished branch")
                .expect("unpublished daemon branch authority");
        drop(database_scope);

        let read_only = registry
            .code_graph_branch_registered(
                &project_root,
                project_id.clone(),
                "main",
                main_path.clone(),
                DatabaseAccessMode::ReadOnly,
            )
            .await
            .expect("read-only facade over retained daemon publication");
        assert_eq!(read_only.database_path(), main_path);
        assert_eq!(
            read_only.retained_runtime().publication().publication_id,
            publication_id,
            "read-only publication must reuse the exact retained runtime"
        );
        let write_error = match read_only
            .begin_write_transaction("write through read-only branch facade")
            .await
        {
            Ok(_) => panic!("read-only branch facade unexpectedly admitted a write"),
            Err(error) => error,
        };
        assert!(
            write_error.to_string().contains("read-only"),
            "unexpected read-only denial: {write_error}"
        );

        let unpublished_error = match registry
            .code_graph_branch_registered(
                &project_root,
                project_id,
                "unpublished",
                unpublished_path,
                DatabaseAccessMode::ReadOnly,
            )
            .await
        {
            Ok(_) => panic!("unpublished branch inherited synthetic write authority"),
            Err(error) => error,
        };
        assert!(
            unpublished_error
                .to_string()
                .contains("managed-daemon or exclusive-maintenance authority"),
            "unexpected unpublished branch denial: {unpublished_error}"
        );
        drop(unpublished_authority);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_only_worktree_mount_never_recreates_a_deleted_database() {
        let temporary = tempfile::tempdir().expect("temporary project parent");
        let root = temporary
            .path()
            .canonicalize()
            .expect("canonical fixture root");
        let profile_root = root.join("profile");
        let project_root = root.join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        gix::init(&project_root).expect("initialize project repository");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        let database_path = profile_root.join("stores/worktree.db");
        std::fs::create_dir_all(database_path.parent().expect("database parent"))
            .expect("database directory");
        rusqlite::Connection::open(&database_path)
            .expect("seed worktree database")
            .execute_batch("CREATE TABLE seed(value INTEGER);")
            .expect("seed worktree schema");
        assert!(database_path.exists(), "lifecycle existence precheck");
        std::fs::remove_file(&database_path).expect("delete after lifecycle existence check");
        let database_authority =
            DatabaseAuthority::acquire_test(&database_path, "read-only deletion race")
                .expect("database authority");
        let result = registry
            .code_graph_worktree(
                &project_root,
                ProjectId::new("project.read-only-race").expect("project id"),
                database_path.clone(),
                database_authority,
                DatabaseAccessMode::ReadOnly,
            )
            .await;

        assert!(
            result.is_err(),
            "read-only mount must fail for a deleted DB"
        );
        assert!(
            !database_path.exists(),
            "read-only mount recreated the deleted worktree DB"
        );
    }
}

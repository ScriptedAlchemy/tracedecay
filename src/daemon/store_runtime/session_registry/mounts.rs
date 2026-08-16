use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use std::sync::atomic::AtomicBool;
use tokio::sync::Mutex;
use tracedecay_application::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
use tracedecay_domain::{BrainNodeId, EnrollmentGrantV1};
use tracedecay_global_db::session_temporal::relations::SessionRelationScope;
use tracedecay_graph_db::{GraphDbRegistry, GraphDbRegistryConfig};
use tracedecay_rusqlite_runtime::remote::{
    RemoteRecoverySqliteAuthorityV1, RemoteSpoolKeyV1, RemoteSpoolKeyringV1,
    RemoteSqliteStorageErrorV1, RemoteSqliteStorageV1,
};
use tracedecay_store::{ProjectId, StoreShardIdV1};

use super::remote_recovery::{
    DaemonRemoteRecoveryPhysicalEffectsV1, RemoteRecoveryPublicationContextV1,
};
use super::{
    DaemonSessionRuntimeRegistryV1, Database, DatabaseAccessMode, LifecycleShardRuntimePublisher,
    LocalProfileIdentityAuthorityV1, LocalProfileStoreAuthorityV1,
    LocalProjectEnrollmentAuthorityV1, LocalStoreRuntimeResolverV1, ProfileAuthorityPinResult,
    RegisteredGlobalDb, RegisteredSchemaConvergenceMaintenance, Result, RetainedHookTasks,
    RetainedMemoryGraphReconciliationTasksV1, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
    StoreRuntimeRegistry, StoreRuntimeResolver, open_runtime, open_runtime_with_presence,
    register_registered_schema_installer, registry_open_error, runtime_incarnation,
    session_registry_error,
};

struct UnavailableRemoteSpoolKeyringV1;

impl RemoteSpoolKeyringV1 for UnavailableRemoteSpoolKeyringV1 {
    fn active_key(&self) -> std::result::Result<Arc<RemoteSpoolKeyV1>, RemoteSqliteStorageErrorV1> {
        Err(RemoteSqliteStorageErrorV1::Unavailable)
    }

    fn key(
        &self,
        _revision: u64,
    ) -> std::result::Result<Option<Arc<RemoteSpoolKeyV1>>, RemoteSqliteStorageErrorV1> {
        Err(RemoteSqliteStorageErrorV1::Unavailable)
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) async fn open(identity: LocalProfileIdentityAuthorityV1) -> Result<Self> {
        // `main` marks long-lived processes before any registry opens, so the
        // process mode is a construction-time fact, not a mutable runtime flag.
        let long_lived =
            super::LONG_LIVED_SESSION_MAINTENANCE.load(std::sync::atomic::Ordering::Relaxed);
        Self::open_with_session_maintenance(identity, long_lived).await
    }

    /// Constructor with an explicit session-maintenance policy. Production
    /// enters through [`Self::open`]; tests exercising convergence pass
    /// `true` directly so the same gate runs without a mutable side channel.
    pub(crate) async fn open_with_session_maintenance(
        identity: LocalProfileIdentityAuthorityV1,
        long_lived_session_maintenance: bool,
    ) -> Result<Self> {
        let project_runtime_capacity = NonZeroUsize::new(
            super::DEFAULT_RETAINED_PROJECT_RUNTIME_CAPACITY,
        )
        .ok_or_else(|| {
            session_registry_error(
                "configure retained project runtime capacity",
                "default retained project runtime capacity is zero".to_owned(),
            )
        })?;
        Self::open_with_session_maintenance_and_retention_capacity(
            identity,
            long_lived_session_maintenance,
            project_runtime_capacity,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn open_with_retention_capacity_for_test(
        identity: LocalProfileIdentityAuthorityV1,
        project_runtime_capacity: usize,
    ) -> Result<Self> {
        let project_runtime_capacity =
            NonZeroUsize::new(project_runtime_capacity).ok_or_else(|| {
                session_registry_error(
                    "configure retained project runtime capacity",
                    "test retained project runtime capacity must be greater than zero".to_owned(),
                )
            })?;
        Self::open_with_session_maintenance_and_retention_capacity(
            identity,
            false,
            project_runtime_capacity,
        )
        .await
    }

    async fn open_with_session_maintenance_and_retention_capacity(
        identity: LocalProfileIdentityAuthorityV1,
        long_lived_session_maintenance: bool,
        project_runtime_capacity: NonZeroUsize,
    ) -> Result<Self> {
        let remote_credential_authority = Arc::new(
            crate::daemon::remote_protocol::DaemonRemoteCredentialAuthorityV1::new(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
            ),
        );
        let remote_replay_transaction = Arc::new(
            crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1::new(
                tokio::runtime::Handle::current(),
            )
            .map_err(|error| {
                session_registry_error("start remote replay transaction authority", error)
            })?,
        );
        // The kernel's registry initialises profile- and session-scoped shards
        // through a fail-closed port, because the registered schema lives in
        // `tracedecay-global-db` (which depends on the kernel transitively).
        // This is the sole constructor of the production registry, so it is the
        // one place that must supply the installer.
        register_registered_schema_installer();
        // `main` registers these for every real invocation; this constructor
        // is also reached by embedded and integration-test runtimes that never
        // pass through it, and transcript ingest starts here. Idempotent.
        crate::register_runtime_ports()?;
        let incarnation = runtime_incarnation(&identity)?;
        let resolver = Arc::new(LocalStoreRuntimeResolverV1::new(
            LocalProfileStoreAuthorityV1::new(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
                identity.profile_root().to_path_buf(),
            ),
        ));
        let registry_resolver: Arc<dyn StoreRuntimeResolver> = resolver.clone();
        let registry =
            StoreRuntimeRegistry::new(registry_resolver, Arc::new(LifecycleShardRuntimePublisher));
        let graph_manifest_provider =
            Arc::new(super::code_graph_manifest::DaemonCodeGraphManifestProviderV1::default());
        let graph_registry = GraphDbRegistry::new_with_manifest_provider(
            GraphDbRegistryConfig {
                max_open: super::RETAINED_SESSION_GRAPH_RUNTIME_CAPACITY,
            },
            graph_manifest_provider.clone(),
        )
        .map_err(|error| {
            session_registry_error("create graph runtime registry", error.to_string())
        })?;
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
        let registry = Self {
            identity,
            incarnation,
            resolver,
            registry,
            graph_registry,
            graph_manifest_provider,
            graph_lifecycle_cancelled: Arc::new(AtomicBool::new(false)),
            profile_pin,
            profile_runtime,
            profile_database: Mutex::new(None),
            profile_memory: Mutex::new(None),
            profile_sessions: Mutex::new(None),
            remote_nodes: Mutex::new(BTreeMap::new()),
            remote_credential_authority,
            remote_replay_transaction,
            remote_recovery_authorities: Mutex::new(BTreeMap::new()),
            project_memory: Arc::new(Mutex::new(BTreeMap::new())),
            project_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            project_runtime_capacity,
            retired_project_memory_runtimes: std::sync::atomic::AtomicU64::new(0),
            retired_project_session_runtimes: std::sync::atomic::AtomicU64::new(0),
            retirement_refusals: std::sync::atomic::AtomicU64::new(0),
            registered_schema_convergence: RegisteredSchemaConvergenceMaintenance::new(),
            retained_hook_tasks: RetainedHookTasks::new(),
            memory_graph_reconciliation_tasks: Arc::new(
                RetainedMemoryGraphReconciliationTasksV1::new(),
            ),
            session_sync_service: Arc::new(std::sync::OnceLock::new()),
            remote_recovery_project_lifecycle: Arc::new(std::sync::OnceLock::new()),
            long_lived_session_maintenance,
        };
        registry.mount_registered_remote_nodes().await?;
        Ok(registry)
    }

    async fn mount_registered_remote_nodes(&self) -> Result<()> {
        let nodes_root = self.identity.profile_root().join("remote").join("nodes");
        if !nodes_root.exists() {
            return Ok(());
        }
        let entries = std::fs::read_dir(&nodes_root).map_err(|error| {
            session_registry_error("discover registered Remote Brain nodes", error.to_string())
        })?;
        let mut databases = entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path().join("remote.db"))
                    .map_err(|error| {
                        session_registry_error(
                            "discover registered Remote Brain node",
                            error.to_string(),
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        databases.retain(|path| path.is_file());
        databases.sort();
        let keyring: Arc<dyn RemoteSpoolKeyringV1> = Arc::new(UnavailableRemoteSpoolKeyringV1);
        for database in databases {
            let node_id = RemoteSqliteStorageV1::discover_registered_node(
                &database,
                self.identity.brain_id(),
                self.identity.profile_id(),
            )
            .map_err(|error| {
                session_registry_error(
                    "discover registered Remote Brain node identity",
                    error.to_string(),
                )
            })?;
            self.remote_node_storage(node_id, Arc::clone(&keyring))
                .await?;
        }
        Ok(())
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
            shard_id.clone(),
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
        let relation_graph = self.retain_session_relation_graph_runtime(shard_id).await?;
        let (relation_graph, graph_binding, graph_verified_locator) = relation_graph.into_parts();
        database.bind_session_relation_graph(
            SessionRelationScope::profile_sessions(self.identity.profile_id().clone()),
            relation_graph,
            graph_binding,
            graph_verified_locator,
        )?;
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
            shard_id.clone(),
            self.incarnation,
            Some(self.profile_pin.clone()),
            None,
            true,
            "mount profile memory store",
        )
        .await?;
        let database =
            Arc::new(Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await?);
        crate::db::migrations::ensure_schema_current(database.as_ref()).await?;
        let graph_runtime = self
            .retain_memory_graph_runtime(shard_id.clone(), Arc::clone(&database))
            .await?;
        database.bind_memory_graph_runtime(Arc::new(graph_runtime))?;
        self.retain_memory_graph_reconciliation_task(&shard_id, database.as_ref())?;
        super::code_graph::schedule_bound_memory_graph_reconciliation(database.as_ref())?;
        *mounted = Some(Arc::clone(&database));
        Ok(database)
    }

    pub(crate) async fn remote_node_storage(
        &self,
        node_id: BrainNodeId,
        keyring: Arc<dyn RemoteSpoolKeyringV1>,
    ) -> Result<RemoteSqliteStorageV1> {
        self.mount_remote_node_storage(node_id, keyring, false)
            .await
    }

    pub(crate) async fn provision_remote_node(
        &self,
        grant: EnrollmentGrantV1,
        admission: RemoteEnrollmentAdmissionEvidenceV1,
    ) -> Result<()> {
        grant.validate().map_err(|error| {
            session_registry_error("provision Remote Brain node", error.to_string())
        })?;
        admission.validate_for(&grant).map_err(|error| {
            session_registry_error("authenticate Remote Brain provisioning", error.to_string())
        })?;
        if &grant.brain_id != self.identity.brain_id() {
            return Err(session_registry_error(
                "provision Remote Brain node",
                "grant brain identity does not match the profile authority".to_owned(),
            ));
        }
        let keyring: Arc<dyn RemoteSpoolKeyringV1> = Arc::new(UnavailableRemoteSpoolKeyringV1);
        let storage = self
            .mount_remote_node_storage(grant.node_id.clone(), keyring, true)
            .await?;
        storage
            .store_enrollment_grant(&grant, &admission)
            .map_err(|error| {
                session_registry_error("publish Remote Brain enrollment grant", error.to_string())
            })?;
        self.remote_credential_authority
            .refresh_storage(&grant.node_id)
            .map_err(|error| {
                session_registry_error("register Remote Brain enrollment grant", error.to_string())
            })
    }

    async fn mount_remote_node_storage(
        &self,
        node_id: BrainNodeId,
        keyring: Arc<dyn RemoteSpoolKeyringV1>,
        provision_if_new: bool,
    ) -> Result<RemoteSqliteStorageV1> {
        let (database, newly_mounted, existed) = {
            let mut mounted = self.remote_nodes.lock().await;
            if let Some(database) = mounted.get(&node_id) {
                (Arc::clone(database), false, true)
            } else {
                let shard_id = StoreShardIdV1::remote_node(
                    self.identity.brain_id().clone(),
                    self.identity.profile_id().clone(),
                    node_id.clone(),
                );
                let (runtime, existed) = open_runtime_with_presence(
                    &self.registry,
                    self.resolver.as_ref(),
                    shard_id,
                    self.incarnation,
                    Some(self.profile_pin.clone()),
                    None,
                    true,
                    false,
                    None,
                    "mount Remote Brain node store",
                )
                .await?;
                let database = Arc::new(
                    Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await?,
                );
                mounted.insert(node_id.clone(), Arc::clone(&database));
                (database, true, existed)
            }
        };
        let authority = database.write_authority()?;
        let runtime = database.retained_runtime();
        let handle = runtime
            .authorized_exact_sql_handle(authority)
            .map_err(|error| {
                session_registry_error(
                    "attach Remote Brain node store",
                    format!("registered storage handle unavailable: {error:?}"),
                )
            })?;
        let recovery_handle = handle.clone();
        let storage = if provision_if_new && newly_mounted && !existed {
            RemoteSqliteStorageV1::provision_registered(handle, runtime.binding().clone(), keyring)
        } else {
            RemoteSqliteStorageV1::from_registered(handle, runtime.binding().clone(), keyring)
        }
        .map_err(|error| {
            session_registry_error("attach Remote Brain node store", error.to_string())
        })?;
        if newly_mounted {
            storage
                .recover_interrupted_replay_attempts(tracedecay_application::clock::now_micros())
                .map_err(|error| {
                    session_registry_error(
                        "recover interrupted Remote Brain replay",
                        error.to_string(),
                    )
                })?;
        }
        self.remote_credential_authority
            .register_storage(node_id.clone(), storage.clone())
            .map_err(|error| {
                session_registry_error(
                    "register Remote Brain credential authority",
                    error.to_string(),
                )
            })?;
        let mut recovery_authorities = self.remote_recovery_authorities.lock().await;
        if !recovery_authorities.contains_key(&node_id) {
            let publication = RemoteRecoveryPublicationContextV1::new(
                self.identity.clone(),
                self.incarnation,
                Arc::clone(&self.resolver),
                self.registry.clone(),
                self.graph_registry.clone(),
                Arc::clone(&self.graph_lifecycle_cancelled),
                self.profile_pin.clone(),
                Arc::clone(&self.project_sessions),
                Arc::clone(&self.remote_replay_transaction),
                self.session_sync_service(),
                self.remote_recovery_project_lifecycle(),
            );
            let backup_root = database
                .database_path()
                .parent()
                .ok_or_else(|| {
                    session_registry_error(
                        "attach remote recovery authority",
                        "RemoteNode database has no parent directory".to_owned(),
                    )
                })?
                .join("recovery-artifacts");
            let effects = Arc::new(DaemonRemoteRecoveryPhysicalEffectsV1::new(
                storage.clone(),
                backup_root,
                Arc::clone(&self.remote_replay_transaction),
                publication,
                tokio::runtime::Handle::current(),
            ));
            let recovery =
                RemoteRecoverySqliteAuthorityV1::from_registered(recovery_handle, effects)
                    .map_err(|error| {
                        session_registry_error(
                            "attach remote recovery authority",
                            error.to_string(),
                        )
                    })?;
            let recovery = Arc::new(recovery);
            self.remote_credential_authority
                .register_recovery_authority(&node_id, Arc::clone(&recovery))
                .map_err(|error| {
                    session_registry_error(
                        "mount remote recovery protocol authority",
                        error.to_string(),
                    )
                })?;
            recovery_authorities.insert(node_id.clone(), recovery);
        } else if let Some(recovery) = recovery_authorities.get(&node_id) {
            self.remote_credential_authority
                .register_recovery_authority(&node_id, Arc::clone(recovery))
                .map_err(|error| {
                    session_registry_error(
                        "remount remote recovery protocol authority",
                        error.to_string(),
                    )
                })?;
        }
        let recovery = recovery_authorities.get(&node_id).cloned();
        drop(recovery_authorities);
        if let Some(recovery) = recovery {
            let projects = self
                .project_sessions
                .lock()
                .await
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            for project_id in projects {
                recovery
                    .reconcile_interrupted_promotions(&project_id)
                    .map_err(|error| {
                        session_registry_error(
                            "reconcile interrupted remote promotion",
                            format!("{error:?}"),
                        )
                    })?;
            }
        }
        Ok(storage)
    }

    pub(crate) fn remote_credential_authority(
        &self,
    ) -> Arc<crate::daemon::remote_protocol::DaemonRemoteCredentialAuthorityV1> {
        Arc::clone(&self.remote_credential_authority)
    }

    pub(crate) fn remote_replay_transaction(
        &self,
    ) -> Arc<crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1>
    {
        Arc::clone(&self.remote_replay_transaction)
    }

    #[cfg(test)]
    pub(crate) async fn remote_recovery_authority(
        &self,
        node_id: &BrainNodeId,
    ) -> Option<Arc<RemoteRecoverySqliteAuthorityV1>> {
        self.remote_recovery_authorities
            .lock()
            .await
            .get(node_id)
            .cloned()
    }

    pub(crate) async fn mounted_session_databases(&self) -> Vec<Arc<RegisteredGlobalDb>> {
        let mut databases = Vec::new();
        if let Some(database) = self.profile_sessions.lock().await.as_ref() {
            databases.push(Arc::clone(database));
        }
        databases.extend(self.project_sessions.lock().await.values().cloned());
        databases
    }

    /// Releases exclusive session-relation Grafeo handles while this registry
    /// is still reachable. Close-then-reopen and harness restart must not wait
    /// for every lingering `Arc` to drop; `GraphDb::close` frees the file lock
    /// even if closed handles remain.
    pub(crate) async fn close_mounted_session_relation_graphs(&self) -> Result<()> {
        let databases = self.mounted_session_databases().await;
        let mut first_error = None;
        for database in databases {
            let Ok((binding, locator)) = database.session_relation_graph_identity() else {
                continue;
            };
            if let Err(error) = super::code_graph::graph_attachment::close_retained_for_shutdown(
                &self.graph_registry,
                binding.clone(),
                locator.clone(),
            )
            .await
            {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
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
        self.mount_registered_project_sessions(project_id).await
    }

    pub(crate) async fn mount_registered_project_sessions(
        &self,
        project_id: ProjectId,
    ) -> Result<Arc<RegisteredGlobalDb>> {
        {
            let mounted = self.project_sessions.lock().await;
            if let Some(database) = mounted.get(&project_id) {
                return Ok(Arc::clone(database));
            }
        }
        self.ensure_project_session_runtime_capacity(&project_id)
            .await?;
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
            shard_id.clone(),
            self.incarnation,
            Some(self.profile_pin.clone()),
            None,
            true,
            "mount project session store",
        )
        .await?;
        let replay_authority = runtime
            .database_authority("register remote replay target")
            .map_err(|failure| registry_open_error("register remote replay target", failure))?;
        let replay_runtime = runtime.clone();
        let database = self
            .attach_registered(runtime, "mount project session store")
            .await?;
        let relation_graph = self.retain_session_relation_graph_runtime(shard_id).await?;
        let (relation_graph, graph_binding, graph_verified_locator) = relation_graph.into_parts();
        database.bind_session_relation_graph(
            SessionRelationScope::project_sessions(project_id.clone()),
            relation_graph,
            graph_binding,
            graph_verified_locator,
        )?;
        self.remote_replay_transaction
            .register_target(project_id.clone(), replay_runtime, replay_authority)
            .map_err(|error| session_registry_error("register remote replay target", error))?;
        mounted.insert(project_id.clone(), Arc::clone(&database));
        let recoveries = self
            .remote_recovery_authorities
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for recovery in recoveries {
            recovery
                .reconcile_interrupted_promotions(&project_id)
                .map_err(|error| {
                    session_registry_error(
                        "reconcile interrupted remote promotion",
                        format!("{error:?}"),
                    )
                })?;
        }
        Ok(database)
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
        {
            let mounted = self.project_memory.lock().await;
            if let Some(database) = mounted.get(&project_id) {
                return Ok(Arc::clone(database));
            }
        }
        self.ensure_project_memory_runtime_capacity(&project_id)
            .await?;
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
            shard_id.clone(),
            self.incarnation,
            Some(self.profile_pin.clone()),
            None,
            true,
            "mount project memory store",
        )
        .await?;
        let database =
            Arc::new(Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await?);
        crate::db::migrations::ensure_schema_current(database.as_ref()).await?;
        let graph_runtime = self
            .retain_memory_graph_runtime(shard_id.clone(), Arc::clone(&database))
            .await?;
        database.bind_memory_graph_runtime(Arc::new(graph_runtime))?;
        self.retain_memory_graph_reconciliation_task(&shard_id, database.as_ref())?;
        super::code_graph::schedule_bound_memory_graph_reconciliation(database.as_ref())?;
        mounted.insert(project_id, Arc::clone(&database));
        Ok(database)
    }

    /// Mounts an existing project-memory shard without initializing it or
    /// verifying its schema, and exposes only a read-only database facade.
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
            let readonly = Database::publish_runtime(
                database.retained_runtime().clone(),
                DatabaseAccessMode::ReadOnly,
            )
            .await?;
            return Ok(readonly);
        }
        let shard_id = StoreShardIdV1::project(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
        );
        let runtime = match self
            .registry
            .open(StoreRuntimeOpenRequest::new_read_only(
                shard_id.clone(),
                self.incarnation,
                Some(self.profile_pin.clone()),
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(runtime) => runtime,
            StoreRuntimeOpenResult::Failed(failure) => {
                return Err(registry_open_error(
                    "mount project memory store read-only",
                    failure,
                ));
            }
        };
        Database::publish_runtime(runtime, DatabaseAccessMode::ReadOnly).await
    }
}

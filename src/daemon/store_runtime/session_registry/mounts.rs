use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use std::sync::atomic::AtomicBool;
use tokio::sync::Mutex;
use tracedecay_application::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
use tracedecay_domain::{BrainNodeId, EnrollmentGrantV1};
use tracedecay_global_db::session_temporal::relations::SessionRelationScope;
use tracedecay_graph_db::{GraphDbRegistry, GraphDbRegistryConfig};
#[cfg(test)]
use tracedecay_rusqlite_runtime::remote::RemoteRecoverySqliteAuthorityV1;
use tracedecay_rusqlite_runtime::remote::{
    RemoteSpoolKeyV1, RemoteSpoolKeyringV1, RemoteSqliteStorageErrorV1, RemoteSqliteStorageV1,
};
use tracedecay_store::{ProjectId, StoreShardIdV1};

use super::remote_recovery::{
    DaemonRemoteRecoveryPhysicalEffectsV1, RemoteRecoveryPublicationContextV1,
};
use super::{
    DaemonSessionRuntimeRegistryV1, Database, DatabaseAccessMode, LifecycleShardRuntimePublisher,
    LocalProfileIdentityAuthorityV1, LocalProfileStoreAuthorityV1,
    LocalProjectEnrollmentAuthorityV1, LocalStoreRuntimeResolverV1, MemoryStoreOwnerV1,
    ProfileAuthorityPinResult, ProjectRuntimeOwnerAdmissionV1, ProjectRuntimeOwnerStateV1,
    RegisteredGlobalDbLeaseV1, RegisteredGlobalDbOwnerV1, RegisteredSchemaConvergenceMaintenance,
    RegisteredSessionOwnerV1, RemoteNodeStoreOwnerV1, Result, RetainedHookTasks,
    StoreRuntimeClientLease, StoreRuntimeOpenRequest, StoreRuntimeOpenResult, StoreRuntimeRegistry,
    StoreRuntimeResolver, open_runtime, open_runtime_with_presence,
    register_registered_schema_installer, registry_open_error, runtime_incarnation,
    session_registry_error,
};
use crate::errors::TraceDecayError;

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
            GraphDbRegistryConfig { max_open: 8 },
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
        // The profile pin is the one durable profile-wide retirement blocker.
        // The opening client is only a bootstrap issuance and must not become
        // an invisible map client once the owner maps take over.
        drop(profile_runtime);
        let registry = Self {
            identity,
            incarnation,
            resolver,
            registry,
            graph_registry,
            graph_manifest_provider,
            graph_lifecycle_cancelled: Arc::new(AtomicBool::new(false)),
            profile_pin: Mutex::new(Some(profile_pin)),
            profile_database: std::sync::Mutex::new(None),
            profile_memory: std::sync::Mutex::new(None),
            profile_sessions: std::sync::Mutex::new(None),
            remote_nodes: std::sync::Mutex::new(BTreeMap::new()),
            remote_credential_authority,
            remote_replay_transaction,
            remote_recovery_authorities: Mutex::new(BTreeMap::new()),
            project_owners: super::ProjectRuntimeOwnerRegistryV1::default(),
            code_graph_publication_gates: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            registered_schema_convergence: RegisteredSchemaConvergenceMaintenance::new(),
            retained_hook_tasks: RetainedHookTasks::new(),
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

    /// Mints one independently counted registered-session client and its
    /// matching graph client. The owner map retains neither issuance.
    fn issue_session_owner_lease(
        &self,
        owner: &RegisteredSessionOwnerV1,
        scope: SessionRelationScope,
    ) -> Result<RegisteredGlobalDbLeaseV1> {
        self.issue_session_owner_lease_parts(&owner.database, &owner.relation_graph.graph, scope)
    }

    fn issue_session_owner_lease_parts(
        &self,
        owner: &RegisteredGlobalDbOwnerV1,
        graph_owner: &tracedecay_graph_db::GraphDbOwnerAttachmentV1,
        scope: SessionRelationScope,
    ) -> Result<RegisteredGlobalDbLeaseV1> {
        let database = owner.issue_lease().map_err(|error| {
            session_registry_error(
                "issue registered session database client",
                format!("{error:?}"),
            )
        })?;
        let graph = graph_owner.issue_lease().map_err(|error| {
            session_registry_error(
                "issue registered session relation graph client",
                error.to_string(),
            )
        })?;
        let graph_binding = graph_owner.binding().clone();
        let graph_verified_locator = graph_owner.verified_locator().clone();
        database
            .bind_session_relation_graph(scope, graph, graph_binding, graph_verified_locator)
            .map_err(|_| {
                session_registry_error(
                    "bind issued registered session relation graph",
                    "issued graph client did not match the exact registered session owner"
                        .to_owned(),
                )
            })?;
        Ok(database)
    }

    pub(crate) async fn profile_database(&self) -> Result<RegisteredGlobalDbLeaseV1> {
        let existing = {
            let mounted = self
                .profile_database
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            mounted.as_ref().map(|database| {
                database.issue_lease().map_err(|error| {
                    session_registry_error(
                        "issue profile authority database client",
                        format!("{error:?}"),
                    )
                })
            })
        };
        if let Some(lease) = existing {
            return lease;
        }
        let shard_id = StoreShardIdV1::profile(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
        );
        let runtime = open_runtime(
            &self.registry,
            self.resolver.as_ref(),
            shard_id,
            self.incarnation,
            None,
            None,
            true,
            "mount profile authority store",
        )
        .await?;
        let database = self
            .attach_registered(runtime, "attach profile authority store")
            .await?;
        let lease = database.issue_lease().map_err(|error| {
            session_registry_error(
                "issue profile authority database client",
                format!("{error:?}"),
            )
        })?;
        *self
            .profile_database
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(database);
        Ok(lease)
    }

    pub(crate) async fn profile_sessions(&self) -> Result<RegisteredGlobalDbLeaseV1> {
        let existing = {
            let mounted = self
                .profile_sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            mounted.as_ref().map(|database| {
                self.issue_session_owner_lease(
                    database,
                    SessionRelationScope::profile_sessions(self.identity.profile_id().clone()),
                )
            })
        };
        if let Some(lease) = existing {
            return lease;
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
            Some(
                self.profile_authority_pin("mount profile session store")
                    .await?,
            ),
            None,
            true,
            "mount profile session store",
        )
        .await?;
        let database = self
            .attach_registered(runtime, "mount profile session store")
            .await?;
        let relation_graph = self.retain_session_relation_graph_owner(shard_id).await?;
        let database = RegisteredSessionOwnerV1 {
            database,
            relation_graph,
        };
        let lease = self.issue_session_owner_lease(
            &database,
            SessionRelationScope::profile_sessions(self.identity.profile_id().clone()),
        )?;
        *self
            .profile_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(database);
        Ok(lease)
    }

    pub(super) async fn publish_memory_owner(
        &self,
        shard_id: StoreShardIdV1,
        runtime: StoreRuntimeClientLease,
    ) -> Result<(MemoryStoreOwnerV1, Arc<Database>)> {
        let owner = Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await?;
        let database = owner.issue_lease().map_err(|error| {
            session_registry_error(
                "issue writable memory database client",
                format!("{error:?}"),
            )
        })?;
        crate::db::migrations::ensure_schema_current(&database).await?;
        let graph_runtime = Arc::new(
            self.retain_memory_graph_runtime(shard_id.clone(), owner)
                .await?,
        );
        let graph_port: Arc<
            dyn tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1,
        > = graph_runtime.clone();
        database.bind_memory_graph_runtime(graph_port)?;
        super::code_graph::schedule_bound_memory_graph_reconciliation(&database)?;
        let reconciliation = database
            .memory_graph_reconciliation_task_owner()
            .ok_or_else(|| {
                session_registry_error(
                    "publish memory runtime owner",
                    "memory graph reconciliation owner was not installed".to_owned(),
                )
            })?;
        Ok((
            MemoryStoreOwnerV1 {
                graph_runtime,
                reconciliation,
            },
            Arc::new(database),
        ))
    }

    /// Mounts the distinct profile-memory shard through this daemon's pinned
    /// profile registry. `ProfileMemory` never aliases the profile/global
    /// shard, and publication never reopens a filesystem path.
    pub(crate) async fn profile_memory(&self) -> Result<Arc<Database>> {
        let existing = {
            let mounted = self
                .profile_memory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            mounted.as_ref().map(|owner| {
                owner
                    .graph_runtime
                    .issue_database_lease()
                    .map(Arc::new)
                    .map_err(|error| {
                        session_registry_error(
                            "issue profile memory database client",
                            error.to_string(),
                        )
                    })
            })
        };
        if let Some(database) = existing {
            return database;
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
            Some(
                self.profile_authority_pin("mount profile memory store")
                    .await?,
            ),
            None,
            true,
            "mount profile memory store",
        )
        .await?;
        let (owner, database) = self.publish_memory_owner(shard_id, runtime).await?;
        *self
            .profile_memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(owner);
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
        let (database, newly_mounted, existed) = match self.admit_remote_node_owner(&node_id)? {
            super::RemoteNodeOwnerAdmissionV1::Existing(database) => (database, false, true),
            super::RemoteNodeOwnerAdmissionV1::Opening(mut admission) => {
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
                    Some(
                        self.profile_authority_pin("mount Remote Brain node store")
                            .await?,
                    ),
                    None,
                    true,
                    false,
                    None,
                    "mount Remote Brain node store",
                )
                .await?;
                let owner = RemoteNodeStoreOwnerV1 {
                    database: Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite)
                        .await?,
                };
                let database = owner.database.issue_lease().map_err(|error| {
                    session_registry_error(
                        "issue Remote Brain node database client",
                        format!("{error:?}"),
                    )
                })?;
                admission.publish(owner)?;
                (database, true, existed)
            }
        };
        let storage = if provision_if_new && newly_mounted && !existed {
            database.provision_remote_storage(keyring)
        } else {
            database.remote_storage(keyring)
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
        let existing_recovery = {
            let recovery_authorities = self.remote_recovery_authorities.lock().await;
            recovery_authorities.get(&node_id).cloned()
        };
        let recovery = if let Some(recovery) = existing_recovery {
            recovery
        } else {
            let publication = RemoteRecoveryPublicationContextV1::new(
                self.identity.clone(),
                self.incarnation,
                Arc::clone(&self.resolver),
                self.registry.clone(),
                self.graph_registry.clone(),
                Arc::clone(&self.graph_lifecycle_cancelled),
                self.profile_authority_pin("attach remote recovery authority")
                    .await?,
                self.project_owners.clone(),
                Arc::clone(&self.remote_replay_transaction),
                self.session_sync_service(),
                self.remote_recovery_project_lifecycle(),
            );
            let backup_root = database
                .canonical_database_path()
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
            let recovery = database.remote_recovery_authority(effects)?;
            let recovery = Arc::new(recovery);
            let mut recovery_authorities = self.remote_recovery_authorities.lock().await;
            if let Some(existing) = recovery_authorities.get(&node_id) {
                Arc::clone(existing)
            } else {
                recovery_authorities.insert(node_id.clone(), Arc::clone(&recovery));
                recovery
            }
        };
        self.remote_credential_authority
            .register_recovery_authority(&node_id, Arc::clone(&recovery))
            .map_err(|error| {
                session_registry_error(
                    "register remote recovery protocol authority",
                    error.to_string(),
                )
            })?;
        let projects = self.project_owners.ready_session_projects()?;
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
        Ok(storage)
    }

    pub(crate) fn remote_credential_authority(
        &self,
    ) -> Arc<crate::daemon::remote_protocol::DaemonRemoteCredentialAuthorityV1> {
        Arc::clone(&self.remote_credential_authority)
    }

    /// Canonical Remote Brain operational read for every operator surface
    /// (Doctor, CLI, MCP, dashboard), composed from the mounted remote
    /// authorities.
    pub(crate) fn remote_operational_status(
        &self,
    ) -> tracedecay_application::remote::status::RemoteOperationalStatusReadV1 {
        self.remote_credential_authority.operational_status()
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

    pub(crate) async fn mounted_session_databases(&self) -> Vec<RegisteredGlobalDbLeaseV1> {
        let mut databases = Vec::new();
        if let Some(database) = self
            .profile_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            && let Ok(database) = self.issue_session_owner_lease(
                database,
                SessionRelationScope::profile_sessions(self.identity.profile_id().clone()),
            )
        {
            databases.push(database);
        }
        let mounted = self
            .project_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (project_id, state) in mounted.iter() {
            let ProjectRuntimeOwnerStateV1::Ready(owners) = state else {
                continue;
            };
            let Some(owner) = owners.sessions.as_ref() else {
                continue;
            };
            if let Ok(database) = self.issue_session_owner_lease(
                owner,
                SessionRelationScope::project_sessions(project_id.clone()),
            ) {
                databases.push(database);
            }
        }
        databases
    }

    /// Releases exclusive Grafeo writers at daemon shutdown, after the
    /// reconciliation workers have joined.
    ///
    /// The shutdown close requires every graph owner to be unleased, so this
    /// first drains the retained owners out of the registry maps: dropping a
    /// session owner releases its standing `GraphDbOwnerAttachmentV1`, and
    /// dropping a memory owner releases the memory-graph attachment the
    /// reconciliation runtime held. Only then can the captured runtimes close
    /// without a structural Conflict. Callers must have joined the
    /// reconciliation workers first; a graph client lease still held by a
    /// live consumer surfaces as a typed Conflict, not a hang.
    pub(crate) async fn close_retained_graph_runtimes_for_shutdown(&self) -> Result<()> {
        let identities = self.drain_retained_graph_owners_for_shutdown();
        let mut first_error = None;
        for (binding, locator) in identities {
            if let Err(error) = super::code_graph::graph_attachment::close_retained_for_shutdown(
                &self.graph_registry,
                binding,
                locator,
            )
            .await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Takes every retained graph-owning runtime out of the registry maps and
    /// returns the exact store identity of each graph runtime to close. The
    /// owners drop here, releasing their `GraphDbOwnerAttachmentV1` map
    /// attachments and owner-bound graph client leases; non-`Ready` project
    /// states are retained untouched for typed recovery inspection.
    fn drain_retained_graph_owners_for_shutdown(
        &self,
    ) -> Vec<(
        tracedecay_store::StoreRuntimeBindingV1,
        tracedecay_store::VerifiedStoreLocatorV1,
    )> {
        let mut identities = Vec::new();
        if let Some(owner) = self
            .profile_memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            identities.push(owner.graph_runtime.graph_store_identity());
        }
        if let Some(owner) = self
            .profile_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            identities.push((
                owner.relation_graph.graph.binding().clone(),
                owner.relation_graph.graph.verified_locator().clone(),
            ));
        }
        let mut projects = self
            .project_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for state in projects.values_mut() {
            let ProjectRuntimeOwnerStateV1::Ready(owners) = state else {
                continue;
            };
            if let Some(memory) = owners.memory.take() {
                identities.push(memory.graph_runtime.graph_store_identity());
            }
            if let Some(sessions) = owners.sessions.take() {
                identities.push((
                    sessions.relation_graph.graph.binding().clone(),
                    sessions.relation_graph.graph.verified_locator().clone(),
                ));
            }
        }
        drop(projects);
        identities
    }

    pub(crate) async fn mounted_project_sessions(
        &self,
        project_id: &ProjectId,
    ) -> Option<RegisteredGlobalDbLeaseV1> {
        let mounted = self
            .project_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ProjectRuntimeOwnerStateV1::Ready(owners) = mounted.get(project_id)? else {
            return None;
        };
        let owner = owners.sessions.as_ref()?;
        self.issue_session_owner_lease(
            owner,
            SessionRelationScope::project_sessions(project_id.clone()),
        )
        .ok()
    }

    pub(crate) async fn project_sessions(
        &self,
        project_id: ProjectId,
        enrollment_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<RegisteredGlobalDbLeaseV1> {
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
    ) -> Result<RegisteredGlobalDbLeaseV1> {
        let has_entry = {
            let mounted = self
                .project_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match mounted.get(&project_id) {
                Some(ProjectRuntimeOwnerStateV1::Ready(owners)) => {
                    if let Some(owner) = owners.sessions.as_ref() {
                        return self.issue_session_owner_lease(
                            owner,
                            SessionRelationScope::project_sessions(project_id.clone()),
                        );
                    }
                    true
                }
                Some(ProjectRuntimeOwnerStateV1::Opening) => {
                    return Err(TraceDecayError::project_route(
                        "project_runtime_opening",
                        true,
                        "Project runtime is already opening",
                    ));
                }
                Some(
                    ProjectRuntimeOwnerStateV1::Retiring
                    | ProjectRuntimeOwnerStateV1::ReplacingSessions
                    | ProjectRuntimeOwnerStateV1::Recovering
                    | ProjectRuntimeOwnerStateV1::RecoveryRequired(_)
                    | ProjectRuntimeOwnerStateV1::Faulted(_),
                ) => {
                    return Err(TraceDecayError::project_route(
                        "project_runtime_retiring",
                        true,
                        "Project runtime is unavailable while retirement is terminal or in progress",
                    ));
                }
                None => false,
            }
        };
        let mut admission = match if has_entry {
            self.extend_project_runtime_owner(&project_id)
        } else {
            self.admit_project_runtime_owner(&project_id)
        }? {
            ProjectRuntimeOwnerAdmissionV1::Opening(admission) => admission,
            ProjectRuntimeOwnerAdmissionV1::Existing => {
                let mounted = self
                    .project_owners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(ProjectRuntimeOwnerStateV1::Ready(owners)) = mounted.get(&project_id)
                else {
                    return Err(TraceDecayError::project_route(
                        "project_runtime_opening",
                        true,
                        "Project runtime changed while issuing a session client",
                    ));
                };
                let Some(owner) = owners.sessions.as_ref() else {
                    return Err(TraceDecayError::project_route(
                        "project_runtime_opening",
                        true,
                        "Project runtime is opening its session authority",
                    ));
                };
                return self.issue_session_owner_lease(
                    owner,
                    SessionRelationScope::project_sessions(project_id.clone()),
                );
            }
        };
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
            Some(
                self.profile_authority_pin("mount project session store")
                    .await?,
            ),
            None,
            true,
            "mount project session store",
        )
        .await?;
        let database = self
            .attach_registered(runtime, "mount project session store")
            .await?;
        let relation_graph = self.retain_session_relation_graph_owner(shard_id).await?;
        let database = RegisteredSessionOwnerV1 {
            database,
            relation_graph,
        };
        let lease = self.issue_session_owner_lease(
            &database,
            SessionRelationScope::project_sessions(project_id.clone()),
        )?;
        let replay_issuer = database.database.weak_lease_issuer();
        let replay_binding = database.database.registered_binding().clone();
        let replay_locator = database.database.registered_verified_locator().clone();
        let replay_path = lease.db_path().to_path_buf();
        self.remote_replay_transaction
            .register_target(
                project_id.clone(),
                replay_issuer,
                replay_binding.clone(),
                replay_locator,
                replay_path,
            )
            .map_err(|error| session_registry_error("register project replay target", error))?;
        if let Err(error) = admission.publish_sessions(database) {
            let cleanup = self
                .remote_replay_transaction
                .unregister_target(&project_id, &replay_binding);
            return Err(session_registry_error(
                "publish project session runtime owner",
                format!("publish={error}; replay cleanup={cleanup:?}"),
            ));
        }
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
        Ok(lease)
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
        let (has_entry, existing) = {
            let mounted = self
                .project_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match mounted.get(&project_id) {
                Some(ProjectRuntimeOwnerStateV1::Ready(owners)) => {
                    if let Some(owner) = owners.memory.as_ref() {
                        owner
                            .graph_runtime
                            .issue_database_lease()
                            .map(Arc::new)
                            .map_err(|error| {
                                session_registry_error(
                                    "issue project memory database client",
                                    error.to_string(),
                                )
                            })
                            .map(|database| (true, Some(database)))
                    } else {
                        Ok((true, None))
                    }
                }
                Some(ProjectRuntimeOwnerStateV1::Opening) => Err(TraceDecayError::project_route(
                    "project_runtime_opening",
                    true,
                    "Project runtime is already opening",
                )),
                Some(
                    ProjectRuntimeOwnerStateV1::Retiring
                    | ProjectRuntimeOwnerStateV1::ReplacingSessions
                    | ProjectRuntimeOwnerStateV1::Recovering
                    | ProjectRuntimeOwnerStateV1::RecoveryRequired(_)
                    | ProjectRuntimeOwnerStateV1::Faulted(_),
                ) => Err(TraceDecayError::project_route(
                    "project_runtime_retiring",
                    true,
                    "Project runtime is unavailable while retirement is terminal or in progress",
                )),
                None => Ok((false, None)),
            }
        }?;
        if let Some(database) = existing {
            return Ok(database);
        }
        let mut admission = match if has_entry {
            self.extend_project_runtime_owner(&project_id)
        } else {
            self.admit_project_runtime_owner(&project_id)
        }? {
            ProjectRuntimeOwnerAdmissionV1::Opening(admission) => admission,
            ProjectRuntimeOwnerAdmissionV1::Existing => {
                return Err(TraceDecayError::project_route(
                    "project_runtime_opening",
                    true,
                    "Project runtime changed while issuing a memory client",
                ));
            }
        };
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
            Some(
                self.profile_authority_pin("mount project memory store")
                    .await?,
            ),
            None,
            true,
            "mount project memory store",
        )
        .await?;
        let (owner, database) = self.publish_memory_owner(shard_id, runtime).await?;
        admission.publish_memory(owner)?;
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
        let existing = {
            let mounted = self
                .project_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match mounted.get(&project_id) {
                Some(ProjectRuntimeOwnerStateV1::Ready(owners)) => {
                    if let Some(owner) = owners.memory.as_ref() {
                        owner
                            .graph_runtime
                            .issue_database_read_only_lease()
                            .map(Some)
                            .map_err(|error| {
                                session_registry_error(
                                    "issue project memory read-only database client",
                                    error.to_string(),
                                )
                            })
                    } else {
                        Ok(None)
                    }
                }
                Some(ProjectRuntimeOwnerStateV1::Opening) => Err(TraceDecayError::project_route(
                    "project_runtime_opening",
                    true,
                    "Project runtime is already opening",
                )),
                Some(
                    ProjectRuntimeOwnerStateV1::Retiring
                    | ProjectRuntimeOwnerStateV1::ReplacingSessions
                    | ProjectRuntimeOwnerStateV1::Recovering
                    | ProjectRuntimeOwnerStateV1::RecoveryRequired(_)
                    | ProjectRuntimeOwnerStateV1::Faulted(_),
                ) => Err(TraceDecayError::project_route(
                    "project_runtime_retiring",
                    true,
                    "Project runtime is unavailable while retirement is terminal or in progress",
                )),
                None => Ok(None),
            }
        }?;
        if let Some(database) = existing {
            return Ok(database);
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
                Some(
                    self.profile_authority_pin("mount project memory store read-only")
                        .await?,
                ),
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
        Database::publish_runtime(runtime, DatabaseAccessMode::ReadOnly)
            .await?
            .issue_read_only_lease()
            .map_err(|error| {
                session_registry_error(
                    "issue project memory read-only database client",
                    format!("{error:?}"),
                )
            })
    }
}

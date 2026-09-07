use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use std::sync::atomic::AtomicBool;
#[cfg(any(test, feature = "test-helpers"))]
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tracedecay_application::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
use tracedecay_domain::{BrainNodeId, EnrollmentGrantV1};
use tracedecay_graph_db::{GraphDbRegistry, GraphDbRegistryConfig};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_rusqlite_runtime::remote::RemoteRecoverySqliteAuthorityV1;
use tracedecay_rusqlite_runtime::remote::{
    RemoteSpoolKeyV1, RemoteSpoolKeyringV1, RemoteSqliteStorageErrorV1, RemoteSqliteStorageV1,
};
use tracedecay_session_temporal_store::relations::SessionRelationScope;
use tracedecay_store::{ProjectId, StoreShardIdV1, StoreShardScopeV1};

use super::remote_recovery::{
    DaemonRemoteRecoveryPhysicalEffectsV1, RemoteRecoveryPublicationContextV1,
};
use super::{
    DaemonSessionRuntimeRegistryV1, Database, DatabaseAccessMode, LifecycleShardRuntimePublisher,
    LocalProfileIdentityAuthorityV1, LocalProfileStoreAuthorityV1,
    LocalProjectEnrollmentAuthorityV1, LocalStoreRuntimeResolverV1, MemoryGraphAttachmentStateV1,
    MemoryStoreOwnerV1, ProfileAuthorityPinResult, ProjectRuntimeOwnerAdmissionV1,
    ProjectRuntimeOwnerStateV1, RegisteredGlobalDbLeaseV1, RegisteredGlobalDbOwnerV1,
    RegisteredSchemaConvergenceMaintenance, RegisteredSessionOwnerV1, RemoteNodeStoreOwnerV1,
    Result, RetainedHookTasks, SessionGraphAttachmentStateV1, SessionGraphOwnerV1,
    StoreRuntimeClientLease, StoreRuntimeOpenRequest, StoreRuntimeOpenResult, StoreRuntimeOpenSpec,
    StoreRuntimeRegistry, StoreRuntimeResolver, bind_ready_project_memory_graph, open_runtime,
    open_runtime_with_presence, registry_open_error, runtime_incarnation, session_registry_error,
};
use crate::register_registered_schema_installer;
use tracedecay_domain::errors::TraceDecayError;

/// Test-only hold installed immediately after the background session
/// relation-graph open task publishes its settled state.
///
/// It exists so a fixture can pin that task in the exact window that used to
/// leak a counted client lease past settlement: with the task parked here, any
/// lease the task still owns is deterministically visible to a retirement
/// reservation instead of depending on how quickly the task future happens to
/// be dropped on another worker thread.
#[cfg(any(test, feature = "test-helpers"))]
pub(super) struct SessionGraphPublicationTestGateState {
    blocked: AtomicBool,
    blocked_notify: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}

#[cfg(any(test, feature = "test-helpers"))]
impl SessionGraphPublicationTestGateState {
    async fn block(&self) {
        self.blocked.store(true, Ordering::Release);
        self.blocked_notify.notify_waiters();
        self.release
            .acquire()
            .await
            .expect("session graph settle test gate remains open")
            .forget();
    }
}

/// Handle returned by
/// [`DaemonSessionRuntimeRegistryV1::block_session_graph_publication_for_test`].
#[cfg(any(test, feature = "test-helpers"))]
pub struct SessionGraphPublicationTestGate {
    state: Arc<SessionGraphPublicationTestGateState>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl SessionGraphPublicationTestGate {
    /// Awaits the graph-open task reaching the post-publication hold.
    pub async fn wait_until_blocked(&self) {
        loop {
            let notified = self.state.blocked_notify.notified();
            if self.state.blocked.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Lets one held graph-open task finish and drop.
    pub fn release(&self) {
        self.state.release.add_permits(1);
    }
}

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

#[cfg_attr(feature = "hotpath", hotpath::measure_all)]
impl DaemonSessionRuntimeRegistryV1 {
    pub async fn open(identity: LocalProfileIdentityAuthorityV1) -> Result<Self> {
        // `main` marks long-lived processes before any registry opens, so the
        // process mode is a construction-time fact, not a mutable runtime flag.
        let long_lived =
            super::LONG_LIVED_SESSION_MAINTENANCE.load(std::sync::atomic::Ordering::Relaxed);
        Self::open_with_session_maintenance(identity, long_lived).await
    }

    /// Constructor with an explicit session-maintenance policy. Production
    /// enters through [`Self::open`]; tests exercising convergence pass
    /// `true` directly so the same gate runs without a mutable side channel.
    pub async fn open_with_session_maintenance(
        identity: LocalProfileIdentityAuthorityV1,
        long_lived_session_maintenance: bool,
    ) -> Result<Self> {
        let remote_credential_authority = Arc::new(crate::DaemonRemoteCredentialAuthorityV1::new(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
        ));
        let remote_replay_transaction = Arc::new(
            crate::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1::new(
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
            // Derived from the project ceiling; see MAX_RETAINED_GRAPH_DB_OWNERS for
            // the arithmetic and for why a mounted project is never evictable.
            GraphDbRegistryConfig {
                max_open: super::MAX_RETAINED_GRAPH_DB_OWNERS,
            },
            graph_manifest_provider.clone(),
        )
        .map_err(|error| {
            session_registry_error("create graph runtime registry", error.to_string())
        })?;
        let profile_shard =
            StoreShardIdV1::profile(identity.brain_id().clone(), identity.profile_id().clone());
        let profile_runtime = hotpath::future!(
            open_runtime(
                &registry,
                resolver.as_ref(),
                StoreRuntimeOpenSpec::new(
                    profile_shard.clone(),
                    incarnation,
                    None,
                    None,
                    true,
                    "mount profile authority store",
                ),
            ),
            label = "daemon.store.profile_authority.bootstrap_open"
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
            profile_database_mount: Mutex::new(()),
            profile_database: std::sync::Mutex::new(None),
            profile_memory: std::sync::Mutex::new(None),
            profile_sessions_mount: Mutex::new(()),
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
            #[cfg(any(test, feature = "test-helpers"))]
            session_graph_publication_gate: std::sync::Mutex::new(None),
        };
        registry.mount_registered_remote_nodes().await?;
        Ok(registry)
    }

    #[hotpath::measure(label = "daemon.session_registry.mount_remote_nodes", future = true)]
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
}

impl DaemonSessionRuntimeRegistryV1 {
    /// Holds every subsequently mounted session owner's relation-graph open
    /// task immediately after its settled state becomes observable. Fixtures
    /// use it to exercise the state fast path while the publishing task is
    /// still parked.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn block_session_graph_publication_for_test(&self) -> SessionGraphPublicationTestGate {
        let state = Arc::new(SessionGraphPublicationTestGateState {
            blocked: AtomicBool::new(false),
            blocked_notify: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        });
        *self
            .session_graph_publication_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&state));
        SessionGraphPublicationTestGate { state }
    }

    /// Mints one independently counted registered-session client and its
    /// matching graph client. The owner map retains neither issuance.
    fn issue_session_owner_lease(
        &self,
        owner: &RegisteredSessionOwnerV1,
        scope: SessionRelationScope,
    ) -> Result<RegisteredGlobalDbLeaseV1> {
        owner.issue_lease(scope)
    }

    fn publish_session_owner(
        &self,
        database: RegisteredGlobalDbOwnerV1,
        shard_id: StoreShardIdV1,
        scope: SessionRelationScope,
    ) -> Result<(RegisteredSessionOwnerV1, RegisteredGlobalDbLeaseV1)> {
        let published_lease = database.issue_lease().map_err(|error| {
            session_registry_error(
                "issue published session database client",
                format!("{error:?}"),
            )
        })?;
        let relation_graph = Arc::new(std::sync::Mutex::new(
            SessionGraphAttachmentStateV1::Warming,
        ));
        let graph_open_task_key = format!("{shard_id:?}");
        let task_relation_graph = Arc::clone(&relation_graph);
        let graph_settled = Arc::new(tokio::sync::Notify::new());
        let task_graph_settled = Arc::clone(&graph_settled);
        let task_published_lease = published_lease.clone();
        #[cfg(any(test, feature = "test-helpers"))]
        let task_publication_gate = self
            .session_graph_publication_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let registry = self.registry.clone();
        let graph_registry = self.graph_registry.clone();
        let graph_lifecycle_cancelled = Arc::clone(&self.graph_lifecycle_cancelled);
        let incarnation = self.incarnation;
        let retained = self.retained_hook_tasks.retain(
            "session-relation-graph-open",
            &graph_open_task_key,
            move |cancellation| async move {
                let opened =
                    super::code_graph::graph_attachment::open_session_relation_owner_for_task(
                        &registry,
                        &graph_registry,
                        &graph_lifecycle_cancelled,
                        cancellation,
                        incarnation,
                        shard_id,
                    )
                    .await;
                let state = match opened {
                    Ok((graph, store_target)) => {
                        let owner = SessionGraphOwnerV1 {
                            graph,
                            store_target,
                        };
                        match RegisteredSessionOwnerV1::bind_relation_graph(
                            &task_published_lease,
                            &owner,
                            scope,
                        ) {
                            Ok(()) => SessionGraphAttachmentStateV1::Attached {
                                owner: Some(Box::new(owner)),
                            },
                            Err(error) => SessionGraphAttachmentStateV1::Detached {
                                error: error.to_string(),
                            },
                        }
                    }
                    Err(error) => SessionGraphAttachmentStateV1::Detached {
                        error: error.to_string(),
                    },
                };
                // The settled state itself is observable: a waiter arriving
                // after publication takes the state fast path without awaiting
                // the notification. Release this task's counted client lease
                // before publishing that state so every observer sees the
                // completed open and lease release through the same mutex
                // boundary.
                drop(task_published_lease);
                *task_relation_graph
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
                #[cfg(any(test, feature = "test-helpers"))]
                if let Some(gate) = task_publication_gate {
                    gate.block().await;
                }
                task_graph_settled.notify_waiters();
            },
        );
        if !retained {
            *relation_graph
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                SessionGraphAttachmentStateV1::Detached {
                    error: "session relation graph open task admission is closed".to_owned(),
                };
            graph_settled.notify_waiters();
        }
        Ok((
            RegisteredSessionOwnerV1 {
                database,
                relation_graph,
                graph_settled,
                graph_open_task_key,
            },
            published_lease,
        ))
    }

    #[hotpath::skip]
    pub async fn profile_database(&self) -> Result<RegisteredGlobalDbLeaseV1> {
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
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("daemon.store.profile_authority.mount_reuse_total").inc(1_u64);
            return lease;
        }
        let _mount = self.profile_database_mount.lock().await;
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
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("daemon.store.profile_authority.mount_reuse_total").inc(1_u64);
            return lease;
        }
        #[cfg(feature = "hotpath")]
        let _mount_observation = super::StoreMountObservationV1::enter();
        let shard_id = StoreShardIdV1::profile(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
        );
        // Boxed store-open composition: keeps this mount machine (and the
        // measured wrapper embedding it by value) pointer-sized per await.
        let runtime = hotpath::future!(
            Box::pin(open_runtime(
                &self.registry,
                self.resolver.as_ref(),
                StoreRuntimeOpenSpec::new(
                    shard_id,
                    self.incarnation,
                    None,
                    None,
                    true,
                    "mount profile authority store",
                ),
            )),
            label = "daemon.store.profile_authority.mount_open"
        )
        .await?;
        let database =
            Box::pin(self.attach_registered(runtime, "attach profile authority store")).await?;
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

    #[hotpath::skip]
    pub async fn profile_sessions(&self) -> Result<RegisteredGlobalDbLeaseV1> {
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
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("daemon.store.profile_sessions.mount_reuse_total").inc(1_u64);
            return lease;
        }
        let _mount = self.profile_sessions_mount.lock().await;
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
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("daemon.store.profile_sessions.mount_reuse_total").inc(1_u64);
            return lease;
        }
        #[cfg(feature = "hotpath")]
        let _mount_observation = super::StoreMountObservationV1::enter();
        let shard_id = StoreShardIdV1::profile_sessions(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
        );
        let pin = Box::pin(self.profile_authority_pin("mount profile session store")).await?;
        let runtime = hotpath::future!(
            Box::pin(open_runtime(
                &self.registry,
                self.resolver.as_ref(),
                StoreRuntimeOpenSpec::new(
                    shard_id.clone(),
                    self.incarnation,
                    Some(pin),
                    None,
                    true,
                    "mount profile session store",
                ),
            )),
            label = "daemon.store.profile_sessions.open"
        )
        .await?;
        let database =
            Box::pin(self.attach_registered(runtime, "mount profile session store")).await?;
        let (database, lease) = self.publish_session_owner(
            database,
            shard_id,
            SessionRelationScope::profile_sessions(self.identity.profile_id().clone()),
        )?;
        *self
            .profile_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(database);
        Ok(lease)
    }

    #[hotpath::skip]
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
        hotpath::future!(
            tracedecay_runtime_core::db::migrations::ensure_schema_current(&database),
            label = "daemon.session_registry.mount.schema_migrate"
        )
        .await?;
        let database_issuer = owner.weak_lease_issuer();
        let graph = Arc::new(std::sync::Mutex::new(
            MemoryGraphAttachmentStateV1::Warming {
                database: Some(owner),
            },
        ));
        let graph_open_task_key = format!("{shard_id:?}");
        let task_graph = Arc::clone(&graph);
        let task_database = database.clone();
        let identity = self.identity.clone();
        let registry = self.registry.clone();
        let graph_registry = self.graph_registry.clone();
        let graph_lifecycle_cancelled = Arc::clone(&self.graph_lifecycle_cancelled);
        let incarnation = self.incarnation;
        let task_shard_id = shard_id.clone();
        let task_project_id = match &task_shard_id.scope {
            StoreShardScopeV1::Project { project_id } => Some(project_id.clone()),
            _ => None,
        };
        let project_owners = self.project_owners.clone();
        let retained = self.retained_hook_tasks.retain(
            "memory-graph-open",
            &graph_open_task_key,
            move |cancellation| async move {
                let owner = {
                    let mut state = task_graph
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match &mut *state {
                        MemoryGraphAttachmentStateV1::Warming { database } => database.take(),
                        MemoryGraphAttachmentStateV1::Attached { .. }
                        | MemoryGraphAttachmentStateV1::Detached { .. } => None,
                    }
                };
                let Some(owner) = owner else {
                    return;
                };
                let opened = DaemonSessionRuntimeRegistryV1::retain_memory_graph_runtime_for_task(
                    super::code_graph::MemoryGraphRuntimeTaskContext::new(
                        identity,
                        registry,
                        graph_registry,
                        graph_lifecycle_cancelled,
                        incarnation,
                    ),
                    task_shard_id,
                    owner,
                    cancellation,
                )
                .await;
                let state = match opened {
                    Ok(runtime) => {
                        let runtime = Arc::new(runtime);
                        let graph_port: Arc<
                            dyn tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1,
                        > = runtime.clone();
                        let activation = task_database.bind_memory_graph_runtime(graph_port);
                        let reconciliation = activation
                            .as_ref()
                            .ok()
                            .and_then(|()| task_database.memory_graph_reconciliation_task_owner());
                        let error = activation.err().map(|error| error.to_string()).or_else(|| {
                            reconciliation.is_none().then(|| {
                                "memory graph reconciliation owner was not installed".to_owned()
                            })
                        });
                        MemoryGraphAttachmentStateV1::Attached {
                            runtime,
                            reconciliation,
                            error,
                        }
                    }
                    Err(failure) => MemoryGraphAttachmentStateV1::Detached {
                        database: failure.database,
                        error: failure.error.to_string(),
                    },
                };
                *task_graph
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
                if let Some(project_id) = task_project_id
                    && let Err(error) =
                        bind_ready_project_memory_graph(&project_owners, &project_id)
                {
                    tracing::error!(
                        project_id = %project_id,
                        error = %error,
                        "background project memory graph could not bind to project sessions"
                    );
                }
            },
        );
        if !retained {
            let mut state = graph
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let MemoryGraphAttachmentStateV1::Warming { database } = &mut *state
                && let Some(database) = database.take()
            {
                *state = MemoryGraphAttachmentStateV1::Detached {
                    database,
                    error: "memory graph open task admission is closed".to_owned(),
                };
            }
        }
        Ok((
            MemoryStoreOwnerV1 {
                database: database_issuer,
                graph,
                graph_open_task_key,
            },
            Arc::new(database),
        ))
    }

    /// Mounts the distinct profile-memory shard through this daemon's pinned
    /// profile registry. `ProfileMemory` never aliases the profile/global
    /// shard, and publication never reopens a filesystem path.
    #[hotpath::skip]
    pub async fn profile_memory(&self) -> Result<Arc<Database>> {
        let existing = {
            let mounted = self
                .profile_memory
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            mounted.as_ref().map(|owner| {
                owner.issue_database_lease().map(Arc::new).map_err(|error| {
                    session_registry_error(
                        "issue profile memory database client",
                        error.to_string(),
                    )
                })
            })
        };
        if let Some(database) = existing {
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("daemon.store.profile_memory.mount_reuse_total").inc(1_u64);
            return database;
        }
        #[cfg(feature = "hotpath")]
        let _mount_observation = super::StoreMountObservationV1::enter();
        let shard_id = StoreShardIdV1::profile_memory(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
        );
        let pin = self
            .profile_authority_pin("mount profile memory store")
            .await?;
        let runtime = hotpath::future!(
            open_runtime(
                &self.registry,
                self.resolver.as_ref(),
                StoreRuntimeOpenSpec::new(
                    shard_id.clone(),
                    self.incarnation,
                    Some(pin),
                    None,
                    true,
                    "mount profile memory store",
                ),
            ),
            label = "daemon.store.profile_memory.open"
        )
        .await?;
        let (owner, database) = self.publish_memory_owner(shard_id, runtime).await?;
        *self
            .profile_memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(owner);
        Ok(database)
    }

    #[hotpath::skip]
    pub async fn remote_node_storage(
        &self,
        node_id: BrainNodeId,
        keyring: Arc<dyn RemoteSpoolKeyringV1>,
    ) -> Result<RemoteSqliteStorageV1> {
        self.mount_remote_node_storage(node_id, keyring, false)
            .await
    }

    #[hotpath::skip]
    pub async fn provision_remote_node(
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

    #[hotpath::skip]
    async fn mount_remote_node_storage(
        &self,
        node_id: BrainNodeId,
        keyring: Arc<dyn RemoteSpoolKeyringV1>,
        provision_if_new: bool,
    ) -> Result<RemoteSqliteStorageV1> {
        let (database, newly_mounted, existed) = match self.admit_remote_node_owner(&node_id)? {
            super::RemoteNodeOwnerAdmissionV1::Existing(database) => {
                #[cfg(feature = "hotpath")]
                hotpath::gauge!("daemon.store.remote_node.mount_reuse_total").inc(1_u64);
                (database, false, true)
            }
            super::RemoteNodeOwnerAdmissionV1::Opening(mut admission) => {
                #[cfg(feature = "hotpath")]
                let _mount_observation = super::StoreMountObservationV1::enter();
                let shard_id = StoreShardIdV1::remote_node(
                    self.identity.brain_id().clone(),
                    self.identity.profile_id().clone(),
                    node_id.clone(),
                );
                let pin = self
                    .profile_authority_pin("mount Remote Brain node store")
                    .await?;
                let (runtime, existed) = hotpath::future!(
                    open_runtime_with_presence(
                        &self.registry,
                        self.resolver.as_ref(),
                        shard_id,
                        self.incarnation,
                        Some(pin),
                        None,
                        true,
                        false,
                        None,
                        "mount Remote Brain node store",
                    ),
                    label = "daemon.store.remote_node.open"
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
            hotpath::measure_block!(
                "daemon.session_registry.mount.remote_replay_recovery",
                storage.recover_interrupted_replay_attempts(
                    tracedecay_application::clock::now_micros()
                )
            )
            .map_err(|error| {
                session_registry_error("recover interrupted Remote Brain replay", error.to_string())
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
            hotpath::measure_block!(
                "daemon.session_registry.mount.remote_promotion_reconcile",
                recovery.reconcile_interrupted_promotions(&project_id)
            )
            .map_err(|error| {
                session_registry_error(
                    "reconcile interrupted remote promotion",
                    format!("{error:?}"),
                )
            })?;
        }
        Ok(storage)
    }

    pub fn remote_credential_authority(&self) -> Arc<crate::DaemonRemoteCredentialAuthorityV1> {
        Arc::clone(&self.remote_credential_authority)
    }

    /// Canonical Remote Brain operational read for every operator surface
    /// (Doctor, CLI, MCP, dashboard), composed from the mounted remote
    /// authorities.
    pub fn remote_operational_status(
        &self,
    ) -> tracedecay_application::remote::status::RemoteOperationalStatusReadV1 {
        self.remote_credential_authority.operational_status()
    }

    pub fn remote_replay_transaction(
        &self,
    ) -> Arc<crate::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1> {
        Arc::clone(&self.remote_replay_transaction)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub async fn remote_recovery_authority(
        &self,
        node_id: &BrainNodeId,
    ) -> Option<Arc<RemoteRecoverySqliteAuthorityV1>> {
        self.remote_recovery_authorities
            .lock()
            .await
            .get(node_id)
            .cloned()
    }

    #[hotpath::skip]
    pub async fn mounted_session_databases(&self) -> Vec<RegisteredGlobalDbLeaseV1> {
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
    #[hotpath::skip]
    pub async fn close_retained_graph_runtimes_for_shutdown(&self) -> Result<()> {
        let identities = self.drain_retained_graph_owners_for_shutdown()?;
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
    /// attachments and owner-bound graph client leases. Terminal diagnostic
    /// states retain their phase/fault evidence while releasing graph-owning
    /// resources; an in-flight transition fails before any owner is drained.
    fn drain_retained_graph_owners_for_shutdown(
        &self,
    ) -> Result<
        Vec<(
            tracedecay_store::StoreRuntimeBindingV1,
            tracedecay_store::VerifiedStoreLocatorV1,
        )>,
    > {
        {
            let projects = self
                .project_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((project_id, state)) = projects.iter().find_map(|(project_id, state)| {
                let state = match state {
                    ProjectRuntimeOwnerStateV1::Opening => "opening",
                    ProjectRuntimeOwnerStateV1::ReplacingSessions => "replacing_sessions",
                    ProjectRuntimeOwnerStateV1::Recovering => "recovering",
                    ProjectRuntimeOwnerStateV1::Retiring => "retiring",
                    ProjectRuntimeOwnerStateV1::Ready(_)
                    | ProjectRuntimeOwnerStateV1::RecoveryRequired(_)
                    | ProjectRuntimeOwnerStateV1::Faulted(_) => return None,
                };
                Some((project_id, state))
            }) {
                return Err(session_registry_error(
                    "drain graph owners for shutdown",
                    format!(
                        "project runtime owner transition is unfinished for '{}': {state}",
                        project_id.as_str()
                    ),
                ));
            }
        }
        let mut identities = Vec::new();
        let mut retain_identity = |identity: (
            tracedecay_store::StoreRuntimeBindingV1,
            tracedecay_store::VerifiedStoreLocatorV1,
        )| {
            if !identities.iter().any(|current| current == &identity) {
                identities.push(identity);
            }
        };
        if let Some(owner) = self
            .profile_memory
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            && let Some(runtime) = owner.graph_runtime()
        {
            retain_identity(runtime.graph_store_identity());
        }
        if let Some(owner) = self
            .profile_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            && let Some(identity) = owner.take_graph_store_identity()
        {
            retain_identity(identity);
        }
        let mut projects = self
            .project_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for state in projects.values_mut() {
            match state {
                ProjectRuntimeOwnerStateV1::Ready(owners) => {
                    if let Some(memory) = owners.memory.take()
                        && let Some(runtime) = memory.graph_runtime()
                    {
                        retain_identity(runtime.graph_store_identity());
                    }
                    if let Some(sessions) = owners.sessions.take()
                        && let Some(identity) = sessions.take_graph_store_identity()
                    {
                        retain_identity(identity);
                    }
                }
                ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery) => {
                    if let Some(memory) = recovery.memory.take()
                        && let Some(runtime) = memory.graph_runtime()
                    {
                        retain_identity(runtime.graph_store_identity());
                    }
                    if let Some(sessions) = recovery.sessions.take() {
                        retain_identity((
                            sessions.graph.binding().clone(),
                            sessions.graph.verified_locator().clone(),
                        ));
                    }
                    if let Some(sessions) = recovery.candidate_sessions.take()
                        && let Some(identity) = sessions.take_graph_store_identity()
                    {
                        retain_identity(identity);
                    }
                }
                ProjectRuntimeOwnerStateV1::Faulted(faulted) => {
                    if let Some(memory) = faulted.retained.memory.take()
                        && let Some(runtime) = memory.graph_runtime()
                    {
                        retain_identity(runtime.graph_store_identity());
                    }
                    if let Some(sessions) = faulted.retained.sessions.take()
                        && let Some(identity) = sessions.take_graph_store_identity()
                    {
                        retain_identity(identity);
                    }
                    if let Some(sessions) = faulted.sessions.take() {
                        retain_identity((
                            sessions.graph.binding().clone(),
                            sessions.graph.verified_locator().clone(),
                        ));
                    }
                }
                ProjectRuntimeOwnerStateV1::Opening
                | ProjectRuntimeOwnerStateV1::ReplacingSessions
                | ProjectRuntimeOwnerStateV1::Recovering
                | ProjectRuntimeOwnerStateV1::Retiring => {
                    return Err(session_registry_error(
                        "drain graph owners for shutdown",
                        "project runtime owner transition changed after terminal preflight"
                            .to_owned(),
                    ));
                }
            }
        }
        drop(projects);
        Ok(identities)
    }

    #[hotpath::skip]
    pub async fn mounted_project_sessions(
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

    #[hotpath::skip]
    pub async fn project_sessions(
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

    #[hotpath::skip]
    pub async fn mount_registered_project_sessions(
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
                        #[cfg(feature = "hotpath")]
                        hotpath::gauge!("daemon.store.project_sessions.mount_reuse_total")
                            .inc(1_u64);
                        return self.issue_session_owner_lease(
                            owner,
                            SessionRelationScope::project_sessions(project_id.clone()),
                        );
                    }
                    true
                }
                Some(ProjectRuntimeOwnerStateV1::Opening) => {
                    #[cfg(feature = "hotpath")]
                    hotpath::gauge!("daemon.session_registry.mount.denied_total").inc(1_u64);
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
                    #[cfg(feature = "hotpath")]
                    hotpath::gauge!("daemon.session_registry.mount.denied_total").inc(1_u64);
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
                #[cfg(feature = "hotpath")]
                hotpath::gauge!("daemon.store.project_sessions.mount_reuse_total").inc(1_u64);
                return self.issue_session_owner_lease(
                    owner,
                    SessionRelationScope::project_sessions(project_id.clone()),
                );
            }
        };
        #[cfg(feature = "hotpath")]
        let _mount_observation = super::StoreMountObservationV1::enter();
        let shard_id = StoreShardIdV1::project_sessions(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id.clone(),
        );
        let pin = self
            .profile_authority_pin("mount project session store")
            .await?;
        let runtime = hotpath::future!(
            open_runtime(
                &self.registry,
                self.resolver.as_ref(),
                StoreRuntimeOpenSpec::new(
                    shard_id.clone(),
                    self.incarnation,
                    Some(pin),
                    None,
                    true,
                    "mount project session store",
                ),
            ),
            label = "daemon.store.project_sessions.open"
        )
        .await?;
        let database = self
            .attach_registered(runtime, "mount project session store")
            .await?;
        let (database, lease) = self.publish_session_owner(
            database,
            shard_id,
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
        bind_ready_project_memory_graph(&self.project_owners, &project_id)?;
        let recoveries = self
            .remote_recovery_authorities
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for recovery in recoveries {
            hotpath::measure_block!(
                "daemon.session_registry.mount.remote_promotion_reconcile",
                recovery.reconcile_interrupted_promotions(&project_id)
            )
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
    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub async fn project_memory(
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
                Some(ProjectRuntimeOwnerStateV1::Opening) => {
                    #[cfg(feature = "hotpath")]
                    hotpath::gauge!("daemon.session_registry.mount.denied_total").inc(1_u64);
                    Err(TraceDecayError::project_route(
                        "project_runtime_opening",
                        true,
                        "Project runtime is already opening",
                    ))
                }
                Some(
                    ProjectRuntimeOwnerStateV1::Retiring
                    | ProjectRuntimeOwnerStateV1::ReplacingSessions
                    | ProjectRuntimeOwnerStateV1::Recovering
                    | ProjectRuntimeOwnerStateV1::RecoveryRequired(_)
                    | ProjectRuntimeOwnerStateV1::Faulted(_),
                ) => {
                    #[cfg(feature = "hotpath")]
                    hotpath::gauge!("daemon.session_registry.mount.denied_total").inc(1_u64);
                    Err(TraceDecayError::project_route(
                        "project_runtime_retiring",
                        true,
                        "Project runtime is unavailable while retirement is terminal or in progress",
                    ))
                }
                None => Ok((false, None)),
            }
        }?;
        if let Some(database) = existing {
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("daemon.store.project_memory.mount_reuse_total").inc(1_u64);
            return Ok(database);
        }
        #[cfg(feature = "hotpath")]
        let _mount_observation = super::StoreMountObservationV1::enter();
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
        let pin = self
            .profile_authority_pin("mount project memory store")
            .await?;
        let runtime = hotpath::future!(
            open_runtime(
                &self.registry,
                self.resolver.as_ref(),
                StoreRuntimeOpenSpec::new(
                    shard_id.clone(),
                    self.incarnation,
                    Some(pin),
                    None,
                    true,
                    "mount project memory store",
                ),
            ),
            label = "daemon.store.project_memory.open"
        )
        .await?;
        let (owner, database) = self.publish_memory_owner(shard_id, runtime).await?;
        admission.publish_memory(owner)?;
        bind_ready_project_memory_graph(&self.project_owners, &project_id)?;
        Ok(database)
    }

    /// Mounts an existing project-memory shard without initializing it or
    /// verifying its schema, and exposes only a read-only database facade.
    #[hotpath::skip]
    pub async fn project_memory_read_only(
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
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("daemon.store.project_memory.mount_reuse_total").inc(1_u64);
            return Ok(database);
        }
        #[cfg(feature = "hotpath")]
        let _mount_observation = super::StoreMountObservationV1::enter();
        let shard_id = StoreShardIdV1::project(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
        );
        let pin = self
            .profile_authority_pin("mount project memory store read-only")
            .await?;
        let runtime = match hotpath::future!(
            self.registry.open(StoreRuntimeOpenRequest::new_read_only(
                shard_id.clone(),
                self.incarnation,
                Some(pin),
            )),
            label = "daemon.store.project_memory.open_read_only"
        )
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

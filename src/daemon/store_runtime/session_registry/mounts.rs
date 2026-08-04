use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(test)]
use std::sync::atomic::AtomicBool;
use tokio::sync::Mutex;
use tracedecay_store::{ProjectId, StoreShardIdV1};

use super::{
    DaemonSessionRuntimeRegistryV1, Database, DatabaseAccessMode, LifecycleShardRuntimePublisher,
    LocalProfileIdentityAuthorityV1, LocalProfileStoreAuthorityV1,
    LocalProjectEnrollmentAuthorityV1, LocalStoreRuntimeResolverV1, ProfileAuthorityPinResult,
    RegisteredGlobalDb, RegisteredSchemaConvergenceMaintenance, Result, StoreRuntimeOpenRequest,
    StoreRuntimeOpenResult, StoreRuntimeRegistry, StoreRuntimeResolver, open_runtime,
    register_registered_schema_installer, registry_open_error, runtime_incarnation,
    session_registry_error,
};

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) async fn open(identity: LocalProfileIdentityAuthorityV1) -> Result<Self> {
        // The kernel's registry initialises profile- and session-scoped shards
        // through a fail-closed port, because the registered schema lives in
        // `tracedecay-global-db` (which depends on the kernel transitively).
        // This is the sole constructor of the production registry, so it is the
        // one place that must supply the installer.
        register_registered_schema_installer();
        // `main` registers these for every real invocation; this constructor
        // is also reached by embedded and integration-test runtimes that never
        // pass through it, and transcript ingest starts here. Idempotent.
        crate::register_runtime_ports();
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
        crate::db::migrations::ensure_schema_current(database.as_ref()).await?;
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
        crate::db::migrations::ensure_schema_current(database.as_ref()).await?;
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
        let runtime = match self
            .registry
            .open(StoreRuntimeOpenRequest::new_read_only(
                shard_id,
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

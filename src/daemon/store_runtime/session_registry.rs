//! Daemon-owned registry assembly for profile and project session shards.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Mutex;
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
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) async fn open(identity: LocalProfileIdentityAuthorityV1) -> Result<Self> {
        let incarnation = runtime_incarnation(&identity)?;
        let resolver = Arc::new(LocalStoreRuntimeResolverV1::new(
            LocalProfileStoreAuthorityV1::from_profile_identity(&identity),
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
        })
    }

    pub(crate) async fn profile_database(&self) -> Result<Arc<RegisteredGlobalDb>> {
        let mut mounted = self.profile_database.lock().await;
        if let Some(database) = mounted.as_ref() {
            return Ok(Arc::clone(database));
        }
        let database = attach_registered(
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
        let database = attach_registered(runtime, "mount profile session store").await?;
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
        let database = attach_registered(runtime, "mount project session store").await?;
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
        database_authority: Option<DatabaseAuthority>,
        initialize_if_missing: bool,
    ) -> Result<StoreRuntimeHandle> {
        self.resolver
            .register_code_authority(
                LocalCodeStoreAuthorityV1::new(shard_id.clone(), database_path).map_err(
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

async fn attach_registered(
    runtime: StoreRuntimeHandle,
    operation: &'static str,
) -> Result<Arc<RegisteredGlobalDb>> {
    let expected_binding: StoreRuntimeBindingV1 = runtime.binding().clone();
    let schedule_structured_backfill = matches!(
        &expected_binding.shard_id.scope,
        tracedecay_store::StoreShardScopeV1::ProfileSessions
            | tracedecay_store::StoreShardScopeV1::ProjectSessions { .. }
    );
    let expected_locator = runtime.locator().verified().clone();
    let authority = runtime
        .database_authority(operation)
        .map_err(|failure| registry_open_error(operation, failure))?;
    let database = Arc::new(
        RegisteredGlobalDb::migrate_and_attach(
            runtime,
            expected_binding,
            expected_locator,
            authority,
        )
        .await?,
    );
    if schedule_structured_backfill && LONG_LIVED_SESSION_MAINTENANCE.load(Ordering::Relaxed) {
        let database = Arc::clone(&database);
        tokio::spawn(async move {
            let _ = crate::sessions::transcript_backfill::backfill_structured_rows(&database).await;
        });
    }
    Ok(database)
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
    use crate::db::engine::TestConnection;

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
            .execute_batch("DROP TABLE projects")
            .await
            .expect("remove required registry table");
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
            error.to_string().contains("authority schema"),
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

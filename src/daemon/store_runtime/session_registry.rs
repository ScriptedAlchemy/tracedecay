//! Daemon-owned registry assembly for profile and project session shards.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

const INITIAL_STORE_INCARNATION: u64 = 1;

/// One canonical registry and profile pin shared by every daemon session shard.
pub(crate) struct DaemonSessionRuntimeRegistryV1 {
    identity: LocalProfileIdentityAuthorityV1,
    resolver: Arc<LocalStoreRuntimeResolverV1>,
    registry: StoreRuntimeRegistry,
    profile_pin: ProfileAuthorityPin,
    profile_runtime: StoreRuntimeHandle,
    profile_database: Mutex<Option<Arc<RegisteredGlobalDb>>>,
    profile_memory: Mutex<Option<Arc<Database>>>,
    profile_sessions: Mutex<Option<Arc<RegisteredGlobalDb>>>,
    project_sessions: Mutex<BTreeMap<ProjectId, Arc<RegisteredGlobalDb>>>,
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) async fn open(identity: LocalProfileIdentityAuthorityV1) -> Result<Self> {
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
            resolver,
            registry,
            profile_pin,
            profile_runtime,
            profile_database: Mutex::new(None),
            profile_memory: Mutex::new(None),
            profile_sessions: Mutex::new(None),
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
            Some(self.profile_pin.clone()),
            None,
            true,
            "mount profile memory store",
        )
        .await?;
        let database =
            Arc::new(Database::publish_runtime(runtime, DatabaseAccessMode::ReadWrite).await?);
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
            Some(self.profile_pin.clone()),
            Some(database_authority),
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
        let identity = crate::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(
            project_root,
        )
        .map_err(|error| {
            session_registry_error("resolve code-shard identity", error.to_string())
        })?;
        let shard_id = StoreShardIdV1::code(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
            identity.repository_id().clone(),
            CodeShardScopeV1::Worktree {
                worktree_id: identity.worktree_id().clone(),
            },
        );
        let runtime = self
            .code_graph(shard_id, database_path, database_authority)
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
            .code_graph(shard_id, database_path, database_authority)
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

async fn open_runtime(
    registry: &StoreRuntimeRegistry,
    resolver: &LocalStoreRuntimeResolverV1,
    shard_id: StoreShardIdV1,
    profile_pin: Option<ProfileAuthorityPin>,
    database_authority: Option<DatabaseAuthority>,
    initialize_if_missing: bool,
    operation: &'static str,
) -> Result<StoreRuntimeHandle> {
    let incarnation = StoreIncarnationV1::new(INITIAL_STORE_INCARNATION)
        .map_err(|error| session_registry_error("create store incarnation", error.to_string()))?;
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
    let expected_locator = runtime.locator().verified().clone();
    let authority = runtime
        .database_authority(operation)
        .map_err(|failure| registry_open_error(operation, failure))?;
    RegisteredGlobalDb::migrate_and_attach(runtime, expected_binding, expected_locator, authority)
        .await
        .map(Arc::new)
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
        assert!(
            registered
                .storage_telemetry(
                    tracedecay_application::storage::StoreKeyV1::new(
                        crate::sessions::USER_SESSIONS_DB_FILENAME,
                    )
                    .expect("session store key"),
                    std::time::Duration::from_secs(1),
                )
                .is_ok(),
            "published production runtime must expose only its writerless telemetry channel"
        );
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
}

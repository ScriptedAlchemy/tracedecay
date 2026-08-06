//! Behavioral production route coverage: profile/project/session shards
//! mount through [`LifecycleShardRuntimePublisher`] and serve health over the
//! reserved reader data port.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_domain::{
    BrainId, BrainNodeId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, UtcMicros,
    WorktreeId,
};
use tracedecay_store::{
    CodeShardScopeV1, ConsistencyModeV1, OperationPriorityV1, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeReadOperationV1,
    RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    StorageRuntimeReadPort, StoreShardIdV1, StoreShardScopeV1, VerifiedStoreLocatorV1,
};

use super::super::*;
use super::support::{id, incarnation, open_published, profile_shard, project_request};

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<tracedecay_store::RuntimeInterruptionV1> {
        None
    }
}

#[derive(Default)]
struct FileResolver {
    roots: Mutex<Vec<PathBuf>>,
    calls: AtomicUsize,
}

#[derive(Default)]
struct InitializingFileResolver {
    roots: Mutex<Vec<PathBuf>>,
    calls: AtomicUsize,
}

impl InitializingFileResolver {
    fn push(&self, path: PathBuf) {
        self.roots.lock().unwrap().push(path);
    }
}

impl StoreRuntimeResolver for InitializingFileResolver {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        mode: StoreRuntimeOpenMode,
        database_authority: Option<&'a crate::db::DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let path = self.roots.lock().unwrap()[call].clone();
        let locator = VerifiedStoreLocatorV1::new(
            key.shard_id.clone(),
            key.incarnation,
            LocatorDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
        );
        Box::pin(async move {
            let authority = database_authority.ok_or_else(|| {
                StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "initialization requires database authority".to_owned(),
                }
            })?;
            if authority.canonical_database_path() != path {
                return Err(StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "database authority does not match prospective path".to_owned(),
                });
            }
            match mode {
                StoreRuntimeOpenMode::Initialize => {
                    Ok(ResolvedStoreLocator::prospective(locator, path))
                }
                StoreRuntimeOpenMode::Existing => Ok(ResolvedStoreLocator::new(locator, path)),
            }
        })
    }
}

impl FileResolver {
    fn push(&self, path: PathBuf) {
        self.roots.lock().unwrap().push(path);
    }
}

impl StoreRuntimeResolver for FileResolver {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        _mode: StoreRuntimeOpenMode,
        _database_authority: Option<&'a crate::db::DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let path = self
            .roots
            .lock()
            .unwrap()
            .get(call)
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/missing-runtime-route.db"));
        let locator = VerifiedStoreLocatorV1::new(
            key.shard_id.clone(),
            key.incarnation,
            LocatorDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        );
        Box::pin(async move { Ok(ResolvedStoreLocator::new(locator, path)) })
    }
}

fn health_request(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
) -> (RuntimeReadRequestV1, Probe) {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new("cancel.runtime-production-health").unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new("deadline.runtime-production-health").unwrap(),
    };
    let control = RuntimeRequestControlV1 {
        requested_at: UtcMicros(1),
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    (
        RuntimeReadRequestV1::new(
            binding.clone(),
            ConsistencyModeV1::LatestAvailable,
            RuntimeReadOperationV1::TemporalHealth,
            OperationPriorityV1::Health,
            1,
            control,
        )
        .unwrap(),
        Probe {
            cancellation,
            deadline,
        },
    )
}

fn seed_db(root: &TempDir, name: &str) -> PathBuf {
    let path = root.path().join(name);
    Connection::open(&path).unwrap();
    path.canonicalize().unwrap()
}

fn sessions_request(project: &str, pin: &ProfileAuthorityPin) -> StoreRuntimeOpenRequest {
    StoreRuntimeOpenRequest::new(
        StoreShardIdV1::project_sessions(
            id::<BrainId>("brain.registry"),
            id::<UserProfileId>("profile.registry"),
            id::<ProjectId>(project),
        ),
        incarnation(),
        Some(pin.clone()),
    )
}

async fn assert_health_route(handle: &StoreRuntimeHandle, writer_expected: bool) {
    assert!(
        !matches!(
            handle.binding().shard_id.scope,
            StoreShardScopeV1::Code { .. }
        ),
        "repository routes must not mount code-shard executors"
    );
    let snapshot = handle.physical_snapshot();
    assert!(snapshot.healthy, "mounted runtime must report healthy");
    assert_eq!(
        snapshot.writer_present, writer_expected,
        "mounted runtime writer ownership must match its requested access"
    );
    assert!(
        snapshot.reader_handles >= 1,
        "mounted runtime must retain reserved readers"
    );

    let (request, probe) = health_request(handle.binding());
    let outcome = StorageRuntimeReadPort::read(handle, request, &probe)
        .await
        .expect("health data port must be mounted");
    assert!(matches!(
        outcome.value(),
        Some(RuntimeReadResultV1::TemporalHealth { healthy: true })
    ));
}

#[tokio::test]
async fn lifecycle_publisher_mounts_profile_project_and_session_health_routes() {
    let root = TempDir::new().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(seed_db(&root, "profile.db"));
    resolver.push(seed_db(&root, "project.db"));
    resolver.push(seed_db(&root, "sessions.db"));

    let registry = StoreRuntimeRegistry::with_config(
        resolver,
        Arc::new(LifecycleShardRuntimePublisher),
        StoreRuntimeRegistryConfig::new(2).unwrap(),
    )
    .unwrap();

    let profile = open_published(
        &registry,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    let pin = match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    };
    let project = open_published(&registry, project_request("project.runtime-route", &pin)).await;
    let sessions = open_published(&registry, sessions_request("project.runtime-route", &pin)).await;

    assert_health_route(&profile, true).await;
    assert_health_route(&project, true).await;
    assert_health_route(&sessions, true).await;
}

#[tokio::test]
async fn read_only_open_mounts_readers_without_acquiring_a_writer() {
    let root = TempDir::new().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(seed_db(&root, "profile.db"));
    resolver.push(seed_db(&root, "project.db"));
    let registry = StoreRuntimeRegistry::new(resolver, Arc::new(LifecycleShardRuntimePublisher));
    let _profile = open_published(
        &registry,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    let pin = match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    };

    let project = open_published(
        &registry,
        StoreRuntimeOpenRequest::new_read_only(
            StoreShardIdV1::project(
                id::<BrainId>("brain.registry"),
                id::<UserProfileId>("profile.registry"),
                id::<ProjectId>("project.reader-only"),
            ),
            incarnation(),
            Some(pin),
        ),
    )
    .await;
    let physical = project.physical_snapshot();
    assert!(!physical.writer_present);
    assert!(physical.reader_handles > 0);
    assert_health_route(&project, false).await;
}

#[tokio::test]
async fn remote_node_initialization_installs_only_the_final_registered_schema() {
    let root = TempDir::new().unwrap();
    let profile_path = root.path().join("profile.db");
    let remote_path = root.path().join("remote.db");
    let resolver = Arc::new(InitializingFileResolver::default());
    resolver.push(profile_path.clone());
    resolver.push(remote_path.clone());
    let registry =
        StoreRuntimeRegistry::new(resolver, Arc::new(LifecycleShardRuntimePublisher));

    let profile_authority =
        crate::db::DatabaseAuthority::acquire_test(&profile_path, "initialize profile").unwrap();
    open_published(
        &registry,
        StoreRuntimeOpenRequest::new_initialize_authorized(
            profile_shard(),
            incarnation(),
            None,
            profile_authority,
        ),
    )
    .await;
    let pin = match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    };
    let remote_authority =
        crate::db::DatabaseAuthority::acquire_test(&remote_path, "initialize remote node").unwrap();
    let remote = open_published(
        &registry,
        StoreRuntimeOpenRequest::new_initialize_authorized(
            StoreShardIdV1::remote_node(
                id::<BrainId>("brain.registry"),
                id::<UserProfileId>("profile.registry"),
                id::<BrainNodeId>("node.remote"),
            ),
            incarnation(),
            Some(pin),
            remote_authority,
        ),
    )
    .await;

    assert!(remote.schema_migrated());
    let tables: Vec<String> = Connection::open(&remote_path)
        .unwrap()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        tables,
        vec![
            "remote_authorities",
            "remote_enrollment_grants",
            "remote_enrollments",
            "remote_node_identity",
            "remote_query_policies",
            "remote_recovery_authorities",
            "remote_recovery_operations",
            "remote_recovery_sink_installations",
            "remote_replay_policies",
            "remote_replay_recovery_lease",
            "remote_spool_frames",
            "remote_store_contract",
        ]
    );
}

#[tokio::test]
async fn distinct_logical_shards_cannot_publish_two_writers_for_one_database() {
    let root = TempDir::new().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(seed_db(&root, "profile.db"));
    let shared_path = seed_db(&root, "shared-project.db");
    resolver.push(shared_path.clone());
    let registry = StoreRuntimeRegistry::new(resolver, Arc::new(LifecycleShardRuntimePublisher));
    let _profile = open_published(
        &registry,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    let pin = match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    };
    let shard = |worktree: &str| {
        StoreShardIdV1::code(
            id::<BrainId>("brain.registry"),
            id::<UserProfileId>("profile.registry"),
            id::<ProjectId>("project.shared-writer"),
            id::<RepositoryId>("repository.shared-writer"),
            CodeShardScopeV1::Worktree {
                worktree_id: id::<WorktreeId>(worktree),
            },
        )
    };
    let authority =
        crate::db::DatabaseAuthority::acquire_test(&shared_path, "shared writer registry test")
            .unwrap();
    let first = open_published(
        &registry,
        StoreRuntimeOpenRequest::new_authorized(
            shard("worktree.first"),
            incarnation(),
            Some(pin.clone()),
            authority.clone(),
        ),
    )
    .await;
    assert!(first.writer_present());

    assert!(matches!(
        registry
            .open(StoreRuntimeOpenRequest::new_authorized(
                shard("worktree.second"),
                incarnation(),
                Some(pin),
                authority,
            ))
            .await,
        StoreRuntimeOpenResult::Failed(
            StoreRuntimeRegistryFailure::DatabaseRuntimeIdentityConflict { path, .. }
        ) if path == shared_path
    ));
}

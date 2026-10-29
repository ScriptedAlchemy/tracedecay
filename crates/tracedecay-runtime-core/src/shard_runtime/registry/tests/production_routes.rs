//! Behavioral production route coverage: profile/project/session shards
//! mount through [`LifecycleShardRuntimePublisher`] and serve health over the
//! reserved reader data port.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_domain::{
    BrainId, BrainNodeId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, UtcMicros,
    WorktreeId,
};
use tracedecay_graph_db::{
    GraphDbOwnerRegistrationV1, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig,
    NeverCancelled,
};
use tracedecay_store::{
    CodeShardScopeV1, ConsistencyModeV1, OperationPriorityV1, RetainedGraphStoreLeaseV1,
    RetainedGraphStoreOwnerAttachmentV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeReadOperationV1, RuntimeReadRequestV1,
    RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1, StoreShardIdV1,
    StoreShardScopeV1, VerifiedStoreLocatorV1, canonical_store_locator_digest,
};

use super::super::*;
use super::support::{
    id, incarnation, open_published, profile_shard, project_request, project_shard,
};

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

    fn try_begin_commit(&self) -> bool {
        false
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
        self.roots
            .lock()
            .unwrap()
            .push(crate::path_safety::canonicalize_path_or_existing_parent(
                &path,
            ));
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
            let authority =
                database_authority.ok_or_else(|| StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "initialization requires database authority".to_owned(),
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

    fn resolve_graph<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        Box::pin(async move {
            let resolved = self
                .resolve(key, StoreRuntimeOpenMode::Existing, None)
                .await?;
            Ok(ResolvedStoreLocator::new(
                VerifiedStoreLocatorV1::new(
                    key.shard_id.clone(),
                    key.incarnation,
                    canonical_store_locator_digest(resolved.path()).unwrap(),
                ),
                resolved.path().to_path_buf(),
            ))
        })
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

async fn seed_final_graph_db(root: &TempDir, name: &str) -> PathBuf {
    let path = root.path().join(name);
    let connection = crate::db::engine::TestConnection::open(&path);
    crate::db::migrations::create_schema_connection(&connection)
        .await
        .expect("seed exact final graph schema");
    drop(connection);
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

async fn assert_health_route(handle: &StoreRuntimeClientLease, writer_expected: bool) {
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
    let outcome = handle
        .dispatch_read(request, Arc::new(probe))
        .await
        .expect("health data port must be mounted");
    assert!(matches!(
        outcome.value(),
        Some(RuntimeReadResultV1::TemporalHealth { healthy: true })
    ));
}

#[asupersync::test]
async fn lifecycle_publisher_mounts_profile_project_and_session_health_routes() {
    let root = TempDir::new().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(seed_db(&root, "profile.db"));
    resolver.push(seed_final_graph_db(&root, "project.db").await);
    resolver.push(seed_db(&root, "sessions.db"));

    let registry = StoreRuntimeRegistry::with_config(
        asupersync::runtime::Runtime::current_handle().unwrap(),
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

#[asupersync::test]
async fn read_only_open_mounts_readers_without_acquiring_a_writer() {
    let root = TempDir::new().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(seed_final_graph_db(&root, "profile.db").await);
    resolver.push(seed_final_graph_db(&root, "project.db").await);
    let registry = StoreRuntimeRegistry::new(
        asupersync::runtime::Runtime::current_handle().unwrap(),
        resolver,
        Arc::new(LifecycleShardRuntimePublisher),
    )
    .unwrap();
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

/// The store file is a FIFO, so opening it waits until a writer opens the
/// other end. Only a task on the runtime's single worker does that while the
/// open waits; a fallback thread ends a wait that parked the worker, so the
/// failure is an assertion rather than a hang.
#[cfg(unix)]
#[asupersync::test]
async fn store_open_keeps_the_async_worker_serving_while_the_file_opens() {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::AtomicBool;

    let root = TempDir::new().unwrap();
    let held = root.path().join("held.db");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&held)
            .status()
            .unwrap()
            .success()
    );
    let held = held.canonicalize().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(held.clone());
    let registry = StoreRuntimeRegistry::new(
        asupersync::runtime::Runtime::current_handle().unwrap(),
        resolver,
        Arc::new(LifecycleShardRuntimePublisher),
    )
    .unwrap();

    let (released_by_worker, fallback_stop) = std::sync::mpsc::channel::<()>();
    let fallback = {
        let held = held.clone();
        std::thread::spawn(move || {
            let parked = fallback_stop.recv_timeout(Duration::from_secs(10)).is_err();
            if parked {
                drop(OpenOptions::new().write(true).open(&held));
            }
            parked
        })
    };
    let settled = AtomicBool::new(false);
    let open = async {
        let outcome = registry
            .open(StoreRuntimeOpenRequest::new(
                profile_shard(),
                incarnation(),
                None,
            ))
            .await;
        settled.store(true, Ordering::SeqCst);
        outcome
    };
    let release = async {
        let cx = asupersync::Cx::current().unwrap();
        while !settled.load(Ordering::SeqCst) {
            match OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&held)
            {
                Ok(_writer) => {
                    released_by_worker.send(()).unwrap();
                    return true;
                }
                Err(error) => assert_eq!(error.raw_os_error(), Some(libc::ENXIO)),
            }
            asupersync::time::sleep(cx.now(), Duration::from_millis(1)).await;
        }
        false
    };
    let (outcome, released) = asupersync::join!(open, release);
    drop(released_by_worker);
    let parked = fallback.join().unwrap();

    assert!(
        released && !parked,
        "the store open parked the runtime's only worker until the fallback thread released the file"
    );
    assert!(
        matches!(
            outcome,
            StoreRuntimeOpenResult::Failed(
                StoreRuntimeRegistryFailure::PhysicalRuntimeFailed { .. }
            )
        ),
        "a FIFO is not a SQLite store: {outcome:?}"
    );
}

#[asupersync::test]
async fn read_only_open_refuses_nonfinal_schema_as_reset_required_before_publication() {
    let root = TempDir::new().unwrap();
    let profile_path = seed_final_graph_db(&root, "profile.db").await;
    let project_path = seed_final_graph_db(&root, "project.db").await;
    Connection::open(&project_path)
        .unwrap()
        .execute_batch(
            "UPDATE tracedecay_schema_version SET version = 31 WHERE component = 'graph';",
        )
        .unwrap();

    let resolver = Arc::new(FileResolver::default());
    resolver.push(profile_path);
    resolver.push(project_path);
    let registry = StoreRuntimeRegistry::new(
        asupersync::runtime::Runtime::current_handle().unwrap(),
        resolver,
        Arc::new(LifecycleShardRuntimePublisher),
    )
    .unwrap();
    let _profile = open_published(
        &registry,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    let pin = match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    };

    let result = registry
        .open(StoreRuntimeOpenRequest::new_read_only(
            StoreShardIdV1::project(
                id::<BrainId>("brain.registry"),
                id::<UserProfileId>("profile.registry"),
                id::<ProjectId>("project.reader-only-reset"),
            ),
            incarnation(),
            Some(pin),
        ))
        .await;

    assert!(matches!(
        result,
        StoreRuntimeOpenResult::Failed(StoreRuntimeRegistryFailure::ResetRequired {
            authority,
            reason,
        }) if authority == "SQLite store" && reason.contains("schema v31")
    ));
    assert_eq!(
        registry
            .inventory(tracedecay_store::AdmissionConfigV1::default(), None)
            .entries
            .len(),
        1,
        "the reset-required project runtime must not be published beside the profile authority"
    );
}

#[asupersync::test]
async fn writable_existing_open_refuses_tampered_final_shape_before_publication() {
    let root = TempDir::new().unwrap();
    let profile_path = seed_db(&root, "profile.db");
    let project_path = seed_final_graph_db(&root, "project.db").await;
    Connection::open(&project_path)
        .unwrap()
        .execute_batch("DROP INDEX idx_read_cache_session;")
        .unwrap();

    let resolver = Arc::new(FileResolver::default());
    resolver.push(profile_path);
    resolver.push(project_path.clone());
    let registry = StoreRuntimeRegistry::new(
        asupersync::runtime::Runtime::current_handle().unwrap(),
        resolver,
        Arc::new(LifecycleShardRuntimePublisher),
    )
    .unwrap();
    let _profile = open_published(
        &registry,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    let pin = match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    };
    let authority = crate::db::DatabaseAuthority::acquire_test(
        &project_path,
        "writable existing final-shape admission",
    )
    .unwrap();

    let result = registry
        .open(StoreRuntimeOpenRequest::new_authorized(
            StoreShardIdV1::project(
                id::<BrainId>("brain.registry"),
                id::<UserProfileId>("profile.registry"),
                id::<ProjectId>("project.writer-reset"),
            ),
            incarnation(),
            Some(pin),
            authority,
        ))
        .await;

    assert!(matches!(
        result,
        StoreRuntimeOpenResult::Failed(StoreRuntimeRegistryFailure::ResetRequired {
            authority,
            reason,
        }) if authority == "SQLite store"
            && reason.contains("missing required index 'idx_read_cache_session'")
    ));
    assert_eq!(
        registry
            .inventory(tracedecay_store::AdmissionConfigV1::default(), None)
            .entries
            .len(),
        1,
        "the tampered writable project runtime must not be published"
    );
}

#[asupersync::test]
async fn remote_node_initialization_installs_only_the_final_registered_schema() {
    let root = TempDir::new().unwrap();
    let profile_path = root.path().join("profile.db");
    let remote_path = root.path().join("remote.db");
    let resolver = Arc::new(InitializingFileResolver::default());
    resolver.push(profile_path.clone());
    resolver.push(remote_path.clone());
    let registry = StoreRuntimeRegistry::new(
        asupersync::runtime::Runtime::current_handle().unwrap(),
        resolver,
        Arc::new(LifecycleShardRuntimePublisher),
    )
    .unwrap();

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
    open_published(
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

#[asupersync::test]
async fn distinct_logical_shards_cannot_publish_two_writers_for_one_database() {
    let root = TempDir::new().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(seed_db(&root, "profile.db"));
    let shared_path = seed_final_graph_db(&root, "shared-project.db").await;
    resolver.push(shared_path.clone());
    let registry = StoreRuntimeRegistry::new(
        asupersync::runtime::Runtime::current_handle().unwrap(),
        resolver,
        Arc::new(LifecycleShardRuntimePublisher),
    )
    .unwrap();
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

/// Rewinds a final-shape graph store to the pre-digest shape a released v34
/// binary left behind: the payload-digest objects are dropped and the stamp
/// moved back one step.
async fn rewind_to_pre_digest_shape(path: &Path) {
    let connection = crate::db::engine::TestConnection::open(path);
    connection
        .execute_batch(
            "DROP TRIGGER memory_v2_payloads_digest_delete;
             DROP TRIGGER memory_v2_assertion_payload_digests_no_update;
             DROP INDEX memory_v2_assertion_payload_digests_lookup;
             DROP TABLE memory_v2_assertion_payload_digests;
             UPDATE tracedecay_schema_version SET version = 34 WHERE component = 'graph';",
        )
        .await
        .expect("rewind graph store to the pre-digest shape");
}

fn user_version(path: &Path) -> u32 {
    let connection = Connection::open(path).unwrap();
    let version = crate::db::schema_version::read_version_sync(
        &connection,
        crate::db::schema_version::GRAPH_COMPONENT,
    )
    .unwrap()
    .unwrap_or(0);
    u32::try_from(version).unwrap()
}

/// A store at an older released schema is never upgraded in place, even by a
/// write-authorized open: admission fails with the typed fresh-start refusal
/// and leaves the stamp untouched.
#[asupersync::test]
async fn existing_store_at_an_older_schema_is_refused_even_with_write_authority() {
    let root = TempDir::new().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(seed_final_graph_db(&root, "profile.db").await);
    let project_path = seed_final_graph_db(&root, "project.db").await;
    rewind_to_pre_digest_shape(&project_path).await;
    resolver.push(project_path.clone());
    let registry = StoreRuntimeRegistry::new(
        asupersync::runtime::Runtime::current_handle().unwrap(),
        resolver,
        Arc::new(LifecycleShardRuntimePublisher),
    )
    .unwrap();
    let _profile = open_published(
        &registry,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    let pin = match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    };
    let authority =
        crate::db::DatabaseAuthority::acquire_test(&project_path, "open an older store")
            .expect("test database authority");

    let outcome = registry
        .open(StoreRuntimeOpenRequest::new_authorized(
            project_shard("project.older-schema"),
            incarnation(),
            Some(pin),
            authority,
        ))
        .await;

    match outcome {
        StoreRuntimeOpenResult::Failed(StoreRuntimeRegistryFailure::ResetRequired {
            authority,
            reason,
        }) => {
            assert_eq!(authority, "SQLite store");
            assert!(
                reason.contains("cannot be upgraded in place"),
                "the refusal must name the fresh-start remedy: {reason}"
            );
        }
        other => panic!("an older store must be refused, not upgraded: {other:?}"),
    }
    assert_eq!(
        user_version(&project_path),
        34,
        "a refused open must leave the stamp alone"
    );
}

struct SubmitProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl RuntimeRequestProbeV1 for SubmitProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<tracedecay_store::RuntimeInterruptionV1> {
        None
    }

    fn try_begin_commit(&self) -> bool {
        true
    }
}

fn diagnostics_submit(
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    name: &str,
) -> (tracedecay_store::RuntimeSubmitRequestV1, SubmitProbe) {
    let metadata: tracedecay_store::StoreOperationMetadataV1 =
        serde_json::from_value(serde_json::json!({
            "operation_id": format!("operation.{name}"),
            "client_id": "client.reopen",
            "shard_id": binding.shard_id,
            "incarnation": binding.incarnation.get(),
            "authority_epoch": binding.authority_epoch.get(),
            "idempotency": {
                "key": format!("key.{name}"),
                "command_digest": format!("sha256:{}", "e".repeat(64)),
            },
            "durability": "full",
            "priority": "foreground",
            "admission_bytes": 128,
            "admitted_at": 1
        }))
        .unwrap();
    let scope = tracedecay_store::RuntimeTransactionScopeV1 {
        transaction_id: tracedecay_store::RuntimeTransactionIdV1::new(format!(
            "transaction.{name}"
        ))
        .unwrap(),
        compatibility: tracedecay_store::RuntimeBatchCompatibilityV1::from_operation(&metadata)
            .unwrap(),
        opened_at: metadata.admitted_at,
    };
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new(format!("cancel.{name}")).unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{name}")).unwrap(),
    };
    let control = RuntimeRequestControlV1 {
        requested_at: UtcMicros(1),
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    let request = tracedecay_store::RuntimeSubmitRequestV1::new(
        tracedecay_store::RepositoryOperationEnvelopeV1 {
            metadata,
            payload: tracedecay_store::RepositoryWritePayloadV1::Diagnostics(Box::new(
                tracedecay_store::SanitizedCleanDiagnosticSnapshotV1::new(
                    tracedecay_domain::CodeGenerationId::new(format!("generation.{name}")).unwrap(),
                    Vec::new(),
                )
                .unwrap(),
            )),
        },
        scope,
        control,
    )
    .unwrap();
    (
        request,
        SubmitProbe {
            cancellation,
            deadline,
        },
    )
}

async fn commit_diagnostics_with_writer(
    path: &Path,
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    name: &str,
) {
    let writer = tracedecay_rusqlite_runtime::PersistentWriter::start(
        &crate::native_runtime::handle().unwrap(),
        tracedecay_rusqlite_runtime::ExistingWriterLocator::new(
            binding.clone(),
            VerifiedStoreLocatorV1::new(
                binding.shard_id.clone(),
                binding.incarnation,
                LocatorDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
            ),
            path.to_path_buf(),
        )
        .unwrap(),
        tracedecay_store::AdmissionConfigV1::default(),
        tracedecay_rusqlite_runtime::repository::ConcreteRepositoryWriteExecutor::default(),
    )
    .unwrap();
    let (request, probe) = diagnostics_submit(binding, name);
    let outcome = writer
        .submit(
            &crate::native_runtime::cx().unwrap(),
            request,
            Arc::new(probe),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            tracedecay_store::RuntimeSubmitOutcomeV1::Committed { .. }
        ),
        "the writer must durably establish its authority: {outcome:?}"
    );
    crate::native_runtime::spawn_blocking(move || writer.shutdown_and_join())
        .await
        .unwrap()
        .unwrap();
}

/// A store this incarnation already wrote under a newer authority (a previous
/// daemon of the same incarnation, or a sibling runtime on the file) fences
/// every writer below that authority. The reopen converges above it instead of
/// publishing a runtime whose every submit would be fenced.
#[asupersync::test]
async fn reopening_a_store_behind_its_persisted_writer_authority_converges_above_it() {
    let root = TempDir::new().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(seed_db(&root, "profile.db"));
    let project_path = seed_final_graph_db(&root, "project.db").await;
    Connection::open(&project_path)
        .unwrap()
        .execute_batch(tracedecay_store::GENERATION_DIAGNOSTICS_SCHEMA_DDL)
        .unwrap();
    resolver.push(project_path.clone());
    resolver.push(project_path.clone());
    let shard = StoreShardIdV1::project(
        id::<BrainId>("brain.registry"),
        id::<UserProfileId>("profile.registry"),
        id::<ProjectId>("project.reopen"),
    );
    let ahead = tracedecay_store::StoreRuntimeBindingV1::new(
        shard.clone(),
        incarnation(),
        tracedecay_store::StoreAuthorityEpochV1::new(1 << 40).unwrap(),
    );
    let runtime = asupersync::runtime::Runtime::current_handle().unwrap();
    commit_diagnostics_with_writer(&project_path, &ahead, "previous-authority").await;

    let registry =
        StoreRuntimeRegistry::new(runtime, resolver, Arc::new(LifecycleShardRuntimePublisher))
            .unwrap();
    let _profile = open_published(
        &registry,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    let pin = match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    };
    let authority = crate::db::DatabaseAuthority::for_runtime(&project_path, "reopen project")
        .expect("project database authority");
    let project_runtime = open_published(
        &registry,
        StoreRuntimeOpenRequest::new_authorized(shard, incarnation(), Some(pin), authority.clone()),
    )
    .await;

    assert!(
        project_runtime.binding().authority_epoch > ahead.authority_epoch,
        "the reopened runtime publishes above the persisted authority: {:?} vs {:?}",
        project_runtime.binding().authority_epoch,
        ahead.authority_epoch
    );
    let (request, probe) = diagnostics_submit(project_runtime.binding(), "converged-authority");
    let outcome = project_runtime
        .dispatch_submit_authorized(request, Arc::new(probe), authority)
        .await
        .expect("converged runtime accepts writes");
    assert!(
        matches!(
            outcome,
            tracedecay_store::RuntimeSubmitOutcomeV1::Committed { .. }
        ),
        "a write through the converged runtime commits: {outcome:?}"
    );
    assert!(
        tracedecay_rusqlite_runtime::exact_sql::native_thread_storage_wait_sites()
            .iter()
            .all(|site| !site.call_site.contains("/ledger/checkpoint.rs:")),
        "persisted writer authority reads must not park native runtime workers"
    );
}

/// A graph-first owner cannot be rebound while any of its graph leases remain
/// live. Reopening refuses once with the exact writer fence, then succeeds
/// above the ledger after the ordinary graph close releases that authority.
#[asupersync::test]
async fn a_stale_graph_writer_authority_refuses_once_then_reopens_after_graph_close() {
    let root = TempDir::new().unwrap();
    let resolver = Arc::new(FileResolver::default());
    resolver.push(seed_db(&root, "profile.db"));
    let graph_path = root.path().join("project.grafeo");
    resolver.push(graph_path.clone());
    let project_path = seed_final_graph_db(&root, "project.db").await;
    Connection::open(&project_path)
        .unwrap()
        .execute_batch(tracedecay_store::GENERATION_DIAGNOSTICS_SCHEMA_DDL)
        .unwrap();
    resolver.push(project_path.clone());
    resolver.push(project_path.clone());
    resolver.push(graph_path);
    let registry = StoreRuntimeRegistry::new(
        asupersync::runtime::Runtime::current_handle().unwrap(),
        resolver.clone(),
        Arc::new(LifecycleShardRuntimePublisher),
    )
    .unwrap();
    let _profile = open_published(
        &registry,
        StoreRuntimeOpenRequest::new(profile_shard(), incarnation(), None),
    )
    .await;
    let pin = match registry.profile_authority_pin(&profile_shard()) {
        ProfileAuthorityPinResult::Pinned(pin) => pin,
        other => panic!("profile was not pinned: {other:?}"),
    };
    let shard = project_shard("project.graph-first-reopen");
    let key = StoreRuntimeKey::new(shard.clone(), incarnation());
    let (store_attachment, store_target) = registry
        .attach_graph_store_owner(key.clone())
        .await
        .unwrap();
    let stale = store_attachment.binding().clone();
    let graph_locator = store_attachment.verified_locator().clone();
    let operation = store_attachment.issue_operation_lease().unwrap();
    let graphs = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let graph_owner = graphs
        .resolve_owner_attachment(GraphDbOwnerRegistrationV1 {
            operation: GraphDbRegistration {
                authority_lease: operation,
                cancellation: Arc::new(NeverCancelled),
                lifecycle_cancellation: Arc::new(NeverCancelled),
                deadline: Instant::now() + Duration::from_secs(30),
            },
            authority_attachment: Box::new(store_attachment),
        })
        .unwrap();
    let client = graph_owner.issue_lease().unwrap();

    let mut ahead = stale.clone();
    ahead.authority_epoch = tracedecay_store::StoreAuthorityEpochV1::new(
        stale.authority_epoch.get().checked_add(1).unwrap(),
    )
    .unwrap();
    commit_diagnostics_with_writer(&project_path, &ahead, "graph-newer-writer").await;
    let authority =
        crate::db::DatabaseAuthority::for_runtime(&project_path, "reopen project").unwrap();
    let request =
        StoreRuntimeOpenRequest::new_authorized(shard, incarnation(), Some(pin), authority.clone());
    let calls = resolver.calls.load(Ordering::SeqCst);
    assert!(matches!(
        registry.open(request.clone()).await,
        StoreRuntimeOpenResult::Failed(
            StoreRuntimeRegistryFailure::WriterAuthorityBehindPersisted { binding, persisted }
        ) if *binding == stale && persisted == ahead.authority_epoch
    ));
    assert_eq!(
        resolver.calls.load(Ordering::SeqCst),
        calls + 1,
        "a live stale graph binding cannot converge through another identical open"
    );
    assert_eq!(graph_owner.binding(), &stale);
    assert!(graph_owner.shares_runtime_with(&client));
    drop(
        client
            .snapshot()
            .expect("the old graph client stays readable"),
    );
    assert!(
        graphs.close_retained(&stale, &graph_locator).is_err(),
        "the refused writer must preserve the graph owner's live close blockers"
    );
    drop(
        graph_owner
            .issue_lease()
            .expect("the old exact graph owner stays usable"),
    );

    drop(client);
    drop(graph_owner);
    assert!(graphs.close_retained(&stale, &graph_locator).unwrap());
    drop(store_target);
    assert_eq!(registry.retained_graph_publications_for_test(), 0);

    let project = open_published(&registry, request).await;
    assert!(project.binding().authority_epoch > ahead.authority_epoch);
    let graph = registry.retain_graph_store(key).await.unwrap();
    assert_eq!(graph.binding(), project.binding());
    let (request, probe) = diagnostics_submit(project.binding(), "graph-reopened-writer");
    let outcome = project
        .dispatch_submit_authorized(request, Arc::new(probe), authority)
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            tracedecay_store::RuntimeSubmitOutcomeV1::Committed { .. }
        ),
        "the reopened shared authority accepts writes: {outcome:?}"
    );
}

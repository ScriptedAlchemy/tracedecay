use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig,
    GraphDbRegistryStatus, GraphEntity, GraphEntityId, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphRelation, GraphRelationId, GraphRelationKind, GraphTraversalDirection,
    GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration, TraversalRequest,
};
use tracedecay_store::{
    BrainId, CodeShardScopeV1, GRAPH_STORE_PRIVATE_DIRECTORY, LocatorDigest, ProjectId,
    RepositoryId, RetainedGraphStoreLeaseV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1, WorktreeId,
    canonical_store_locator_digest,
};

#[derive(Debug)]
struct TestGraphLease {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: std::path::PathBuf,
    drop_counter: Option<Arc<AtomicUsize>>,
}

impl RetainedGraphStoreLeaseV1 for TestGraphLease {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    fn canonical_path(&self) -> &std::path::Path {
        &self.canonical_path
    }
}

impl Drop for TestGraphLease {
    fn drop(&mut self) {
        if let Some(counter) = &self.drop_counter {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Debug)]
struct Cancelled;

impl GraphCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[derive(Debug)]
struct CancelOnPoll {
    polls: AtomicUsize,
    cancel_on: usize,
}

impl GraphCancellation for CancelOnPoll {
    fn is_cancelled(&self) -> bool {
        self.polls.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_on
    }
}

fn identity(profile: &str, project: &str) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            BrainId::try_from("brain-a".to_owned()).unwrap(),
            UserProfileId::try_from(profile.to_owned()).unwrap(),
            ProjectId::try_from(project.to_owned()).unwrap(),
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(1).unwrap(),
    )
}

fn profile_sessions_identity(profile: &str) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::profile_sessions(
            BrainId::try_from("brain-a".to_owned()).unwrap(),
            UserProfileId::try_from(profile.to_owned()).unwrap(),
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(1).unwrap(),
    )
}

fn project_sessions_identity(profile: &str, project: &str) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::project_sessions(
            BrainId::try_from("brain-a".to_owned()).unwrap(),
            UserProfileId::try_from(profile.to_owned()).unwrap(),
            ProjectId::try_from(project.to_owned()).unwrap(),
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(1).unwrap(),
    )
}

fn code_identity(profile: &str, project: &str) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::code(
            BrainId::try_from("brain-a".to_owned()).unwrap(),
            UserProfileId::try_from(profile.to_owned()).unwrap(),
            ProjectId::try_from(project.to_owned()).unwrap(),
            RepositoryId::try_from("repository-a".to_owned()).unwrap(),
            CodeShardScopeV1::Worktree {
                worktree_id: WorktreeId::try_from("worktree-a".to_owned()).unwrap(),
            },
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(1).unwrap(),
    )
}

fn broad_profile_identity(profile: &str) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::profile(
            BrainId::try_from("brain-a".to_owned()).unwrap(),
            UserProfileId::try_from(profile.to_owned()).unwrap(),
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(1).unwrap(),
    )
}

fn registration(
    binding: StoreRuntimeBindingV1,
    store_root: &std::path::Path,
) -> GraphDbRegistration {
    create_private_graph_directory(store_root);
    let canonical_path = graph_path(store_root);
    let verified_locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        canonical_store_locator_digest(&canonical_path).unwrap(),
    );
    GraphDbRegistration {
        authority_lease: Arc::new(TestGraphLease {
            binding,
            verified_locator,
            canonical_path,
            drop_counter: None,
        }),
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(NeverCancelled),
        deadline: std::time::Instant::now() + Duration::from_secs(30),
    }
}

fn graph_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join(GRAPH_STORE_PRIVATE_DIRECTORY)
        .join("graph.grafeo")
}

fn create_private_graph_directory(root: &std::path::Path) {
    match tracedecay_private_fs::create_private_directory(&root.join(GRAPH_STORE_PRIVATE_DIRECTORY))
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("create private graph directory: {error}"),
    }
}

fn entity(value: &str) -> GraphEntity {
    GraphEntity {
        identity: GraphEntityId::new(value).unwrap(),
        labels: BTreeSet::new(),
        properties: BTreeMap::new(),
    }
}

fn batch(
    projection: &str,
    generation: &str,
    watermark: &str,
    mutations: Vec<GraphMutation>,
) -> GraphWriteBatch {
    GraphWriteBatch::new(
        GraphNamespace::new("project").unwrap(),
        GraphProjectionId::new(projection).unwrap(),
        SourceGeneration::new(generation).unwrap(),
        GraphWatermark::new(watermark).unwrap(),
        mutations,
        Arc::new(NeverCancelled),
    )
    .unwrap()
}

#[test]
fn exact_project_profile_identity_reuses_one_persistent_handle() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 4 }).unwrap();
    let request = registration(identity("profile-a", "project-a"), temp.path());

    let first = registry.resolve(request.clone()).unwrap();
    let second = registry.resolve(request).unwrap();

    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(
        registry
            .status(&registration(
                identity("profile-a", "project-a"),
                temp.path(),
            ))
            .unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    assert!(graph_path(temp.path()).is_file());
}

#[test]
fn profile_sessions_scope_uses_exact_profile_authority() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
    let first = registry
        .resolve(registration(
            profile_sessions_identity("profile-a"),
            first_root.path(),
        ))
        .unwrap();
    let second = registry
        .resolve(registration(
            profile_sessions_identity("profile-b"),
            second_root.path(),
        ))
        .unwrap();

    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(
        registry
            .status(&registration(
                profile_sessions_identity("profile-a"),
                first_root.path(),
            ))
            .unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    assert_eq!(
        registry
            .status(&registration(
                profile_sessions_identity("profile-b"),
                second_root.path(),
            ))
            .unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
}

#[test]
fn broad_profile_scope_is_rejected() {
    let root = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();

    assert!(matches!(
        registry.resolve(registration(
            broad_profile_identity("profile-a"),
            root.path(),
        )),
        Err(GraphDbError::InvalidRequest { .. })
    ));
    assert!(!graph_path(root.path()).exists());
}

#[test]
fn project_session_and_code_scopes_keep_distinct_locator_authority() {
    let project_root = TempDir::new().unwrap();
    let sessions_root = TempDir::new().unwrap();
    let code_root = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 3 }).unwrap();
    let project_binding = identity("profile-a", "project-a");
    let sessions_binding = project_sessions_identity("profile-a", "project-a");
    let code_binding = code_identity("profile-a", "project-a");

    let project = registry
        .resolve(registration(project_binding.clone(), project_root.path()))
        .unwrap();
    let sessions = registry
        .resolve(registration(sessions_binding.clone(), sessions_root.path()))
        .unwrap();
    let code = registry
        .resolve(registration(code_binding.clone(), code_root.path()))
        .unwrap();

    assert!(!Arc::ptr_eq(&project, &sessions));
    assert!(!Arc::ptr_eq(&project, &code));
    assert!(!Arc::ptr_eq(&sessions, &code));
    assert_eq!(
        registry
            .status(&registration(project_binding, project_root.path()))
            .unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    assert_eq!(
        registry
            .status(&registration(sessions_binding, sessions_root.path()))
            .unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    assert_eq!(
        registry
            .status(&registration(code_binding, code_root.path()))
            .unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
}

#[test]
fn concurrent_resolution_singleflights_one_persistent_handle() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 4 }).unwrap();
    let request = registration(identity("profile-a", "project-a"), temp.path());
    let barrier = Arc::new(Barrier::new(3));
    let workers = (0..2)
        .map(|_| {
            let registry = registry.clone();
            let request = request.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                registry.resolve(request).unwrap()
            })
        })
        .collect::<Vec<_>>();

    barrier.wait();
    let handles = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();

    assert!(Arc::ptr_eq(&handles[0], &handles[1]));
}

#[test]
fn identity_and_canonical_path_cannot_be_rebound() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 4 }).unwrap();
    let first_identity = identity("profile-a", "project-a");
    registry
        .resolve(registration(first_identity.clone(), first_root.path()))
        .unwrap();

    assert_eq!(
        registry
            .resolve(registration(first_identity, second_root.path()))
            .unwrap_err(),
        GraphDbError::Conflict
    );
    assert_eq!(
        registry
            .resolve(registration(
                identity("profile-a", "project-b"),
                first_root.path(),
            ))
            .unwrap_err(),
        GraphDbError::Conflict
    );
    let mut changed_locator = registration(identity("profile-a", "project-a"), first_root.path());
    let mut verified_locator = changed_locator.authority_lease.verified_locator().clone();
    verified_locator.locator_digest =
        LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    changed_locator.authority_lease = Arc::new(TestGraphLease {
        binding: changed_locator.authority_lease.binding().clone(),
        verified_locator,
        canonical_path: changed_locator
            .authority_lease
            .canonical_path()
            .to_path_buf(),
        drop_counter: None,
    });
    assert_eq!(
        registry.resolve(changed_locator).unwrap_err(),
        GraphDbError::InvalidRequest {
            message: "verified graph locator digest does not bind the canonical graph path"
                .to_owned()
        }
    );
}

#[test]
fn stale_binding_cannot_close_or_rebind_the_registered_store() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
    let registered = identity("profile-a", "project-a");
    let mut stale = registered.clone();
    stale.authority_epoch = StoreAuthorityEpochV1::new(2).unwrap();
    let handle = registry
        .resolve(registration(registered.clone(), temp.path()))
        .unwrap();
    drop(handle);

    assert_eq!(
        registry
            .status(&registration(stale.clone(), temp.path()))
            .unwrap_err(),
        GraphDbError::Conflict
    );
    assert_eq!(
        registry
            .reopen(registration(stale, temp.path()))
            .unwrap_err(),
        GraphDbError::Conflict
    );
    assert_eq!(
        registry
            .status(&registration(registered.clone(), temp.path()))
            .unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    assert!(
        registry
            .resolve(registration(registered, temp.path()))
            .is_ok()
    );
}

#[cfg(unix)]
#[test]
fn symlinked_graph_directory_is_rejected_before_open() {
    use std::os::unix::fs::symlink;

    let store = TempDir::new().unwrap();
    let target = TempDir::new().unwrap();
    let alias = store.path().join("graph-alias");
    symlink(target.path(), &alias).unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();

    assert!(matches!(
        registry.resolve(registration(identity("profile-a", "project-a"), &alias,)),
        Err(GraphDbError::InvalidRequest { .. })
    ));
    assert!(!graph_path(target.path()).exists());
}

#[cfg(unix)]
#[test]
fn symlinked_graph_file_is_rejected_before_open() {
    use std::os::unix::fs::symlink;

    let store = TempDir::new().unwrap();
    let target = TempDir::new().unwrap();
    let target_file = target.path().join("target.grafeo");
    std::fs::write(&target_file, []).unwrap();
    create_private_graph_directory(store.path());
    symlink(&target_file, graph_path(store.path())).unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();

    assert!(matches!(
        registry.resolve(registration(
            identity("profile-a", "project-a"),
            store.path(),
        )),
        Err(GraphDbError::InvalidRequest { .. })
    ));
}

#[test]
fn close_and_reopen_preserve_cross_domain_traversal() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let request = registration(store_identity.clone(), temp.path());
    let database = registry.resolve(request.clone()).unwrap();
    database
        .apply(batch(
            "code",
            "code-1",
            "code-watermark-1",
            vec![GraphMutation::UpsertEntity(entity("symbol:caller"))],
        ))
        .unwrap();
    database
        .apply(batch(
            "work",
            "work-1",
            "work-watermark-1",
            vec![
                GraphMutation::UpsertEntity(entity("task:fix")),
                GraphMutation::UpsertRelation(GraphRelation {
                    identity: GraphRelationId::new("evidence:task-to-symbol").unwrap(),
                    from: GraphEntityId::new("task:fix").unwrap(),
                    to: GraphEntityId::new("symbol:caller").unwrap(),
                    kind: GraphRelationKind::new("evidence_for").unwrap(),
                    properties: BTreeMap::new(),
                }),
            ],
        ))
        .unwrap();
    drop(database);

    assert!(registry.close(&request).unwrap());
    let reopened = registry.reopen(request).unwrap();
    let result = reopened
        .traverse(TraversalRequest {
            namespace: GraphNamespace::new("project").unwrap(),
            start: GraphEntityId::new("task:fix").unwrap(),
            relation_kinds: BTreeSet::from([GraphRelationKind::new("evidence_for").unwrap()]),
            direction: GraphTraversalDirection::Outgoing,
            max_depth: 1,
            max_visits: 2,
            max_results: 2,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();

    assert_eq!(
        result
            .visits
            .into_iter()
            .map(|visit| visit.entity)
            .collect::<Vec<_>>(),
        vec![
            GraphEntityId::new("task:fix").unwrap(),
            GraphEntityId::new("symbol:caller").unwrap(),
        ]
    );
}

#[test]
fn close_and_retention_refuse_an_active_handle() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let first_identity = identity("profile-a", "project-a");
    let first_request = registration(first_identity.clone(), first_root.path());
    let active = registry.resolve(first_request.clone()).unwrap();

    assert_eq!(
        registry.close(&first_request).unwrap_err(),
        GraphDbError::Conflict
    );
    assert_eq!(
        registry
            .resolve(registration(
                identity("profile-a", "project-b"),
                second_root.path(),
            ))
            .unwrap_err(),
        GraphDbError::BudgetExhausted
    );
    assert!(active.snapshot().is_ok());
}

#[test]
fn snapshot_lease_prevents_close_after_operation_handle_is_dropped() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let request = registration(store_identity.clone(), temp.path());
    let database = registry.resolve(request.clone()).unwrap();
    let snapshot = database.snapshot().unwrap();
    drop(database);

    assert_eq!(
        registry.close(&request).unwrap_err(),
        GraphDbError::Conflict
    );
    assert!(
        snapshot
            .entity(
                &GraphNamespace::new("project").unwrap(),
                &GraphEntityId::new("missing").unwrap(),
                Arc::new(NeverCancelled),
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn idle_retention_closes_and_evicts_unleased_handles() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let first_identity = identity("profile-a", "project-a");
    let first = registry
        .resolve(registration(first_identity.clone(), first_root.path()))
        .unwrap();
    drop(first);

    let evicted = registry
        .evict_idle(
            Duration::ZERO,
            Arc::new(NeverCancelled),
            std::time::Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
    assert_eq!(evicted, vec![first_identity.clone()]);
    assert_eq!(
        registry
            .status(&registration(first_identity, first_root.path()))
            .unwrap(),
        None
    );

    registry
        .resolve(registration(
            identity("profile-a", "project-b"),
            second_root.path(),
        ))
        .unwrap();
}

#[test]
fn cancelled_open_does_not_create_or_register_a_store() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let mut request = registration(store_identity.clone(), temp.path());
    request.cancellation = Arc::new(Cancelled);

    assert_eq!(
        registry.resolve(request).unwrap_err(),
        GraphDbError::Cancelled
    );
    assert_eq!(
        registry
            .status(&registration(store_identity.clone(), temp.path()))
            .unwrap(),
        None
    );
    assert!(!graph_path(temp.path()).exists());
}

#[test]
fn lifecycle_cancellation_after_file_initialization_rolls_back_the_file() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let mut request = registration(store_identity.clone(), temp.path());
    request.lifecycle_cancellation = Arc::new(CancelOnPoll {
        polls: AtomicUsize::new(0),
        cancel_on: 2,
    });

    assert_eq!(
        registry.resolve(request).unwrap_err(),
        GraphDbError::Cancelled
    );
    assert_eq!(
        registry
            .status(&registration(store_identity, temp.path()))
            .unwrap(),
        None
    );
    assert!(!graph_path(temp.path()).exists());
}

#[cfg(unix)]
#[test]
fn non_private_graph_directory_is_rejected_without_creating_a_file() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().unwrap();
    create_private_graph_directory(temp.path());
    let directory = temp.path().join(GRAPH_STORE_PRIVATE_DIRECTORY);
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();

    assert!(matches!(
        registry.resolve(registration(
            identity("profile-a", "project-a"),
            temp.path(),
        )),
        Err(GraphDbError::InvalidRequest { .. })
    ));
    assert!(!graph_path(temp.path()).exists());
}

#[cfg(unix)]
#[test]
fn non_private_existing_graph_file_is_rejected() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().unwrap();
    create_private_graph_directory(temp.path());
    let path = graph_path(temp.path());
    std::fs::write(&path, b"not a private graph").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();

    assert!(matches!(
        registry.resolve(registration(
            identity("profile-a", "project-a"),
            temp.path(),
        )),
        Err(GraphDbError::InvalidRequest { .. })
    ));
}

#[test]
fn expired_deadline_does_not_open_or_close_a_registered_store() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let mut expired = registration(store_identity.clone(), temp.path());
    expired.deadline = std::time::Instant::now();
    assert_eq!(
        registry.resolve(expired).unwrap_err(),
        GraphDbError::DeadlineExceeded
    );
    assert!(!graph_path(temp.path()).exists());

    let request = registration(store_identity.clone(), temp.path());
    let handle = registry.resolve(request.clone()).unwrap();
    drop(handle);
    let mut expired_close = request;
    expired_close.deadline = std::time::Instant::now();
    assert_eq!(
        registry.close(&expired_close).unwrap_err(),
        GraphDbError::DeadlineExceeded
    );
    assert_eq!(
        registry
            .status(&registration(store_identity.clone(), temp.path()))
            .unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
}

#[test]
fn final_open_cancellation_removes_the_unpublished_store() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let mut request = registration(store_identity.clone(), temp.path());
    request.cancellation = Arc::new(CancelOnPoll {
        polls: AtomicUsize::new(0),
        cancel_on: 3,
    });

    assert_eq!(
        registry.resolve(request).unwrap_err(),
        GraphDbError::Cancelled
    );
    assert_eq!(
        registry
            .status(&registration(store_identity.clone(), temp.path()))
            .unwrap(),
        None
    );
    assert!(!graph_path(temp.path()).exists());
    assert!(
        registry
            .resolve(registration(store_identity, temp.path()))
            .is_ok()
    );
}

#[test]
fn registry_retains_authority_lease_until_the_graph_is_closed() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let mut request = registration(store_identity.clone(), temp.path());
    let dropped = Arc::new(AtomicUsize::new(0));
    request.authority_lease = Arc::new(TestGraphLease {
        binding: request.authority_lease.binding().clone(),
        verified_locator: request.authority_lease.verified_locator().clone(),
        canonical_path: request.authority_lease.canonical_path().to_path_buf(),
        drop_counter: Some(Arc::clone(&dropped)),
    });

    let database = registry.resolve(request).unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(database);
    assert!(
        registry
            .close(&registration(store_identity, temp.path()))
            .unwrap()
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn reset_required_is_retained_until_an_explicit_reopen() {
    use grafeo_engine::Config;
    use grafeo_engine::config::StorageFormat;

    let temp = TempDir::new().unwrap();
    create_private_graph_directory(temp.path());
    let graph_path = graph_path(temp.path());
    let raw = grafeo_engine::GrafeoDB::with_config(
        Config::persistent(&graph_path).with_storage_format(StorageFormat::SingleFile),
    )
    .unwrap();
    raw.create_node(&["Foreign"]);
    raw.close().unwrap();
    make_graph_file_private(&graph_path);

    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let request = registration(store_identity.clone(), temp.path());
    assert!(matches!(
        registry.resolve(request.clone()),
        Err(GraphDbError::ResetRequired { .. })
    ));
    assert_eq!(
        registry
            .status(&registration(store_identity.clone(), temp.path()))
            .unwrap(),
        Some(GraphDbRegistryStatus::ResetRequired)
    );

    std::fs::remove_file(&graph_path).unwrap();
    let reopened = registry.reopen(request).unwrap();
    assert!(reopened.snapshot().is_ok());
}

#[cfg(unix)]
fn make_graph_file_private(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
fn make_graph_file_private(_path: &std::path::Path) {}

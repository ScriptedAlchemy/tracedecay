use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphBudgetKind, GraphCancellation, GraphDbError, GraphDbRegistration, GraphDbRegistry,
    GraphDbRegistryConfig, GraphDbRegistryStatus, GraphDbRetirementOutcome,
    GraphDbRetirementTarget, GraphEntity, GraphEntityId, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphRelation, GraphRelationId, GraphRelationKind, GraphTraversalDirection,
    GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration, TraversalRequest,
};
use tracedecay_runtime_core::storage;
use tracedecay_runtime_core::store_runtime::registry::StoreRuntimeKey;
use tracedecay_runtime_core::store_runtime::resolver::{
    LocalProfileStoreAuthorityV1, LocalProjectEnrollmentAuthorityV1, LocalStoreLocatorResolutionV1,
    LocalStoreRuntimeResolverV1,
};
use tracedecay_store::{
    BrainId, CodeShardScopeV1, LocatorDigest, ProjectId, RepositoryId, RetainedGraphStoreLeaseV1,
    StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
    UserProfileId, VerifiedStoreLocatorV1, WorktreeId, canonical_store_locator_digest,
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

fn profile_memory_identity(profile: &str) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::profile_memory(
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
    root.join("graph.grafeo")
}

fn retirement_target(
    registry: &GraphDbRegistry,
    registration: &GraphDbRegistration,
) -> GraphDbRetirementTarget {
    registry
        .resolve_owner_attachment(registration.clone())
        .unwrap()
        .retirement_target()
}

fn sidecar_wal_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".wal");
    std::path::PathBuf::from(sidecar)
}

#[test]
fn canonical_runtime_resolver_locator_opens_through_graph_registry() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir(&profile_root).unwrap();
    std::fs::create_dir(&project_root).unwrap();

    let binding = identity("profile-a", "project-a");
    storage::write_enrollment_marker(
        &project_root,
        &storage::EnrollmentMarker {
            project_id: "project-a".to_owned(),
            storage_mode: storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let store_root = storage::profile_sharded_data_root(&profile_root, "project-a");
    std::fs::create_dir_all(&store_root).unwrap();

    let resolver = LocalStoreRuntimeResolverV1::new(LocalProfileStoreAuthorityV1::new(
        binding.shard_id.brain_id.clone(),
        binding.shard_id.profile_id.clone(),
        profile_root,
    ));
    resolver
        .register_project_authority(LocalProjectEnrollmentAuthorityV1::new(
            ProjectId::try_from("project-a".to_owned()).unwrap(),
            [project_root],
        ))
        .unwrap();
    let key = StoreRuntimeKey::new(binding.shard_id.clone(), binding.incarnation);
    let resolved = match resolver.resolve_graph_key(&key) {
        LocalStoreLocatorResolutionV1::Resolved(locator) => locator,
        LocalStoreLocatorResolutionV1::Unavailable(unavailable) => {
            panic!("expected canonical graph locator: {unavailable:?}")
        }
    };
    assert_eq!(
        resolved.locator().path().parent(),
        Some(store_root.as_path())
    );
    assert_eq!(
        resolved.locator().path().extension(),
        Some(std::ffi::OsStr::new("grafeo"))
    );

    let registration = GraphDbRegistration {
        authority_lease: Arc::new(TestGraphLease {
            binding,
            verified_locator: resolved.locator().verified().clone(),
            canonical_path: resolved.locator().path().to_path_buf(),
            drop_counter: None,
        }),
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(NeverCancelled),
        deadline: std::time::Instant::now() + Duration::from_secs(30),
    };
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();

    let database = registry.resolve(registration).unwrap();
    assert!(database.snapshot().is_ok());
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

    first
        .apply_unverified(batch(
            "code",
            "generation-one",
            "watermark-one",
            vec![GraphMutation::UpsertEntity(entity("shared"))],
        ))
        .unwrap();
    assert_eq!(
        second
            .entity(
                &GraphNamespace::new("project").unwrap(),
                &GraphEntityId::new("shared").unwrap(),
                Arc::new(NeverCancelled),
            )
            .unwrap(),
        Some(entity("shared"))
    );
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

    first
        .apply_unverified(batch(
            "sessions",
            "generation-one",
            "watermark-one",
            vec![GraphMutation::UpsertEntity(entity("profile-a"))],
        ))
        .unwrap();
    assert_eq!(
        second
            .entity(
                &GraphNamespace::new("project").unwrap(),
                &GraphEntityId::new("profile-a").unwrap(),
                Arc::new(NeverCancelled),
            )
            .unwrap(),
        None
    );
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
fn profile_memory_scope_uses_exact_profile_authority() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
    let first = registry
        .resolve(registration(
            profile_memory_identity("profile-a"),
            first_root.path(),
        ))
        .unwrap();
    let second = registry
        .resolve(registration(
            profile_memory_identity("profile-b"),
            second_root.path(),
        ))
        .unwrap();

    first
        .apply_unverified(batch(
            "memory",
            "generation-one",
            "watermark-one",
            vec![GraphMutation::UpsertEntity(entity("profile-a"))],
        ))
        .unwrap();
    assert_eq!(
        second
            .entity(
                &GraphNamespace::new("project").unwrap(),
                &GraphEntityId::new("profile-a").unwrap(),
                Arc::new(NeverCancelled),
            )
            .unwrap(),
        None
    );
    assert_eq!(
        registry
            .status(&registration(
                profile_memory_identity("profile-a"),
                first_root.path(),
            ))
            .unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    assert_eq!(
        registry
            .status(&registration(
                profile_memory_identity("profile-b"),
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

    project
        .apply_unverified(batch(
            "project",
            "generation-one",
            "watermark-one",
            vec![GraphMutation::UpsertEntity(entity("project-only"))],
        ))
        .unwrap();
    for lease in [&sessions, &code] {
        assert_eq!(
            lease
                .entity(
                    &GraphNamespace::new("project").unwrap(),
                    &GraphEntityId::new("project-only").unwrap(),
                    Arc::new(NeverCancelled),
                )
                .unwrap(),
            None
        );
    }
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

    handles[0]
        .apply_unverified(batch(
            "code",
            "generation-one",
            "watermark-one",
            vec![GraphMutation::UpsertEntity(entity("shared"))],
        ))
        .unwrap();
    assert_eq!(
        handles[1]
            .entity(
                &GraphNamespace::new("project").unwrap(),
                &GraphEntityId::new("shared").unwrap(),
                Arc::new(NeverCancelled),
            )
            .unwrap(),
        Some(entity("shared"))
    );
}

#[test]
fn cloned_lease_is_one_client_but_independent_resolves_are_two_clients() {
    let temporary = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let request = registration(identity("profile-a", "project-a"), temporary.path());

    let first = registry.resolve(request.clone()).unwrap();
    let clone = first.clone();
    drop(first);
    assert_eq!(
        registry.close(&request).unwrap_err(),
        GraphDbError::Conflict
    );
    drop(clone);
    assert!(registry.close(&request).unwrap());

    let first = registry.resolve(request.clone()).unwrap();
    let second = registry.resolve(request.clone()).unwrap();
    drop(first);
    assert_eq!(
        registry.close(&request).unwrap_err(),
        GraphDbError::Conflict
    );
    drop(second);
    assert!(registry.close(&request).unwrap());
}

#[test]
fn retirement_reservation_denies_new_resolves_and_drop_restores_ready_entry() {
    let temporary = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let request = registration(identity("profile-a", "project-a"), temporary.path());
    let target = retirement_target(&registry, &request);

    let reservation = registry.reserve_retirement_batch(vec![target]).unwrap();
    assert_eq!(
        registry.status(&request).unwrap(),
        Some(GraphDbRegistryStatus::Closing)
    );
    assert!(matches!(
        registry.resolve(request.clone()),
        Err(GraphDbError::Conflict)
    ));

    drop(reservation);
    assert_eq!(
        registry.status(&request).unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    assert!(registry.resolve(request).is_ok());
}

#[test]
fn retirement_batch_is_all_or_nothing_when_any_exact_entry_has_a_client() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
    let first_request = registration(identity("profile-a", "project-a"), first_root.path());
    let second_request = registration(identity("profile-a", "project-b"), second_root.path());
    let first = registry.resolve(first_request.clone()).unwrap();
    drop(registry.resolve(second_request.clone()).unwrap());

    assert!(matches!(
        registry.reserve_retirement_batch(vec![
            retirement_target(&registry, &first_request),
            retirement_target(&registry, &second_request),
        ]),
        Err(GraphDbError::Conflict)
    ));
    assert_eq!(
        registry.status(&first_request).unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    assert_eq!(
        registry.status(&second_request).unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    drop(first);
}

#[test]
fn retirement_reservation_rejects_foreign_identity_without_transitioning_entry() {
    let temporary = TempDir::new().unwrap();
    let foreign_temporary = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let request = registration(identity("profile-a", "project-a"), temporary.path());
    let foreign_registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let foreign_request =
        registration(identity("profile-a", "project-a"), foreign_temporary.path());
    drop(registry.resolve(request.clone()).unwrap());
    let foreign_target = retirement_target(&foreign_registry, &foreign_request);

    assert!(matches!(
        registry.reserve_retirement_batch(vec![foreign_target]),
        Err(GraphDbError::Conflict)
    ));
    assert_eq!(
        registry.status(&request).unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
}

#[test]
fn cancelled_retirement_commit_restores_all_preclose_entries() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
    let first_request = registration(identity("profile-a", "project-a"), first_root.path());
    let second_request = registration(identity("profile-a", "project-b"), second_root.path());
    drop(registry.resolve(first_request.clone()).unwrap());
    drop(registry.resolve(second_request.clone()).unwrap());
    let mut reservation = registry
        .reserve_retirement_batch(vec![
            retirement_target(&registry, &first_request),
            retirement_target(&registry, &second_request),
        ])
        .unwrap();

    assert_eq!(
        reservation
            .commit(
                Arc::new(Cancelled),
                std::time::Instant::now() + Duration::from_secs(30)
            )
            .unwrap_err(),
        GraphDbError::Cancelled
    );
    assert_eq!(
        registry.status(&first_request).unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    assert_eq!(
        registry.status(&second_request).unwrap(),
        Some(GraphDbRegistryStatus::Ready)
    );
    assert_eq!(
        reservation
            .commit(
                Arc::new(NeverCancelled),
                std::time::Instant::now() + Duration::from_secs(30),
            )
            .unwrap_err(),
        GraphDbError::Conflict
    );
}

#[test]
fn retirement_commit_reports_every_closed_exact_entry() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
    let first_request = registration(identity("profile-a", "project-a"), first_root.path());
    let second_request = registration(identity("profile-a", "project-b"), second_root.path());
    drop(registry.resolve(first_request.clone()).unwrap());
    drop(registry.resolve(second_request.clone()).unwrap());
    let first_target = retirement_target(&registry, &first_request);
    let second_target = retirement_target(&registry, &second_request);
    let mut reservation = registry
        .reserve_retirement_batch(vec![first_target.clone(), second_target.clone()])
        .unwrap();

    assert_eq!(
        reservation
            .commit(
                Arc::new(NeverCancelled),
                std::time::Instant::now() + Duration::from_secs(30),
            )
            .unwrap()
            .outcomes(),
        &[
            GraphDbRetirementOutcome::Closed(first_target),
            GraphDbRetirementOutcome::Closed(second_target),
        ]
    );
    assert_eq!(registry.status(&first_request).unwrap(), None);
    assert_eq!(registry.status(&second_request).unwrap(), None);
    assert_eq!(
        reservation
            .commit(
                Arc::new(NeverCancelled),
                std::time::Instant::now() + Duration::from_secs(30),
            )
            .unwrap_err(),
        GraphDbError::Conflict
    );
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
            .reopen_for_harness(registration(stale, temp.path()))
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
fn symlinked_graph_database_file_is_rejected_before_open() {
    use std::os::unix::fs::symlink;

    let store = TempDir::new().unwrap();
    let target = TempDir::new().unwrap();
    let target_file = target.path().join("target.grafeo");
    std::fs::write(&target_file, b"not a Grafeo database").unwrap();
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
fn single_file_reopens_with_identical_traversal() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 2 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let request = registration(store_identity.clone(), temp.path());
    let database = registry.resolve(request.clone()).unwrap();
    database
        .apply_unverified(batch(
            "code",
            "code-1",
            "code-watermark-1",
            vec![GraphMutation::UpsertEntity(entity("symbol:caller"))],
        ))
        .unwrap();
    database
        .apply_unverified(batch(
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
    assert!(graph_path(temp.path()).is_file());
    assert!(sidecar_wal_path(&graph_path(temp.path())).is_dir());
    drop(database);

    assert!(registry.close(&request).unwrap());
    assert!(graph_path(temp.path()).is_file());
    assert!(!sidecar_wal_path(&graph_path(temp.path())).exists());
    let reopened = registry.reopen_for_harness(request).unwrap();
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
        GraphDbError::budget_exhausted(GraphBudgetKind::Capacity, 1)
    );
    assert!(active.snapshot().is_ok());
}

#[cfg(unix)]
#[test]
fn retained_close_uses_exact_registry_identity_after_root_loss() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let binding = identity("profile-a", "project-a");
    let request = registration(binding.clone(), temp.path());
    let locator = request.authority_lease.verified_locator().clone();
    let database = registry.resolve(request).unwrap();
    drop(database);

    let foreign_binding = StoreRuntimeBindingV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        StoreAuthorityEpochV1::new(binding.authority_epoch.get() + 1).unwrap(),
    );
    assert_eq!(
        registry
            .close_retained(&foreign_binding, &locator)
            .unwrap_err(),
        GraphDbError::Conflict
    );

    std::fs::remove_dir_all(temp.path()).unwrap();
    assert!(registry.close_retained(&binding, &locator).unwrap());
    assert!(!registry.close_retained(&binding, &locator).unwrap());
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
fn lifecycle_cancellation_before_database_file_creation_is_typed() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let mut request = registration(store_identity.clone(), temp.path());
    let cancellation = Arc::new(CancelOnPoll {
        polls: AtomicUsize::new(0),
        cancel_on: 1,
    });
    request.lifecycle_cancellation = cancellation.clone();

    assert_eq!(
        registry.resolve(request).unwrap_err(),
        GraphDbError::Cancelled
    );
    assert!(
        cancellation.polls.load(Ordering::SeqCst) >= cancellation.cancel_on,
        "lifecycle cancellation must be sampled before database file creation"
    );
    assert_eq!(
        registry
            .status(&registration(store_identity, temp.path()))
            .unwrap(),
        None
    );
    assert!(!graph_path(temp.path()).exists());
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
fn cancellation_after_database_file_creation_finishes_format_before_retry() {
    let temp = TempDir::new().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let store_identity = identity("profile-a", "project-a");
    let mut request = registration(store_identity.clone(), temp.path());
    let cancellation = Arc::new(CancelOnPoll {
        polls: AtomicUsize::new(0),
        cancel_on: 4,
    });
    request.cancellation = cancellation.clone();

    assert_eq!(
        registry.resolve(request).unwrap_err(),
        GraphDbError::Cancelled
    );
    assert!(
        cancellation.polls.load(Ordering::SeqCst) >= cancellation.cancel_on,
        "the request must be observed after the created database file is initialized"
    );
    assert_eq!(
        registry
            .status(&registration(store_identity.clone(), temp.path()))
            .unwrap(),
        None
    );
    assert!(graph_path(temp.path()).is_file());
    assert!(
        registry
            .resolve(registration(store_identity.clone(), temp.path()))
            .is_ok(),
        "retry must open the fully initialized store instead of requiring a reset"
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
fn reset_required_fault_is_retained_and_cannot_reopen() {
    use grafeo_engine::Config;
    use grafeo_engine::config::StorageFormat;

    let temp = TempDir::new().unwrap();
    let graph_path = graph_path(temp.path());
    let raw = grafeo_engine::GrafeoDB::with_config(
        Config::persistent(&graph_path).with_storage_format(StorageFormat::SingleFile),
    )
    .unwrap();
    raw.create_node(&["Foreign"]);
    raw.close().unwrap();

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
    assert!(matches!(
        registry.reopen_for_harness(request.clone()),
        Err(GraphDbError::ResetRequired { .. })
    ));
    assert_eq!(
        registry.status(&request).unwrap(),
        Some(GraphDbRegistryStatus::ResetRequired)
    );
    assert!(matches!(
        registry.resolve(request),
        Err(GraphDbError::ResetRequired { .. })
    ));
}

#[test]
fn preexisting_empty_graph_file_is_corrupt() {
    let temp = TempDir::new().unwrap();
    std::fs::File::create(graph_path(temp.path())).unwrap();

    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let request = registration(identity("profile-a", "project-a"), temp.path());

    assert!(matches!(
        registry.resolve(request),
        Err(GraphDbError::Corrupt { .. })
    ));
}

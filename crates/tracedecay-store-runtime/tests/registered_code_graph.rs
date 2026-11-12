use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};

use tracedecay_code_index_runtime::CodeGraphSeatRuntimePortV1;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexHintPolicyV1, CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
};
use tracedecay_daemon_identity::profile_identity;
use tracedecay_domain::ProjectId;
use tracedecay_runtime_core::resident_memory::{ProcessResidentMemoryV1, ResidentMemoryPressureV1};
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git").args(args).current_dir(root).output().unwrap();
    assert!(output.status.success(), "{args:?}: {:?}", output.stderr);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_publication_replays_graph_and_retires_without_loose_payloads() {
    tracedecay_global_db::register_registered_schema_installer();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let checkout = root.join("checkout");
    let profile = root.join("profile");
    std::fs::create_dir_all(checkout.join("src")).unwrap();
    std::fs::write(checkout.join("src/lib.rs"),
        "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 7 }\n").unwrap();
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(&checkout, &["config", "user.name", "Test"]);
    git(&checkout, &["config", "user.email", "test@example.invalid"]);
    git(&checkout, &["add", "."]);
    git(&checkout, &["commit", "-qm", "fixture"]);
    let project = ProjectId::new("project-registered-graph").unwrap();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &checkout, project.as_str(),
    ).unwrap();
    let identity = profile_identity::load_or_create(&profile).unwrap();
    let _scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile, 44, "registered graph fixture",
    ).unwrap();
    let registry = Arc::new(DaemonSessionRuntimeRegistryV1::open(identity).await.unwrap());
    let database = registry.project_memory(project.clone(), [checkout.clone()]).await.unwrap();
    let limit = std::num::NonZeroU64::new(1024 * 1024 * 1024).unwrap();
    let memory = Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit, Arc::new(ResidentMemoryPressureV1::with_sampler(limit, Arc::new(|| None))),
    ));
    let db = Arc::clone(&database);
    let build_project = project.clone();
    let build_root = checkout.clone();
    let store = root.join("index");
    let build_store = store.clone();
    let build_memory = Arc::clone(&memory);
    let mut scheduler = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_policy(
            build_project, &build_root, build_store, Arc::new(SharedCodeIndexBytePoolV1::default()),
            CodeIndexHintPolicyV1::default(),
            tracedecay_code_index_runtime::registry_default_index_path_policy(), db,
        ).unwrap();
        scheduler.bind_resident_memory(build_memory);
        scheduler.reconcile_now().unwrap();
        scheduler
    }).await.unwrap();
    let latest = scheduler.latest_complete().unwrap();
    let generation = latest.generation().manifest().generation_id.clone();
    let snapshot = latest.generation().snapshot();
    let binding = scheduler.code_graph_replay_binding(&generation).unwrap();
    let storage = binding.storage.clone();
    let worktree = binding.worktree_id.clone();
    let digest = tracedecay_domain::ManifestDigest::new(binding.sealed_state_digest.as_str()).unwrap();
    let seat = CodeGraphSeatRuntimePortV1::retain_code_graph_runtime(
        registry.as_ref(), project.clone(), snapshot.repository.clone(), worktree.clone(),
        snapshot.reference.clone(), generation.clone(), Arc::clone(&database), binding,
    ).await.unwrap();
    let seat = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        let first = seat.publish_verified_snapshot(Arc::new(AtomicBool::new(false))).unwrap();
        let second = seat.publish_verified_snapshot(Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(first.verified_head(), second.verified_head());
        assert!(first.read_projection(tracedecay_graph_db::GraphProjectionReadRequest {
            namespace: first.projection().namespace.clone(),
            projection: first.projection().projection.clone(),
            after_entity: None, after_relation: None, max_entities: 100, max_relations: 100,
            cancellation: Arc::new(tracedecay_graph_db::NeverCancelled),
        }).unwrap().entities.len() >= 2);
        drop(first);
        drop(second);
        seat
    }).await.unwrap();
    std::fs::write(checkout.join("src/lib.rs"),
        "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 8 }\n").unwrap();
    let prepare_storage = storage.clone();
    let prepare_worktree = worktree.clone();
    scheduler = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        scheduler.reconcile_now().unwrap();
        tracedecay_code_index_retention::code_index_generations::retain_registered_code_index(
            &prepare_storage, &prepare_worktree, &BTreeSet::new(), &|| false,
        ).unwrap();
        scheduler
    }).await.unwrap();
    let key = tracedecay_runtime_core::db::CodeIndexGenerationKey {
        worktree_id: worktree.clone(), generation_id: generation.clone(),
    };
    let pending_storage = storage.clone();
    let pending_key = key.clone();
    let pending_digest = digest.clone();
    let (operation, _) = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        assert!(pending_storage.read_manifest(&pending_key, &pending_digest).is_ok());
        pending_storage.pending_retirement(&pending_key).unwrap().unwrap()
    }).await.unwrap();
    let successor = scheduler.latest_complete().unwrap();
    let successor_generation = successor.generation().manifest().generation_id.clone();
    let successor_snapshot = successor.generation().snapshot();
    let successor_binding = scheduler.code_graph_replay_binding(&successor_generation).unwrap();
    let successor_seat = CodeGraphSeatRuntimePortV1::retain_code_graph_runtime(
        registry.as_ref(), project.clone(), successor_snapshot.repository.clone(),
        worktree.clone(), successor_snapshot.reference.clone(), successor_generation,
        Arc::clone(&database), successor_binding,
    ).await.unwrap();
    tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        let snapshot = successor_seat
            .publish_verified_snapshot(Arc::new(AtomicBool::new(false))).unwrap();
        assert!(snapshot.read_projection(tracedecay_graph_db::GraphProjectionReadRequest {
            namespace: snapshot.projection().namespace.clone(),
            projection: snapshot.projection().projection.clone(),
            after_entity: None, after_relation: None, max_entities: 100, max_relations: 100,
            cancellation: Arc::new(tracedecay_graph_db::NeverCancelled),
        }).unwrap().entities.len() >= 2);
    }).await.unwrap();
    drop(seat);
    drop(latest);
    assert!(registry.reconcile_deleted_code_generation_graph_replays(
        project, &database, &generation,
        &format!("generation-{}.json", tracedecay_domain::sha256_hex_suffix(digest.as_str()).unwrap()),
        &tracedecay_runtime_core::cancellation::CancellationToken::new(),
    ).await.unwrap());
    tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        storage.complete_retirement(&key, &operation).unwrap();
        assert!(matches!(storage.read_manifest(&key, &digest),
            Err(tracedecay_runtime_core::db::CodeIndexStorageError::Retired)));
    }).await.unwrap();
    assert!(!store.join("active-code-generation-v1.json").exists());
    assert!(!store.join("code-generations-v1").exists());
    assert!(!store.join("code-generation-segments-v1").exists());
    drop(scheduler);
}

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig, GraphEntity, GraphEntityId,
    GraphMutation, GraphNamespace, GraphProjectionId, GraphWatermark, GraphWriteBatch,
    NeverCancelled, SourceGeneration,
};
use tracedecay_store::{
    BrainId, GRAPH_STORE_PRIVATE_DIRECTORY, ProjectId, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
};

const ENTITY_COUNT: usize = 100_000;

fn registration(root: &std::path::Path) -> GraphDbRegistration {
    create_private_graph_directory(root);
    let canonical_path = root
        .join(GRAPH_STORE_PRIVATE_DIRECTORY)
        .join("graph.grafeo");
    let binding = StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            BrainId::try_from("brain.benchmark".to_owned()).expect("valid brain"),
            UserProfileId::try_from("profile.benchmark".to_owned()).expect("valid profile"),
            ProjectId::try_from("project.benchmark".to_owned()).expect("valid project"),
        ),
        StoreIncarnationV1::new(1).expect("valid incarnation"),
        StoreAuthorityEpochV1::new(1).expect("valid epoch"),
    );
    GraphDbRegistration {
        verified_locator: VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            canonical_store_locator_digest(&canonical_path).expect("valid locator digest"),
        ),
        binding,
        canonical_path,
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(NeverCancelled),
        deadline: Instant::now() + Duration::from_secs(3_600),
    }
}

#[cfg(unix)]
fn create_private_graph_directory(root: &std::path::Path) {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(root.join(GRAPH_STORE_PRIVATE_DIRECTORY)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("create private graph directory: {error}"),
    }
}

#[cfg(windows)]
fn create_private_graph_directory(root: &std::path::Path) {
    let path = root.join(GRAPH_STORE_PRIVATE_DIRECTORY);
    match tracedecay_runtime_core::windows_security::create_private_directory(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("create private graph directory: {error}"),
    }
}

#[cfg(not(any(unix, windows)))]
fn create_private_graph_directory(_root: &std::path::Path) {
    panic!("private graph storage is unsupported on this platform");
}

fn registry() -> GraphDbRegistry {
    GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 })
        .expect("benchmark registry config is valid")
}

fn populate(registry: &GraphDbRegistry, request: &GraphDbRegistration) {
    let db = registry
        .resolve(request.clone())
        .expect("benchmark store opens");
    let mutations = (0..ENTITY_COUNT)
        .map(|index| {
            GraphMutation::UpsertEntity(
                GraphEntity::new(
                    GraphEntityId::new(format!("entity-{index:06}"))
                        .expect("benchmark identity is valid"),
                    BTreeSet::new(),
                    BTreeMap::new(),
                )
                .expect("benchmark entity is valid"),
            )
        })
        .collect();
    let batch = GraphWriteBatch::new(
        GraphNamespace::new("benchmark").expect("benchmark namespace is valid"),
        GraphProjectionId::new("code").expect("benchmark projection is valid"),
        SourceGeneration::new("generation-1").expect("benchmark generation is valid"),
        GraphWatermark::new("watermark-1").expect("benchmark watermark is valid"),
        mutations,
        Arc::new(NeverCancelled),
    )
    .expect("benchmark batch is valid");
    db.apply(batch).expect("100k-node batch commits");
    drop(db);
    registry.close(request).expect("benchmark store closes");
}

fn native_state_100k(criterion: &mut Criterion) {
    let temp = TempDir::new().expect("benchmark temporary directory exists");
    let registry = registry();
    let request = registration(temp.path());
    populate(&registry, &request);

    criterion.bench_function("native_state/reopen_100k_without_graph_scan", |bencher| {
        bencher.iter_batched(
            || (registry.clone(), request.clone()),
            |(registry, request)| {
                let database = registry.reopen(request.clone()).expect("store reopens");
                drop(database);
                registry.close(&request).expect("store closes");
            },
            BatchSize::SmallInput,
        );
    });

    let db = registry
        .resolve(request.clone())
        .expect("benchmark store opens for point reads");
    let namespace = GraphNamespace::new("benchmark").expect("benchmark namespace is valid");
    let identity = GraphEntityId::new("entity-099999").expect("benchmark identity is valid");
    criterion.bench_function("native_state/indexed_point_read_100k", |bencher| {
        bencher.iter(|| {
            db.entity(&namespace, &identity, Arc::new(NeverCancelled))
                .expect("indexed point read succeeds")
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = native_state_100k
}
criterion_main!(benches);

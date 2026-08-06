use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig, GraphEntity, GraphEntityId,
    GraphMutation, GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName,
    GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
};
use tracedecay_store::{
    BrainId, ProjectId, RetainedGraphStoreLeaseV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
};

const ENTITY_COUNT: usize = 100_000;

#[derive(Debug)]
struct BenchmarkGraphLease {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: std::path::PathBuf,
}

impl RetainedGraphStoreLeaseV1 for BenchmarkGraphLease {
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

fn registration(root: &std::path::Path) -> GraphDbRegistration {
    let canonical_path = root.join("graph.grafeo");
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
        authority_lease: Arc::new(BenchmarkGraphLease {
            verified_locator: VerifiedStoreLocatorV1::new(
                binding.shard_id.clone(),
                binding.incarnation,
                canonical_store_locator_digest(&canonical_path).expect("valid locator digest"),
            ),
            binding,
            canonical_path,
        }),
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(NeverCancelled),
        deadline: Instant::now() + Duration::from_secs(3_600),
    }
}

fn registry() -> GraphDbRegistry {
    GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 })
        .expect("benchmark registry config is valid")
}

fn populate(registry: &GraphDbRegistry, request: &GraphDbRegistration, entity_count: usize) {
    let db = registry
        .resolve(request.clone())
        .expect("benchmark store opens");
    let mutations = (0..entity_count)
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
    db.apply_unverified(batch)
        .expect("native-node batch commits");
    drop(db);
    registry.close(request).expect("benchmark store closes");
}

fn small_update(entity_count: usize, sequence: usize) -> GraphWriteBatch {
    let identity = format!("entity-{:06}", entity_count - 1);
    GraphWriteBatch::new(
        GraphNamespace::new("benchmark").expect("benchmark namespace is valid"),
        GraphProjectionId::new("code").expect("benchmark projection is valid"),
        SourceGeneration::new(format!("small-update-generation-{sequence}"))
            .expect("benchmark generation is valid"),
        GraphWatermark::new(format!("small-update-watermark-{sequence}"))
            .expect("benchmark watermark is valid"),
        vec![GraphMutation::UpsertEntity(
            GraphEntity::new(
                GraphEntityId::new(identity).expect("benchmark identity is valid"),
                BTreeSet::new(),
                BTreeMap::from([(
                    GraphPropertyName::new("revision").expect("benchmark property is valid"),
                    GraphProperty::I64(sequence as i64),
                )]),
            )
            .expect("benchmark entity is valid"),
        )],
        Arc::new(NeverCancelled),
    )
    .expect("benchmark batch is valid")
}

fn native_state_100k(criterion: &mut Criterion) {
    let temp = TempDir::new().expect("benchmark temporary directory exists");
    let registry = registry();
    let request = registration(temp.path());
    populate(&registry, &request, ENTITY_COUNT);

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
    let sequence = AtomicUsize::new(2);
    criterion.bench_function("native_state/small_update_100k", |bencher| {
        bencher.iter(|| {
            let sequence = sequence.fetch_add(1, Ordering::Relaxed);
            db.apply_unverified(small_update(ENTITY_COUNT, sequence))
                .expect("small indexed update succeeds")
        });
    });
    drop(db);
    registry.close(&request).expect("100k store closes");

    let ten_x_temp = TempDir::new().expect("10x benchmark temporary directory exists");
    let ten_x_registry = self::registry();
    let ten_x_request = registration(ten_x_temp.path());
    let ten_x_entity_count = ENTITY_COUNT * 10;
    populate(&ten_x_registry, &ten_x_request, ten_x_entity_count);
    let ten_x_db = ten_x_registry
        .resolve(ten_x_request.clone())
        .expect("10x benchmark store opens");
    let ten_x_sequence = AtomicUsize::new(2);
    criterion.bench_function("native_state/small_update_1m", |bencher| {
        bencher.iter(|| {
            let sequence = ten_x_sequence.fetch_add(1, Ordering::Relaxed);
            ten_x_db
                .apply_unverified(small_update(ten_x_entity_count, sequence))
                .expect("10x small indexed update succeeds")
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = native_state_100k
}
criterion_main!(benches);

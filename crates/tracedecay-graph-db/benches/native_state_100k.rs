use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphDb, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntity, GraphEntityId,
    GraphFormatVersion, GraphMutation, GraphNamespace, GraphProjectionId, GraphWatermark,
    GraphWriteBatch, NeverCancelled, SourceGeneration,
};

const ENTITY_COUNT: usize = 100_000;

fn options(path: std::path::PathBuf) -> GraphDbOpenOptions {
    GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path),
        expected_format: GraphFormatVersion::new(2).expect("benchmark format is valid"),
        durability: GraphDurability::Sync,
        cancellation: Arc::new(NeverCancelled),
    }
}

fn populate(path: &std::path::Path) {
    let db = GraphDb::open(options(path.to_path_buf())).expect("benchmark store opens");
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
    db.close().expect("benchmark store closes");
}

fn native_state_100k(criterion: &mut Criterion) {
    let temp = TempDir::new().expect("benchmark temporary directory exists");
    let path = temp.path().join("native-state-100k.grafeo");
    populate(&path);

    criterion.bench_function("native_state/reopen_100k_without_graph_scan", |bencher| {
        bencher.iter_batched(
            || path.clone(),
            |path| {
                let db = GraphDb::open(options(path)).expect("100k-node store reopens");
                db.close().expect("100k-node store closes");
            },
            BatchSize::SmallInput,
        );
    });

    let db = GraphDb::open(options(path)).expect("100k-node store opens for point reads");
    let namespace = GraphNamespace::new("benchmark").expect("benchmark namespace is valid");
    let identity = GraphEntityId::new("entity-099999").expect("benchmark identity is valid");
    criterion.bench_function("native_state/indexed_point_read_100k", |bencher| {
        bencher.iter(|| {
            db.entity(&namespace, &identity, Arc::new(NeverCancelled))
                .expect("indexed point read succeeds")
        });
    });
    db.close().expect("benchmark store closes");
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = native_state_100k
}
criterion_main!(benches);

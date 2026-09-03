use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use tracedecay_graph_db::{
    GraphEntityId, GraphNamespace, GraphRelationKind, GraphTraversalDirection, NeverCancelled,
    TraversalRequest, VerifiedGraphSnapshot,
};

#[path = "support/code.rs"]
mod code_fixture;
mod support;

use code_fixture::{CALLS_RELATION, CODE_NAMESPACE, code_manifest};
use support::PersistentBenchmarkGraph;

const BRANCHING: usize = 8;
const MAX_DEPTH: usize = 4;
const ENTITY_COUNT: usize = 1 + 8 + 64 + 512 + 4_096;

fn traversal_request(depth: usize) -> TraversalRequest {
    TraversalRequest {
        namespace: GraphNamespace::new(CODE_NAMESPACE).expect("benchmark namespace is valid"),
        start: GraphEntityId::new("symbol:root").expect("benchmark start identity is valid"),
        relation_kinds: BTreeSet::from([
            GraphRelationKind::new(CALLS_RELATION).expect("benchmark relation kind is valid")
        ]),
        direction: GraphTraversalDirection::Outgoing,
        max_depth: depth,
        max_visits: ENTITY_COUNT,
        max_results: ENTITY_COUNT,
        cancellation: Arc::new(NeverCancelled),
    }
}

fn visited_entities(depth: usize) -> usize {
    (0..=depth).map(|level| BRANCHING.pow(level as u32)).sum()
}

fn measure_warm_traversal(criterion: &mut Criterion, snapshot: &VerifiedGraphSnapshot) {
    let mut group = criterion.benchmark_group("code_traversal/warm_verified_snapshot");
    for depth in [1, 2, 4] {
        let expected_visits = visited_entities(depth);
        assert_eq!(
            snapshot
                .traverse(traversal_request(depth))
                .expect("benchmark traversal preflight succeeds")
                .visits
                .len(),
            expected_visits,
            "benchmark traversal fixture must exercise every bounded hop",
        );
        group.throughput(Throughput::Elements(expected_visits as u64));
        group.bench_with_input(
            BenchmarkId::new("bounded_hops", depth),
            &depth,
            |bencher, depth| {
                bencher.iter(|| {
                    black_box(
                        snapshot
                            .traverse(traversal_request(*depth))
                            .expect("benchmark traversal succeeds"),
                    )
                });
            },
        );
    }
    group.finish();
}

fn code_traversal(criterion: &mut Criterion) {
    let manifest = code_manifest(BRANCHING, MAX_DEPTH, 1);
    let mut persistent = PersistentBenchmarkGraph::new();
    let snapshot = persistent.publish(manifest, None);
    measure_warm_traversal(criterion, &snapshot);
    drop(snapshot);

    let mut group = criterion.benchmark_group("code_traversal/reopen_verified_snapshot");
    group.throughput(Throughput::Elements(ENTITY_COUNT as u64));
    group.bench_function("open_verify_and_traverse_4_hops", |bencher| {
        bencher.iter(|| {
            let recovered = persistent.recover_snapshot();
            let result = recovered
                .traverse(traversal_request(MAX_DEPTH))
                .expect("recovered benchmark traversal succeeds");
            assert_eq!(result.visits.len(), ENTITY_COUNT);
            black_box(result)
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(20));
    targets = code_traversal
}
criterion_main!(benches);

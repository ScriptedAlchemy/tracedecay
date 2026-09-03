use std::sync::Arc;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use tracedecay_graph_db::{
    GraphNamespace, GraphProjectionId, GraphPropertyName, NeverCancelled, VectorMetric,
    VectorSearchRequest,
};

mod support;
#[path = "support/vector.rs"]
mod vector_fixture;

use support::PersistentBenchmarkGraph;
use vector_fixture::{
    VECTOR_NAMESPACE, VECTOR_PROJECTION, VECTOR_PROPERTY, deterministic_vector, vector_manifest,
};

// This workload isolates index-cardinality scaling. Sixteen dimensions keep
// the million-row manifest below the graph boundary's canonical 1 GiB batch
// limit without pretending to measure model inference or embedding generation.
const VECTOR_DIMENSION: usize = 16;
const RESULT_LIMIT: usize = 20;

// The candidate workload holds the index small and the vectors production
// shaped (the semantic lane publishes 768-dim FastEmbed vectors) and sweeps
// the result limit, so what scales is the per-candidate identity check the
// search runs after the HNSW walk, not the walk itself. The top limit is the
// crate's `MAX_VECTOR_SEARCH_LIMIT`.
const CANDIDATE_ENTITY_COUNT: usize = 20_000;
const CANDIDATE_DIMENSION: usize = 768;
const CANDIDATE_LIMITS: [usize; 3] = [64, 512, 4_096];

fn request(query: Vec<f32>) -> VectorSearchRequest {
    request_with(query, VECTOR_DIMENSION, RESULT_LIMIT)
}

fn request_with(query: Vec<f32>, dimension: usize, limit: usize) -> VectorSearchRequest {
    VectorSearchRequest {
        namespace: GraphNamespace::new(VECTOR_NAMESPACE)
            .expect("benchmark vector namespace is valid"),
        projection: GraphProjectionId::new(VECTOR_PROJECTION)
            .expect("benchmark vector projection is valid"),
        property: GraphPropertyName::new(VECTOR_PROPERTY)
            .expect("benchmark vector property is valid"),
        query,
        dimension,
        metric: VectorMetric::Cosine,
        limit,
        cancellation: Arc::new(NeverCancelled),
    }
}

fn vector_search_candidates(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("vector_search/candidate_post_filter");
    let mut persistent = PersistentBenchmarkGraph::new();
    let snapshot = persistent.publish(
        vector_manifest(CANDIDATE_ENTITY_COUNT, CANDIDATE_DIMENSION, 1),
        None,
    );
    drop(snapshot);
    let snapshot = persistent.recover_snapshot();
    let query = deterministic_vector(CANDIDATE_ENTITY_COUNT - 1, CANDIDATE_DIMENSION);
    let expected = format!("chunk:{:07}", CANDIDATE_ENTITY_COUNT - 1);
    for limit in CANDIDATE_LIMITS {
        let result = snapshot
            .vector_search(request_with(query.clone(), CANDIDATE_DIMENSION, limit))
            .expect("benchmark candidate search preflight succeeds");
        assert!(
            result
                .matches
                .iter()
                .any(|candidate| candidate.entity.as_str() == expected.as_str()),
            "benchmark query must retrieve its exact source vector",
        );
        assert_eq!(
            result.matches.len(),
            limit,
            "every requested candidate must survive the post-filter",
        );
        group.bench_with_input(
            BenchmarkId::new(format!("cosine_{CANDIDATE_DIMENSION}d"), limit),
            &limit,
            |bencher, &limit| {
                bencher.iter_batched(
                    || request_with(query.clone(), CANDIDATE_DIMENSION, limit),
                    |request| {
                        black_box(
                            snapshot
                                .vector_search(request)
                                .expect("benchmark candidate search succeeds"),
                        )
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn vector_search(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("vector_search/verified_generation");
    for entity_count in [100_000_usize, 1_000_000] {
        let mut persistent = PersistentBenchmarkGraph::new();
        let snapshot = persistent.publish(vector_manifest(entity_count, VECTOR_DIMENSION, 1), None);
        drop(snapshot);
        let snapshot = persistent.recover_snapshot();
        let query = deterministic_vector(entity_count - 1, VECTOR_DIMENSION);
        let expected = format!("chunk:{:07}", entity_count - 1);
        assert!(
            snapshot
                .vector_search(request(query.clone()))
                .expect("benchmark vector search preflight succeeds")
                .matches
                .iter()
                .any(|candidate| candidate.entity.as_str() == expected.as_str()),
            "benchmark query must retrieve its exact source vector",
        );
        group.bench_with_input(
            BenchmarkId::new(
                format!("cosine_{VECTOR_DIMENSION}d_top_{RESULT_LIMIT}"),
                entity_count,
            ),
            &entity_count,
            |bencher, _| {
                bencher.iter_batched(
                    || request(query.clone()),
                    |request| {
                        black_box(
                            snapshot
                                .vector_search(request)
                                .expect("benchmark vector search succeeds"),
                        )
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(20));
    targets = vector_search, vector_search_candidates
}
criterion_main!(benches);

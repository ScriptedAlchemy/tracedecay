//! Commit-time cost of one semantic vector page.
//!
//! Run (wall times only):
//!   cargo bench -p tracedecay-graph-db --bench vector_commit_refresh \
//!     --features test-helpers
//! Run with phase attribution (`graph_db.vector_index.*`, `graph_db.write.*`):
//!   cargo bench -p tracedecay-graph-db --bench vector_commit_refresh \
//!     --features test-helpers,hotpath

use std::sync::Arc;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use tracedecay_graph_db::{
    GraphDbLeaseV1, GraphDbOwner, GraphGenerationManifest, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphPropertyName, GraphWriteBatch, NeverCancelled, VectorMetric,
    VectorSearchRequest,
};

#[path = "support/vector.rs"]
mod vector_fixture;

use vector_fixture::{
    VECTOR_NAMESPACE, VECTOR_PROJECTION, VECTOR_PROPERTY, deterministic_vector, vector_manifest,
};

// The semantic lane commits pages of chunk embeddings, one production-shaped
// 768-dim vector per entity, into a projection whose HNSW index already
// exists. Every committed vector row then passes through the post-commit
// index refresh, so the page size is what scales that phase.
const VECTOR_DIMENSION: usize = 768;
const PAGE_ENTITY_COUNTS: [usize; 2] = [5_000, 20_000];

fn memory_db() -> GraphDbLeaseV1 {
    GraphDbOwner::memory(Arc::new(NeverCancelled))
        .expect("benchmark memory graph opens")
        .issue_lease()
        .expect("benchmark memory graph issues a lease")
}

fn page_batch(manifest: GraphGenerationManifest) -> GraphWriteBatch {
    GraphWriteBatch::new(
        manifest.projection.namespace,
        manifest.projection.projection,
        manifest.source_generation,
        manifest.watermark,
        manifest
            .entities
            .into_iter()
            .map(GraphMutation::UpsertEntity)
            .collect(),
        Arc::new(NeverCancelled),
    )
    .expect("benchmark vector page batch is valid")
}

/// A store whose projection and vector index already exist, so the timed
/// apply takes the refresh branch rather than creating the index.
fn seeded_db() -> GraphDbLeaseV1 {
    let db = memory_db();
    db.apply_unverified(page_batch(vector_manifest(1, VECTOR_DIMENSION, 1)))
        .expect("benchmark seed page commits");
    db
}

fn search_request(query: Vec<f32>) -> VectorSearchRequest {
    VectorSearchRequest {
        namespace: GraphNamespace::new(VECTOR_NAMESPACE)
            .expect("benchmark vector namespace is valid"),
        projection: GraphProjectionId::new(VECTOR_PROJECTION)
            .expect("benchmark vector projection is valid"),
        property: GraphPropertyName::new(VECTOR_PROPERTY)
            .expect("benchmark vector property is valid"),
        query,
        dimension: VECTOR_DIMENSION,
        metric: VectorMetric::Cosine,
        limit: 1,
        cancellation: Arc::new(NeverCancelled),
    }
}

/// The refresh must leave the last committed row searchable through the
/// index the apply refreshed; a page that commits without reaching the HNSW
/// would time an apply that no longer serves search.
fn assert_page_is_searchable(entity_count: usize) {
    let db = seeded_db();
    db.apply_unverified(page_batch(vector_manifest(
        entity_count,
        VECTOR_DIMENSION,
        2,
    )))
    .expect("benchmark preflight page commits");
    let expected = format!("chunk:{:07}", entity_count - 1);
    let result = db
        .vector_search(search_request(deterministic_vector(
            entity_count - 1,
            VECTOR_DIMENSION,
        )))
        .expect("benchmark preflight search succeeds");
    assert_eq!(
        result
            .matches
            .iter()
            .map(|candidate| candidate.entity.as_str())
            .collect::<Vec<_>>(),
        vec![expected.as_str()],
        "the committed page must be served by the refreshed vector index",
    );
}

fn vector_commit_refresh(criterion: &mut Criterion) {
    #[cfg(feature = "hotpath")]
    let _hotpath = hotpath::HotpathGuardBuilder::new("vector-commit-refresh").build();

    let mut group = criterion.benchmark_group("vector_commit_refresh/page_apply");
    for entity_count in PAGE_ENTITY_COUNTS {
        assert_page_is_searchable(entity_count);
        group.bench_with_input(
            BenchmarkId::new(format!("cosine_{VECTOR_DIMENSION}d"), entity_count),
            &entity_count,
            |bencher, &entity_count| {
                bencher.iter_batched(
                    || {
                        (
                            seeded_db(),
                            page_batch(vector_manifest(entity_count, VECTOR_DIMENSION, 2)),
                        )
                    },
                    |(db, batch)| {
                        black_box(
                            db.apply_unverified(batch)
                                .expect("benchmark vector page commits"),
                        );
                        db
                    },
                    BatchSize::PerIteration,
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
        .measurement_time(Duration::from_secs(30));
    targets = vector_commit_refresh
}
criterion_main!(benches);

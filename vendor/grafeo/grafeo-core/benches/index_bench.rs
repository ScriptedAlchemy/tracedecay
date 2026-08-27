//! Benchmarks for index structures.
//!
//! Uses `codspeed-criterion-compat` in place of `criterion` directly so the
//! same bench file runs under plain `cargo bench` (pass-through to criterion)
//! and under `cargo codspeed build && cargo codspeed run` (instrumented via
//! Callgrind for commit-level regression tracking on CodSpeed).
// reason: bench code uses small known constants
#![allow(clippy::cast_possible_truncation)]
// reason: criterion_group! expansion from codspeed-criterion-compat does not
// carry doc comments on the generated wrapper functions.
#![allow(missing_docs)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};

use grafeo_common::types::{EdgeId, NodeId};
use grafeo_core::index::adjacency::ChunkedAdjacency;
use grafeo_core::index::hash::HashIndex;
use grafeo_core::index::vector::{
    DistanceMetric, HnswConfig, HnswIndex, ProductQuantizer, ScalarQuantizer, brute_force_knn,
    compute_distance,
};

fn bench_adjacency_insert(c: &mut Criterion) {
    c.bench_function("adjacency_insert_1000", |b| {
        b.iter(|| {
            let adj = ChunkedAdjacency::new();
            for i in 0..1000u64 {
                adj.add_edge(NodeId(i % 100), NodeId(i), EdgeId(i));
            }
            black_box(adj)
        });
    });
}

fn bench_adjacency_lookup(c: &mut Criterion) {
    let adj = ChunkedAdjacency::new();
    for i in 0..10000u64 {
        adj.add_edge(NodeId(i % 100), NodeId(i), EdgeId(i));
    }

    c.bench_function("adjacency_lookup", |b| {
        b.iter(|| {
            for i in 0..100u64 {
                black_box(adj.neighbors(NodeId(i)));
            }
        });
    });
}

fn bench_hash_index_insert(c: &mut Criterion) {
    c.bench_function("hash_index_insert_1000", |b| {
        b.iter(|| {
            let index: HashIndex<u64, NodeId> = HashIndex::new();
            for i in 0..1000u64 {
                index.insert(i, NodeId(i));
            }
            black_box(index)
        });
    });
}

fn bench_hash_index_lookup(c: &mut Criterion) {
    let index: HashIndex<u64, NodeId> = HashIndex::new();
    for i in 0..10000u64 {
        index.insert(i, NodeId(i));
    }

    c.bench_function("hash_index_lookup", |b| {
        b.iter(|| {
            for i in 0..1000u64 {
                black_box(index.get(&i));
            }
        });
    });
}

// ============================================================================
// Vector Index Benchmarks
// ============================================================================

fn generate_random_vectors(count: usize, dims: usize, seed: u64) -> Vec<Vec<f32>> {
    // Simple deterministic pseudo-random generator
    let mut state = seed;
    (0..count)
        .map(|_| {
            (0..dims)
                .map(|_| {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    ((state >> 33) as f32) / (u32::MAX as f32) - 0.5
                })
                .collect()
        })
        .collect()
}

fn bench_distance_computation(c: &mut Criterion) {
    let dims = 384; // Common embedding size
    let vectors = generate_random_vectors(2, dims, 42);
    let v1 = &vectors[0];
    let v2 = &vectors[1];

    let mut group = c.benchmark_group("distance");

    group.bench_function("cosine_384d", |b| {
        b.iter(|| black_box(compute_distance(v1, v2, DistanceMetric::Cosine)));
    });

    group.bench_function("euclidean_384d", |b| {
        b.iter(|| black_box(compute_distance(v1, v2, DistanceMetric::Euclidean)));
    });

    group.bench_function("dot_product_384d", |b| {
        b.iter(|| black_box(compute_distance(v1, v2, DistanceMetric::DotProduct)));
    });

    group.finish();
}

fn bench_brute_force_knn(c: &mut Criterion) {
    let dims = 128;
    let count = 1000;

    let vectors = generate_random_vectors(count, dims, 42);
    let query = generate_random_vectors(1, dims, 123)[0].clone();

    let indexed: Vec<(NodeId, &[f32])> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| (NodeId::new(i as u64), v.as_slice()))
        .collect();

    c.bench_function("brute_force_knn_1k_k10", |b| {
        b.iter(|| {
            black_box(brute_force_knn(
                indexed.iter().map(|(id, v)| (*id, *v)),
                &query,
                10,
                DistanceMetric::Cosine,
            ))
        });
    });
}

fn bench_hnsw_insert(c: &mut Criterion) {
    use std::collections::HashMap;
    use std::sync::Arc;

    let dims = 128;
    let count = 500;

    let vectors = generate_random_vectors(count, dims, 42);

    // Build accessor map
    let map: HashMap<NodeId, Arc<[f32]>> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| (NodeId::new(i as u64), Arc::from(v.as_slice())))
        .collect();
    let accessor = move |id: NodeId| -> Option<Arc<[f32]>> { map.get(&id).cloned() };

    c.bench_function("hnsw_insert_500", |b| {
        b.iter(|| {
            let config = HnswConfig::new(dims, DistanceMetric::Cosine);
            let index = HnswIndex::with_seed(config, 12345);
            for (i, vec) in vectors.iter().enumerate() {
                index.insert(NodeId::new(i as u64), vec, &accessor);
            }
            black_box(index)
        });
    });
}

fn bench_hnsw_search(c: &mut Criterion) {
    use std::collections::HashMap;
    use std::sync::Arc;

    let dims = 128;
    let count = 2000;

    let vectors = generate_random_vectors(count, dims, 42);

    // Build accessor map
    let map: HashMap<NodeId, Arc<[f32]>> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| (NodeId::new(i as u64), Arc::from(v.as_slice())))
        .collect();
    let accessor = move |id: NodeId| -> Option<Arc<[f32]>> { map.get(&id).cloned() };

    let config = HnswConfig::new(dims, DistanceMetric::Cosine);
    let index = HnswIndex::with_seed(config, 12345);

    for (i, vec) in vectors.iter().enumerate() {
        index.insert(NodeId::new(i as u64), vec, &accessor);
    }

    let query = generate_random_vectors(1, dims, 999)[0].clone();

    c.bench_function("hnsw_search_2k_k10", |b| {
        b.iter(|| black_box(index.search(&query, 10, &accessor)));
    });
}

fn bench_scalar_quantization(c: &mut Criterion) {
    let dims = 384;
    let count = 1000;

    let vectors = generate_random_vectors(count, dims, 42);
    let refs: Vec<&[f32]> = vectors.iter().map(|v| v.as_slice()).collect();
    let quantizer = ScalarQuantizer::train(&refs);

    let query = &vectors[0];

    let mut group = c.benchmark_group("scalar_quantization");

    group.bench_function("train_1000x384", |b| {
        b.iter(|| black_box(ScalarQuantizer::train(&refs)));
    });

    group.bench_function("quantize_384d", |b| {
        b.iter(|| black_box(quantizer.quantize(query)));
    });

    let quantized = quantizer.quantize(query);
    let quantized2 = quantizer.quantize(&vectors[1]);

    group.bench_function("distance_u8_384d", |b| {
        b.iter(|| black_box(quantizer.distance_u8(&quantized, &quantized2)));
    });

    group.finish();
}

fn bench_product_quantization(c: &mut Criterion) {
    let dims = 128; // Must be divisible by num_subvectors
    let count = 500;
    let num_subvectors = 8;

    let vectors = generate_random_vectors(count, dims, 42);
    let refs: Vec<&[f32]> = vectors.iter().map(|v| v.as_slice()).collect();
    let quantizer = ProductQuantizer::train(&refs, num_subvectors, 256, 5);

    let query = &vectors[0];

    let mut group = c.benchmark_group("product_quantization");

    group.bench_function("train_500x128_pq8", |b| {
        b.iter(|| black_box(ProductQuantizer::train(&refs, num_subvectors, 256, 5)));
    });

    group.bench_function("quantize_128d_pq8", |b| {
        b.iter(|| black_box(quantizer.quantize(query)));
    });

    let codes = quantizer.quantize(query);

    group.bench_function("asymmetric_distance_128d_pq8", |b| {
        b.iter(|| black_box(quantizer.asymmetric_distance(query, &codes)));
    });

    group.bench_function("build_distance_table_128d_pq8", |b| {
        b.iter(|| black_box(quantizer.build_distance_table(query)));
    });

    let table = quantizer.build_distance_table(query);

    group.bench_function("distance_with_table_pq8", |b| {
        b.iter(|| black_box(quantizer.distance_with_table(&table, &codes)));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// CompactStore benchmarks (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "compact-store")]
fn build_compact_store(
    num_items: usize,
    num_activities: usize,
) -> grafeo_core::graph::compact::CompactStore {
    use grafeo_core::graph::compact::CompactStoreBuilder;

    let scores: Vec<u64> = (0..num_items).map(|i| (i % 10) as u64).collect();
    let names: Vec<&str> = (0..num_items)
        .map(|i| match i % 5 {
            0 => "alpha",
            1 => "beta",
            2 => "gamma",
            3 => "delta",
            _ => "epsilon",
        })
        .collect();
    let ratings: Vec<u64> = (0..num_activities).map(|i| (i % 5 + 1) as u64).collect();
    let edges: Vec<(u32, u32)> = (0..num_activities)
        .map(|i| (i as u32, (i % num_items) as u32))
        .collect();

    CompactStoreBuilder::new()
        .node_table("Item", |t| {
            t.column_bitpacked("score", &scores, 4)
                .column_dict("name", &names)
        })
        .node_table("Activity", |t| t.column_bitpacked("rating", &ratings, 4))
        .rel_table("ACTIVITY_ON", "Activity", "Item", |r| {
            r.edges(edges).backward(true)
        })
        .build()
        .expect("CompactStore build failed")
}

#[cfg(feature = "compact-store")]
fn bench_compact_nodes_by_label(c: &mut Criterion) {
    use grafeo_core::graph::traits::GraphStore;

    let store = build_compact_store(10_000, 100_000);
    c.bench_function("compact/nodes_by_label_100K", |b| {
        b.iter(|| black_box(store.nodes_by_label("Activity").len()));
    });
}

#[cfg(feature = "compact-store")]
fn bench_compact_get_node_property(c: &mut Criterion) {
    use grafeo_common::types::PropertyKey;
    use grafeo_core::graph::traits::GraphStore;

    let store = build_compact_store(10_000, 100_000);
    let ids = store.nodes_by_label("Activity");
    let key = PropertyKey::from("rating");

    let mut state = 12345u64;
    let lookups: Vec<usize> = (0..10_000)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state as usize) % ids.len()
        })
        .collect();

    c.bench_function("compact/get_node_property_10K", |b| {
        b.iter(|| {
            let mut sum = 0i64;
            for &i in &lookups {
                if let Some(grafeo_common::types::Value::Int64(v)) =
                    store.get_node_property(ids[i], &key)
                {
                    sum += v;
                }
            }
            black_box(sum)
        });
    });
}

#[cfg(feature = "compact-store")]
fn bench_compact_edges_from(c: &mut Criterion) {
    use grafeo_core::graph::Direction;
    use grafeo_core::graph::traits::GraphStore;

    let store = build_compact_store(10_000, 100_000);
    let ids = store.nodes_by_label("Activity");

    let mut state = 22222u64;
    let lookups: Vec<usize> = (0..10_000)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (state as usize) % ids.len()
        })
        .collect();

    c.bench_function("compact/edges_from_outgoing_10K", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for &i in &lookups {
                total += store.edges_from(ids[i], Direction::Outgoing).len();
            }
            black_box(total)
        });
    });
}

#[cfg(feature = "compact-store")]
fn bench_compact_find_nodes_by_property(c: &mut Criterion) {
    use grafeo_common::types::Value;
    use grafeo_core::graph::traits::GraphStore;

    let mut group = c.benchmark_group("compact/find_nodes_by_property");

    for &n in &[1_000usize, 10_000, 100_000] {
        let store = build_compact_store(n, n * 5);

        group.bench_function(format!("dict_eq/{n}"), |b| {
            b.iter(|| {
                black_box(
                    store
                        .find_nodes_by_property("name", &Value::String("gamma".into()))
                        .len(),
                )
            });
        });

        group.bench_function(format!("int_eq/{n}"), |b| {
            b.iter(|| {
                black_box(
                    store
                        .find_nodes_by_property("score", &Value::Int64(5))
                        .len(),
                )
            });
        });
    }

    group.finish();
}

#[cfg(feature = "compact-store")]
fn bench_compact_find_nodes_in_range(c: &mut Criterion) {
    use grafeo_common::types::Value;
    use grafeo_core::graph::traits::GraphStore;

    let mut group = c.benchmark_group("compact/find_nodes_in_range");

    for &n in &[1_000usize, 10_000, 100_000] {
        let store = build_compact_store(n, n * 5);

        group.bench_function(format!("score_gt_7/{n}"), |b| {
            b.iter(|| {
                black_box(
                    store
                        .find_nodes_in_range("score", Some(&Value::Int64(7)), None, false, false)
                        .len(),
                )
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_adjacency_insert,
    bench_adjacency_lookup,
    bench_hash_index_insert,
    bench_hash_index_lookup,
    bench_distance_computation,
    bench_brute_force_knn,
    bench_hnsw_insert,
    bench_hnsw_search,
    bench_scalar_quantization,
    bench_product_quantization,
);

#[cfg(feature = "compact-store")]
criterion_group!(
    compact_benches,
    bench_compact_nodes_by_label,
    bench_compact_get_node_property,
    bench_compact_edges_from,
    bench_compact_find_nodes_by_property,
    bench_compact_find_nodes_in_range,
);

#[cfg(not(feature = "compact-store"))]
criterion_main!(benches);

#[cfg(feature = "compact-store")]
criterion_main!(benches, compact_benches);

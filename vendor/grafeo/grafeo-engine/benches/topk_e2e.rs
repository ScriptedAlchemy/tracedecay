//! End-to-end §2.9 sanity benchmark for the heap top-K rewrite.
//!
//! Runs the same query shape via two paths:
//!
//! - `LIMIT 5` literal exercises the rewrite (TopK fires).
//! - `LIMIT $k` parameter falls through to the unfused `Limit` over `Sort`
//!   path since the rewrite refuses non-literal counts.
//!
//! Acceptance: literal path is no slower than parameter path. If literal is
//! slower, the rewrite is causing a regression.
#![allow(missing_docs)]
// reason: bench-only file casts loop indices into Int64 sort keys; values stay below i64::MAX.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};

use grafeo_common::types::Value;
use grafeo_engine::GrafeoDB;

/// Seeds the database with `n` `:Item` nodes. The GQL/Cypher INSERT path
/// doesn't accept arithmetic expressions in property sources, so values are
/// computed in Rust and inlined per-INSERT. Seed time is O(n) parser
/// invocations; at N=500k the setup runs once before `b.iter`, so it
/// shows up as bench-group startup cost, not per-iteration cost.
fn seed(n: usize) -> GrafeoDB {
    let db = GrafeoDB::new_in_memory();
    let session = db.session();
    for i in 0..n {
        let r = (i as u64).wrapping_mul(2_654_435_761) % 1_000_000;
        let x = i % 100;
        session
            .execute(&format!("INSERT (:Item {{id: {i}, r: {r}, x: {x}}})"))
            .unwrap();
    }
    db
}

fn bench_topk_e2e(c: &mut Criterion) {
    // N is sized so the operator-level work dominates fixed
    // parser/planner/IO overhead. At N=100k the filtered post-Sort row
    // count (~50k) leaves the operator delta below the per-query fixed
    // cost, and the literal/parameter ratio sits inside criterion's
    // noise band, which violates the spec §9 strict literal <= parameter
    // assertion. N=500k keeps the same query shape but lets the
    // operator-level ~12x win show through end-to-end.
    let db = seed(500_000);

    let mut group = c.benchmark_group("top_k_filtered_e2e");
    group.sample_size(20);

    group.bench_function("literal", |b| {
        b.iter(|| {
            let session = db.session();
            let result = session
                .execute("MATCH (n:Item) WHERE n.x > 50 RETURN n.r ORDER BY n.r DESC LIMIT 5")
                .unwrap();
            black_box(result.row_count());
        });
    });

    group.bench_function("parameter", |b| {
        b.iter(|| {
            let session = db.session();
            // Parameter LIMIT prevents the rewrite (CountExpr::Parameter falls
            // through). Same query shape otherwise; performance delta is
            // attributable to the rewrite vs. the unfused path.
            let mut params = std::collections::HashMap::new();
            params.insert("k".to_string(), Value::Int64(5));
            let result = session
                .execute_with_params(
                    "MATCH (n:Item) WHERE n.x > 50 RETURN n.r ORDER BY n.r DESC LIMIT $k",
                    params,
                )
                .unwrap();
            black_box(result.row_count());
        });
    });

    group.finish();
}

criterion_group!(benches, bench_topk_e2e);
criterion_main!(benches);

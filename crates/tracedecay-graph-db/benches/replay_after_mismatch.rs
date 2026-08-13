use std::time::Duration;

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};

#[path = "support/code.rs"]
mod code_fixture;
mod support;

use code_fixture::code_manifest;
use support::mismatch::ExactMismatchReplay;

const BRANCHING: usize = 6;
const DEPTH: usize = 4;

fn replay_after_exact_recovered_digest_mismatch(criterion: &mut Criterion) {
    criterion.bench_function(
        "recovery/replay_after_exact_recovered_digest_mismatch",
        |bencher| {
            bencher.iter_batched(
                || ExactMismatchReplay::prepare(code_manifest(BRANCHING, DEPTH, 1)),
                |prepared| {
                    let snapshot = prepared.replay();
                    assert_eq!(snapshot.generation().as_str(), "generation:1");
                    assert_eq!(snapshot.projection().namespace.as_str(), "benchmark-code");
                    black_box(snapshot)
                },
                BatchSize::PerIteration,
            );
        },
    );
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(20));
    targets = replay_after_exact_recovered_digest_mismatch
}
criterion_main!(benches);

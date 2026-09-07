use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use tracedecay_graph_db::{
    GraphEntityId, GraphNamespace, GraphRelationKind, GraphTraversalDirection, NeverCancelled,
    TraversalRequest, VerifiedGraphSnapshot,
};

#[path = "support/code.rs"]
mod code_fixture;
mod support;

use code_fixture::{CALLS_RELATION, CODE_NAMESPACE, code_manifest};
use support::PersistentBenchmarkGraph;

const BRANCHING: usize = 6;
const DEPTH: usize = 4;
const ENTITY_COUNT: usize = 1 + 6 + 36 + 216 + 1_296;
const READER_COUNT: usize = 4;

fn traversal_request() -> TraversalRequest {
    TraversalRequest {
        namespace: GraphNamespace::new(CODE_NAMESPACE).expect("benchmark namespace is valid"),
        start: GraphEntityId::new("symbol:root").expect("benchmark start identity is valid"),
        relation_kinds: BTreeSet::from([
            GraphRelationKind::new(CALLS_RELATION).expect("benchmark relation kind is valid")
        ]),
        direction: GraphTraversalDirection::Outgoing,
        max_depth: DEPTH,
        max_visits: ENTITY_COUNT,
        max_results: ENTITY_COUNT,
        cancellation: Arc::new(NeverCancelled),
    }
}

fn retained_reader(
    snapshot: VerifiedGraphSnapshot,
    ready: Arc<Barrier>,
    writer_started: Arc<AtomicBool>,
    writer_completed: Arc<AtomicBool>,
) -> usize {
    ready.wait();
    while !writer_started.load(Ordering::Acquire) {
        std::hint::spin_loop();
    }
    let mut observed = 0_usize;
    while !writer_completed.load(Ordering::Acquire) {
        observed += snapshot
            .traverse(traversal_request())
            .expect("retained verified reader traversal succeeds")
            .visits
            .len();
    }
    observed
}

fn generation_replacement(criterion: &mut Criterion) {
    let mut persistent = PersistentBenchmarkGraph::new();
    let mut generation = 1_usize;
    drop(persistent.publish(code_manifest(BRANCHING, DEPTH, generation), None));
    criterion.bench_function(
        "mixed_read_write/generation_replacement_and_verify",
        |bencher| {
            bencher.iter_batched(
                || {
                    generation += 1;
                    code_manifest(BRANCHING, DEPTH, generation)
                },
                |manifest| black_box(persistent.publish(manifest, None)),
                BatchSize::LargeInput,
            );
        },
    );
}

fn close_reopen_recovery(criterion: &mut Criterion) {
    let mut persistent = PersistentBenchmarkGraph::new();
    drop(persistent.publish(code_manifest(BRANCHING, DEPTH, 1), None));
    criterion.bench_function(
        "mixed_read_write/close_reopen_full_digest_recovery",
        |bencher| {
            bencher.iter(|| black_box(persistent.recover_snapshot()));
        },
    );
}

fn concurrent_readers_and_writer(criterion: &mut Criterion) {
    let mut persistent = PersistentBenchmarkGraph::new();
    let mut generation = 1_usize;
    let mut current = persistent.publish(code_manifest(BRANCHING, DEPTH, generation), None);
    criterion.bench_function(
        "mixed_read_write/four_retained_readers_one_convergence_writer",
        |bencher| {
            bencher.iter_batched(
                || {
                    generation += 1;
                    code_manifest(BRANCHING, DEPTH, generation)
                },
                |manifest| {
                    let ready = Arc::new(Barrier::new(READER_COUNT + 1));
                    let writer_started = Arc::new(AtomicBool::new(false));
                    let writer_completed = Arc::new(AtomicBool::new(false));
                    let (next, observed_visits) = std::thread::scope(|scope| {
                        let readers = (0..READER_COUNT)
                            .map(|_| {
                                let snapshot = current.clone();
                                let ready = Arc::clone(&ready);
                                let writer_started = Arc::clone(&writer_started);
                                let writer_completed = Arc::clone(&writer_completed);
                                scope.spawn(move || {
                                    retained_reader(
                                        snapshot,
                                        ready,
                                        writer_started,
                                        writer_completed,
                                    )
                                })
                            })
                            .collect::<Vec<_>>();
                        ready.wait();
                        let next = persistent
                            .publish(manifest, Some((&writer_started, &writer_completed)));
                        let observed_visits = readers
                            .into_iter()
                            .map(|reader| {
                                reader
                                    .join()
                                    .expect("benchmark reader thread remains healthy")
                            })
                            .collect::<Vec<_>>();
                        (next, observed_visits)
                    });
                    current = next;
                    assert!(
                        observed_visits
                            .iter()
                            .all(|visits| *visits >= ENTITY_COUNT),
                        "every reader must complete against the retained generation while the writer converges",
                    );
                    black_box(observed_visits.into_iter().sum::<usize>());
                },
                BatchSize::LargeInput,
            );
        },
    );
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .measurement_time(Duration::from_secs(20));
    targets = generation_replacement, close_reopen_recovery, concurrent_readers_and_writer
}
criterion_main!(benches);

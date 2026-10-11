//! Publication and search protocol benchmark on fresh real store files.
//!
//! Submitters publish diagnostics through production writer actors while
//! foreground readers run the current-generation read and a full-text search
//! through production reader pools. Each process runs every shape once and
//! prints one JSON line per shape. `benchmark_data/storage-concurrency/README.md`
//! describes the shapes and the runner that records fresh runs.

#[allow(dead_code, unused_imports)]
#[path = "../../../tests/storage_runtime_rusqlite_suite/runtime_test_support.rs"]
mod runtime_test_support;
#[allow(dead_code, unused_imports)]
#[path = "../tests/rusqlite_suite/storage_contention.rs"]
mod storage_contention;

use std::sync::{
    Barrier,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use runtime_test_support::{Probe, TestDatabase, reader_locator};
use storage_contention::{
    binding, commit, durable_sequences, generation, publication, readers, repository_read, seed,
    start_writer,
};
use tracedecay_domain::test_fixtures::id;
use tracedecay_rusqlite_runtime::{
    PersistentWriter, WriterActorError,
    reader::{ReaderPool, ReaderQueryExecutor},
    repository::ConcreteRepositoryWriteExecutor,
};
use tracedecay_sql::{ENGINE, Engine, Transaction};
use tracedecay_store::{
    AdmissionConfigV1, DiagnosticReadResultV1, ProjectReadResultV1, RepositoryReadResultV1,
    RuntimeReadCoverageV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeReadResultV1,
    RuntimeSubmitOutcomeV1, StorageRuntimeErrorV1, StoreRuntimeBindingV1,
};

const RECORDS: usize = 8;
const READERS_PER_KIND: usize = 2;
/// Runs above this one-minute load average on the benchmark host are recorded
/// but flagged as not representative.
const MAX_REPRESENTATIVE_LOAD: f64 = 200.0;

/// Which idempotency keys the submitters on one shard use.
#[derive(Clone, Copy)]
enum Keys {
    /// Every submitter publishes its own keys; contention is queueing only.
    Unique,
    /// Every submitter on a shard publishes every key. Odd submitters send a
    /// different command digest. Each key commits once; identical attempts
    /// coalesce onto it or replay it, and the other digest conflicts.
    Contended,
}

struct Shape {
    name: &'static str,
    shards: usize,
    submitters: usize,
    operations: usize,
    keys: Keys,
}

const SHAPES: [Shape; 4] = [
    Shape {
        name: "queue-1x1",
        shards: 1,
        submitters: 1,
        operations: 1024,
        keys: Keys::Unique,
    },
    Shape {
        name: "queue-1x8",
        shards: 1,
        submitters: 8,
        operations: 128,
        keys: Keys::Unique,
    },
    Shape {
        name: "queue-4x8",
        shards: 4,
        submitters: 8,
        operations: 128,
        keys: Keys::Unique,
    },
    Shape {
        name: "conflict-1x8",
        shards: 1,
        submitters: 8,
        operations: 128,
        keys: Keys::Contended,
    },
];

/// Full-text index over published diagnostic messages, maintained inside the
/// publishing transaction.
const SEARCH_SCHEMA: &str = match ENGINE {
    Engine::Sqlite => {
        "CREATE VIRTUAL TABLE generation_diagnostics_search
             USING fts5(message, content = 'generation_diagnostics');
         CREATE TRIGGER generation_diagnostics_search_insert
             AFTER INSERT ON generation_diagnostics BEGIN
             INSERT INTO generation_diagnostics_search(rowid, message)
             VALUES (new.rowid, new.message);
         END;"
    }
    Engine::Native => {
        "CREATE INDEX generation_diagnostics_search
             ON generation_diagnostics USING fts (message);"
    }
};

const SEARCH_QUERY: &str = match ENGINE {
    Engine::Sqlite => {
        "SELECT d.generation_id FROM generation_diagnostics_search s
         JOIN generation_diagnostics d ON d.rowid = s.rowid
         WHERE generation_diagnostics_search MATCH ?1 AND d.record_state = 'current'
         ORDER BY bm25(generation_diagnostics_search) LIMIT 10"
    }
    Engine::Native => {
        "SELECT generation_id FROM generation_diagnostics
         WHERE fts_match(message, ?1) AND record_state = 'current'
         ORDER BY fts_score(message, ?1) DESC LIMIT 10"
    }
};

/// Answers the current-generation request from the top search hit, so the
/// reader pool validates a typed result and an empty search fails the read.
/// The snapshot is the `tracedecay_sql` type: a native build compiles only
/// when the runtime's readers run on that driver, so the recorded `engine`
/// is the engine that executed the workload.
#[derive(Clone, Default)]
struct SearchReader;

impl ReaderQueryExecutor for SearchReader {
    fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        let failed = |stage: &str| StorageRuntimeErrorV1::Infrastructure {
            operation: format!("benchmark search {stage}"),
        };
        let hits: Vec<String> = snapshot
            .prepare_cached(SEARCH_QUERY)
            .map_err(|_| failed("prepare"))?
            .query_map(["mismatched"], |row| row.get(0))
            .map_err(|_| failed("query"))?
            .collect::<Result<_, _>>()
            .map_err(|_| failed("row"))?;
        let top = hits.first().ok_or_else(|| failed("returned no hits"))?;
        RuntimeReadOutcomeV1::new(
            Some(RuntimeReadResultV1::Repository {
                result: RepositoryReadResultV1::Project(Box::new(
                    ProjectReadResultV1::Diagnostics(DiagnosticReadResultV1::CurrentGeneration(
                        Some(id(top)),
                    )),
                )),
            }),
            RuntimeReadCoverageV1::Latest { observed: None },
        )
        .map_err(|error| StorageRuntimeErrorV1::Infrastructure {
            operation: error.to_string(),
        })
    }
}

fn percentiles(mut samples: Vec<Duration>) -> serde_json::Value {
    samples.sort_unstable();
    let percentile = |percent: usize| {
        let index = (samples.len() * percent).div_ceil(100).saturating_sub(1);
        samples
            .get(index)
            .map_or(0.0, |sample| sample.as_secs_f64() * 1_000.0)
    };
    serde_json::json!({
        "samples": samples.len(),
        "p50_ms": percentile(50),
        "p95_ms": percentile(95),
        "p99_ms": percentile(99),
    })
}

fn load_average() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[derive(Default)]
struct Submissions {
    latencies: Vec<Duration>,
    committed: usize,
    exact_replay: usize,
    idempotency_conflict: usize,
    in_flight_retries: usize,
    saturated_retries: usize,
    failed: usize,
}

impl Submissions {
    fn merge(&mut self, other: Self) {
        self.latencies.extend(other.latencies);
        self.committed += other.committed;
        self.exact_replay += other.exact_replay;
        self.idempotency_conflict += other.idempotency_conflict;
        self.in_flight_retries += other.in_flight_retries;
        self.saturated_retries += other.saturated_retries;
        self.failed += other.failed;
    }
}

/// Submits one publication until it settles. Latency spans the first attempt
/// to the settled receipt, so queueing and retries are inside it.
fn settle(
    runtime: &tokio::runtime::Runtime,
    writer: &PersistentWriter,
    binding: &StoreRuntimeBindingV1,
    name: &str,
    records: usize,
    totals: &mut Submissions,
) {
    let request = publication(binding, name, records);
    let started = Instant::now();
    loop {
        let probe = Probe::for_submit(&request);
        match runtime.block_on(writer.submit(request.clone(), probe)) {
            Ok(RuntimeSubmitOutcomeV1::Committed { .. }) => totals.committed += 1,
            Ok(RuntimeSubmitOutcomeV1::ExactReplay { .. }) => totals.exact_replay += 1,
            Ok(RuntimeSubmitOutcomeV1::IdempotencyConflict { .. }) => {
                totals.idempotency_conflict += 1;
            }
            Ok(RuntimeSubmitOutcomeV1::Saturated { retry_after_ms, .. }) => {
                totals.saturated_retries += 1;
                std::thread::sleep(Duration::from_millis(retry_after_ms.max(1)));
                continue;
            }
            Err(WriterActorError::StorageFailure(
                StorageRuntimeErrorV1::DuplicateOperationInFlight { .. },
            )) => {
                totals.in_flight_retries += 1;
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            _ => totals.failed += 1,
        }
        totals.latencies.push(started.elapsed());
        return;
    }
}

struct Reads {
    latencies: Vec<Duration>,
    failed: usize,
}

fn read_until<E: ReaderQueryExecutor>(
    pool: &ReaderPool<E>,
    request: &RuntimeReadRequestV1,
    writing: &AtomicBool,
) -> Reads {
    let mut reads = Reads {
        latencies: Vec::new(),
        failed: 0,
    };
    while writing.load(Ordering::Acquire) {
        let probe = Probe::for_read(request);
        let started = Instant::now();
        let result = pool
            .acquire(request, &probe, Duration::from_secs(1))
            .map_err(|error| error.to_string())
            .and_then(|mut lease| {
                lease
                    .begin_snapshot()
                    .map_err(|error| error.to_string())?
                    .execute(request.clone(), &probe)
                    .map_err(|error| error.to_string())
            });
        reads.latencies.push(started.elapsed());
        match result {
            Ok(outcome) => {
                generation(&outcome);
            }
            Err(_) => reads.failed += 1,
        }
    }
    reads
}

struct StopReadersOnDrop<'a>(&'a AtomicBool);
impl Drop for StopReadersOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn measure(shape: &Shape) -> serde_json::Value {
    let load_start = load_average();
    let databases: Vec<_> = (0..shape.shards)
        .map(|_| {
            let database = TestDatabase::new("contention-benchmark.db");
            seed(&database);
            database.connect().execute_batch(SEARCH_SCHEMA).unwrap();
            database
        })
        .collect();
    let bindings: Vec<_> = (0..shape.shards).map(binding).collect();
    let writers: Vec<_> = databases
        .iter()
        .zip(&bindings)
        .map(|(database, binding)| {
            start_writer(
                database,
                binding,
                ConcreteRepositoryWriteExecutor::default(),
            )
        })
        .collect();
    let generation_pools: Vec<_> = databases
        .iter()
        .zip(&bindings)
        .map(|(database, binding)| readers(database, binding))
        .collect();
    let search_pools: Vec<_> = databases
        .iter()
        .zip(&bindings)
        .map(|(database, binding)| {
            ReaderPool::start(
                reader_locator(binding, &database.path),
                AdmissionConfigV1::default().readers,
                SearchReader,
            )
            .unwrap()
        })
        .collect();
    // A durable seed gives every measured read and search a real generation.
    for (writer, binding) in writers.iter().zip(&bindings) {
        commit(writer, publication(binding, "generation.seed", RECORDS));
    }
    let barrier = Barrier::new(shape.submitters + 2 * READERS_PER_KIND + 1);
    let writing = AtomicBool::new(true);
    let (writer_elapsed, reader_elapsed, submissions, reads, searches) =
        std::thread::scope(|scope| {
            let stop_readers = StopReadersOnDrop(&writing);
            let submit_tasks: Vec<_> = (0..shape.submitters)
                .map(|submitter| {
                    let shard = submitter % shape.shards;
                    let (writer, binding, barrier) = (&writers[shard], &bindings[shard], &barrier);
                    scope.spawn(move || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_time()
                            .build()
                            .unwrap();
                        let mut totals = Submissions::default();
                        barrier.wait();
                        for operation in 0..shape.operations {
                            let (name, records) = match shape.keys {
                                Keys::Unique => {
                                    (format!("generation.p{submitter}.o{operation}"), RECORDS)
                                }
                                Keys::Contended => {
                                    (format!("generation.o{operation}"), RECORDS + submitter % 2)
                                }
                            };
                            settle(&runtime, writer, binding, &name, records, &mut totals);
                        }
                        totals
                    })
                })
                .collect();
            let read_tasks: Vec<_> = (0..READERS_PER_KIND)
                .map(|reader| {
                    let shard = reader % shape.shards;
                    let request = repository_read(&bindings[shard]);
                    let (pool, barrier, writing) = (&generation_pools[shard], &barrier, &writing);
                    scope.spawn(move || {
                        barrier.wait();
                        read_until(pool, &request, writing)
                    })
                })
                .collect();
            let search_tasks: Vec<_> = (0..READERS_PER_KIND)
                .map(|reader| {
                    let shard = reader % shape.shards;
                    let request = repository_read(&bindings[shard]);
                    let (pool, barrier, writing) = (&search_pools[shard], &barrier, &writing);
                    scope.spawn(move || {
                        barrier.wait();
                        read_until(pool, &request, writing)
                    })
                })
                .collect();
            let started = Instant::now();
            barrier.wait();
            let mut submissions = Submissions::default();
            for task in submit_tasks {
                submissions.merge(task.join().unwrap());
            }
            let writer_elapsed = started.elapsed();
            drop(stop_readers);
            let join = |tasks: Vec<std::thread::ScopedJoinHandle<'_, Reads>>| {
                tasks.into_iter().map(|task| task.join().unwrap()).fold(
                    Reads {
                        latencies: Vec::new(),
                        failed: 0,
                    },
                    |mut all, reads| {
                        all.latencies.extend(reads.latencies);
                        all.failed += reads.failed;
                        all
                    },
                )
            };
            let reads = join(read_tasks);
            let searches = join(search_tasks);
            (
                writer_elapsed,
                started.elapsed(),
                submissions,
                reads,
                searches,
            )
        });
    let telemetry: Vec<_> = writers
        .iter()
        .map(PersistentWriter::telemetry_snapshot)
        .collect();
    let sum = |field: fn(&tracedecay_rusqlite_runtime::WriterTelemetrySnapshot) -> u64| {
        telemetry.iter().map(field).sum::<u64>()
    };
    let batch_operations = sum(|snapshot| snapshot.batches.batch_operations).max(1);
    let committed_batches = sum(|snapshot| snapshot.batches.committed_batches).max(1);
    let load_end = load_average();
    let peak_load = load_start
        .into_iter()
        .chain(load_end)
        .fold(None, |peak: Option<f64>, load| {
            Some(peak.map_or(load, |peak| peak.max(load)))
        });
    let per_second = |count: usize, elapsed: Duration| count as f64 / elapsed.as_secs_f64();
    let settled = submissions.latencies.len();
    let record = serde_json::json!({
        "source_sha": std::env::var("TRACEDECAY_BENCH_SOURCE_SHA").ok(),
        "host": std::env::var("TRACEDECAY_BENCH_HOST").ok(),
        "engine": format!("{ENGINE:?}").to_lowercase(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpus": std::thread::available_parallelism().map_or(0, usize::from),
        "debug_assertions": cfg!(debug_assertions),
        "load_average_1m": { "start": load_start, "end": load_end },
        "representative": peak_load.is_none_or(|load| load <= MAX_REPRESENTATIVE_LOAD),
        "shape": shape.name,
        "shards": shape.shards,
        "submitters": shape.submitters,
        "operations_per_submitter": shape.operations,
        "diagnostics_per_publication": RECORDS,
        "generation_readers": READERS_PER_KIND,
        "search_readers": READERS_PER_KIND,
        "writer_elapsed_s": writer_elapsed.as_secs_f64(),
        "reader_elapsed_s": reader_elapsed.as_secs_f64(),
        "settled_per_s": per_second(settled, writer_elapsed),
        "committed_per_s": per_second(submissions.committed, writer_elapsed),
        "reads_per_s": per_second(reads.latencies.len() - reads.failed, reader_elapsed),
        "searches_per_s": per_second(searches.latencies.len() - searches.failed, reader_elapsed),
        "receipt_latency": percentiles(submissions.latencies),
        "read_latency": percentiles(reads.latencies),
        "search_latency": percentiles(searches.latencies),
        "outcomes": {
            "committed": submissions.committed,
            "exact_replay": submissions.exact_replay,
            "idempotency_conflict": submissions.idempotency_conflict,
        },
        "retries": {
            "in_flight": submissions.in_flight_retries,
            "saturated": submissions.saturated_retries,
        },
        "failed_submissions": submissions.failed,
        "failed_reads": reads.failed,
        "failed_searches": searches.failed,
        "busy_events": sum(|snapshot| snapshot.busy_events),
        "committed_transactions_including_seeds":
            sum(|snapshot| snapshot.transactions.committed_transactions),
        "mean_queue_wait_ms":
            sum(|snapshot| snapshot.batches.queue_wait_micros) as f64 / batch_operations as f64 / 1_000.0,
        "mean_transaction_ms":
            sum(|snapshot| snapshot.batches.transaction_micros) as f64 / committed_batches as f64 / 1_000.0,
    });
    drop((generation_pools, search_pools));
    for writer in writers {
        writer.shutdown_and_join().unwrap();
    }
    assert_eq!(submissions.failed, 0, "rejected submissions: {record}");
    assert_eq!(reads.failed, 0, "rejected reads: {record}");
    assert_eq!(searches.failed, 0, "rejected searches: {record}");
    let attempts = shape.submitters * shape.operations;
    assert_eq!(settled, attempts, "every attempt settles: {record}");
    let keys_per_shard = match shape.keys {
        Keys::Unique => {
            assert_eq!(
                submissions.committed, attempts,
                "every key commits: {record}"
            );
            shape.submitters / shape.shards * shape.operations
        }
        Keys::Contended => {
            assert!(
                submissions.idempotency_conflict > 0,
                "contended keys must conflict: {record}"
            );
            shape.operations
        }
    };
    // One durable commit per key, whatever outcome each attempt reported.
    for database in &databases {
        durable_sequences(database, u64::try_from(keys_per_shard + 1).unwrap());
    }
    record
}

fn main() {
    for shape in &SHAPES {
        println!("{}", measure(shape));
    }
}

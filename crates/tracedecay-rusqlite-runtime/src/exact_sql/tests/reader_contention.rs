//! Reader latency while the serialized writer holds a transaction open.
//!
//! WAL readers observe a committed snapshot, so the writer's transaction must
//! never appear in a reader's latency. Concurrency here stays below the general
//! lane's lease ceiling so a recorded latency is the read path's own cost and
//! not pool queueing.

use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use super::*;

const SEED_ROWS: i64 = 5_000;
/// Below the general lane's ceiling of 8, so lease acquisition never queues.
const READER_THREADS: usize = 4;
const READ_WAIT: Duration = Duration::from_secs(5);
/// Reads run for a fixed window rather than a fixed count, so the writer's
/// transaction is provably held open for seconds while they execute.
const READ_WINDOW: Duration = Duration::from_secs(2);
/// A point read costs tens of microseconds; a read that waited on the held
/// writer transaction would land in the hundreds of milliseconds. The bound
/// sits far above the real cost so scheduling noise cannot fail it, and far
/// below a genuine wait so the contract still bites.
const READ_LATENCY_BOUND: Duration = Duration::from_millis(250);

fn seeded() -> (Fixture, Arc<ExactSqlHandle>) {
    let fixture = fixture('a', 'a');
    let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::new(AtomicBool::new(true)))))
        .unwrap();
    handle
        .execute_batch(
            "CREATE TABLE reading (key INTEGER PRIMARY KEY, payload TEXT NOT NULL)".to_owned(),
        )
        .unwrap();
    handle
        .execute(statement(
            "INSERT INTO reading (key, payload) \
             SELECT i, 'seed-' || i FROM \
             (WITH RECURSIVE seed(i) AS \
              (SELECT 0 UNION ALL SELECT i + 1 FROM seed WHERE i < ?) SELECT i FROM seed)",
            vec![ExactSqlValue::Integer(SEED_ROWS - 1)],
        ))
        .unwrap();
    (fixture, Arc::new(handle))
}

/// Point reads until `deadline`, recording each latency separately.
fn point_reads(handle: &ExactSqlHandle, thread_index: usize, deadline: Instant) -> Vec<Duration> {
    let mut samples = Vec::new();
    let mut iteration = 0_i64;
    while Instant::now() < deadline {
        let key = (thread_index as i64 * SEED_ROWS / 4 + iteration * 7) % SEED_ROWS;
        iteration += 1;
        let started = Instant::now();
        let rows = handle
            .query_with_priority(
                statement(
                    "SELECT payload FROM reading WHERE key = ?",
                    vec![ExactSqlValue::Integer(key)],
                ),
                OperationPriorityV1::Foreground,
                READ_WAIT,
            )
            .unwrap();
        samples.push(started.elapsed());
        assert_eq!(rows.rows.len(), 1, "seeded key {key} must resolve");
    }
    samples
}

fn concurrent_point_reads(handle: &Arc<ExactSqlHandle>) -> Vec<Duration> {
    let barrier = Arc::new(Barrier::new(READER_THREADS));
    let deadline = Instant::now() + READ_WINDOW;
    let readers = (0..READER_THREADS)
        .map(|index| {
            let handle = Arc::clone(handle);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                point_reads(&handle, index, deadline)
            })
        })
        .collect::<Vec<_>>();
    readers
        .into_iter()
        .flat_map(|reader| reader.join().unwrap())
        .collect()
}

struct Distribution {
    samples: Vec<Duration>,
}

impl Distribution {
    fn new(mut samples: Vec<Duration>) -> Self {
        samples.sort_unstable();
        Self { samples }
    }

    fn quantile(&self, fraction: f64) -> Duration {
        let last = self.samples.len() - 1;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let index = ((last as f64) * fraction).round() as usize;
        self.samples[index]
    }

    fn report(&self, label: &str) {
        println!(
            "{label}: n={} p50={:?} p95={:?} p99={:?} max={:?}",
            self.samples.len(),
            self.quantile(0.50),
            self.quantile(0.95),
            self.quantile(0.99),
            self.samples[self.samples.len() - 1]
        );
    }
}

/// Concurrent point reads stay bounded while one writer transaction is held
/// open across the whole read phase.
///
/// Falsifiable in both directions: the writer reports the window during which
/// it actually held an uncommitted transaction, and the assertion fails if any
/// read outside a small tail exceeded an ordinary bound. A reader lease that
/// serialized behind the writer, or a checkpoint that stalled readers, moves
/// these reads into the hundreds of milliseconds.
#[test]
fn concurrent_reads_stay_bounded_while_a_writer_transaction_is_held() {
    let (_fixture, handle) = seeded();

    Distribution::new(concurrent_point_reads(&handle)).report("uncontended");

    let writing = Arc::new(AtomicBool::new(true));
    let holding = Arc::new(Barrier::new(2));
    let writer_handle = Arc::clone(&handle);
    let writer_running = Arc::clone(&writing);
    let writer_holding = Arc::clone(&holding);
    let writer = thread::spawn(move || {
        let transaction = writer_handle.begin_immediate().unwrap();
        let mut key = SEED_ROWS;
        // Establish the write lock before the readers start, so the whole read
        // phase runs inside an uncommitted transaction.
        transaction
            .execute(statement(
                "INSERT INTO reading (key, payload) VALUES (?, 'held')",
                vec![ExactSqlValue::Integer(key)],
            ))
            .unwrap();
        key += 1;
        let held_from = Instant::now();
        writer_holding.wait();
        // The transaction stays open across every insert; its idle bound
        // requires continuous progress, which a bulk writer makes anyway.
        while writer_running.load(Ordering::Acquire) {
            transaction
                .execute(statement(
                    "INSERT INTO reading (key, payload) VALUES (?, 'held')",
                    vec![ExactSqlValue::Integer(key)],
                ))
                .unwrap();
            key += 1;
        }
        let held_for = held_from.elapsed();
        transaction.rollback().unwrap();
        (key - SEED_ROWS, held_for)
    });

    holding.wait();
    let reads_from = Instant::now();
    let contended = Distribution::new(concurrent_point_reads(&handle));
    let reads_took = reads_from.elapsed();
    writing.store(false, Ordering::Release);
    let (written, held_for) = writer.join().unwrap();
    contended.report("writer-held");
    println!("writer held one open transaction for {held_for:?} across {written} inserts");

    assert!(
        written > 1 && held_for >= reads_took && held_for >= READ_WINDOW,
        "the writer must have held one uncommitted transaction across every read: \
         held {held_for:?} for {written} inserts, reads took {reads_took:?}"
    );
    assert!(
        contended.quantile(0.99) < READ_LATENCY_BOUND,
        "reads waited on the held writer transaction: p99 {:?}",
        contended.quantile(0.99)
    );
}

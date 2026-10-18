//! Readers while the serialized writer holds a transaction open.
//!
//! WAL readers observe a committed snapshot, so the writer's open transaction
//! must never gate a read. Concurrency here stays below the general lane's
//! lease ceiling so every read is served by its own worker, not by pool
//! queueing.

use std::sync::{Arc, Barrier, atomic::AtomicBool, mpsc};
use std::thread;

use super::*;

const SEED_ROWS: i64 = 5_000;
/// Below the general lane's ceiling of 8, so lease acquisition never queues.
const READER_THREADS: usize = 4;
const READS_PER_THREAD: i64 = 64;

fn seeded() -> (Fixture, Arc<ExactSqlHandle>) {
    let fixture = fixture('a', 'a');
    let handle = ExactSqlHandle::attach(&fixture.writer, &fixture.readers)
        .unwrap()
        .with_write_authority(Arc::new(AtomicWriteAuthority(Arc::new(AtomicBool::new(
            true,
        )))))
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

fn point_read(handle: &ExactSqlHandle, key: i64) -> ExactSqlRows {
    // Acquisition is unbounded: a read gated on the writer would wait for a
    // release that only comes after every read returns, so it hangs rather
    // than racing a wall-clock bound the host's scheduling decides.
    handle
        .query_with_priority(
            statement(
                "SELECT payload FROM reading WHERE key = ?",
                vec![ExactSqlValue::Integer(key)],
            ),
            OperationPriorityV1::Foreground,
            Duration::MAX,
        )
        .unwrap()
}

/// Concurrent point reads complete while one writer transaction is held open
/// with an uncommitted row, and none of them observes that row.
///
/// The writer parks inside its transaction until every read has returned, so
/// a reader lease that serialized behind the writer, or a reader lane gated
/// by it, never returns and the test cannot finish.
#[test]
fn concurrent_reads_complete_while_a_writer_transaction_is_held() {
    let (_fixture, handle) = seeded();
    let held_key = SEED_ROWS;

    let (held, holding) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let writer = thread::spawn({
        let handle = Arc::clone(&handle);
        move || {
            let transaction = handle.begin_immediate().unwrap();
            transaction
                .execute(statement(
                    "INSERT INTO reading (key, payload) VALUES (?, 'held')",
                    vec![ExactSqlValue::Integer(held_key)],
                ))
                .unwrap();
            held.send(()).unwrap();
            released.recv().unwrap();
            transaction.rollback().unwrap();
        }
    });
    holding.recv().unwrap();

    let barrier = Arc::new(Barrier::new(READER_THREADS));
    let readers = (0..READER_THREADS)
        .map(|index| {
            let handle = Arc::clone(&handle);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for read in 0..READS_PER_THREAD {
                    let key = (index as i64 * SEED_ROWS / 4 + read * 7) % SEED_ROWS;
                    let rows = point_read(&handle, key);
                    assert_eq!(rows.rows.len(), 1, "seeded key {key} must resolve");
                }
                assert!(
                    point_read(&handle, held_key).rows.is_empty(),
                    "a reader must not observe the writer's uncommitted row"
                );
            })
        })
        .collect::<Vec<_>>();
    for reader in readers {
        reader.join().unwrap();
    }

    release.send(()).unwrap();
    writer.join().unwrap();
}

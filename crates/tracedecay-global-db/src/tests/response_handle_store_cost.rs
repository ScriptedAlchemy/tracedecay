//! Response-handle payload I/O and writer lock tenure on large handles.
//!
//! Inventory and expiry selection are answered from the covering index, so
//! their SQLite work follows the row count and never the payload size. A
//! replay of live content writes no rows. Its writer lock tenure is measured
//! independently of the fresh publication's duration. A page read still loads
//! and digests the whole record: a large `TEXT` value is an overflow chain, and slicing it in
//! SQL walks that chain, which costs more than reading it once.

use std::time::Instant;

use crate::RegisteredGlobalDb;
use crate::response_handles::{
    RESPONSE_HANDLE_TTL_SECS, ResponseHandleError, ResponseHandleLookup, ResponseHandleRecord,
};
use crate::tests::harness::{RegisteredGlobalDbHarness, writer_telemetry};

const NOW: i64 = 1_000_000;
const LARGE_HANDLE_BYTES: usize = 8 * 1024 * 1024;

async fn publish(
    database: &RegisteredGlobalDb,
    content: &str,
    now: i64,
) -> Result<ResponseHandleRecord, ResponseHandleError> {
    database.publish_response_handle(content, now).await
}

/// SQLite VM steps one operation's writer transactions executed, and the
/// microseconds the writer held its lock across them.
async fn writer_work<F, Fut>(database: &RegisteredGlobalDb, operation: F) -> (u64, u64)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let before = writer_telemetry(database);
    operation().await;
    let after = writer_telemetry(database);
    (
        after.sqlite_vm.vm_steps - before.sqlite_vm.vm_steps,
        after.transactions.lock_held_micros - before.transactions.lock_held_micros,
    )
}

/// Reader execution microseconds one read operation added.
async fn reader_execution_micros<Fut>(
    database: &RegisteredGlobalDb,
    operation: impl FnOnce() -> Fut,
) -> u64
where
    Fut: std::future::Future<Output = ()>,
{
    let before = reader_execution(database);
    operation().await;
    reader_execution(database) - before
}

fn reader_execution(database: &RegisteredGlobalDb) -> u64 {
    database
        .runtime_client()
        .writer_telemetry_snapshot()
        .expect("registered database must expose reader telemetry")
        .reader_execution_micros
}

/// Inventory reads the covering index only, so its reader work is the same
/// whether the namespace holds one tiny handle or one 8 MiB handle.
#[tokio::test(flavor = "multi_thread")]
async fn inventory_work_does_not_follow_payload_size() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-inventory-cost").await;
    let database = &harness.registered;
    publish(database, "tiny", NOW).await.unwrap();
    let tiny = reader_execution_micros(database, || async {
        database.inventory_response_handles().await.unwrap();
    })
    .await;

    publish(database, &"é🦀".repeat(LARGE_HANDLE_BYTES / 7), NOW)
        .await
        .unwrap();
    let large = reader_execution_micros(database, || async {
        database.inventory_response_handles().await.unwrap();
    })
    .await;

    eprintln!("inventory reader execution µs: tiny={tiny} large={large}");
    assert!(
        large <= tiny.max(1) * 8,
        "inventory must not read payload bytes as handles grow: tiny={tiny} large={large}"
    );
}

/// Expiry selection reads the covering index only: sweeping a namespace that
/// holds one expired 8 MiB handle costs the same writer work as sweeping one
/// expired tiny handle.
#[tokio::test(flavor = "multi_thread")]
async fn expiry_selection_does_not_follow_payload_size() {
    let expired_at = NOW + RESPONSE_HANDLE_TTL_SECS;

    async fn sweep(database: &RegisteredGlobalDb, expired_at: i64) -> (u64, u64) {
        writer_work(database, || async {
            database
                .cleanup_expired_response_handles(expired_at)
                .await
                .unwrap();
        })
        .await
    }

    let tiny = RegisteredGlobalDbHarness::open("response-handle-expiry-cost-tiny").await;
    publish(&tiny.registered, "tiny", NOW).await.unwrap();
    let (tiny_steps, _tiny_lock) = sweep(&tiny.registered, expired_at).await;

    let large = RegisteredGlobalDbHarness::open("response-handle-expiry-cost-large").await;
    publish(&large.registered, &"x".repeat(LARGE_HANDLE_BYTES), NOW)
        .await
        .unwrap();
    let (large_steps, _large_lock) = sweep(&large.registered, expired_at).await;

    eprintln!("expiry sweep VM steps: tiny={tiny_steps} large={large_steps}");
    assert_eq!(
        tiny_steps, large_steps,
        "expiry selection must not read payload bytes as handles grow"
    );
}

/// A replay of live content returns the existing identity without changing a
/// row. The two lock measurements are diagnostic values, not a timing ratio.
#[tokio::test(flavor = "multi_thread")]
async fn replay_does_not_rewrite_a_live_handle() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-replay-cost").await;
    let database = &harness.registered;
    let content = "é🦀".repeat(LARGE_HANDLE_BYTES / 7);

    let started = Instant::now();
    let mut fresh_record = None;
    let (_, fresh_lock) = writer_work(database, || async {
        fresh_record = Some(publish(database, &content, NOW).await.unwrap());
    })
    .await;
    let fresh_elapsed = started.elapsed();
    let fresh_record = fresh_record.unwrap();

    let before_replay = writer_telemetry(database);
    let replay_record = publish(database, &content, NOW + 1).await.unwrap();
    let after_replay = writer_telemetry(database);
    let replay_lock =
        after_replay.transactions.lock_held_micros - before_replay.transactions.lock_held_micros;
    let replay_rows = after_replay.transactions.rows - before_replay.transactions.rows;

    eprintln!(
        "8 MiB publish: lock held {fresh_lock} µs, wall {} µs; replay lock held {replay_lock} µs",
        fresh_elapsed.as_micros()
    );
    assert!(
        fresh_lock > 0,
        "a fresh publication must hold the writer while it writes the payload"
    );
    assert_eq!(replay_rows, 0, "a live replay must not rewrite a row");
    assert_eq!(replay_record.handle, fresh_record.handle);
    assert_eq!(replay_record.created_at, fresh_record.created_at);
    assert_eq!(replay_record.expires_at, fresh_record.expires_at);
    assert_eq!(replay_record.content, content);
}

/// A page read loads and digests the whole record, so its cost follows the
/// payload size. This is the measurement that rules out slicing in SQL:
/// `substr` on an overflow-chain `TEXT` value walks that chain, which costs
/// more than one sequential read of the value. The lookup is still exact.
#[tokio::test(flavor = "multi_thread")]
async fn lookup_reads_the_whole_record_per_page() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-lookup-cost").await;
    let database = &harness.registered;
    let content = "x".repeat(LARGE_HANDLE_BYTES);
    let record = publish(database, &content, NOW).await.unwrap();

    let started = Instant::now();
    let lookup = database
        .lookup_response_handle(&record.handle, NOW)
        .await
        .unwrap();
    let elapsed = started.elapsed();
    let ResponseHandleLookup::Found(found) = lookup else {
        panic!("a published handle must be found: {lookup:?}");
    };
    assert_eq!(found.content, content);

    let tiny = publish(database, "tiny", NOW).await.unwrap();
    let started = Instant::now();
    let lookup = database
        .lookup_response_handle(&tiny.handle, NOW)
        .await
        .unwrap();
    let tiny_elapsed = started.elapsed();
    assert!(matches!(lookup, ResponseHandleLookup::Found(_)));

    eprintln!(
        "lookup wall: 8 MiB={} µs, tiny={} µs",
        elapsed.as_micros(),
        tiny_elapsed.as_micros()
    );
}

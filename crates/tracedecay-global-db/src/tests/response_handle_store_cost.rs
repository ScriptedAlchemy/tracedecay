//! Response-handle payload I/O and writer lock tenure on large handles.
//!
//! Inventory and expiry selection are answered from the covering index, so
//! their result does not require materializing valid payload text. A
//! replay of live content writes no rows. Its writer lock tenure is measured
//! independently of the fresh publication's duration. A page read still loads
//! and digests the whole record. Timings below are measurements, not test
//! thresholds: scheduler delays are not evidence of payload I/O.

use std::time::Instant;

use tracedecay_runtime_core::db::engine::params;

use crate::RegisteredGlobalDb;
use crate::response_handles::{
    RESPONSE_HANDLE_TTL_SECS, ResponseHandleError, ResponseHandleInventory, ResponseHandleLookup,
    ResponseHandleRecord,
};
use crate::tests::harness::{RegisteredGlobalDbHarness, writer_telemetry};

const NOW: i64 = 1_000_000;
const LARGE_HANDLE_BYTES: usize = 8 * 1024 * 1024;

fn large_unicode_content() -> String {
    const TOKEN: &str = "é🦀";
    let mut content = TOKEN.repeat(LARGE_HANDLE_BYTES / TOKEN.len());
    content.push_str(&"x".repeat(LARGE_HANDLE_BYTES % TOKEN.len()));
    assert_eq!(content.len(), LARGE_HANDLE_BYTES);
    content
}

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

/// Inventory returns exact metadata for large handles without materializing
/// their text. Invalid UTF-8 is a deterministic tripwire: a content read
/// fails even when the elapsed time would happen to fit a timing ratio.
#[tokio::test(flavor = "multi_thread")]
async fn inventory_measures_large_handles_without_materializing_content() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-inventory-cost").await;
    let database = &harness.registered;
    publish(database, "tiny", NOW).await.unwrap();
    let tiny = reader_execution_micros(database, || async {
        assert_eq!(
            database.inventory_response_handles().await.unwrap(),
            ResponseHandleInventory {
                record_count: 1,
                total_bytes: 4,
                oldest_expires_at: Some(NOW + RESPONSE_HANDLE_TTL_SECS),
                newest_expires_at: Some(NOW + RESPONSE_HANDLE_TTL_SECS),
            }
        );
    })
    .await;

    let record = publish(database, &large_unicode_content(), NOW + 1)
        .await
        .unwrap();
    let expected = ResponseHandleInventory {
        record_count: 2,
        total_bytes: 4 + LARGE_HANDLE_BYTES as u64,
        oldest_expires_at: Some(NOW + RESPONSE_HANDLE_TTL_SECS),
        newest_expires_at: Some(NOW + 1 + RESPONSE_HANDLE_TTL_SECS),
    };
    let large = reader_execution_micros(database, || async {
        assert_eq!(
            database.inventory_response_handles().await.unwrap(),
            expected
        );
    })
    .await;

    eprintln!("inventory reader execution µs: tiny={tiny} large={large}");
    let transaction = database.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "UPDATE response_handles SET content = CAST(x'80' AS TEXT) WHERE handle = ?1",
            params![record.handle.as_str()],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert!(
        matches!(
            database
                .lookup_response_handle(&record.handle, NOW + 1)
                .await,
            Err(ResponseHandleError::Store(_))
        ),
        "the payload tripwire must reject a content read"
    );
    assert_eq!(
        database.inventory_response_handles().await.unwrap(),
        expected,
        "inventory must use metadata even when the content cannot be decoded"
    );
}

/// Removing one expired handle uses the same number of SQLite VM steps for
/// tiny and 8 MiB payloads. VM steps measure SQL execution, not disk I/O.
#[tokio::test(flavor = "multi_thread")]
async fn expiry_selection_does_not_follow_payload_size() {
    let expired_at = NOW + RESPONSE_HANDLE_TTL_SECS;

    async fn sweep(database: &RegisteredGlobalDb, expired_at: i64) -> (u64, u64) {
        writer_work(database, || async {
            assert_eq!(
                database
                    .cleanup_expired_response_handles(expired_at)
                    .await
                    .unwrap(),
                1,
                "the measured sweep must remove its expired handle"
            );
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
        "the expired-row deletion must keep the same VM work as handles grow"
    );
}

/// A replay of live content returns the existing identity without changing a
/// row. The two lock measurements are diagnostic values, not a timing ratio.
#[tokio::test(flavor = "multi_thread")]
async fn replay_does_not_rewrite_a_live_handle() {
    let harness = RegisteredGlobalDbHarness::open("response-handle-replay-cost").await;
    let database = &harness.registered;
    let content = large_unicode_content();

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

/// A page read loads and digests the whole record. Compare observed lookup
/// times without asserting a scheduler-dependent ratio. This measures the
/// current lookup path; it does not compare alternative SQL slicing paths.
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

use crate::RegisteredGlobalDb;
use crate::registered_maintenance::{REGISTERED_WAL_RECLAIM_TRIGGER_BYTES, RegisteredWalReclaimV1};
use crate::tests::harness::RegisteredGlobalDbHarness;

/// One mebibyte of incompressible payload per insert; each row spans ~257
/// database pages, so every insert appends about 1 MiB of WAL frames.
const WAL_LOAD_STATEMENT: &str = "INSERT INTO wal_load(value) VALUES (randomblob(1048576))";

/// The retained writer's passive checkpoint lane engages at its 32 MiB soft
/// budget; synthetic load must exceed it for a pinned reader to surface as a
/// typed pending failure instead of a below-budget no-op.
const PRESSURED_WAL_LOAD_BATCHES: usize = 40;

fn wal_file_bytes(database: &RegisteredGlobalDb) -> u64 {
    let mut wal = database.db_path().as_os_str().to_owned();
    wal.push("-wal");
    std::fs::metadata(std::path::PathBuf::from(wal))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// Creates the load table, pins a read snapshot on the empty WAL position,
/// then appends enough incompressible frames to exceed the writer's soft
/// checkpoint budget. Every frame stays unbackfillable while the returned
/// snapshot lives.
async fn grow_pressured_wal(
    database: &RegisteredGlobalDb,
) -> tracedecay_runtime_core::db::DatabaseEngineReadSnapshot {
    database
        .writer_connection()
        .unwrap()
        .execute_batch("CREATE TABLE wal_load(value BLOB NOT NULL)")
        .await
        .unwrap();

    let reader = database.read_snapshot().await.unwrap();
    let mut rows = reader
        .query("SELECT COUNT(*) FROM wal_load", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);

    let writer = database.writer_connection().unwrap();
    for _ in 0..PRESSURED_WAL_LOAD_BATCHES {
        writer.execute(WAL_LOAD_STATEMENT, ()).await.unwrap();
    }
    reader
}

#[tokio::test]
async fn pressured_checkpoint_reports_pinned_reader_and_reclaims_after_release() {
    let harness = RegisteredGlobalDbHarness::open("pressured-wal-checkpoint").await;
    let reader = grow_pressured_wal(&harness.registered).await;
    let high_water = wal_file_bytes(&harness.registered);
    assert!(
        high_water > REGISTERED_WAL_RECLAIM_TRIGGER_BYTES,
        "synthetic load must exceed the reclaim trigger, measured {high_water} bytes"
    );

    // A pinned WAL under size pressure is a typed failure, not a vacuous
    // success: the runtime checkpoint lane reports it cannot complete.
    let error = harness
        .registered
        .checkpoint_result()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("pending"), "{error}");

    drop(reader);
    let receipt = harness.registered.checkpoint_result().await.unwrap();
    assert!(
        receipt.wal_bytes_before >= high_water,
        "receipt must report the measured high-water WAL file, got {} for {high_water}",
        receipt.wal_bytes_before
    );
    // The harness client holds fixture (non-maintenance) write authority, so
    // the drained file keeps its high-water size and the receipt says exactly
    // why it was not reclaimed.
    assert_eq!(
        receipt.reclaim,
        RegisteredWalReclaimV1::RequiresExclusiveMaintenance {
            trigger_bytes: REGISTERED_WAL_RECLAIM_TRIGGER_BYTES
        }
    );
    assert_eq!(receipt.wal_bytes_after, receipt.wal_bytes_before);

    // Drain evidence: the checkpoint backfilled every frame, so the next
    // write rewinds into the existing file instead of appending past the
    // high-water mark.
    harness
        .registered
        .writer_connection()
        .unwrap()
        .execute("INSERT INTO wal_load(value) VALUES (x'01')", ())
        .await
        .unwrap();
    let after_write = wal_file_bytes(&harness.registered);
    assert!(
        after_write <= receipt.wal_bytes_after,
        "post-checkpoint write must reuse the drained WAL, grew {} past {}",
        after_write,
        receipt.wal_bytes_after
    );
}

/// The full Plan 38 reclaim journey: a PASSIVE-busy WAL past the trigger
/// stays a typed failure while pinned, and once the reader releases, the
/// exclusive-maintenance truncation lane drains it to a zero-byte file.
#[tokio::test]
async fn maintenance_truncate_drains_passive_busy_wal_to_zero_bytes() {
    crate::register_test_schema_installer();
    let directory = tempfile::tempdir().unwrap();
    let profile_root = directory.path().join("profile");
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile_root).unwrap();
    let lease = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "wal reclaim maintenance test",
    )
    .unwrap();
    let _scope = tracedecay_runtime_core::db::enter_maintenance_database_scope(
        &lease,
        &profile_root,
        "wal reclaim maintenance test",
    )
    .unwrap();
    let db_path = profile_root.join("projects/wal-reclaim/store.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let authority = tracedecay_runtime_core::db::DatabaseAuthority::for_runtime(
        &db_path,
        "wal reclaim maintenance test",
    )
    .unwrap();
    assert_eq!(
        authority.role(),
        tracedecay_runtime_core::db::DatabaseAuthorityRole::Maintenance,
        "an entered maintenance scope must grant exclusive maintenance authority"
    );
    let database = tracedecay_runtime_core::db::Database::publish_maintenance_test_runtime(
        &db_path,
        &authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap()
    .0;
    let registered = RegisteredGlobalDb::from_database_for_wal_maintenance_test(database);

    let reader = grow_pressured_wal(&registered).await;
    let high_water = wal_file_bytes(&registered);
    assert!(
        high_water > REGISTERED_WAL_RECLAIM_TRIGGER_BYTES,
        "synthetic load must exceed the reclaim trigger, measured {high_water} bytes"
    );

    // Maintenance authority does not bypass reader safety: the pinned WAL is
    // still a typed pending failure, never a forced truncation.
    let error = registered
        .checkpoint_result()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("pending"), "{error}");
    assert_eq!(wal_file_bytes(&registered), high_water);

    drop(reader);
    let receipt = registered.checkpoint_result().await.unwrap();
    assert!(
        receipt.wal_bytes_before >= high_water,
        "receipt must report the measured high-water WAL file, got {} for {high_water}",
        receipt.wal_bytes_before
    );
    assert_eq!(
        receipt.reclaim,
        RegisteredWalReclaimV1::Truncated {
            trigger_bytes: REGISTERED_WAL_RECLAIM_TRIGGER_BYTES
        }
    );
    assert_eq!(
        receipt.wal_bytes_after, 0,
        "truncation must reclaim the WAL file"
    );
    assert_eq!(wal_file_bytes(&registered), 0);

    // The truncated store keeps serving reads and writes.
    let writer = registered.writer_connection().unwrap();
    writer
        .execute("INSERT INTO wal_load(value) VALUES (x'01')", ())
        .await
        .unwrap();
    let mut rows = registered
        .read_connection()
        .query("SELECT COUNT(*) FROM wal_load", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        (PRESSURED_WAL_LOAD_BATCHES + 1) as i64
    );
}

#[tokio::test]
async fn below_trigger_checkpoint_reports_measured_wal_bytes() {
    let harness = RegisteredGlobalDbHarness::open("below-trigger-checkpoint").await;
    harness
        .registered
        .writer_connection()
        .unwrap()
        .execute_batch(
            "CREATE TABLE checkpoint_probe(value INTEGER NOT NULL);
             INSERT INTO checkpoint_probe(value) VALUES (1);",
        )
        .await
        .unwrap();

    let receipt = harness.registered.checkpoint_result().await.unwrap();
    assert!(
        receipt.wal_bytes_before > 0,
        "the seeded store must have live WAL frames"
    );
    assert!(receipt.wal_bytes_before < REGISTERED_WAL_RECLAIM_TRIGGER_BYTES);
    assert_eq!(
        receipt.reclaim,
        RegisteredWalReclaimV1::BelowTrigger {
            trigger_bytes: REGISTERED_WAL_RECLAIM_TRIGGER_BYTES
        }
    );
    assert_eq!(receipt.wal_bytes_after, receipt.wal_bytes_before);
}

#[tokio::test]
async fn public_checkpoint_remains_best_effort_when_reader_is_busy() {
    let harness = RegisteredGlobalDbHarness::open("best-effort-checkpoint").await;
    let reader = grow_pressured_wal(&harness.registered).await;

    // The best-effort entry point must swallow the pinned-reader failure so
    // shutdown paths never abort on a busy WAL.
    harness.registered.checkpoint().await;

    drop(reader);
    harness.registered.checkpoint_result().await.unwrap();
}

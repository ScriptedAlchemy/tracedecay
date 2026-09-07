use std::time::Duration;

use tracedecay_rusqlite_runtime::{
    WriterState, reader::ReaderPool, runtime::SqliteDoctorHealthLane,
};
use tracedecay_store::AdmissionConfigV1;

use crate::runtime_test_support::{
    CountExecutor, Probe, TestDatabase, maintenance_binding, read_request, reader_locator,
};

#[test]
fn checkpoint_health_exposes_wal_pressure_while_a_snapshot_blocks_progress() {
    let binding = maintenance_binding();
    let database = TestDatabase::new("runtime-checkpoint.sqlite3");
    let mut writer = database.connect();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE acceptance_rows(value INTEGER NOT NULL);
             INSERT INTO acceptance_rows(value) VALUES (1);",
        )
        .expect("seed checkpoint authority");
    let pool = ReaderPool::start(
        reader_locator(&binding, &database.path),
        AdmissionConfigV1::default().readers,
        CountExecutor,
    )
    .expect("start reader pool");
    let request = read_request(&binding, "foreground");
    let probe = Probe::for_read(&request);
    let mut reader = pool
        .acquire(&request, &probe, Duration::ZERO)
        .expect("acquire snapshot blocker");
    let mut snapshot = reader.begin_snapshot().expect("begin pinned snapshot");
    snapshot
        .execute(request, &probe)
        .expect("establish snapshot");

    let transaction = writer.transaction().expect("begin WAL pressure write");
    for value in 0..4096 {
        transaction
            .execute("INSERT INTO acceptance_rows(value) VALUES (?1)", [value])
            .expect("extend WAL under pinned snapshot");
    }
    transaction.commit().expect("commit WAL pressure write");

    let health = SqliteDoctorHealthLane::from_health_connection(binding, database.connect())
        .inspect(WriterState::Ready, pool.snapshot(), false)
        .expect("inspect real WAL and reader blocker health");
    assert!(health.wal.enabled);
    assert!(health.wal.log_frames > health.wal.checkpointed_frames);
    assert_eq!(health.leased_readers, 1);
    assert_eq!(health.available_health_readers, 1);
}

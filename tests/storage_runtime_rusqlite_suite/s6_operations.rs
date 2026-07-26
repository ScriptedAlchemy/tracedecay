use std::time::Duration;

use tracedecay_rusqlite_runtime::{
    WriterState, reader::ReaderPool, runtime::SqliteDoctorHealthLane,
};
use tracedecay_store::{AdmissionConfigV1, StoreRuntimeBindingV1};

use crate::cutover_support::{
    CountExecutor, Probe, TestDatabase, fixture, read_request, reader_locator,
};

#[test]
fn checkpoint_health_exposes_wal_pressure_while_a_snapshot_blocks_progress() {
    let telemetry = fixture().s6.maintenance_telemetry;
    let binding = StoreRuntimeBindingV1::new(
        telemetry.shard_id,
        telemetry.incarnation,
        telemetry.authority_epoch,
    );
    let database = TestDatabase::new("s6-checkpoint.sqlite3");
    let mut writer = database.connect();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE acceptance_rows(value INTEGER NOT NULL);
             INSERT INTO acceptance_rows(value) VALUES (1);",
        )
        .expect("seed S6 checkpoint authority");
    let pool = ReaderPool::start(
        reader_locator(&binding, &database.path),
        AdmissionConfigV1::default().readers,
        CountExecutor,
    )
    .expect("start S6 reader pool");
    let request = read_request(&binding, "foreground");
    let probe = Probe::for_read(&request);
    let mut reader = pool
        .acquire(&request, &probe, Duration::ZERO)
        .expect("acquire S6 snapshot blocker");
    let mut snapshot = reader.begin_snapshot().expect("begin S6 pinned snapshot");
    snapshot
        .execute(request, &probe)
        .expect("establish S6 snapshot");

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

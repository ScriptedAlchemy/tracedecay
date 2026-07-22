use std::fs::{self, File};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::Connection;
use tracedecay_rusqlite_runtime::{
    WriterState,
    backup::{Cancellation, SqliteBackupFilesystem, SqliteBackupOptions, backup_sqlite},
    reader::ReaderPool,
    repair::{CorruptionClass, RepairCoordinator, SqliteCorruptionProbe},
    runtime::SqliteDoctorHealthLane,
};
use tracedecay_store::{AdmissionConfigV1, StoreOperationIdV1, StoreRuntimeBindingV1};

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

#[test]
fn sqlite_backup_restore_and_repair_probe_use_bounded_public_capabilities() {
    let source = TestDatabase::new("s6-source.sqlite3");
    let connection = source.connect();
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE acceptance_rows(value TEXT NOT NULL);
             INSERT INTO acceptance_rows(value) VALUES ('durable-cutover');",
        )
        .expect("seed S6 source authority");

    let backup_root = tempfile::tempdir().expect("create S6 backup staging root");
    let mut backup_filesystem = PrivateDestination::new(backup_root.path().join("backup.sqlite3"));
    let mut progress = Vec::new();
    let backup_path = backup_sqlite(
        &connection,
        &mut backup_filesystem,
        SqliteBackupOptions::default(),
        &NeverCancelled,
        |step| progress.push(step),
    )
    .expect("complete bounded online backup");
    assert!(!progress.is_empty());

    let backup = Connection::open(&backup_path).expect("open completed private backup");
    assert_eq!(
        backup
            .query_row("SELECT value FROM acceptance_rows", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("read completed backup"),
        "durable-cutover"
    );

    let restore_root = tempfile::tempdir().expect("create S6 restore staging root");
    let mut restore_filesystem =
        PrivateDestination::new(restore_root.path().join("restored.sqlite3"));
    let restored_path = backup_sqlite(
        &backup,
        &mut restore_filesystem,
        SqliteBackupOptions::default(),
        &NeverCancelled,
        |_| {},
    )
    .expect("restore through bounded SQLite copy");
    let restored = Connection::open(&restored_path).expect("open restored authority");
    assert_eq!(
        restored
            .query_row("SELECT value FROM acceptance_rows", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("read restored authority"),
        "durable-cutover"
    );

    let binding = fixture().s5.binding;
    let diagnosis = RepairCoordinator::new(
        StoreOperationIdV1::new("repair.cutover.acceptance")
            .expect("valid repair receipt identity"),
    )
    .diagnose(&SqliteCorruptionProbe::new(
        &restored,
        binding,
        StoreOperationIdV1::new("evidence.cutover.acceptance")
            .expect("valid corruption evidence identity"),
        &[],
    ))
    .expect("read-only corruption diagnosis");
    assert_eq!(diagnosis.class, CorruptionClass::Healthy);
}

struct NeverCancelled;

impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct PrivateDestination {
    path: PathBuf,
}

impl PrivateDestination {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl SqliteBackupFilesystem for PrivateDestination {
    type Destination = PathBuf;
    type Completed = PathBuf;
    type Error = io::Error;

    fn create_new_private_destination(
        &mut self,
    ) -> Result<(Self::Destination, Connection), Self::Error> {
        if self.path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "private destination already exists",
            ));
        }
        File::create_new(&self.path)?;
        let connection = Connection::open(&self.path).map_err(io::Error::other)?;
        Ok((self.path.clone(), connection))
    }

    fn close_and_sync_destination(
        &mut self,
        destination: Self::Destination,
        connection: Connection,
    ) -> Result<Self::Completed, Self::Error> {
        connection
            .close()
            .map_err(|(_, error)| io::Error::other(error))?;
        File::open(&destination)?.sync_all()?;
        Ok(destination)
    }

    fn abandon_destination(&mut self, destination: Self::Destination, connection: Connection) {
        drop(connection);
        let _ = fs::remove_file(destination);
    }
}

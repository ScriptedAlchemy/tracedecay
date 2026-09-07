use std::fs;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use rusqlite::backup::StepResult;
use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

use super::{
    SnapshotReadControl, backup_live_sqlite_database, backup_live_sqlite_database_with,
    family_state, first_backup_step, open, open_foreign_in, with_suffix,
};
use crate::db::sqlite_generation_identity;

fn wal_writer(path: &std::path::Path) -> Connection {
    let writer = Connection::open(path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE durable(id INTEGER PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO durable(id, value) VALUES (0, 'checkpointed');
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO durable(id, value) VALUES (1, 'wal-resident');",
        )
        .unwrap();
    assert!(with_suffix(path, "-wal").metadata().unwrap().len() > 0);
    writer
}

fn integrity_ok(path: &std::path::Path) -> String {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap()
}

fn seed_existing_destination(path: &std::path::Path, marker: &str) -> (u64, Vec<u8>) {
    Connection::open(path)
        .unwrap()
        .execute_batch(&format!(
            "CREATE TABLE durable(id INTEGER PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO durable(id, value) VALUES (99, '{marker}');"
        ))
        .unwrap();
    (
        sqlite_generation_identity(path).unwrap(),
        fs::read(path).unwrap(),
    )
}

fn assert_destination_survived(path: &std::path::Path, identity: u64, bytes: &[u8]) {
    assert_eq!(
        fs::read(path).expect("existing destination must remain"),
        bytes,
        "failed backup must not rewrite destination bytes"
    );
    assert_eq!(
        sqlite_generation_identity(path).unwrap(),
        identity,
        "failed backup must not replace the destination file identity"
    );
    assert_no_attempt_scratch(path);
}

fn assert_no_attempt_scratch(destination: &std::path::Path) {
    let Some(parent) = destination.parent() else {
        return;
    };
    let stem = destination.file_name().unwrap().to_string_lossy();
    for entry in fs::read_dir(parent).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        assert!(
            !(name.starts_with(&*stem) && name.contains(".backup-partial")),
            "attempt-owned scratch leaked: {name}"
        );
    }
}

fn snapshot_ids(path: &std::path::Path) -> Vec<i64> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut statement = connection
        .prepare("SELECT id FROM durable ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|id| id.unwrap())
        .collect()
}

#[tokio::test]
async fn live_backup_includes_wal_resident_rows_and_does_not_checkpoint_the_source() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    let destination = temp.path().join("snapshot.db");
    let writer = wal_writer(&source);
    let before = family_state(&source).unwrap();

    backup_live_sqlite_database(&source, &destination)
        .await
        .unwrap();

    assert_eq!(integrity_ok(&destination), "ok");
    assert_eq!(snapshot_ids(&destination), [0, 1]);
    assert!(
        ["-wal", "-shm"]
            .into_iter()
            .all(|suffix| !with_suffix(&destination, suffix).exists()),
        "backup must publish one standalone file"
    );
    assert_eq!(family_state(&source).unwrap(), before);
    assert!(
        with_suffix(&source, "-wal").metadata().unwrap().len() > 0,
        "read-only backup must not fold the live source WAL"
    );
    drop(writer);
}

#[tokio::test]
async fn live_backup_of_a_concurrent_wal_writer_is_a_contiguous_committed_prefix() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    let destination = temp.path().join("snapshot.db");
    let seed = Connection::open(&source).unwrap();
    seed.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE durable(id INTEGER PRIMARY KEY, value TEXT NOT NULL);",
    )
    .unwrap();
    drop(seed);

    let stop = Arc::new(AtomicBool::new(false));
    let committed = Arc::new(AtomicI64::new(0));
    let writer_source = source.clone();
    let writer_stop = Arc::clone(&stop);
    let writer_committed = Arc::clone(&committed);
    let writer = thread::spawn(move || {
        let connection = Connection::open(&writer_source).unwrap();
        for id in 1..=4_000 {
            if writer_stop.load(Ordering::Relaxed) {
                break;
            }
            connection
                .execute(
                    "INSERT INTO durable(id, value) VALUES (?1, ?2)",
                    rusqlite::params![id, format!("row-{id}")],
                )
                .unwrap();
            writer_committed.store(id, Ordering::Release);
        }
    });

    while committed.load(Ordering::Acquire) < 32 {
        thread::sleep(Duration::from_millis(1));
    }

    backup_live_sqlite_database(&source, &destination)
        .await
        .unwrap();
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();

    assert_eq!(integrity_ok(&destination), "ok");
    let ids = snapshot_ids(&destination);
    assert!(
        !ids.is_empty(),
        "backup of a live WAL writer must capture at least the rows that existed when it started"
    );
    assert_eq!(ids[0], 1);
    assert_eq!(ids.last().copied(), Some(ids.len() as i64));
    assert!(
        ids.len() as i64 <= committed.load(Ordering::Acquire),
        "snapshot cannot invent commits the writer never acknowledged"
    );
    assert!(
        ["-wal", "-shm"]
            .into_iter()
            .all(|suffix| !with_suffix(&destination, suffix).exists())
    );
}

#[test]
fn live_backup_cancellation_retires_partial_scratch_and_never_publishes_destination() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    let destination = temp.path().join("snapshot.db");
    let writer = Connection::open(&source).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE durable(value BLOB NOT NULL);
             INSERT INTO durable(value) VALUES (zeroblob(33554432));",
        )
        .unwrap();
    let before = family_state(&source).unwrap();
    let checkpoints = AtomicUsize::new(0);
    let error = backup_live_sqlite_database_with(&source, &destination, || {
        if checkpoints.fetch_add(1, Ordering::Relaxed) >= 3 {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "SQLite read snapshot cancelled",
            ));
        }
        Ok(())
    })
    .expect_err("cooperative cancellation must interrupt page-copy work");

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(
        !destination.exists(),
        "incomplete backup must not be published onto a new destination"
    );
    assert_no_attempt_scratch(&destination);
    assert_eq!(family_state(&source).unwrap(), before);
    drop(writer);
}

#[test]
fn live_backup_preserves_existing_destination_when_source_cannot_be_opened() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("missing.db");
    let destination = temp.path().join("snapshot.db");
    let (identity, bytes) = seed_existing_destination(&destination, "keep-me");

    backup_live_sqlite_database_with(&source, &destination, || Ok(()))
        .expect_err("unreadable source must fail without touching destination");
    assert_destination_survived(&destination, identity, &bytes);
}

#[test]
fn live_backup_preserves_existing_destination_when_cancelled_before_first_step() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    let destination = temp.path().join("snapshot.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch("CREATE TABLE durable(value TEXT NOT NULL);")
        .unwrap();
    let (identity, bytes) = seed_existing_destination(&destination, "keep-me");

    let error = backup_live_sqlite_database_with(&source, &destination, || {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "SQLite read snapshot cancelled",
        ))
    })
    .expect_err("first checkpoint cancel must not touch destination");

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_destination_survived(&destination, identity, &bytes);
}

#[test]
fn live_backup_preserves_existing_destination_when_cancelled_after_copying() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    let destination = temp.path().join("snapshot.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE durable(value BLOB NOT NULL);
             INSERT INTO durable(value) VALUES (zeroblob(33554432));",
        )
        .unwrap();
    let (identity, bytes) = seed_existing_destination(&destination, "keep-me");
    let checkpoints = AtomicUsize::new(0);
    let error = backup_live_sqlite_database_with(&source, &destination, || {
        if checkpoints.fetch_add(1, Ordering::Relaxed) >= 3 {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "SQLite read snapshot cancelled",
            ));
        }
        Ok(())
    })
    .expect_err("mid-copy cancel must not replace destination");

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_destination_survived(&destination, identity, &bytes);
}

#[test]
fn live_backup_preserves_existing_destination_when_publish_fails() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    let destination = temp.path().join("snapshot.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch(
            "CREATE TABLE durable(id INTEGER PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO durable(id, value) VALUES (1, 'fresh');",
        )
        .unwrap();
    let (identity, bytes) = seed_existing_destination(&destination, "keep-me");
    super::fail_next_backup_publish();

    let error = backup_live_sqlite_database_with(&source, &destination, || Ok(()))
        .expect_err("publish failure must leave the prior snapshot in place");

    assert!(error.to_string().contains("publish"));
    assert_destination_survived(&destination, identity, &bytes);
}

#[test]
fn exclusive_staging_refuses_a_collision_without_deleting_it() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("owned.backup-partial");
    fs::write(&path, b"foreign-scratch").unwrap();
    let identity = sqlite_generation_identity(&path).unwrap();
    let bytes = fs::read(&path).unwrap();

    let error = super::reserve_exclusive_file(&path)
        .expect_err("create_new must refuse a pre-existing scratch name");

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(sqlite_generation_identity(&path).unwrap(), identity);
}

#[cfg(unix)]
#[tokio::test]
async fn live_backup_replaces_an_existing_destination_atomically() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    let destination = temp.path().join("snapshot.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch(
            "CREATE TABLE durable(id INTEGER PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO durable(id, value) VALUES (1, 'fresh');",
        )
        .unwrap();
    seed_existing_destination(&destination, "stale");

    backup_live_sqlite_database(&source, &destination)
        .await
        .unwrap();

    assert_eq!(integrity_ok(&destination), "ok");
    assert_eq!(snapshot_ids(&destination), [1]);
    assert_eq!(snapshot_ids_text(&destination), ["fresh"]);
}

#[cfg(not(unix))]
#[tokio::test]
async fn live_backup_refuses_to_replace_an_existing_destination() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    let destination = temp.path().join("snapshot.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch(
            "CREATE TABLE durable(id INTEGER PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO durable(id, value) VALUES (1, 'fresh');",
        )
        .unwrap();
    let (identity, bytes) = seed_existing_destination(&destination, "keep-me");

    let error = backup_live_sqlite_database(&source, &destination)
        .await
        .expect_err("Windows cannot atomically replace an existing destination");

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_destination_survived(&destination, identity, &bytes);
}

#[test]
fn live_backup_rejects_source_destination_alias() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch(
            "CREATE TABLE durable(id INTEGER PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO durable(id, value) VALUES (1, 'self');",
        )
        .unwrap();
    let identity = sqlite_generation_identity(&source).unwrap();
    let bytes = fs::read(&source).unwrap();

    let error = backup_live_sqlite_database_with(&source, &source, || Ok(()))
        .expect_err("backing up a path onto itself must be rejected");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(fs::read(&source).unwrap(), bytes);
    assert_eq!(sqlite_generation_identity(&source).unwrap(), identity);
}

#[test]
fn rollback_journal_exclusive_lock_makes_the_first_backup_step_busy() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("locked.db");
    let writer = Connection::open(&source).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE durable(value TEXT NOT NULL);
             INSERT INTO durable(value) VALUES ('held');
             BEGIN EXCLUSIVE;",
        )
        .unwrap();

    let step = first_backup_step(&source).expect("probe the exclusive reader conflict");
    assert!(
        matches!(step, StepResult::Busy | StepResult::Locked),
        "DELETE-journal EXCLUSIVE must exclude backup readers, got {step:?}"
    );
    writer.execute_batch("ROLLBACK;").unwrap();
    drop(writer);
}

#[test]
fn live_backup_deadline_interrupts_busy_locked_retries() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("locked.db");
    let destination = temp.path().join("snapshot.db");
    let writer = Connection::open(&source).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE durable(value TEXT NOT NULL);
             INSERT INTO durable(value) VALUES ('held');
             BEGIN EXCLUSIVE;",
        )
        .unwrap();
    let step = first_backup_step(&source).expect("fixture must actually contend");
    assert!(
        matches!(step, StepResult::Busy | StepResult::Locked),
        "deadline test requires a Busy/Locked observation first, got {step:?}"
    );
    let (identity, bytes) = seed_existing_destination(&destination, "keep-me");
    let control = SnapshotReadControl::new(
        std::time::Instant::now() + Duration::from_millis(50),
        || false,
    );
    let error = backup_live_sqlite_database_with(&source, &destination, || control.checkpoint())
        .expect_err("Busy/Locked retries must honour the snapshot deadline");

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_destination_survived(&destination, identity, &bytes);
    writer.execute_batch("ROLLBACK;").unwrap();
    drop(writer);
}

#[tokio::test]
async fn live_backup_of_a_checkpointed_family_does_not_require_sidecars() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("offline.db");
    let destination = temp.path().join("snapshot.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch(
            "CREATE TABLE durable(value TEXT NOT NULL);
             INSERT INTO durable(value) VALUES ('checkpointed');",
        )
        .unwrap();
    assert!(!with_suffix(&source, "-wal").exists());
    assert!(!with_suffix(&source, "-shm").exists());
    let before = family_state(&source).unwrap();

    backup_live_sqlite_database(&source, &destination)
        .await
        .unwrap();

    assert_eq!(integrity_ok(&destination), "ok");
    assert_eq!(snapshot_ids_text(&destination), ["checkpointed"]);
    assert_eq!(family_state(&source).unwrap(), before);
}

fn snapshot_ids_text(path: &std::path::Path) -> Vec<String> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut statement = connection
        .prepare("SELECT value FROM durable ORDER BY rowid")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|value| value.unwrap())
        .collect()
}

#[tokio::test]
async fn foreign_copy_snapshot_leaves_the_source_main_wal_and_shm_family_untouched() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("foreign.db");
    let writer = wal_writer(&source);
    let before = family_state(&source).unwrap();

    let snapshot = open_foreign_in(
        &source,
        &temp.path().join("scratch"),
        SnapshotReadControl::unlimited(),
    )
    .await
    .unwrap();
    let mut rows = snapshot
        .connection()
        .query("SELECT value FROM durable ORDER BY id", ())
        .await
        .unwrap();
    let mut values = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        values.push(row.get::<String>(0).unwrap());
    }

    assert_eq!(values, ["checkpointed", "wal-resident"]);
    assert_eq!(family_state(&source).unwrap(), before);
    assert!(
        with_suffix(&source, "-wal").metadata().unwrap().len() > 0,
        "foreign snapshot must not checkpoint the source"
    );
    drop(writer);
}

#[tokio::test]
async fn copied_snapshot_survives_absent_and_cleaned_writer_sidecars() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.db");
    let writer = Connection::open(&source).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE durable(value TEXT NOT NULL);
             INSERT INTO durable(value) VALUES ('present');
             PRAGMA wal_checkpoint(TRUNCATE);
             BEGIN IMMEDIATE;",
        )
        .unwrap();
    let wal = with_suffix(&source, "-wal");
    let shm = with_suffix(&source, "-shm");
    assert_eq!(fs::metadata(&wal).unwrap().len(), 0);
    assert!(shm.is_file());

    let snapshot = open(&source).await.unwrap();
    assert_ne!(snapshot.identity_path, source);

    writer.execute_batch("ROLLBACK;").unwrap();
    drop(writer);
    for sidecar in [&wal, &shm] {
        match fs::remove_file(sidecar) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("could not remove transient sidecar: {error}"),
        }
    }
    assert!(!wal.exists());
    assert!(!shm.exists());

    let mut rows = snapshot
        .connection()
        .query("SELECT value FROM durable", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "present"
    );
    assert_eq!(
        snapshot.attach_token().unwrap().verified_path().unwrap(),
        snapshot.path()
    );
}

#[tokio::test]
async fn copied_snapshot_does_not_claim_freshness_after_the_source_changes() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.db");
    let writer = wal_writer(&source);
    let snapshot = open_foreign_in(
        &source,
        &temp.path().join("scratch"),
        SnapshotReadControl::unlimited(),
    )
    .await
    .unwrap();
    snapshot.validate_source().unwrap();
    writer
        .execute("INSERT INTO durable(id, value) VALUES (2, 'later')", [])
        .unwrap();

    assert!(
        snapshot.validate_source().is_err(),
        "a successful backup is not a freshness claim after the source family changes"
    );
    assert!(snapshot.attach_token().unwrap().verified_path().is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn windows_live_wal_writer_survives_copy_mode_backup() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    let destination = temp.path().join("snapshot.db");
    let writer = wal_writer(&source);

    backup_live_sqlite_database(&source, &destination)
        .await
        .unwrap();
    writer
        .execute(
            "INSERT INTO durable(id, value) VALUES (2, 'after-backup')",
            [],
        )
        .unwrap();
    let illegal = temp.path().join("illegal-copy.db");
    let error = fs::copy(&source, &illegal).expect_err("copying a live Windows store must fail");
    assert!(
        matches!(error.raw_os_error(), Some(32 | 33)),
        "expected sharing/lock violation, got {error}"
    );
    assert_eq!(snapshot_ids(&destination), [0, 1]);
    drop(writer);
}

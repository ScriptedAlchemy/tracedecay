use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use rusqlite::backup::StepResult;
use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

use super::{
    SnapshotReadControl, backup_live_sqlite_database, backup_live_sqlite_database_sync,
    backup_live_sqlite_database_with, family_state, first_backup_step, open, open_foreign_in,
    with_suffix,
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

    writer
        .execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO durable(value) VALUES (zeroblob(1));
             ROLLBACK;",
        )
        .unwrap();
    drop(writer);

    backup_live_sqlite_database_sync(&source, &destination).unwrap();
    assert_eq!(integrity_ok(&destination), "ok");
    assert_eq!(
        Connection::open_with_flags(&destination, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM durable", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1,
        "the cancelled attempt must release its read transaction, and the source rollback must stay absent after a successful retry and reopen"
    );
}

#[test]
fn existing_destination_is_refused_before_the_source_is_opened() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("missing.db");
    let destination = temp.path().join("snapshot.db");
    let (identity, bytes) = seed_existing_destination(&destination, "keep-me");

    let error = backup_live_sqlite_database_with(&source, &destination, || Ok(()))
        .expect_err("an occupied destination must be refused");

    // A missing source would fail with NotFound once opened; AlreadyExists
    // proves the destination family was refused before any source or
    // staging work started.
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_destination_survived(&destination, identity, &bytes);
}

#[test]
fn first_checkpoint_cancel_reserves_no_staging_and_publishes_nothing() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    let destination = temp.path().join("snapshot.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch("CREATE TABLE durable(value TEXT NOT NULL);")
        .unwrap();

    let error = backup_live_sqlite_database_with(&source, &destination, || {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "SQLite read snapshot cancelled",
        ))
    })
    .expect_err("first checkpoint cancel must stop before any exclusive create");

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(!destination.exists());
    assert_no_attempt_scratch(&destination);
}

#[test]
fn forced_publish_failure_retires_staging_and_publishes_nothing() {
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
    super::before_next_publish(|| Err(io::Error::other("forced backup publish failure")));

    let error = backup_live_sqlite_database_with(&source, &destination, || Ok(()))
        .expect_err("publish failure must retire the attempt's staging");

    assert!(error.to_string().contains("publish"));
    assert!(!destination.exists());
    assert_no_attempt_scratch(&destination);
}

#[test]
fn early_source_open_error_skips_colliding_scratch_and_retires_only_owned_staging() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("missing.db");
    let destination = temp.path().join("snapshot.db");
    // Occupy the next candidate staging names with foreign scratch. The
    // reservation must step past them without opening, truncating, or
    // deleting them, and the source-open failure must retire only the name
    // this attempt exclusively created.
    let next = super::NEXT_BACKUP_STAGING.load(Ordering::Relaxed);
    let foreign: BTreeSet<PathBuf> = (next..next + 16)
        .map(|id| super::backup_staging_path(&destination, id))
        .collect();
    for path in &foreign {
        fs::write(path, b"foreign-scratch").unwrap();
    }

    let error = backup_live_sqlite_database_with(&source, &destination, || Ok(()))
        .expect_err("missing source must fail after staging is reserved");

    assert_ne!(
        error.kind(),
        io::ErrorKind::AlreadyExists,
        "colliding scratch names must be skipped, not fatal: {error}"
    );
    assert!(!destination.exists());
    for path in &foreign {
        assert_eq!(
            fs::read(path).unwrap(),
            b"foreign-scratch",
            "{} must survive untouched",
            path.display()
        );
    }
    let remaining: BTreeSet<PathBuf> = fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().contains(".backup-partial"))
        .collect();
    assert_eq!(
        remaining, foreign,
        "only this attempt's reserved staging may be retired"
    );
}

#[test]
fn retiring_owned_staging_removes_the_complete_sqlite_family() {
    let temp = TempDir::new().unwrap();
    let destination = temp.path().join("snapshot.db");
    let staging = super::reserve_attempt_staging(&destination).unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        fs::write(with_suffix(&staging, suffix), b"attempt-owned sidecar").unwrap();
    }

    super::retire_attempt_scratch(&staging).unwrap();

    assert!(
        std::iter::once(staging.clone())
            .chain(
                ["-wal", "-shm", "-journal"]
                    .into_iter()
                    .map(|suffix| with_suffix(&staging, suffix)),
            )
            .all(|member| !member.exists())
    );
}

#[cfg(unix)]
#[tokio::test]
async fn live_backup_refuses_to_replace_destination_with_wal_sidecars() {
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
    let dest_writer = wal_writer(&destination);
    let dest_wal = with_suffix(&destination, "-wal");
    let dest_shm = with_suffix(&destination, "-shm");
    let identity = sqlite_generation_identity(&destination).unwrap();
    let bytes = fs::read(&destination).unwrap();
    let wal_bytes = fs::read(&dest_wal).unwrap();

    let error = backup_live_sqlite_database(&source, &destination)
        .await
        .expect_err("replacing a destination that still has a WAL family is not coherent");

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert_eq!(sqlite_generation_identity(&destination).unwrap(), identity);
    assert_eq!(fs::read(&dest_wal).unwrap(), wal_bytes);
    assert!(dest_shm.is_file());
    assert_no_attempt_scratch(&destination);
    drop(dest_writer);
}

#[cfg(unix)]
#[tokio::test]
async fn live_backup_refuses_to_replace_destination_with_rollback_journal() {
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
    let journal = with_suffix(&destination, "-journal");
    fs::write(&journal, b"stale-hot-journal").unwrap();

    let error = backup_live_sqlite_database(&source, &destination)
        .await
        .expect_err("a leftover dest journal must block replace");

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_destination_survived(&destination, identity, &bytes);
    assert_eq!(fs::read(&journal).unwrap(), b"stale-hot-journal");
}

#[tokio::test]
async fn live_backup_of_wal_without_shm_does_not_write_the_source_directory() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("offline.db");
    let destination = temp.path().join("snapshot.db");
    let writer = wal_writer(&source);
    writer
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    drop(writer);
    let shm = with_suffix(&source, "-shm");
    fs::remove_file(&shm).expect("offline WAL family must start with a removable SHM file");
    assert!(with_suffix(&source, "-wal").metadata().unwrap().len() > 0);
    assert!(!shm.exists());
    let before = family_state(&source).unwrap();

    backup_live_sqlite_database(&source, &destination)
        .await
        .unwrap();

    assert_eq!(integrity_ok(&destination), "ok");
    assert_eq!(snapshot_ids(&destination), [0, 1]);
    assert_eq!(family_state(&source).unwrap(), before);
    assert!(!shm.exists());
}

#[test]
fn stray_destination_sidecar_without_a_main_is_refused_and_kept() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("live.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch(
            "CREATE TABLE durable(id INTEGER PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO durable(id, value) VALUES (1, 'fresh');",
        )
        .unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        let destination = temp.path().join(format!("snapshot{suffix}.db"));
        let stray = with_suffix(&destination, suffix);
        fs::write(&stray, b"foreign-family-member").unwrap();

        let error = backup_live_sqlite_database_with(&source, &destination, || Ok(()))
            .expect_err("a stray sidecar would be replayed into a fresh main");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{suffix}");
        assert!(
            error.to_string().contains(&stray.display().to_string()),
            "{error}"
        );
        assert!(!destination.exists(), "{suffix}: nothing may be published");
        assert_eq!(fs::read(&stray).unwrap(), b"foreign-family-member");
        assert_no_attempt_scratch(&destination);
    }
}

/// The race #1019 left open: the destination family is absent when it is
/// checked, then a concurrent opener creates the main and its WAL/SHM before
/// the backup publishes. The publication must fail without replacing that
/// main or removing any of its sidecars, and the writer must keep working.
#[test]
fn destination_family_created_after_the_check_is_refused_and_kept_intact() {
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
    let dest_wal = with_suffix(&destination, "-wal");
    let dest_shm = with_suffix(&destination, "-shm");
    struct ConcurrentFamily {
        writer: Connection,
        identity: u64,
        main_bytes: Vec<u8>,
        wal_bytes: Vec<u8>,
    }
    let concurrent: Rc<RefCell<Option<ConcurrentFamily>>> = Rc::new(RefCell::new(None));
    let hook_destination = destination.clone();
    let hook_slot = Rc::clone(&concurrent);
    super::before_next_publish(move || {
        assert!(
            !hook_destination.exists(),
            "the seam must run after the destination-family check found nothing"
        );
        let writer = wal_writer(&hook_destination);
        *hook_slot.borrow_mut() = Some(ConcurrentFamily {
            identity: sqlite_generation_identity(&hook_destination).unwrap(),
            main_bytes: fs::read(&hook_destination).unwrap(),
            wal_bytes: fs::read(with_suffix(&hook_destination, "-wal")).unwrap(),
            writer,
        });
        Ok(())
    });

    let error = backup_live_sqlite_database_with(&source, &destination, || Ok(()))
        .expect_err("a main that appeared after the check must never be replaced");

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    let ConcurrentFamily {
        writer,
        identity,
        main_bytes,
        wal_bytes,
    } = concurrent
        .borrow_mut()
        .take()
        .expect("the seam must have created the concurrent destination family");
    assert_eq!(sqlite_generation_identity(&destination).unwrap(), identity);
    assert_eq!(fs::read(&destination).unwrap(), main_bytes);
    assert_eq!(fs::read(&dest_wal).unwrap(), wal_bytes);
    assert!(dest_shm.is_file(), "the writer's SHM must not be removed");
    assert_no_attempt_scratch(&destination);
    writer
        .execute(
            "INSERT INTO durable(id, value) VALUES (2, 'after-refusal')",
            [],
        )
        .expect("the concurrent writer's family must still be coherent");
    drop(writer);
    assert_eq!(
        snapshot_ids_text(&destination),
        ["checkpointed", "wal-resident", "after-refusal"]
    );
}

/// After the destination name is published, a legitimate opener of the new
/// standalone file may create its own WAL/SHM. Nothing in the backup may
/// remove those: pathname existence cannot prove which main they belong to.
#[test]
fn sidecars_a_legitimate_opener_creates_after_publication_are_never_removed() {
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
    let dest_wal = with_suffix(&destination, "-wal");
    let dest_shm = with_suffix(&destination, "-shm");
    let opener: Rc<RefCell<Option<Connection>>> = Rc::new(RefCell::new(None));
    let hook_destination = destination.clone();
    let hook_slot = Rc::clone(&opener);
    super::after_next_publish(move || {
        let connection = Connection::open(&hook_destination).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 INSERT INTO durable(id, value) VALUES (2, 'post-publish');",
            )
            .unwrap();
        assert!(
            with_suffix(&hook_destination, "-wal")
                .metadata()
                .unwrap()
                .len()
                > 0
        );
        *hook_slot.borrow_mut() = Some(connection);
    });

    backup_live_sqlite_database_with(&source, &destination, || Ok(())).unwrap();

    let opener = opener
        .borrow_mut()
        .take()
        .expect("the seam must have opened the published file");
    assert!(
        dest_wal.metadata().unwrap().len() > 0,
        "the opener's WAL must survive publication"
    );
    assert!(
        dest_shm.is_file(),
        "the opener's SHM must survive publication"
    );
    assert_eq!(integrity_ok(&destination), "ok");
    assert_eq!(
        snapshot_ids_text(&destination),
        ["fresh", "post-publish"],
        "the WAL-resident row must still be readable through the intact family"
    );
    drop(opener);
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
fn live_backup_deadline_interrupts_busy_locked_retries() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("locked.db");
    let destination = temp.path().join("snapshot.db");
    let writer = Connection::open(&source).unwrap();
    writer
        .execute_batch(
            // WAL-mode EXCLUSIVE transactions still admit readers. DELETE
            // mode makes this a real source read-lock conflict so the test
            // deterministically exercises the Busy/Locked retry loop.
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
    let control = SnapshotReadControl::new(
        std::time::Instant::now() + Duration::from_millis(50),
        || false,
    );
    let error = backup_live_sqlite_database_with(&source, &destination, || control.checkpoint())
        .expect_err("Busy/Locked retries must honour the snapshot deadline");

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(!destination.exists());
    assert_no_attempt_scratch(&destination);
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

use std::{
    env,
    fs::{self, OpenOptions},
    os::{fd::AsRawFd, unix::fs::FileExt},
    path::Path,
    process::Command,
    time::Duration,
};

use rusqlite::{Connection, Error, ErrorCode, OpenFlags};
use tempfile::TempDir;

use super::{OpenedDatabaseFile, OpenedDatabaseFileError, live_path_identity};

const PROBE_PATH: &str = "TRACEDECAY_PATH_IDENTITY_PROBE_PATH";
const PROBE_OPERATION: &str = "TRACEDECAY_PATH_IDENTITY_PROBE_OPERATION";
const PROBE_EXPECTATION: &str = "TRACEDECAY_PATH_IDENTITY_PROBE_EXPECTATION";
const PROBE_COMPLETE: &str = "tracedecay-path-identity-probe-complete";

// A second connection in this process is not a lock probe: SQLite's Unix VFS
// shares inode/lock bookkeeping within a process and can hide a lost OS lock.
// Execute only the child test in a fresh process, with a marker to reject a
// successful test harness invocation that accidentally selected zero tests.
fn probe_external_connection(path: &Path, operation: &str, blocked: bool) {
    probe_external_connection_expectation(
        path,
        operation,
        if blocked { "blocked" } else { "allowed" },
    );
}

fn probe_external_connection_expectation(path: &Path, operation: &str, expectation: &str) {
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args([
            "--exact",
            &format!("{module}::external_connection_probe"),
            "--nocapture",
        ])
        .env(PROBE_PATH, path)
        .env(PROBE_OPERATION, operation)
        .env(PROBE_EXPECTATION, expectation)
        .output()
        .expect("run external SQLite connection");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(PROBE_COMPLETE),
        "external {operation} probe failed: {}\n{stdout}\n{stderr}",
        output.status,
    );
}

#[test]
fn external_connection_probe() {
    let Some(path) = env::var_os(PROBE_PATH) else {
        return;
    };
    let operation = env::var(PROBE_OPERATION).expect("probe operation");
    let expectation = env::var(PROBE_EXPECTATION).expect("probe expectation");
    let flags = if operation == "read" {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX
    };
    let connection = Connection::open_with_flags(Path::new(&path), flags)
        .expect("open existing database in another process");
    connection
        .busy_timeout(Duration::ZERO)
        .expect("no lock wait");
    let result = match operation.as_str() {
        "write" => connection.execute_batch("BEGIN IMMEDIATE; ROLLBACK"),
        "journal" => connection
            .query_row("PRAGMA journal_mode = DELETE", [], |row| {
                row.get::<_, String>(0)
            })
            .map(|mode| assert_eq!(mode, "delete")),
        "read" => connection
            .query_row("SELECT value FROM sample", [], |row| row.get::<_, i64>(0))
            .and_then(|value| {
                let expected = expectation
                    .strip_prefix("read:")
                    .expect("read expectation")
                    .parse::<i64>()
                    .expect("numeric read expectation");
                assert_eq!(
                    value, expected,
                    "external reader must see committed WAL data"
                );
                connection
                    .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                    .map(|integrity| assert_eq!(integrity, "ok"))
            }),
        _ => panic!("unknown probe operation: {operation}"),
    };
    match expectation.as_str() {
        "blocked" => assert!(
            matches!(
                &result,
                Err(Error::SqliteFailure(error, _)) if error.code == ErrorCode::DatabaseBusy
            ),
            "expected SQLITE_BUSY from external {operation}, got {result:?}",
        ),
        "allowed" => result.expect("external operation after lock release"),
        value if operation == "read" && value.starts_with("read:") => {
            result.expect("external read-only SQLite query")
        }
        _ => panic!("unknown probe expectation: {expectation}"),
    }
    println!("{PROBE_COMPLETE}");
}

fn assert_committed_data(path: &Path) {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("reopen committed database");
    let value: i64 = connection
        .query_row("SELECT value FROM sample", [], |row| row.get(0))
        .expect("committed value");
    assert_eq!(value, 2);
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity check");
    assert_eq!(integrity, "ok");
}

#[test]
fn worker_identity_checks_keep_cross_process_writer_exclusion() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("worker.db");
    let connection = Connection::open(&path).expect("open database");
    connection
        .execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE sample(value INTEGER);
             INSERT INTO sample VALUES (1);
             BEGIN IMMEDIATE;
             UPDATE sample SET value = 2;",
        )
        .expect("hold live SQLite writer lock");
    // The old worker check opened another raw descriptor and dropped it
    // immediately. On POSIX that released this transaction's advisory lock.
    probe_external_connection(&path, "write", true);
    let identity = live_path_identity(&path).expect("inspect live worker identity");
    assert_ne!(identity, 0);
    probe_external_connection(&path, "write", true);
    assert_eq!(live_path_identity(&path).unwrap(), identity);
    probe_external_connection(&path, "write", true);

    connection.execute_batch("COMMIT").expect("commit");
    probe_external_connection(&path, "write", false);
    drop(connection);
    assert_committed_data(&path);

    let absent = temp.path().join("absent.db");
    assert_eq!(
        live_path_identity(&absent),
        Err(OpenedDatabaseFileError::Open)
    );
    let directory = temp.path().join("not-a-database");
    fs::create_dir(&directory).expect("directory");
    assert_eq!(
        live_path_identity(&directory),
        Err(OpenedDatabaseFileError::NotFile)
    );
}

/// A zero-byte existing SQLite file may belong to a live connection holding
/// a rollback writer reservation. Rejecting that unfinished attachment must
/// not open and close a second raw descriptor and unlock the other writer.
#[test]
fn rejecting_empty_attachment_does_not_release_another_writer_lock() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("empty-locked.db");
    let writer = Connection::open(&path).expect("open SQLite connection");
    writer
        .execute_batch("BEGIN IMMEDIATE")
        .expect("reserve the SQLite writer lock without initializing pages");
    assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    probe_external_connection(&path, "write", true);

    assert!(matches!(
        OpenedDatabaseFile::pin_initialized(&path),
        Err(OpenedDatabaseFileError::InvalidHeader)
    ));
    probe_external_connection(&path, "write", true);

    writer
        .execute_batch("ROLLBACK")
        .expect("release writer reservation");
    probe_external_connection(&path, "write", false);
}

#[test]
fn header_magic_without_a_complete_sqlite_page_is_refused() {
    let temp = TempDir::new().expect("tempdir");
    for length in [16, 100, 511] {
        let path = temp.path().join(format!("short-{length}.db"));
        let mut bytes = vec![0_u8; length];
        bytes[..16].copy_from_slice(b"SQLite format 3\0");
        fs::write(&path, &bytes).expect("write incomplete SQLite header");
        assert!(matches!(
            OpenedDatabaseFile::pin_initialized(&path),
            Err(OpenedDatabaseFileError::InvalidHeader)
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn path_checks_preserve_rollback_writer_lock() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("rollback.db");
    // Pin before SQLite takes locks, and keep the pin until after it closes.
    // This isolates pathname verification from the separate pin-drop lifetime.
    let pinned = OpenedDatabaseFile::create_new(&path).expect("pin database");
    let connection = Connection::open(&path).expect("open database");
    connection
        .execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE sample(value INTEGER);
             INSERT INTO sample VALUES (1);
             BEGIN IMMEDIATE;
             UPDATE sample SET value = 2;",
        )
        .expect("hold write transaction");
    probe_external_connection(&path, "write", true);

    pinned.verify_current_path(&path).expect("same inode");
    probe_external_connection(&path, "write", true);
    pinned
        .verify_connection(&connection, &path)
        .expect("same SQLite connection");
    probe_external_connection(&path, "write", true);

    connection.execute_batch("COMMIT").expect("commit");
    probe_external_connection(&path, "write", false);
    drop(connection);
    drop(pinned);
    assert_committed_data(&path);
}

#[test]
fn path_checks_preserve_live_wal_journal_mode_guard() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("wal.db");
    let pinned = OpenedDatabaseFile::create_new(&path).expect("pin database");
    let connection = Connection::open(&path).expect("open database");
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE sample(value INTEGER);
             INSERT INTO sample VALUES (1);
             BEGIN IMMEDIATE;
             UPDATE sample SET value = 2;",
        )
        .expect("hold WAL write transaction");
    probe_external_connection(&path, "journal", true);

    pinned.verify_current_path(&path).expect("same inode");
    probe_external_connection(&path, "journal", true);
    pinned
        .verify_connection(&connection, &path)
        .expect("same SQLite connection");
    probe_external_connection(&path, "journal", true);

    connection.execute_batch("COMMIT").expect("commit");
    // A live WAL connection still prevents another process switching modes,
    // even between transactions. Only closing the connection releases it.
    probe_external_connection(&path, "journal", true);
    drop(connection);
    probe_external_connection(&path, "journal", false);
    drop(pinned);
    assert_committed_data(&path);
}

#[test]
fn external_read_only_wal_reader_survives_live_writer_identity_checks() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("sessions.db");
    let pinned = OpenedDatabaseFile::create_new(&path).expect("pin database");
    let connection = Connection::open(&path).expect("open database");
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE sample(value INTEGER);
             INSERT INTO sample VALUES (1);",
        )
        .expect("initialize committed WAL snapshot");

    // A separate read-only process must see the last committed snapshot,
    // even when the daemon writer holds an uncommitted WAL transaction.
    probe_external_connection_expectation(&path, "read", "read:1");
    connection
        .execute_batch("BEGIN IMMEDIATE; UPDATE sample SET value = 2;")
        .expect("hold live WAL write transaction");
    probe_external_connection_expectation(&path, "read", "read:1");

    // Path and SQLite-connection identity checks must not break either the
    // writer's lock or another process's read-only WAL access.
    pinned.verify_current_path(&path).expect("same inode");
    probe_external_connection_expectation(&path, "read", "read:1");
    pinned
        .verify_connection(&connection, &path)
        .expect("same SQLite connection");
    probe_external_connection_expectation(&path, "read", "read:1");

    connection.execute_batch("COMMIT").expect("commit");
    probe_external_connection_expectation(&path, "read", "read:2");
    drop(connection);
    drop(pinned);
    assert_committed_data(&path);
}

#[test]
fn dropping_secondary_identity_pins_never_releases_rollback_writer_locks() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("shared-pin-rollback.db");
    let pinned = OpenedDatabaseFile::create_new(&path).expect("pin database");
    let reader_pin = pinned.try_clone().expect("share reader identity");
    let writer_pin = pinned.try_clone().expect("share writer identity");
    let connection = Connection::open(&path).expect("open SQLite connection");
    connection
        .execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE sample(value INTEGER);
             INSERT INTO sample VALUES (1);
             BEGIN IMMEDIATE;
             UPDATE sample SET value = 2;",
        )
        .expect("hold writer transaction");
    probe_external_connection(&path, "write", true);

    // If these pins were independent dup(2) descriptors, dropping either one
    // would discard SQLite's unrelated process-wide lock.
    drop(reader_pin);
    probe_external_connection(&path, "write", true);
    drop(writer_pin);
    probe_external_connection(&path, "write", true);
    pinned
        .verify_connection(&connection, &path)
        .expect("shared pin still identifies the live file");
    probe_external_connection(&path, "write", true);

    connection.execute_batch("COMMIT").expect("commit");
    probe_external_connection(&path, "write", false);
    drop(connection);
    drop(pinned);
    assert_committed_data(&path);
}

#[test]
fn dropping_secondary_identity_pins_never_unlocks_live_wal_journal() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("shared-pin-wal.db");
    let pinned = OpenedDatabaseFile::create_new(&path).expect("pin database");
    let reader_pin = pinned.try_clone().expect("share reader identity");
    let writer_pin = pinned.try_clone().expect("share writer identity");
    let connection = Connection::open(&path).expect("open SQLite connection");
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE sample(value INTEGER);
             INSERT INTO sample VALUES (1);
             BEGIN IMMEDIATE;
             UPDATE sample SET value = 2;",
        )
        .expect("hold WAL write transaction");
    probe_external_connection(&path, "journal", true);

    drop(reader_pin);
    probe_external_connection(&path, "journal", true);
    drop(writer_pin);
    probe_external_connection(&path, "journal", true);
    pinned
        .verify_current_path(&path)
        .expect("original file identity");
    probe_external_connection(&path, "journal", true);

    connection.execute_batch("COMMIT").expect("commit");
    probe_external_connection(&path, "journal", true);
    drop(connection);
    drop(pinned);
    probe_external_connection(&path, "journal", false);
    assert_committed_data(&path);
}

#[test]
fn path_checks_keep_pinned_identity_and_reject_missing_or_replaced_files() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("identity.db");
    let alias = temp.path().join("same-inode.db");
    let symlink = temp.path().join("symlink.db");
    let pinned = OpenedDatabaseFile::create_new(&path).expect("pin database");
    fs::hard_link(&path, &alias).expect("hard link");
    std::os::unix::fs::symlink(&path, &symlink).expect("symlink");
    pinned.verify_current_path(&path).expect("original inode");
    pinned.verify_current_path(&alias).expect("hard-link inode");
    pinned
        .verify_current_path(&symlink)
        .expect("symlink target");

    fs::remove_file(&path).expect("unlink original path");
    assert_eq!(
        pinned.verify_current_path(&path),
        Err(OpenedDatabaseFileError::Open),
    );
    assert_eq!(
        pinned.verify_current_path(&symlink),
        Err(OpenedDatabaseFileError::Open),
    );
    fs::write(&path, b"").expect("create different inode");
    assert_eq!(
        pinned.verify_current_path(&path),
        Err(OpenedDatabaseFileError::Replaced),
    );
    assert_eq!(
        pinned.verify_current_path(&symlink),
        Err(OpenedDatabaseFileError::Replaced),
    );
    pinned.verify_current_path(&alias).expect("retained inode");

    fs::remove_file(&path).expect("remove replacement");
    fs::create_dir(&path).expect("directory at database path");
    assert_eq!(
        pinned.verify_current_path(&path),
        Err(OpenedDatabaseFileError::Replaced),
    );
}

const RESERVED_LOCK_PROBE_PATH: &str = "TRACEDECAY_SQLITE_RESERVED_LOCK_PROBE_PATH";
const RESERVED_LOCK_PROBE_EXPECTATION: &str = "TRACEDECAY_SQLITE_RESERVED_LOCK_PROBE_EXPECTATION";
const RESERVED_LOCK_PROBE_COMPLETE: &str = "tracedecay-sqlite-reserved-lock-probe-complete";

/// Inspect SQLite's POSIX RESERVED byte from a different process. This avoids
/// an SQLite schema read after we deliberately corrupt the fixture header.
#[test]
fn external_reserved_lock_probe() {
    let Some(path) = env::var_os(RESERVED_LOCK_PROBE_PATH) else {
        return;
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut reservation = libc::flock {
        l_type: libc::F_WRLCK as libc::c_short,
        l_whence: libc::SEEK_SET as libc::c_short,
        l_start: 0x4000_0001,
        l_len: 1,
        l_pid: 0,
    };
    // SAFETY: the file owns a valid descriptor and reservation is initialized
    // and writable for the F_SETLK request.
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &mut reservation) };
    match env::var(RESERVED_LOCK_PROBE_EXPECTATION).unwrap().as_str() {
        "blocked" => {
            assert_eq!(result, -1, "SQLite writer's RESERVED lock was lost");
            let error = std::io::Error::last_os_error().raw_os_error();
            assert!(
                error == Some(libc::EACCES) || error == Some(libc::EAGAIN),
                "unexpected advisory-lock refusal: {error:?}"
            );
        }
        "allowed" => assert_eq!(
            result,
            0,
            "RESERVED byte remained locked after the writer closed: {}",
            std::io::Error::last_os_error()
        ),
        other => panic!("unknown reserved lock expectation: {other}"),
    }
    println!("{RESERVED_LOCK_PROBE_COMPLETE}");
}

fn probe_reserved_lock(path: &Path, blocked: bool) {
    let (_, module) = module_path!().split_once("::").unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{module}::external_reserved_lock_probe"),
            "--nocapture",
        ])
        .env(RESERVED_LOCK_PROBE_PATH, path)
        .env(
            RESERVED_LOCK_PROBE_EXPECTATION,
            if blocked { "blocked" } else { "allowed" },
        )
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success()
            && stdout.contains("1 passed")
            && stdout.contains(RESERVED_LOCK_PROBE_COMPLETE),
        "cross-process lock probe failed: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn malformed_existing_store_refusal_keeps_other_sqlite_writer_locks() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("sessions.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE sample(value INTEGER);
             INSERT INTO sample VALUES (1);",
        )
        .unwrap();
    // Retain this descriptor through the original writer's entire lifetime:
    // closing it early would itself discard the POSIX locks under test.
    let mutation = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE sample SET value = 2;")
        .unwrap();
    probe_reserved_lock(&path, true);

    // Corrupt only a disposable fixture, in place and without closing a raw
    // descriptor. The main file remains page-aligned but is no longer SQLite.
    mutation.write_all_at(b"Not SQLite data!", 0).unwrap();
    probe_reserved_lock(&path, true);
    assert!(matches!(
        OpenedDatabaseFile::pin_initialized(&path),
        Err(OpenedDatabaseFileError::InvalidHeader)
    ));
    probe_reserved_lock(&path, true);

    drop(writer);
    drop(mutation);
    probe_reserved_lock(&path, false);
}

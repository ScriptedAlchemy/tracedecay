use std::{env, fs, path::Path, process::Command, time::Duration};

use rusqlite::{Connection, Error, ErrorCode, OpenFlags};
use tempfile::TempDir;

use super::{OpenedDatabaseFile, OpenedDatabaseFileError};

const PROBE_PATH: &str = "TRACEDECAY_PATH_IDENTITY_PROBE_PATH";
const PROBE_OPERATION: &str = "TRACEDECAY_PATH_IDENTITY_PROBE_OPERATION";
const PROBE_EXPECTATION: &str = "TRACEDECAY_PATH_IDENTITY_PROBE_EXPECTATION";
const PROBE_COMPLETE: &str = "tracedecay-path-identity-probe-complete";

// A second connection in this process is not a lock probe: SQLite's Unix VFS
// shares inode/lock bookkeeping within a process and can hide a lost OS lock.
// Execute only the child test in a fresh process, with a marker to reject a
// successful test harness invocation that accidentally selected zero tests.
fn probe_external_connection(path: &Path, operation: &str, blocked: bool) {
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args([
            "--exact",
            &format!("{module}::external_connection_probe"),
            "--nocapture",
        ])
        .env(PROBE_PATH, path)
        .env(PROBE_OPERATION, operation)
        .env(PROBE_EXPECTATION, if blocked { "blocked" } else { "allowed" })
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
    let connection = Connection::open_with_flags(
        Path::new(&path),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
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

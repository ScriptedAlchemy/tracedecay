use std::path::Path;

use rusqlite::{Connection, ErrorCode, limits::Limit};
use tempfile::NamedTempFile;

use super::{
    ConnectionMode, OpenedDatabaseFile, OpenedDatabaseFileError, open, open_immutable_reader,
    with_progress_cancellation,
};

fn database() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary database");
    let connection = Connection::open(file.path()).expect("initialize database");
    connection
        .execute_batch("CREATE TABLE items(value INTEGER); INSERT INTO items VALUES (1);")
        .expect("initialize schema");
    drop(connection);
    file
}

fn pragma_i64(connection: &Connection, name: &str) -> i64 {
    connection
        .pragma_query_value(None, name, |row| row.get(0))
        .expect("read pragma")
}

fn sidecar_path(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    sidecar.into()
}

#[test]
fn writer_mode_applies_wal_integrity_and_write_policy() {
    let file = database();
    let connection = open(file.path(), ConnectionMode::Writer).expect("writer policy");

    let journal: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("journal mode");
    assert_eq!(journal.to_ascii_lowercase(), "wal");
    assert_eq!(pragma_i64(&connection, "wal_autocheckpoint"), 0);
    assert_eq!(pragma_i64(&connection, "synchronous"), 1);
    assert_eq!(pragma_i64(&connection, "foreign_keys"), 1);
    assert_eq!(pragma_i64(&connection, "trusted_schema"), 0);
    connection
        .execute("INSERT INTO items VALUES (2)", [])
        .expect("ordinary writer DML");
    connection
        .query_row("PRAGMA wal_checkpoint(NOOP)", [], |_| Ok(()))
        .expect("writer-owned checkpoint observation remains authorized");
    assert!(
        connection
            .pragma_update(None, "cache_size", 1_000_i64)
            .is_err()
    );
    connection
        .execute_batch("CREATE TABLE initialized(value)")
        .expect("non-destructive writer initialization");
    assert!(connection.execute_batch("DROP TABLE initialized").is_err());
}

#[test]
fn reader_mode_is_private_query_only_and_denies_writes() {
    let file = database();
    let writer = open(file.path(), ConnectionMode::Writer).expect("prepare WAL database");
    drop(writer);
    let connection = open(file.path(), ConnectionMode::Reader).expect("reader policy");

    assert_eq!(pragma_i64(&connection, "query_only"), 1);
    assert_eq!(pragma_i64(&connection, "foreign_keys"), 1);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        connection
            .execute("INSERT INTO items VALUES (2)", [])
            .is_err()
    );
}

#[test]
fn immutable_reader_applies_full_reader_policy_without_sidecars() {
    let file = database();
    let wal = sidecar_path(file.path(), "-wal");
    let shm = sidecar_path(file.path(), "-shm");
    let journal = sidecar_path(file.path(), "-journal");
    let before = std::fs::read(file.path()).unwrap();

    let connection = open_immutable_reader(file.path()).expect("immutable reader policy");
    assert_eq!(pragma_i64(&connection, "query_only"), 1);
    assert_eq!(pragma_i64(&connection, "foreign_keys"), 1);
    assert_eq!(pragma_i64(&connection, "trusted_schema"), 0);
    assert_eq!(pragma_i64(&connection, "busy_timeout"), 0);
    assert_eq!(connection.limit(Limit::SQLITE_LIMIT_ATTACHED).unwrap(), 0);
    assert!(
        connection
            .execute("INSERT INTO items VALUES (2)", [])
            .is_err()
    );
    assert!(
        connection
            .execute_batch("ATTACH DATABASE ':memory:' AS other")
            .is_err()
    );
    drop(connection);

    assert_eq!(std::fs::read(file.path()).unwrap(), before);
    assert!(!wal.exists());
    assert!(!shm.exists());
    assert!(!journal.exists());
}

#[test]
fn maintenance_mode_makes_schema_exceptions_explicit() {
    let file = database();
    let connection = open(file.path(), ConnectionMode::Maintenance).expect("maintenance policy");

    connection
        .execute_batch("CREATE TABLE maintained(value); DROP TABLE maintained;")
        .expect("maintenance schema operation");
    assert!(connection.limit(Limit::SQLITE_LIMIT_ATTACHED).unwrap() > 0);
    connection
        .execute_batch("ATTACH DATABASE ':memory:' AS maintenance_aux; DETACH maintenance_aux;")
        .expect("maintenance attachment");
}

#[test]
fn limits_and_authorizer_reject_oversized_or_unsafe_sql() {
    let file = database();
    let connection = open(file.path(), ConnectionMode::Writer).expect("writer policy");

    assert_eq!(connection.limit(Limit::SQLITE_LIMIT_ATTACHED).unwrap(), 0);
    assert!(connection.limit(Limit::SQLITE_LIMIT_SQL_LENGTH).unwrap() <= 1024 * 1024);
    assert!(
        connection
            .execute_batch("ATTACH DATABASE ':memory:' AS other")
            .is_err()
    );
    let oversized = format!("SELECT 1 /*{}*/", "x".repeat(1024 * 1024));
    assert!(connection.prepare(&oversized).is_err());
}

#[test]
fn progress_cancellation_interrupts_and_is_removed_after_scope() {
    let file = database();
    let mut connection =
        open(file.path(), ConnectionMode::Maintenance).expect("maintenance policy");
    let result = with_progress_cancellation(
        &mut connection,
        || true,
        |connection| {
            connection.query_row(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT sum(x) FROM n",
                [],
                |row| row.get::<_, i64>(0),
            )
        },
    )
    .expect("progress handler setup");
    assert!(
        matches!(result, Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == ErrorCode::OperationInterrupted)
    );
    assert_eq!(
        connection
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn policy_requires_an_existing_database() {
    let directory = tempfile::tempdir().unwrap();
    let missing = Path::new(directory.path()).join("missing.db");
    assert!(
        open(&missing, ConnectionMode::Writer)
            .unwrap_err()
            .is_open_failure()
    );
}

#[test]
fn create_new_pins_and_discards_the_exact_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fresh.db");

    let created = OpenedDatabaseFile::create_new(&path).unwrap();
    assert!(path.is_file());
    assert_ne!(created.identity(), 0);
    created.discard_created(&path).unwrap();

    assert!(!path.exists());
}

#[test]
fn create_new_refuses_to_replace_an_existing_database() {
    let file = NamedTempFile::new().unwrap();

    assert_eq!(
        OpenedDatabaseFile::create_new(file.path()).unwrap_err(),
        OpenedDatabaseFileError::Create
    );
}

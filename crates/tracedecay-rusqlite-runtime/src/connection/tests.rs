use std::path::Path;

use rusqlite::{Connection, ErrorCode, config::DbConfig, limits::Limit};
use tempfile::NamedTempFile;

use super::{
    ConnectionMode, OpenedDatabaseFile, OpenedDatabaseFileError, open, open_immutable_reader,
    open_writer, with_progress_cancellation,
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
    assert!(
        connection
            .db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE)
            .unwrap()
    );
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
fn writer_close_never_bypasses_explicit_checkpoint_policy() {
    let file = database();
    let wal = sidecar_path(file.path(), "-wal");
    let connection = open(file.path(), ConnectionMode::Writer).expect("writer policy");
    connection
        .execute("INSERT INTO items VALUES (2)", [])
        .expect("write WAL frame");
    let wal_bytes = std::fs::metadata(&wal)
        .expect("WAL exists before close")
        .len();
    assert!(wal_bytes > 0);

    drop(connection);

    assert_eq!(
        std::fs::metadata(&wal)
            .expect("close must retain uncheckpointed WAL")
            .len(),
        wal_bytes
    );
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
fn reader_authorizer_allows_integrity_diagnostics_and_denies_mutating_pragmas() {
    let file = database();
    let writer = open(file.path(), ConnectionMode::Writer).expect("prepare WAL database");
    drop(writer);
    let connection = open(file.path(), ConnectionMode::Reader).expect("reader policy");

    for pragma in [
        "PRAGMA quick_check",
        "PRAGMA quick_check(1000)",
        "PRAGMA integrity_check",
        "PRAGMA integrity_check(1000)",
    ] {
        let result = connection
            .query_row(pragma, [], |row| row.get::<_, String>(0))
            .unwrap_or_else(|error| panic!("{pragma} must be authorized: {error}"));
        assert_eq!(result, "ok", "{pragma} must report a healthy database");
    }

    for pragma in [
        "PRAGMA application_id = 1",
        "PRAGMA cache_size = 1000",
        "PRAGMA journal_mode = DELETE",
        "PRAGMA user_version = 1",
    ] {
        assert!(
            connection.execute_batch(pragma).is_err(),
            "{pragma} must remain denied"
        );
    }
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
fn writer_bootstraps_fresh_incremental_auto_vacuum_before_wal() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("fresh.sqlite3");
    std::fs::File::create(&path).expect("create empty database file");
    let connection = open(&path, ConnectionMode::Writer).expect("writer policy");

    assert_eq!(
        connection
            .query_row("PRAGMA auto_vacuum", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    connection
        .execute_batch("PRAGMA auto_vacuum = INCREMENTAL")
        .expect("repeat safe incremental auto-vacuum");
    assert!(
        connection
            .execute_batch("PRAGMA auto_vacuum = NONE")
            .is_err()
    );
}

#[cfg(any(unix, windows))]
#[test]
fn fresh_writer_uses_a_sidecar_compatible_path() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("fresh-writer.sqlite3");
    let pinned = OpenedDatabaseFile::create_new(&path).expect("pin fresh database");
    let worker_path = pinned.writer_open_path(&path).expect("select writer path");
    let connection = open_writer(&worker_path, Some(&pinned), &path).expect("writer policy");

    connection
        .execute_batch(
            "CREATE TABLE sidecar_probe(value INTEGER);
             INSERT INTO sidecar_probe VALUES (1);",
        )
        .expect("fresh writer schema and WAL write");

    assert!(
        sidecar_path(&path, "-wal").is_file(),
        "fresh writer must create WAL beside the canonical database"
    );
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

    assert!(matches!(
        OpenedDatabaseFile::create_new(file.path()),
        Err(OpenedDatabaseFileError::Create)
    ));
}

#[cfg(unix)]
#[test]
fn worker_open_path_stays_on_the_pinned_file_across_an_a_b_a_swap() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.db");
    let retired = directory.path().join("identity.retired.db");
    let replacement = directory.path().join("identity.replacement.db");
    std::fs::write(&path, b"original").unwrap();
    std::fs::write(&replacement, b"replacement").unwrap();
    let pinned = OpenedDatabaseFile::pin(&path).unwrap();
    let retained = pinned.try_clone().unwrap();
    let worker_path = retained.worker_open_path(&path).unwrap();

    std::fs::rename(&path, &retired).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(std::fs::read(&worker_path).unwrap(), b"original");

    std::fs::rename(&path, &replacement).unwrap();
    std::fs::rename(&retired, &path).unwrap();
    assert_eq!(std::fs::read(&worker_path).unwrap(), b"original");
    pinned.verify_current_path(&path).unwrap();
}

#[cfg(any(unix, windows))]
#[test]
fn writer_open_path_preserves_platform_identity_policy() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("writer-path.db");
    std::fs::File::create(&path).unwrap();
    let pinned = OpenedDatabaseFile::pin(&path).unwrap();
    let worker_path = pinned.writer_open_path(&path).unwrap();

    #[cfg(unix)]
    if cfg!(any(target_os = "linux", target_os = "android")) {
        assert!(worker_path.starts_with("/proc/self/fd/"));
    } else {
        assert_eq!(worker_path, path);
    }
    #[cfg(windows)]
    assert_eq!(worker_path, path);
}

#[cfg(unix)]
#[test]
fn writer_identity_fence_rejects_a_replacement_hidden_by_path_restore() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.db");
    let replacement = directory.path().join("identity.replacement.db");
    let retired = directory.path().join("identity.retired.db");
    for candidate in [&path, &replacement] {
        let connection = Connection::open(candidate).unwrap();
        connection
            .execute_batch("CREATE TABLE identity(value INTEGER);")
            .unwrap();
    }

    let pinned = OpenedDatabaseFile::pin(&path).unwrap();
    std::fs::rename(&path, &retired).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    let connection = Connection::open(&path).unwrap();
    std::fs::rename(&path, &replacement).unwrap();
    std::fs::rename(&retired, &path).unwrap();

    assert_eq!(
        pinned.verify_connection(&connection, &path),
        Err(OpenedDatabaseFileError::Replaced)
    );
}

#[cfg(windows)]
#[test]
fn windows_pinned_file_blocks_replacement_until_authority_closes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.db");
    let retired = directory.path().join("identity.retired.db");
    std::fs::write(&path, b"original").unwrap();
    let pinned = OpenedDatabaseFile::pin(&path).unwrap();
    let retained = pinned.try_clone().unwrap();

    assert_eq!(retained.worker_open_path(&path).unwrap(), path);
    assert_eq!(
        std::fs::rename(&path, &retired).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    pinned.verify_current_path(&path).unwrap();

    drop(retained);
    drop(pinned);
    std::fs::rename(&path, &retired)
        .expect("replacement must become possible after retained handles close");
}

#[cfg(windows)]
#[test]
fn windows_discard_created_removes_the_complete_sqlite_family() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fresh.db");
    let created = OpenedDatabaseFile::create_new(&path).unwrap();
    let sidecars = [
        sidecar_path(&path, "-wal"),
        sidecar_path(&path, "-shm"),
        sidecar_path(&path, "-journal"),
    ];
    for sidecar in &sidecars {
        std::fs::write(sidecar, b"sidecar").unwrap();
    }

    created.discard_created(&path).unwrap();

    assert!(!path.exists());
    assert!(sidecars.iter().all(|sidecar| !sidecar.exists()));
}

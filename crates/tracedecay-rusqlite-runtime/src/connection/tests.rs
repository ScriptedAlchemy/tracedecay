use std::path::{Path, PathBuf};

use rusqlite::{Connection, ErrorCode, config::DbConfig, ffi, limits::Limit};
use tempfile::NamedTempFile;
use tracedecay_store::WAL_SOFT_LIMIT_BYTES;

use super::{
    ConnectionMode, Durability, OpenedDatabaseFile, OpenedDatabaseFileError, open,
    open_immutable_reader, open_writer, with_progress_cancellation,
};
#[cfg(unix)]
use super::{VerifiedReaderError, open_verified_reader};

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

/// Writer policy: WAL, `synchronous=NORMAL` unless the store holds durable
/// memory, and no automatic checkpoint. It does not consult any process
/// environment variable.
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
    assert_eq!(
        connection
            .query_row("PRAGMA temp.auto_vacuum", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1,
        "production writers must install TEMP reclamation before using TEMP"
    );
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

/// `wal_autocheckpoint = 0` means SQLite never shrinks the WAL on its own, and
/// a checkpoint resets WAL *contents* without returning the file's blocks. The
/// writer must therefore declare the retained WAL span explicitly.
#[test]
fn writer_bounds_the_retained_wal_file_to_the_soft_limit_ceiling() {
    let file = database();
    let connection = open(file.path(), ConnectionMode::Writer).expect("writer policy");

    assert_eq!(
        pragma_i64(&connection, "journal_size_limit"),
        i64::try_from(WAL_SOFT_LIMIT_BYTES).expect("soft limit fits in i64"),
        "writer must cap the retained WAL file at the configurable soft-limit ceiling"
    );
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
fn rebinding_a_cached_statement_keeps_its_compiled_plan() {
    let file = database();
    let writer = open(file.path(), ConnectionMode::Writer).expect("prepare WAL database");
    let reader = open(file.path(), ConnectionMode::Reader).expect("reader policy");
    for connection in [&writer, &reader] {
        let mut reprepares = 0;
        for limit in 0..32_i64 {
            let mut statement = connection
                .prepare_cached("SELECT count(*) FROM (SELECT value FROM items LIMIT ?1)")
                .unwrap();
            let count: i64 = statement.query_row([limit], |row| row.get(0)).unwrap();
            assert_eq!(count, limit.min(1));
            reprepares += statement.reset_status(rusqlite::StatementStatus::RePrepare);
        }
        assert_eq!(
            reprepares, 0,
            "a bound LIMIT must not recompile the statement"
        );
    }
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
    assert_eq!(connection.limit(Limit::SQLITE_LIMIT_ATTACHED).unwrap(), 4);
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

fn temp_file_bytes(connection: &Connection) -> i64 {
    let mut file: *mut ffi::sqlite3_file = std::ptr::null_mut();
    let mut bytes = 0;
    // The connection exclusively owns its TEMP pager. FILE_POINTER borrows
    // the open VFS file, including on systems where its name was unlinked.
    unsafe {
        assert_eq!(
            ffi::sqlite3_file_control(
                connection.handle(),
                c"temp".as_ptr(),
                ffi::SQLITE_FCNTL_FILE_POINTER,
                (&raw mut file).cast(),
            ),
            ffi::SQLITE_OK
        );
        let methods = file.as_ref().unwrap().pMethods.as_ref().unwrap();
        assert_eq!(methods.xFileSize.unwrap()(file, &mut bytes), ffi::SQLITE_OK);
    }
    bytes
}

/// Reclaim the actual FILE-backed SQLite TEMP pager while leaving both the
/// main database and an independent live TEMP table intact. Clearing temp_store
/// would also free the file, but would silently delete the survivor's rows.
#[test]
fn temp_auto_vacuum_reclaims_pages_without_clearing_live_tables() {
    let file = database();
    let connection = Connection::open(file.path()).expect("open FILE-backed test connection");
    connection
        .execute_batch("PRAGMA temp_store = FILE")
        .expect("select filesystem TEMP storage before connection policy");
    super::apply_pragmas(
        &connection,
        ConnectionMode::Writer,
        false,
        Durability::Normal,
    )
    .expect("apply canonical connection policy");
    assert_eq!(
        connection
            .query_row("PRAGMA temp.auto_vacuum", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1,
        "TEMP must use FULL auto-vacuum to reclaim dropped b-trees"
    );
    connection
        .execute_batch(
            "CREATE TEMP TABLE survivor(value TEXT NOT NULL);
             INSERT INTO temp.survivor VALUES ('live row');
             CREATE TEMP TABLE disposable(body BLOB NOT NULL);
             BEGIN IMMEDIATE;",
        )
        .expect("create a live and a disposable TEMP relation");
    let payload = vec![0x53_u8; 8192];
    {
        let mut insert = connection
            .prepare("INSERT INTO temp.disposable(body) VALUES (?1)")
            .unwrap();
        for _ in 0..1024 {
            insert.execute([payload.as_slice()]).unwrap();
        }
    }
    connection.execute_batch("COMMIT").unwrap();
    let before: i64 = connection
        .query_row("PRAGMA temp.page_count", [], |row| row.get(0))
        .unwrap();
    assert!(
        before > 1024,
        "fixture must allocate actual FILE TEMP pages: {before}"
    );
    let bytes_before = temp_file_bytes(&connection);
    assert!(
        bytes_before >= 8 * 1024 * 1024,
        "TEMP data must spill to disk"
    );
    // Rollback must restore a dropped TEMP b-tree and every committed row.
    // Reclaiming pages on a failed transaction would silently lose data.
    connection
        .execute_batch("BEGIN IMMEDIATE; DROP TABLE temp.disposable; ROLLBACK;")
        .expect("undo a tentative TEMP reclamation");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM temp.disposable", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1024,
        "rolled-back TEMP drop must preserve every committed row"
    );
    assert_eq!(
        connection
            .query_row("PRAGMA temp.page_count", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        before,
        "rollback must restore the original TEMP page allocation"
    );
    connection
        .execute_batch("DROP TABLE temp.disposable")
        .expect("drop disposable TEMP b-tree");
    let bytes_after = temp_file_bytes(&connection);
    assert!(
        bytes_after > 0 && bytes_after * 8 < bytes_before,
        "the still-open TEMP file must shrink: {bytes_before} -> {bytes_after}"
    );
    let after: i64 = connection
        .query_row("PRAGMA temp.page_count", [], |row| row.get(0))
        .unwrap();
    assert!(
        after * 8 < before,
        "dropping a TEMP table must shrink its pages: {before} -> {after}"
    );
    assert_eq!(
        connection
            .query_row("SELECT value FROM temp.survivor", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "live row",
        "reclamation must not reset active TEMP objects"
    );
    assert_eq!(
        connection
            .query_row("SELECT value FROM main.items", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1,
        "main database rows must survive"
    );
    for schema in ["temp", "main"] {
        assert_eq!(
            connection
                .query_row(&format!("PRAGMA {schema}.integrity_check"), [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "ok",
            "{schema} corruption after reclaiming TEMP pages"
        );
    }
    drop(connection);

    // Public opens must install the policy before the first TEMP allocation.
    let reopened = open(file.path(), ConnectionMode::Writer).unwrap();
    assert_eq!(
        reopened
            .query_row("PRAGMA temp.auto_vacuum", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        reopened
            .query_row("SELECT value FROM main.items", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn maintenance_vacuum_preserves_each_stores_auto_vacuum_mode() {
    let root = tempfile::tempdir().unwrap();
    let main = root.path().join("main.db");
    let attached = root.path().join("attached.db");
    for (path, mode) in [(&main, "INCREMENTAL"), (&attached, "NONE")] {
        let db = Connection::open(path).unwrap();
        db.pragma_update(None, "auto_vacuum", mode).unwrap();
        db.execute_batch("CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(7)")
            .unwrap();
    }

    let db = open(&main, ConnectionMode::Maintenance).unwrap();
    db.execute("ATTACH ?1 AS auxiliary", [attached.to_str().unwrap()])
        .unwrap();
    db.execute_batch("VACUUM main; VACUUM auxiliary").unwrap();
    drop(db);
    for (path, expected) in [(&main, 2), (&attached, 0)] {
        let db = Connection::open(path).unwrap();
        assert_eq!(pragma_i64(&db, "auto_vacuum"), expected);
        assert_eq!(
            db.query_row("SELECT value FROM sample", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            7
        );
    }
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

/// Facts and the anchors they cite are never rebuilt, so the stores that hold
/// them sync every commit; session and registry stores keep `NORMAL` until
/// their durable rows move into classes of their own.
#[cfg(any(unix, windows))]
#[test]
fn stores_holding_durable_memory_commit_at_full_sync() {
    use tracedecay_store::StoreShardScopeV1;

    let project_id = tracedecay_domain::ProjectId::new("project.durability").unwrap();
    for (scope, synchronous) in [
        (
            StoreShardScopeV1::Project {
                project_id: project_id.clone(),
            },
            2,
        ),
        (StoreShardScopeV1::ProfileMemory, 2),
        (StoreShardScopeV1::ProjectSessions { project_id }, 1),
        (StoreShardScopeV1::ProfileSessions, 1),
        (StoreShardScopeV1::Profile, 1),
    ] {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("store.sqlite3");
        let pinned = OpenedDatabaseFile::create_new(&path).expect("pin fresh database");
        let worker_path = pinned.writer_open_path(&path).expect("select writer path");
        let connection = open_writer(
            &worker_path,
            Some(&pinned),
            &path,
            Durability::for_scope(&scope),
        )
        .expect("writer policy");

        assert_eq!(
            pragma_i64(&connection, "synchronous"),
            synchronous,
            "{scope:?}"
        );
    }
}

#[cfg(any(unix, windows))]
#[test]
fn fresh_writer_uses_a_sidecar_compatible_path() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("fresh-writer.sqlite3");
    let pinned = OpenedDatabaseFile::create_new(&path).expect("pin fresh database");
    let worker_path = pinned.writer_open_path(&path).expect("select writer path");
    let connection =
        open_writer(&worker_path, Some(&pinned), &path, Durability::Normal).expect("writer policy");

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
fn create_new_reserves_the_full_sqlite_basename_without_unverified_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fresh.db");
    let created = OpenedDatabaseFile::create_new(&path).unwrap();
    assert!(path.is_file());
    assert_ne!(created.identity(), 0);
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = sidecar_path(&path, suffix);
        assert_eq!(std::fs::metadata(&sidecar).unwrap().len(), 0);
    }
    drop(created);
    // Dropping a reservation must not unlink mutable names. In particular, a
    // foreign process may have substituted an unrelated inode since creation.
    assert!(path.is_file());
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(sidecar_path(&path, suffix).is_file());
    }
}

#[test]
fn create_new_refuses_existing_sidecar_members_without_touching_their_bytes() {
    for suffix in ["-wal", "-shm", "-journal"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fresh.db");
        let foreign = sidecar_path(&path, suffix);
        let bytes = format!("foreign SQLite {suffix} bytes");
        std::fs::write(&foreign, bytes.as_bytes()).unwrap();
        assert!(matches!(
            OpenedDatabaseFile::create_new(&path),
            Err(OpenedDatabaseFileError::Reserve)
        ));
        assert_eq!(std::fs::read(&foreign).unwrap(), bytes.as_bytes());
    }
}

/// A reserved basename that failed on a foreign sidecar is left with an
/// empty main DB. A later existing-mode attachment must not let SQLite
/// initialize that empty file and destroy the foreign sidecar on retry.
#[test]
fn failed_sidecar_reservation_cannot_be_reopened_as_existing_sqlite() {
    for suffix in ["-wal", "-shm", "-journal"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.db");
        let foreign = sidecar_path(&path, suffix);
        let bytes = format!("foreign occupied {suffix} bytes");
        std::fs::write(&foreign, bytes.as_bytes()).unwrap();

        assert!(matches!(
            OpenedDatabaseFile::create_new(&path),
            Err(OpenedDatabaseFileError::Reserve)
        ));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert!(matches!(
            OpenedDatabaseFile::pin_initialized(&path),
            Err(OpenedDatabaseFileError::InvalidHeader)
        ));
        assert_eq!(std::fs::read(&foreign).unwrap(), bytes.as_bytes());
    }
}

#[test]
fn existing_sqlite_admission_requires_a_completed_header() {
    let directory = tempfile::tempdir().unwrap();
    for size in [512_usize, 513, 1023, 1025, 4096] {
        let path = directory.path().join(format!("incomplete-{size}.db"));
        let mut bytes = vec![0_u8; size];
        bytes[..16].copy_from_slice(b"SQLite format 3\0");
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            OpenedDatabaseFile::pin_initialized(&path),
            Err(OpenedDatabaseFileError::InvalidHeader)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    let path = directory.path().join("sessions.db");
    std::fs::write(&path, b"invalid database, not SQLite").unwrap();
    assert!(matches!(
        OpenedDatabaseFile::pin_initialized(&path),
        Err(OpenedDatabaseFileError::InvalidHeader)
    ));
    std::fs::remove_file(&path).unwrap();
    let database = Connection::open(&path).unwrap();
    database
        .execute_batch("CREATE TABLE probe(value INTEGER)")
        .unwrap();
    drop(database);
    assert!(OpenedDatabaseFile::pin_initialized(&path).is_ok());
}

#[test]
fn initialized_wal_with_uncheckpointed_schema_remains_attachable() {
    const EXTERNAL_READER: &str = "TRACEDECAY_TEST_INSPECT_LIVE_FIRST_SCHEMA";
    if let Some(path) = std::env::var_os(EXTERNAL_READER) {
        let path = PathBuf::from(path);
        let shm = sidecar_path(&path, "-shm");
        let before = std::fs::read(&shm).unwrap();
        let pinned = OpenedDatabaseFile::pin_initialized(&path).unwrap();
        assert_eq!(std::fs::read(&shm).unwrap(), before);
        pinned.release_after_workers_exit();
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE probe(value INTEGER);
             INSERT INTO probe VALUES (42);",
        )
        .unwrap();
    let wal = sidecar_path(&path, "-wal");
    let before = std::fs::read(&wal).unwrap();
    assert!(!before.is_empty());
    // A separate process has no connection sharing the writer's VFS node.
    // It must discover the live WAL index through SQLite without rewriting it.
    let name = format!(
        "{}::initialized_wal_with_uncheckpointed_schema_remains_attachable",
        module_path!().split_once("::").unwrap().1
    );
    let external = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--nocapture"])
        .env(EXTERNAL_READER, &path)
        .output()
        .unwrap();
    assert!(
        external.status.success() && String::from_utf8_lossy(&external.stdout).contains("1 passed"),
        "external WAL inspection failed: {external:?}"
    );
    let pinned = OpenedDatabaseFile::pin_initialized(&path).unwrap();
    assert_ne!(pinned.identity(), 0);
    assert_eq!(std::fs::read(&wal).unwrap(), before);
    drop(writer);
    pinned.release_after_workers_exit();
    let reader = Connection::open(&path).unwrap();
    assert_eq!(
        reader.query_row("SELECT value FROM probe", [], |row| row.get::<_, i64>(0)),
        Ok(42)
    );
}

#[test]
fn live_wal_recovers_an_inconsistent_immutable_checkpoint_view() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.db");
    let wal_path = sidecar_path(&path, "-wal");
    let shm_path = sidecar_path(&path, "-shm");
    let journal_path = sidecar_path(&path, "-journal");
    let writer = Connection::open(&path).unwrap();
    writer
        .pragma_update(
            None,
            "application_id",
            crate::schema_version::REGISTERED_STORE_APPLICATION_ID,
        )
        .unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA wal_checkpoint(TRUNCATE);
             CREATE TABLE lcm_raw_messages(value INTEGER);
             CREATE TRIGGER git_history_message_update
             AFTER UPDATE ON lcm_raw_messages BEGIN SELECT 1; END;",
        )
        .unwrap();
    let main_header = std::fs::read(&path).unwrap()[..100].to_vec();
    let wal = std::fs::read(&wal_path).unwrap();
    let shm = std::fs::read(&shm_path).unwrap();

    // Model a main-only immutable view assembled from different committed
    // generations: the trigger is visible but its parent table is not. Keep a
    // valid WAL snapshot whose read view contains both objects.
    writer
        .execute_batch(
            "PRAGMA writable_schema = ON;
             DELETE FROM sqlite_schema WHERE type = 'table' AND name = 'lcm_raw_messages';
             PRAGMA writable_schema = OFF;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
    assert!(matches!(
        OpenedDatabaseFile::pin_initialized(&path),
        Err(OpenedDatabaseFileError::InvalidHeader)
    ));

    std::fs::write(&wal_path, wal).unwrap();
    std::fs::write(&shm_path, shm).unwrap();
    let mut main = std::fs::read(&path).unwrap();
    main[..100].copy_from_slice(&main_header);
    std::fs::write(&path, main).unwrap();
    if journal_path.exists() {
        std::fs::remove_file(&journal_path).unwrap();
    }
    let pinned = OpenedDatabaseFile::pin_initialized(&path)
        .expect("the authenticated live WAL completes the checkpoint-bounded main view");
    pinned.release_after_workers_exit();
    drop(writer);
}

#[test]
fn create_new_refuses_to_replace_an_existing_database() {
    let file = NamedTempFile::new().unwrap();

    assert!(matches!(
        OpenedDatabaseFile::create_new(file.path()),
        Err(OpenedDatabaseFileError::Create)
    ));
}

#[test]
fn legacy_first_schema_in_disconnected_wal_preserves_unclaimed_members() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.db");
    let wal = sidecar_path(&path, "-wal");
    let shm = sidecar_path(&path, "-shm");
    {
        let writer = Connection::open(&path).unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE probe(value); INSERT INTO probe VALUES (7)",
            )
            .unwrap();
    }
    let main_bytes = std::fs::read(&path).unwrap();
    let wal_bytes = std::fs::read(&wal).unwrap();
    let shm_bytes = std::fs::read(&shm).unwrap();
    assert!(!wal_bytes.is_empty());
    assert!(matches!(
        OpenedDatabaseFile::pin_initialized(&path),
        Err(OpenedDatabaseFileError::IncompleteInitialization)
    ));
    assert_eq!(std::fs::read(&shm).unwrap(), shm_bytes);
    std::fs::remove_file(&shm).unwrap();
    assert!(matches!(
        OpenedDatabaseFile::pin_initialized(&path),
        Err(OpenedDatabaseFileError::IncompleteInitialization)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), main_bytes);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_bytes);
    assert!(!shm.exists());
    // The owner can still run ordinary SQLite recovery on its own database.
    let writer = Connection::open(&path).unwrap();
    assert_eq!(
        writer.query_row("SELECT value FROM probe", [], |row| row.get::<_, i64>(0)),
        Ok(7)
    );
    drop(writer);
    OpenedDatabaseFile::pin_initialized(&path).unwrap();
}

#[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
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

/// A WAL reader opens the `-shm` sidecar, so it needs the same sidecar-safe
/// pathname a writer does.
///
/// SQLite derives sidecar names by appending to the full pathname it resolved.
/// Linux `/proc/self/fd/*` is a symlink, so the real `<database>-shm` is
/// derived and the descriptor pathname is safe; every other Unix host exposes
/// `/dev/fd/*` as a non-symlink devfs entry, so SQLite would look for
/// `/dev/fd/<fd>-shm` and fail the first schema read with `SQLITE_CANTOPEN`.
/// Readers once used the raw descriptor policy and macOS CI failed 2284 tests
/// on that single divergence; pinning the two policies together is what keeps
/// them from drifting apart again on a host this suite cannot run on.
#[cfg(any(unix, windows))]
#[test]
fn reader_and_writer_open_paths_share_one_sidecar_policy() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reader-path.db");
    std::fs::File::create(&path).unwrap();
    let pinned = OpenedDatabaseFile::pin(&path).unwrap();

    let reader_path = pinned.reader_open_path(&path).unwrap();
    let writer_path = pinned.writer_open_path(&path).unwrap();
    assert_eq!(reader_path, writer_path);

    #[cfg(unix)]
    if cfg!(any(target_os = "linux", target_os = "android")) {
        assert!(reader_path.starts_with("/proc/self/fd/"));
    } else {
        assert_eq!(reader_path, path);
    }
    #[cfg(windows)]
    assert_eq!(reader_path, path);
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

#[cfg(unix)]
#[test]
fn worker_identity_refuses_a_vfs_without_move_attestation() {
    let file = database();
    let pinned = OpenedDatabaseFile::pin(file.path()).unwrap();
    // SQLite's built-in memdb VFS does not implement HAS_MOVED, although
    // it accepts the same pathname as the pinned on-disk database.
    let virtual_connection = Connection::open_with_flags_and_vfs(
        file.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE,
        "memdb",
    )
    .expect("open bundled SQLite's memdb VFS");
    assert_eq!(
        super::sqlite_connection_has_moved(&virtual_connection),
        Ok(None),
        "the regression must exercise an actual unsupported file control"
    );
    assert_eq!(
        pinned.verify_connection(&virtual_connection, file.path()),
        Err(OpenedDatabaseFileError::Inspect),
        "an unchanged filename cannot authenticate an unverified SQLite VFS handle"
    );

    // Rejecting that connection must leave the real database intact and
    // permit a worker using the ordinary SQLite VFS.
    let real_connection = Connection::open(file.path()).unwrap();
    pinned
        .verify_connection(&real_connection, file.path())
        .unwrap();
    assert_eq!(
        real_connection.query_row("SELECT value FROM items", [], |row| row.get::<_, i64>(0)),
        Ok(1)
    );
}

#[cfg(unix)]
#[test]
fn verified_immutable_reader_never_binds_transient_b_to_restored_a_identity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identity.db");
    let replacement = directory.path().join("identity.replacement.db");
    let retired = directory.path().join("identity.retired.db");
    for (candidate, marker) in [(&path, "a"), (&replacement, "b")] {
        let connection = Connection::open(candidate).unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE identity(composer_id TEXT, bubble_id TEXT, marker TEXT);\
                 INSERT INTO identity VALUES ('same-composer', 'same-bubble', '{marker}');"
            ))
            .unwrap();
    }
    let expected_a = OpenedDatabaseFile::pin(&path).unwrap().identity();

    let pinned = std::sync::Arc::new(std::sync::Barrier::new(2));
    let swapped = std::sync::Arc::new(std::sync::Barrier::new(2));
    let opened = std::sync::Arc::new(std::sync::Barrier::new(2));
    let restored = std::sync::Arc::new(std::sync::Barrier::new(2));
    let replacer = {
        let path = path.clone();
        let replacement = replacement.clone();
        let retired = retired.clone();
        let pinned = std::sync::Arc::clone(&pinned);
        let swapped = std::sync::Arc::clone(&swapped);
        let opened = std::sync::Arc::clone(&opened);
        let restored = std::sync::Arc::clone(&restored);
        std::thread::spawn(move || {
            pinned.wait();
            std::fs::rename(&path, &retired).unwrap();
            std::fs::rename(&replacement, &path).unwrap();
            swapped.wait();
            opened.wait();
            std::fs::rename(&path, &replacement).unwrap();
            std::fs::rename(&retired, &path).unwrap();
            restored.wait();
        })
    };

    let result = open_verified_reader(
        &path,
        open_immutable_reader,
        || {
            pinned.wait();
            swapped.wait();
        },
        || {
            opened.wait();
            restored.wait();
        },
    );
    replacer.join().unwrap();

    match result {
        Ok(reader) => {
            assert_eq!(reader.file_identity(), expected_a);
            assert_eq!(
                reader
                    .connection()
                    .query_row("SELECT marker FROM identity", [], |row| row
                        .get::<_, String>(0))
                    .unwrap(),
                "a"
            );
        }
        Err(VerifiedReaderError::Identity(OpenedDatabaseFileError::Replaced)) => {}
        Err(error) => panic!("unexpected verified immutable open failure: {error}"),
    }
}

#[test]
fn header_only_existing_store_preserves_ambiguous_sidecars() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("existing.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA journal_mode=WAL")
        .unwrap();
    let member = super::sidecar_path(&path, "-journal");
    std::fs::write(&member, b"unclaimed state").unwrap();
    assert!(matches!(
        OpenedDatabaseFile::pin_initialized(&path),
        Err(OpenedDatabaseFileError::IncompleteInitialization)
    ));
    assert_eq!(std::fs::read(&member).unwrap(), b"unclaimed state");
    std::fs::remove_file(&member).unwrap();
    let database = Connection::open(&path).unwrap();
    database
        .execute_batch(
            "CREATE TABLE existing_data(value INTEGER); INSERT INTO existing_data VALUES (7)",
        )
        .unwrap();
    drop(database);
    OpenedDatabaseFile::pin_initialized(&path).unwrap();
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

    assert_eq!(retained.writer_open_path(&path).unwrap(), path);
    // The block surfaces as ERROR_SHARING_VIOLATION (32) from the pin's
    // share mode, or ERROR_ACCESS_DENIED (5) on hosts that deny through the
    // handle instead; std's ErrorKind mapping for 32 varies by release, so
    // assert the raw contract.
    let blocked = std::fs::rename(&path, &retired).unwrap_err();
    assert!(
        matches!(blocked.raw_os_error(), Some(5 | 32)),
        "pinned replacement must be blocked while the authority is open: {blocked}"
    );
    pinned.verify_current_path(&path).unwrap();

    drop(retained);
    drop(pinned);
    std::fs::rename(&path, &retired)
        .expect("replacement must become possible after retained handles close");
}

#[cfg(windows)]
#[test]
fn windows_dropping_reservations_preserves_sidecars_written_after_creation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fresh.db");
    let created = OpenedDatabaseFile::create_new(&path).unwrap();
    let sidecars = [
        sidecar_path(&path, "-wal"),
        sidecar_path(&path, "-shm"),
        sidecar_path(&path, "-journal"),
    ];
    for sidecar in &sidecars {
        std::fs::write(sidecar, b"foreign sidecar").unwrap();
    }
    drop(created);
    assert!(path.is_file());
    for sidecar in &sidecars {
        assert_eq!(std::fs::read(sidecar).unwrap(), b"foreign sidecar");
    }
}

/// `sqlite_host_path` shortens a verbatim *disk* path so SQLite's win32 VFS
/// uses per-handle byte locking instead of the UNC shared-handle emulation;
/// it must keep the prefix for genuine UNC and over-long paths, which need
/// the extended-length spelling to open at all.
#[cfg(windows)]
#[test]
fn sqlite_host_path_shortens_verbatim_disk_paths_only() {
    use super::sqlite_host_path;

    let verbatim = Path::new(r"\\?\C:\data\store.db");
    assert_eq!(sqlite_host_path(verbatim), Path::new(r"C:\data\store.db"));

    let unc = Path::new(r"\\?\UNC\server\share\store.db");
    assert_eq!(sqlite_host_path(unc), unc);

    let long = PathBuf::from(format!(r"\\?\C:\{}", "d".repeat(300)));
    assert_eq!(sqlite_host_path(&long), long);

    // 255 plain chars strip (sidecars still fit); 256 keep the prefix
    // because `-wal`/`-shm` would push past the classic 259-char limit.
    let fits_with_sidecars = PathBuf::from(format!(r"\\?\C:\{}", "d".repeat(252)));
    assert_eq!(
        sqlite_host_path(&fits_with_sidecars),
        PathBuf::from(format!(r"C:\{}", "d".repeat(252)))
    );
    let sidecars_too_long = PathBuf::from(format!(r"\\?\C:\{}", "d".repeat(253)));
    assert_eq!(sqlite_host_path(&sidecars_too_long), sidecars_too_long);

    // An empty `\\` component collapses to a plain separator either way,
    // so dunce accepts the conversion; the literal spelling is preserved.
    let empty_component = Path::new(r"\\?\C:\data\\store.db");
    assert_eq!(
        sqlite_host_path(empty_component),
        Path::new(r"C:\data\\store.db")
    );

    // The classic limit is measured in UTF-16 code units, not UTF-8 bytes:
    // 150 multibyte chars are 300 bytes but 150 units, so they may strip.
    let wide_fits = PathBuf::from(format!("\\\\?\\C:\\{}", "é".repeat(150)));
    assert_eq!(
        sqlite_host_path(&wide_fits),
        PathBuf::from(format!("C:\\{}", "é".repeat(150)))
    );

    // Components whose identity would change under Win32 name parsing keep
    // the prefix: trailing dots/spaces are stripped, dot components resolve,
    // `/` is a separator, and reserved DOS-device stems get special
    // treatment. `dunce::simplified` refuses all of these and the refusal
    // is honored — refused verbatim paths stay verbatim. (An empty `\\`
    // component collapses to a plain separator either way, so dunce accepts
    // that one — covered above.)
    for raw in [
        r"\\?\C:\data\store.",
        r"\\?\C:\data\store ",
        r"\\?\C:\data\.\store.db",
        r"\\?\C:\data\..\store.db",
        r"\\?\C:\data/store.db",
        r"\\?\C:\data\CON.db",
        r"\\?\C:\data\con.db",
        r"\\?\C:\data\NUL",
        r"\\?\C:\data\com1.db",
        r"\\?\C:\data\lpt9.log",
        "\\\\?\\C:\\data\\COM\u{b9}.db",
        "\\\\?\\C:\\data\\lpt\u{b3}",
    ] {
        let path = Path::new(raw);
        assert_eq!(sqlite_host_path(path), path, "{raw}");
    }

    let plain = Path::new(r"C:\data\store.db");
    assert_eq!(sqlite_host_path(plain), plain);

    let relative = Path::new("data\\store.db");
    assert_eq!(sqlite_host_path(relative), relative);
}

/// On non-Windows hosts the `\\?\` byte sequence is an ordinary relative
/// filename — a database path spelled that way must reach SQLite unchanged.
#[cfg(not(windows))]
#[test]
fn sqlite_host_path_keeps_literal_windows_prefix_names() {
    use super::sqlite_host_path;

    let name = PathBuf::from(r"\\?\C:\data\store.db");
    assert_eq!(sqlite_host_path(&name), name);
}

/// Two real directories can coexist whose names differ only by a trailing
/// dot, and stripping `\\?\` from the dotted spelling would change which
/// name the OS sees — so `dunce::simplified` refuses and the verbatim
/// path is passed through untouched. SQLite itself still canonicalizes
/// the name and aliases `store.` to `store`: a separate SQLite
/// limitation, not a reason to normalize further here. What the
/// production boundary must guarantee is that a refused verbatim
/// spelling keeps its prefix and still opens.
#[cfg(windows)]
#[test]
fn dot_suffixed_directory_retains_verbatim_and_opens() {
    use super::sqlite_host_path;

    let temp = tempfile::tempdir().unwrap();
    let canonical = std::fs::canonicalize(temp.path()).unwrap();
    assert!(
        canonical.to_str().unwrap().starts_with(r"\\?\"),
        "test requires a verbatim tempdir path"
    );

    // Both creations must go through verbatim spelling: a plain `store.`
    // would collapse onto `store`.
    let dotted = canonical.join("store.");
    let plain = canonical.join("store");
    std::fs::create_dir(&dotted).unwrap();
    std::fs::create_dir(&plain).unwrap();
    assert!(dotted.is_dir() && plain.is_dir());

    // The refused verbatim spelling is passed through untouched.
    let dotted_db = dotted.join("db.sqlite");
    assert_eq!(sqlite_host_path(&dotted_db), dotted_db);

    // Seed the `store` database (production open is READ_WRITE-only).
    let db = Connection::open(plain.join("db.sqlite")).expect("create database");
    db.execute_batch(
        "CREATE TABLE sentinel(v TEXT); INSERT INTO sentinel VALUES('canonical-store');",
    )
    .unwrap();
    drop(db);

    // The verbatim open succeeds through the production boundary. SQLite
    // resolves the name to `store`'s database (its own canonicalization
    // aliases the dotted directory — a SQLite limitation, not this
    // conversion's identity claim) — proven by the sentinel row; a
    // separate `store.` database is unreachable through SQLite either way.
    let reader = open(&dotted_db, ConnectionMode::Writer).expect("open via dotted verbatim path");
    let value: String = reader
        .query_row("SELECT v FROM sentinel", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "canonical-store");
}

/// A database opened through a verbatim `\\?\` path is the same file, the
/// same WAL journal, and the same lock domain as its plain spelling — the
/// normalization only changes the pathname the VFS sees.
#[cfg(windows)]
#[test]
fn verbatim_disk_path_opens_the_same_wal_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("verbatim.db");
    std::fs::write(&path, b"").unwrap();
    // `canonicalize` yields the `\\?\` extended-length spelling that store
    // paths carry into the engine on Windows.
    let verbatim = path.canonicalize().unwrap();
    assert!(verbatim.to_str().unwrap().starts_with(r"\\?\"));

    let writer = open(&verbatim, ConnectionMode::Writer).expect("verbatim writer open");
    writer
        .execute_batch("CREATE TABLE items(value INTEGER); INSERT INTO items VALUES (1);")
        .unwrap();
    let journal_mode: String = writer
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .expect("read pragma");
    assert_eq!(journal_mode, "wal");

    let reader = open(&path, ConnectionMode::Reader).expect("plain reader open");
    assert_eq!(
        reader.query_row("SELECT value FROM items", [], |row| row.get::<_, i64>(0)),
        Ok(1)
    );
    drop(reader);
    drop(writer);
}

#[test]
fn verified_live_snapshot_retains_committed_wal_view_and_denies_writes() {
    let file = database();
    let writer = Connection::open(file.path()).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    let reader = super::open_verified_read_snapshot(file.path()).unwrap();
    writer.execute("UPDATE items SET value = 2", []).unwrap();
    assert_eq!(
        reader
            .connection()
            .query_row("SELECT value FROM items", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        reader
            .connection()
            .execute("UPDATE items SET value = 3", [])
            .is_err()
    );
    drop(reader);
    let reader = super::open_verified_read_snapshot(file.path()).unwrap();
    assert_eq!(
        reader
            .connection()
            .query_row("SELECT value FROM items", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(reader);
    let busy: i64 = writer
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        busy, 0,
        "dropping the reader must release its checkpoint lock"
    );
}

//! Persisted-schema admission regressions for the one accepted runtime shape.

use std::{
    fs,
    path::{Path, PathBuf},
};

use tempfile::TempDir;

use crate::db::engine::TestConnection;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

use super::super::{
    PAYLOAD_DIGEST_STEP_SOURCE_VERSION, SCHEMA_VERSION, create_schema_connection,
    verify_final_schema_connection,
};

#[derive(Debug, PartialEq, Eq)]
struct StoreSnapshot {
    user_version: i64,
    schema_bytes: Vec<u8>,
    file_bytes: Vec<u8>,
}

async fn fresh_current_store() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("create final-shape fixture directory");
    let path = directory.path().join("final-shape.db");
    let connection = TestConnection::open(&path);
    create_schema_connection(&connection)
        .await
        .expect("create final-shape fixture");
    drop(connection);
    (directory, path)
}

fn object_sql(path: &Path, object_type: &str, name: &str) -> Option<String> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open final-shape fixture read-only");
    connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = ?1 AND name = ?2",
            [object_type, name],
            |row| row.get(0),
        )
        .ok()
        .flatten()
}

fn table_has_column(path: &Path, table: &str, column: &str) -> bool {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open final-shape fixture read-only");
    let mut statement = connection
        .prepare("SELECT 1 FROM pragma_table_xinfo(?1) WHERE name = ?2 COLLATE NOCASE")
        .expect("prepare final-shape column probe");
    statement.query_row([table, column], |_| Ok(())).is_ok()
}

fn store_snapshot(path: &Path) -> StoreSnapshot {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open final-shape snapshot read-only");
    let user_version = connection
        .query_row("PRAGMA user_version", (), |row| row.get(0))
        .expect("read final-shape snapshot version");
    let schema_bytes = connection
        .query_row(
            "SELECT CAST(COALESCE(group_concat(entry, char(0)), '') AS BLOB)
             FROM (
                 SELECT type || ':' || name || ':' || COALESCE(sql, '') AS entry
                 FROM sqlite_master
                 WHERE name NOT LIKE 'sqlite_%'
                 ORDER BY type, name
             )",
            (),
            |row| row.get(0),
        )
        .expect("read final-shape snapshot schema");
    drop(connection);
    StoreSnapshot {
        user_version,
        schema_bytes,
        file_bytes: fs::read(path).expect("read final-shape fixture bytes"),
    }
}

fn tamper(path: &Path, sql: &str) {
    let connection = rusqlite::Connection::open(path).expect("open final-shape fixture to tamper");
    connection
        .execute_batch(sql)
        .expect("apply literal final-shape tamper");
}

/// Opens the store through the production writer, requires the typed
/// reset-required refusal, and returns its reason.
async fn assert_reset_required_without_repair(path: &Path, mutation: &str) -> String {
    let before = store_snapshot(path);
    let authority = DatabaseAuthority::acquire_test(path, "final-shape admission fixture")
        .expect("acquire final-shape admission authority");
    let error =
        match Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Existing)
            .await
        {
            Ok(_) => panic!("a stamped final store with a structural tamper must be refused"),
            Err(error) => error,
        };
    let (authority, reason) = error
        .reset_required_context()
        .expect("final-shape refusal must remain typed reset-required");
    assert_eq!(authority, "SQLite store", "{mutation} refusal authority");
    assert_eq!(
        store_snapshot(path),
        before,
        "{mutation} refusal must not repair or otherwise rewrite the store"
    );
    reason.to_owned()
}

/// The canonical project store exactly as every release from v0.1.0-beta.25
/// through v0.1.0-beta.37 wrote it. The fixture header carries the
/// tag-to-inventory table; it is assembled from the tagged DDL rather than
/// from the current contract, because a released shape derived from the
/// current contract agrees with whatever this binary expects and so cannot
/// detect an admission that refuses what shipped.
const RELEASED_V34_PROJECT_STORE_SQL: &str =
    include_str!("../../../../tests/fixtures/project-store-released-v34.sql");

/// Writes the released project store into an empty file, in the WAL mode
/// every shipped binary ran, and stamps the `user_version` a shipped binary
/// left, which is what selects the released admission path.
fn released_v34_project_store(directory: &TempDir) -> PathBuf {
    let path = directory.path().join("released-v34.db");
    let connection = rusqlite::Connection::open(&path).expect("create released store fixture");
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .expect("run the released store in WAL mode");
    connection
        .execute_batch(RELEASED_V34_PROJECT_STORE_SQL)
        .expect("install the released project schema");
    connection
        .execute_batch(&format!(
            "PRAGMA user_version = {PAYLOAD_DIGEST_STEP_SOURCE_VERSION};"
        ))
        .expect("stamp the released schema version");
    path
}

/// Every release from v0.1.0-beta.25 through v0.1.0-beta.37 wrote one
/// byte-identical project store, and every one of them carries the
/// `semantic_vector_*` staging family v36 retired with dense code retrieval.
/// That family has no forward path, so opening a released store must refuse
/// with the typed reset remedy, naming the retired object, before the
/// released-shape convergence writes anything: on the writer path and on a
/// read-only mount, which must not promise a writer-side step that would
/// itself refuse.
#[tokio::test]
async fn released_project_store_is_refused_without_mutation() {
    let directory = tempfile::tempdir().expect("create released fixture directory");
    let path = released_v34_project_store(&directory);
    assert!(
        object_sql(&path, "table", "semantic_vector_stages").is_some(),
        "the released fixture must carry the retired staging family"
    );

    let read_only = verify_final_schema_connection(&TestConnection::open(&path))
        .await
        .expect_err("a read-only mount must refuse a released dense store");
    let (authority, reason) = read_only
        .reset_required_context()
        .expect("read-only refusal of a released dense store is typed reset-required");
    assert_eq!(authority, "SQLite store");
    assert!(
        reason.contains("semantic_vector_"),
        "read-only refusal must name the retired family: {reason}"
    );

    let reason = assert_reset_required_without_repair(&path, "released dense store").await;
    assert!(
        reason.contains("semantic_vector_"),
        "writer refusal must name the retired family: {reason}"
    );
    assert_eq!(
        store_snapshot(&path).user_version,
        i64::from(PAYLOAD_DIGEST_STEP_SOURCE_VERSION)
    );
}

#[tokio::test]
async fn current_final_store_is_admitted_without_mutation() {
    let (_directory, path) = fresh_current_store().await;
    // Diagnostics install this same DDL when publishing their first result.
    // That ordinary operation must not make the store fail its next open.
    tamper(&path, tracedecay_store::GENERATION_DIAGNOSTICS_SCHEMA_DDL);
    // The runtime writer likewise re-ensures its ledger before its first
    // idempotency lookup. A fresh store followed by an ordinary write must
    // remain the exact shape accepted on restart.
    tamper(
        &path,
        tracedecay_rusqlite_runtime::runtime_ledger::RUNTIME_LEDGER_SCHEMA,
    );
    let before = store_snapshot(&path);
    assert_eq!(before.user_version, i64::from(SCHEMA_VERSION));

    let authority = DatabaseAuthority::acquire_test(&path, "final-shape admission fixture")
        .expect("acquire final-shape admission authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Existing)
            .await
            .expect("the exact current store shape must remain admissible");
    drop(database);

    assert_eq!(
        store_snapshot(&path),
        before,
        "current-shape admission must remain a query-only identity check"
    );
}

async fn admit_existing(path: &Path, context: &str) {
    let authority = DatabaseAuthority::acquire_test(path, "final-shape admission fixture")
        .expect("acquire final-shape admission authority");
    let (database, _) =
        Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Existing)
            .await
            .unwrap_or_else(|error| panic!("{context}: {error}"));
    drop(database);
}

fn ledger_tables(path: &Path) -> Vec<String> {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open final-shape fixture read-only");
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name LIKE 'td_runtime_writer_%' ORDER BY name",
        )
        .expect("prepare ledger table probe");
    statement
        .query_map((), |row| row.get(0))
        .expect("query ledger tables")
        .collect::<Result<_, _>>()
        .expect("read ledger tables")
}

/// The runtime writer creates its ledger lazily inside the canonical store, so
/// a store's first lifetime used to leave a shape its next open refused. The
/// ledger is part of the exact shape now: a store that predates it gains it
/// on open, one that already carries it is admitted unchanged, and one still
/// holding the retired idempotency table has it folded into the current one.
#[tokio::test]
async fn runtime_writer_ledger_is_part_of_the_final_shape() {
    let (_directory, path) = fresh_current_store().await;
    let expected_ledger = ledger_tables(&path);
    assert_eq!(expected_ledger.len(), 4, "fresh store carries the ledger");
    let before = store_snapshot(&path);
    admit_existing(&path, "store carrying the ledger must be admitted").await;
    assert_eq!(
        store_snapshot(&path),
        before,
        "ledger-carrying admission stays query-only"
    );

    tamper(
        &path,
        "DROP TABLE td_runtime_writer_checkpoint_v1;
         DROP TABLE td_runtime_writer_idempotency_v2;
         DROP TABLE td_runtime_writer_outbox_v1;
         DROP TABLE td_runtime_writer_inbox_v1;",
    );
    assert!(ledger_tables(&path).is_empty());
    admit_existing(&path, "store predating the ledger must be admitted").await;
    assert_eq!(
        ledger_tables(&path),
        expected_ledger,
        "open installs the ledger"
    );
    assert_eq!(store_snapshot(&path).schema_bytes, before.schema_bytes);

    tamper(
        &path,
        "DROP TABLE td_runtime_writer_idempotency_v2;
         CREATE TABLE td_runtime_writer_idempotency_v1 (
             shard_json TEXT NOT NULL, incarnation INTEGER NOT NULL,
             authority_epoch INTEGER NOT NULL, idempotency_key TEXT NOT NULL,
             request_digest TEXT NOT NULL, original_receipt_json TEXT NOT NULL,
             transaction_scope_json TEXT NOT NULL, operation_id TEXT NOT NULL,
             durability_json TEXT NOT NULL, committed_at_micros INTEGER NOT NULL,
             PRIMARY KEY (shard_json, incarnation, authority_epoch, idempotency_key)
         ) WITHOUT ROWID;
         INSERT INTO td_runtime_writer_idempotency_v1 VALUES
             ('{}', 1, 1, 'key-1', 'digest', '{}', '{}', 'op-1', '{}', 42);",
    );
    admit_existing(
        &path,
        "store with the retired idempotency ledger must be admitted",
    )
    .await;
    assert_eq!(
        ledger_tables(&path),
        expected_ledger,
        "open folds the retired ledger"
    );
    let connection =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open final-shape fixture read-only");
    let migrated: (String, i64) = connection
        .query_row(
            "SELECT idempotency_key, committed_at_micros FROM td_runtime_writer_idempotency_v2",
            (),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("retired receipt survives the fold");
    assert_eq!(migrated, ("key-1".to_owned(), 42));
}

#[tokio::test]
async fn automation_run_receipt_indexes_are_required_final_shape() {
    let (_directory, path) = fresh_current_store().await;
    for name in [
        "idx_memory_v2_operation_receipts_automation_run",
        "idx_memory_v2_automatic_fact_receipts_automation_run",
    ] {
        let sql = object_sql(&path, "index", name).expect("automation-run index exists");
        assert!(
            sql.contains("json_extract"),
            "{name} must index the run identity"
        );
    }

    tamper(
        &path,
        "DROP INDEX idx_memory_v2_automatic_fact_receipts_automation_run;",
    );
    assert_reset_required_without_repair(&path, "missing automatic-run lookup index").await;
}

#[tokio::test]
async fn stamped_final_store_with_missing_or_tampered_required_shape_is_reset_required() {
    let (_directory, path) = fresh_current_store().await;
    tamper(&path, "DROP TABLE metadata;");
    assert!(object_sql(&path, "table", "metadata").is_none());
    assert_reset_required_without_repair(&path, "missing required table").await;

    let (_directory, path) = fresh_current_store().await;
    tamper(&path, "DROP INDEX idx_read_cache_session;");
    assert!(object_sql(&path, "index", "idx_read_cache_session").is_none());
    assert_reset_required_without_repair(&path, "missing required index").await;

    let (_directory, path) = fresh_current_store().await;
    tamper(
        &path,
        "ALTER TABLE metadata ADD COLUMN final_shape_tamper TEXT;",
    );
    assert!(table_has_column(&path, "metadata", "final_shape_tamper"));
    assert_reset_required_without_repair(&path, "unexpected final-shape column").await;

    let (_directory, path) = fresh_current_store().await;
    tamper(
        &path,
        "DROP TRIGGER memory_v2_automatic_fact_receipts_require_keys;",
    );
    assert!(
        object_sql(
            &path,
            "trigger",
            "memory_v2_automatic_fact_receipts_require_keys"
        )
        .is_none()
    );
    assert_reset_required_without_repair(&path, "missing required trigger").await;

    let (_directory, path) = fresh_current_store().await;
    tamper(
        &path,
        "CREATE TABLE unexpected_final_shape_object (id INTEGER PRIMARY KEY);",
    );
    assert!(object_sql(&path, "table", "unexpected_final_shape_object").is_some());
    assert_reset_required_without_repair(&path, "unexpected final-shape object").await;
}

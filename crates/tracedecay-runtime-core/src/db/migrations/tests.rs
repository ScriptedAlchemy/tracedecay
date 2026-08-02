use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_rusqlite_runtime::migration_sql::{
    MigrationSqlError, MigrationSqlWriteAuthority, MigrationSqlWriteIntent,
};

use crate::db::engine::{Connection, TestConnection};

use super::{SCHEMA_VERSION, create_schema_connection, ensure_schema_current_connection};

mod fts;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct AllowSchemaWrites;

impl MigrationSqlWriteAuthority for AllowSchemaWrites {
    fn verify(&self, intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
        if intent == MigrationSqlWriteIntent::Vacuum {
            Err(MigrationSqlError::AuthorityDenied(
                "ordinary schema fixture cannot vacuum".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}

/// Creates an empty database owned by the engine test runtime.
async fn create_raw_db() -> (TestConnection, TempDir) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    let setup = rusqlite::Connection::open(&db_path).expect("open schema fixture");
    setup
        .execute_batch(
            "PRAGMA auto_vacuum = INCREMENTAL;
             PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .expect("failed to apply pragmas");
    drop(setup);
    let conn = TestConnection::open_with_write_authority(&db_path, Arc::new(AllowSchemaWrites));
    (conn, dir)
}

/// Creates a fresh, fully-shaped database on the engine test runtime.
async fn create_schema_db() -> (TestConnection, TempDir) {
    let (conn, dir) = create_raw_db().await;
    create_schema_connection(&conn)
        .await
        .expect("failed to create the schema");
    (conn, dir)
}

/// Sets PRAGMA `user_version` on the connection.
async fn set_user_version(conn: &Connection, version: u32) {
    conn.execute(&format!("PRAGMA user_version = {version}"), ())
        .await
        .expect("failed to set user_version");
}

/// Reads PRAGMA `user_version` from the connection.
async fn get_user_version(conn: &Connection) -> u32 {
    let mut rows = conn
        .query("PRAGMA user_version", ())
        .await
        .expect("failed to query user_version");
    let row = rows
        .next()
        .await
        .expect("failed to read user_version row")
        .expect("user_version should return a row");
    let v: i64 = row.get(0).expect("failed to read user_version value");
    v as u32
}

/// Checks whether a table exists in `sqlite_master`.
async fn table_exists(conn: &Connection, table_name: &str) -> bool {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            (table_name,),
        )
        .await
        .expect("failed to query sqlite_master");
    rows.next()
        .await
        .expect("failed to read sqlite_master row")
        .is_some()
}

/// Returns the first column from the first row as i64.
async fn scalar_i64(conn: &Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.expect("failed to query scalar");
    let row = rows
        .next()
        .await
        .expect("failed to read scalar row")
        .expect("scalar query should return a row");
    row.get(0).expect("failed to read scalar value")
}

async fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .expect("failed to query table_info");
    while let Some(row) = rows.next().await.expect("failed to read table_info row") {
        let name: String = row.get::<String>(1).expect("failed to read column name");
        if name == column {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// An empty file is created at the one supported shape, and reopening it is a
/// pure identity check.
#[tokio::test]
async fn an_empty_database_is_created_at_the_supported_schema_version() {
    let (conn, _dir) = create_raw_db().await;

    assert_eq!(super::get_version(&*conn).await.unwrap(), 0);
    ensure_schema_current_connection(&conn).await.unwrap();
    assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);

    ensure_schema_current_connection(&conn)
        .await
        .expect("reopening a current store is an identity check");
    assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);
}

/// A store stamped with any other version was written by an incompatible
/// binary. This binary has no ladder, so it refuses with the fresh-start
/// remedy instead of upgrading in place.
#[tokio::test]
async fn a_store_at_another_schema_version_is_refused_with_a_fresh_start_remedy() {
    for stamped in [1_u32, 18, 24, SCHEMA_VERSION + 1] {
        let (conn, _dir) = create_schema_db().await;
        set_user_version(&conn, stamped).await;

        let error = ensure_schema_current_connection(&conn)
            .await
            .expect_err("a store at another version must be refused");
        let message = error.to_string();
        assert!(
            message.contains("created by an incompatible binary"),
            "v{stamped} refusal must name the cause: {message}"
        );
        assert!(
            message.contains("Remove the store directory"),
            "v{stamped} refusal must name the fresh-start remedy: {message}"
        );
    }
}

/// Creation is atomic: an interrupted create leaves neither DDL nor a version
/// stamp behind, and the retry still produces the full shape.
#[tokio::test]
async fn interrupted_fresh_schema_rolls_back_ddl_and_version_before_retry() {
    let (conn, _dir) = create_raw_db().await;
    super::configure_fresh_auto_vacuum(&conn, "test interrupted fresh schema")
        .await
        .unwrap();

    let transaction = conn.authorized_long_lease_transaction().await.unwrap();
    super::create_schema_transaction(&transaction)
        .await
        .unwrap();
    assert_eq!(
        super::get_version(&transaction).await.unwrap(),
        SCHEMA_VERSION
    );
    transaction.rollback().await.unwrap();

    assert_eq!(get_user_version(&conn).await, 0);
    assert!(!table_exists(&conn, "nodes").await);

    ensure_schema_current_connection(&conn).await.unwrap();
    assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);
    assert!(column_exists(&conn, "nodes", "branches").await);
    assert!(column_exists(&conn, "nodes", "unsafe_blocks").await);
}

/// The creation DDL installs the whole final shape in one transaction: graph,
/// holographic memory, memory V2 lineage, the V22/V23 compatibility
/// projections, evidence assembly, and external sources.
#[tokio::test]
async fn fresh_creation_installs_every_stage_of_the_final_shape() {
    let (conn, _dir) = create_schema_db().await;

    for table in [
        "nodes",
        "edges",
        "files",
        "metadata",
        "node_fingerprints",
        "read_cache",
        "redundancy_pairs",
        "memory_facts",
        "memory_oplog",
        "memory_fact_relations",
        "memory_v2_facts",
        "memory_v2_assertions",
        "memory_v2_lineage_events",
        "memory_v2_current_facts",
        "memory_v2_proposals",
        "memory_v2_proposal_transitions",
        "memory_v2_proposal_current",
        "memory_v2_fact_relations",
    ] {
        assert!(table_exists(&conn, table).await, "missing table {table}");
    }

    // Columns the retired v20/v21 upgrades used to add are born with the table.
    assert!(column_exists(&conn, "memory_v2_proposals", "idempotency_key").await);
    assert!(column_exists(&conn, "memory_v2_proposals", "request_digest").await);
    assert!(column_exists(&conn, "memory_v2_proposal_transitions", "origin").await);
    assert!(column_exists(&conn, "memory_v2_backfill_progress", "cutover_receipt_json").await);
    for column in [
        "retrieval_count",
        "access_count",
        "helpful_count",
        "unhelpful_count",
        "last_retrieved_at",
        "last_recalled_at",
        "last_feedback_at",
        "projection_state",
        "vector_watermark_json",
    ] {
        assert!(
            column_exists(&conn, "memory_v2_current_facts", column).await,
            "missing memory_v2_current_facts.{column}"
        );
    }

    // The proposal projection is born at its V22 shape.
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'trigger'
               AND name = 'memory_v2_proposal_transitions_no_new_applying'",
        )
        .await,
        1
    );
    assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);
}

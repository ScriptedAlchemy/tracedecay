use std::sync::Arc;
use tempfile::TempDir;
use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
};

use crate::db::engine::{Connection, TestConnection};

use super::{SCHEMA_VERSION, create_schema_connection, ensure_schema_current_connection};

mod final_shape;
mod fts;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct AllowSchemaWrites;

impl ExactSqlWriteAuthority for AllowSchemaWrites {
    fn verify(&self, intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if intent == ExactSqlWriteIntent::Vacuum {
            Err(ExactSqlError::AuthorityDenied(
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

async fn string_column(conn: &Connection, sql: &str) -> Vec<String> {
    let mut rows = conn.query(sql, ()).await.expect("failed to query strings");
    let mut values = Vec::new();
    while let Some(row) = rows.next().await.expect("failed to read string row") {
        values.push(row.get(0).expect("failed to read string value"));
    }
    values
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
    for stamped in [1_u32, 18, 24, SCHEMA_VERSION - 1, SCHEMA_VERSION + 1] {
        let (conn, _dir) = create_schema_db().await;
        set_user_version(&conn, stamped).await;

        let error = ensure_schema_current_connection(&conn)
            .await
            .expect_err("a store at another version must be refused");
        let message = error.to_string();
        assert_eq!(
            error
                .reset_required_context()
                .map(|(authority, _reason)| authority),
            Some("SQLite store")
        );
        assert!(
            message.contains("created by an incompatible binary"),
            "v{stamped} refusal must name the cause: {message}"
        );
        assert!(
            message.contains("Remove the store directory"),
            "v{stamped} refusal must name the fresh-start remedy: {message}"
        );
        assert_eq!(
            get_user_version(&conn).await,
            stamped,
            "refusal must never rewrite an incompatible schema stamp"
        );
    }
}

#[tokio::test]
async fn the_former_v26_shape_is_refused_without_mutation() {
    let (conn, _dir) = create_raw_db().await;
    conn.execute_batch(
        "CREATE TABLE graph_publication_replay_v1 (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            shard_id TEXT NOT NULL,
            namespace TEXT NOT NULL,
            projection TEXT NOT NULL,
            generation TEXT NOT NULL,
            idempotency_key TEXT NOT NULL,
            input_digest TEXT NOT NULL,
            dependency_generation_closure_digest TEXT NOT NULL,
            direct_dependency_bytes INTEGER NOT NULL,
            expected_recovered_digest TEXT NOT NULL,
            canonical_replay_source_digest TEXT NOT NULL,
            canonical_replay_source BLOB NOT NULL
        ) STRICT;",
    )
    .await
    .unwrap();
    set_user_version(&conn, 26).await;

    let error = ensure_schema_current_connection(&conn)
        .await
        .expect_err("a superseded schema stamp must not be admitted as current");
    assert_eq!(
        error
            .reset_required_context()
            .map(|(authority, _reason)| authority),
        Some("SQLite store")
    );

    assert_eq!(get_user_version(&conn).await, 26);
    assert!(table_exists(&conn, "graph_publication_replay_v1").await);
    assert!(!column_exists(&conn, "graph_publication_replay_v1", "expected_prior_head").await);
}

#[tokio::test]
async fn a_current_stamp_with_retired_code_graph_tables_is_reset_required() {
    for retired in [
        "nodes",
        "edges",
        "files",
        "unresolved_refs",
        "nodes_fts",
        "nodes_fts_data",
        "node_fingerprints",
        "redundancy_pairs",
    ] {
        let (conn, _dir) = create_schema_db().await;
        conn.execute_batch(&format!("CREATE TABLE {retired} (id INTEGER);"))
            .await
            .unwrap();

        let error = ensure_schema_current_connection(&conn)
            .await
            .expect_err("a current stamp must not conceal retired graph storage");
        assert_eq!(
            error
                .reset_required_context()
                .map(|(authority, _reason)| authority),
            Some("SQLite store")
        );
        assert!(
            error.to_string().contains(retired),
            "the refusal must identify the retired object: {error}"
        );
        assert!(table_exists(&conn, retired).await);
        assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);
    }
}

#[tokio::test]
async fn a_current_stamp_with_retired_memory_projection_objects_is_reset_required() {
    for retired in [
        "memory_facts",
        "memory_entities",
        "memory_fact_entities",
        "memory_feedback_events",
        "memory_oplog",
        "memory_fact_relations",
        "memory_v2_fact_relations",
        "memory_facts_fts",
        "memory_facts_fts_data",
        "memory_banks",
        "memory_bank_dirty",
        "memory_v2_banks",
        "memory_v2_bank_dirty",
        "memory_v2_assertion_vectors",
        "memory_v2_legacy_map",
        "memory_v2_legacy_quarantine",
        "memory_v2_backfill_progress",
        "memory_v2_legacy_proposal_map",
        "memory_v2_proposals",
        "memory_v2_proposal_transitions",
        "memory_v2_proposal_current",
        "memory_v2_legacy_feedback_event_map",
        "memory_v2_feedback_history_repair_progress",
        "memory_v2_compatibility_operation_receipts",
        "memory_v2_compatibility_banks",
        "memory_v2_compatibility_bank_dirty",
    ] {
        let (conn, _dir) = create_schema_db().await;
        conn.execute_batch(&format!("CREATE TABLE {retired} (id INTEGER);"))
            .await
            .unwrap();

        let error = ensure_schema_current_connection(&conn)
            .await
            .expect_err("a current stamp must not conceal retired memory storage");
        assert_eq!(
            error
                .reset_required_context()
                .map(|(authority, _reason)| authority),
            Some("SQLite store")
        );
        assert!(
            error.to_string().contains(retired),
            "the refusal must identify the retired object: {error}"
        );
        assert!(table_exists(&conn, retired).await);
        assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);
    }
}

#[tokio::test]
async fn a_current_stamp_with_retired_memory_projection_columns_is_reset_required() {
    for retired in ["source_label", "projection_state", "vector_watermark_json"] {
        let (conn, _dir) = create_schema_db().await;
        conn.execute_batch(&format!(
            "ALTER TABLE memory_v2_current_facts ADD COLUMN {retired} TEXT;"
        ))
        .await
        .unwrap();

        let error = ensure_schema_current_connection(&conn)
            .await
            .expect_err("a current stamp must not conceal retired projection columns");
        assert_eq!(
            error
                .reset_required_context()
                .map(|(authority, _reason)| authority),
            Some("SQLite store")
        );
        assert!(
            error.to_string().contains("memory_v2_current_facts"),
            "the refusal must identify the incompatible projection table: {error}"
        );
        assert!(column_exists(&conn, "memory_v2_current_facts", retired).await);
        assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);
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
    assert!(table_exists(&conn, "metadata").await);
    assert!(table_exists(&conn, "read_cache").await);
    assert!(!table_exists(&conn, "nodes").await);
}

/// The creation DDL installs the retained relational shape in one transaction:
/// canonical memory, evidence assembly, external sources, and graph publication
/// manifests, without recreating either superseded `SQLite` projection.
#[tokio::test]
async fn fresh_creation_installs_every_stage_of_the_final_shape() {
    let (conn, _dir) = create_schema_db().await;

    for table in [
        "metadata",
        "read_cache",
        "memory_v2_facts",
        "memory_v2_assertions",
        "memory_v2_lineage_events",
        "memory_v2_current_facts",
        "memory_v2_automatic_fact_receipts",
    ] {
        assert!(table_exists(&conn, table).await, "missing table {table}");
    }

    assert_eq!(
        string_column(
            &conn,
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name GLOB 'memory_v2_*'
             ORDER BY name",
        )
        .await,
        [
            "memory_v2_assertion_evidence",
            "memory_v2_assertion_payloads",
            "memory_v2_assertion_payloads_fts",
            "memory_v2_assertion_payloads_fts_config",
            "memory_v2_assertion_payloads_fts_data",
            "memory_v2_assertion_payloads_fts_docsize",
            "memory_v2_assertion_payloads_fts_idx",
            "memory_v2_assertion_supersession",
            "memory_v2_assertions",
            "memory_v2_automatic_fact_receipts",
            "memory_v2_current_facts",
            "memory_v2_evidence",
            "memory_v2_facts",
            "memory_v2_feedback_history",
            "memory_v2_lineage_events",
            "memory_v2_operation_receipts",
        ],
        "fresh creation must install exactly the final memory table inventory",
    );

    for retired in [
        "nodes",
        "edges",
        "files",
        "unresolved_refs",
        "nodes_fts",
        "nodes_fts_data",
        "node_fingerprints",
        "redundancy_pairs",
        "memory_facts",
        "memory_entities",
        "memory_fact_entities",
        "memory_feedback_events",
        "memory_oplog",
        "memory_fact_relations",
        "memory_facts_fts",
        "memory_facts_fts_data",
    ] {
        assert!(
            !table_exists(&conn, retired).await,
            "retired SQLite projection table {retired} must not be created"
        );
    }

    conn.execute(
        "INSERT INTO graph_publication_replay_v1 (
            shard_id, namespace, projection, generation, idempotency_key,
            input_digest, dependency_generation_closure_digest,
            direct_dependency_bytes, expected_recovered_digest,
            canonical_replay_source_digest, canonical_replay_source
         ) VALUES (
            'project-fixture', 'project', 'code', 'generation-1', 'publish-1',
            'sha256:input', 'sha256:dependencies', 2, 'sha256:recovered',
            'sha256:source', x'01'
         )",
        (),
    )
    .await
    .expect("fresh project schema must accept relational graph replay state");
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT length(canonical_replay_source)
             FROM graph_publication_replay_v1
             WHERE generation = 'generation-1'",
        )
        .await,
        1
    );

    assert!(
        column_exists(
            &conn,
            "memory_v2_automatic_fact_receipts",
            "idempotency_key"
        )
        .await
    );
    assert!(column_exists(&conn, "memory_v2_automatic_fact_receipts", "request_digest").await);
    for retired in [
        "memory_v2_legacy_map",
        "memory_v2_legacy_quarantine",
        "memory_v2_backfill_progress",
        "memory_v2_legacy_proposal_map",
        "memory_v2_legacy_feedback_event_map",
        "memory_v2_feedback_history_repair_progress",
        "memory_v2_compatibility_operation_receipts",
        "memory_v2_compatibility_banks",
        "memory_v2_compatibility_bank_dirty",
        "memory_v2_banks",
        "memory_v2_bank_dirty",
        "memory_v2_assertion_vectors",
        "memory_v2_fact_relations",
    ] {
        assert!(
            !table_exists(&conn, retired).await,
            "retired table {retired} must not be created"
        );
    }
    for table in ["memory_v2_operation_receipts", "memory_v2_feedback_history"] {
        assert!(table_exists(&conn, table).await, "missing table {table}");
    }
    for column in [
        "retrieval_count",
        "access_count",
        "helpful_count",
        "unhelpful_count",
        "last_retrieved_at",
        "last_recalled_at",
        "last_feedback_at",
    ] {
        assert!(
            column_exists(&conn, "memory_v2_current_facts", column).await,
            "missing memory_v2_current_facts.{column}"
        );
    }
    for retired in ["source_label", "projection_state", "vector_watermark_json"] {
        assert!(
            !column_exists(&conn, "memory_v2_current_facts", retired).await,
            "retired memory_v2_current_facts.{retired} must not be created"
        );
    }

    // The terminal receipt table admits a truthful quarantine and rejects any
    // intermediate lifecycle state.
    conn.execute(
        "INSERT INTO memory_v2_automatic_fact_receipts (
            apply_id, owner_kind, project_id, owner_json, idempotency_key,
            request_digest, request_json, evidence_json, state, quarantine_reason,
            applied_fact_id, applied_assertion_id, applied_event_id, recorded_at
         ) VALUES ('automatic.fixture', 'profile', '', '{\"kind\":\"profile\"}',
                   'idempotency.fixture', 'digest.fixture', '{}', '{}', 'quarantined',
                   'privacy sanitizer declined content', NULL, NULL, NULL, 1)",
        (),
    )
    .await
    .expect("fresh schema must accept a terminal automatic quarantine");
    let intermediate = conn
        .execute(
            "INSERT INTO memory_v2_automatic_fact_receipts (
                apply_id, owner_kind, project_id, owner_json, idempotency_key,
                request_digest, request_json, evidence_json, state, quarantine_reason,
                applied_fact_id, applied_assertion_id, applied_event_id, recorded_at
             ) VALUES ('intermediate.fixture', 'profile', '', '{\"kind\":\"profile\"}',
                       'idempotency.intermediate', 'digest.intermediate', '{}', '{}', 'applying',
                       NULL, NULL, NULL, NULL, 1)",
            (),
        )
        .await;
    assert!(
        intermediate.is_err(),
        "fresh terminal receipt schema must reject an intermediate state"
    );
    assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);
}

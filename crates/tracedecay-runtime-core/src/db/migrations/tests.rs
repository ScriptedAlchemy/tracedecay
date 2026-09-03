use std::sync::Arc;
use tempfile::TempDir;
use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
};

use crate::db::engine::{Connection, TestConnection};

use super::{
    PAYLOAD_DIGEST_BACKFILL_RECEIPT_KEY, PAYLOAD_DIGEST_STEP_SOURCE_VERSION, SCHEMA_VERSION,
    create_schema_connection, ensure_schema_current_connection, verify_final_schema_connection,
};
use crate::db::engine::params;

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
    for stamped in [1_u32, 18, 24, SCHEMA_VERSION - 2, SCHEMA_VERSION + 1] {
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
            "memory_v2_assertion_payload_digests",
            "memory_v2_assertion_payload_purges",
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

// ---------------------------------------------------------------------------
// v34 -> v35: persisted payload content digests (#834)
// ---------------------------------------------------------------------------

const PAYLOAD_DIGEST_OBJECT_DROPS: &str = "DROP TRIGGER memory_v2_payloads_digest_delete;
    DROP TRIGGER memory_v2_assertion_payload_digests_no_update;
    DROP INDEX memory_v2_assertion_payload_digests_lookup;
    DROP TABLE memory_v2_assertion_payload_digests;";

const V34_FIXTURE_CONTENTS: [&str; 3] = [
    "Unicode café naïve 東京 🚀",
    "JSON escaped quote \" and backslash \\ with\ttab and\nnewline",
    "trailing whitespace ",
];

fn expected_payload_digest(content: &str) -> String {
    use sha2::Digest as _;
    tracedecay_domain::canonical_text::encode_tagged_lowercase_hex(
        "sha256:",
        &sha2::Sha256::digest(content.as_bytes()),
    )
}

/// Seeds one profile fact with a single asserted payload through the raw
/// authority tables, the way a pre-#834 binary left them.
async fn seed_payload(conn: &Connection, ordinal: usize, content: &str) {
    let fact_id = format!("fact.v34.{ordinal}");
    let assertion_id = format!("assertion.v34.{ordinal}");
    conn.execute(
        "INSERT INTO memory_v2_facts (
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES (?1, 'profile', '', '{\"kind\":\"profile\"}', '{}', ?2)",
        params![fact_id.as_str(), ordinal as i64],
    )
    .await
    .expect("seed v34 fact");
    conn.execute(
        "INSERT INTO memory_v2_assertions (
            assertion_id, fact_id, owner_kind, project_id, owner_json,
            assertion_header_json, kind_json, payload_reference_json, receipt_json,
            asserted_at, actor_id
         ) VALUES (?1, ?2, 'profile', '', '{\"kind\":\"profile\"}', '{}', '{}', '{}', '{}', ?3, NULL)",
        params![assertion_id.as_str(), fact_id.as_str(), ordinal as i64],
    )
    .await
    .expect("seed v34 assertion");
    conn.execute(
        "INSERT INTO memory_v2_assertion_payloads (
            assertion_id, fact_id, owner_kind, project_id, payload_json, content
         ) VALUES (?1, ?2, 'profile', '', '{}', ?3)",
        params![assertion_id.as_str(), fact_id.as_str(), content],
    )
    .await
    .expect("seed v34 payload");
}

/// A v35 store whose payloads were written before the digest objects
/// existed: the objects are dropped and the stamp rewound, exactly the shape
/// a pre-#834 binary leaves behind.
async fn create_v34_db_with_payloads() -> (TestConnection, TempDir) {
    let (conn, dir) = create_schema_db().await;
    for (ordinal, content) in V34_FIXTURE_CONTENTS.iter().enumerate() {
        seed_payload(&conn, ordinal, content).await;
    }
    conn.execute_batch(PAYLOAD_DIGEST_OBJECT_DROPS)
        .await
        .expect("drop payload digest objects");
    set_user_version(&conn, PAYLOAD_DIGEST_STEP_SOURCE_VERSION).await;
    assert!(
        !table_exists(&conn, "memory_v2_assertion_payload_digests").await,
        "fixture must start without the digest table"
    );
    (conn, dir)
}

async fn digest_rows(conn: &Connection) -> Vec<(String, String)> {
    let mut rows = conn
        .query(
            "SELECT fact_id, content_digest FROM memory_v2_assertion_payload_digests
             ORDER BY payload_rowid ASC",
            (),
        )
        .await
        .expect("query digest rows");
    let mut values = Vec::new();
    while let Some(row) = rows.next().await.expect("read digest row") {
        values.push((
            row.get(0).expect("read digest fact id"),
            row.get(1).expect("read digest value"),
        ));
    }
    values
}

fn expected_digest_rows() -> Vec<(String, String)> {
    V34_FIXTURE_CONTENTS
        .iter()
        .enumerate()
        .map(|(ordinal, content)| {
            (
                format!("fact.v34.{ordinal}"),
                expected_payload_digest(content),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_v34_store_is_stepped_to_v35_with_a_digest_for_every_payload() {
    let (conn, _dir) = create_v34_db_with_payloads().await;

    ensure_schema_current_connection(&conn)
        .await
        .expect("a v34 store must step forward in place");

    assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);
    assert_eq!(digest_rows(&conn).await, expected_digest_rows());
    let receipts = string_column(
        &conn,
        &format!("SELECT value FROM metadata WHERE key = '{PAYLOAD_DIGEST_BACKFILL_RECEIPT_KEY}'"),
    )
    .await;
    let receipt: serde_json::Value =
        serde_json::from_str(receipts.first().expect("step must journal a receipt"))
            .expect("receipt is JSON");
    assert_eq!(receipt["from_version"], PAYLOAD_DIGEST_STEP_SOURCE_VERSION);
    assert_eq!(receipt["to_version"], SCHEMA_VERSION);
    assert_eq!(receipt["backfilled_rows"], V34_FIXTURE_CONTENTS.len());

    ensure_schema_current_connection(&conn)
        .await
        .expect("a stepped store is the exact final shape");
    assert_eq!(digest_rows(&conn).await.len(), V34_FIXTURE_CONTENTS.len());
}

#[tokio::test]
async fn an_interrupted_payload_digest_step_resumes_from_the_rows_still_missing() {
    let (conn, _dir) = create_v34_db_with_payloads().await;
    // A previous run created the objects and fingerprinted the first payload
    // before losing the writer; the stamp never moved.
    conn.execute_batch(crate::db::memory_v2::PAYLOAD_DIGESTS_SCHEMA)
        .await
        .expect("recreate digest objects as an interrupted step left them");
    conn.execute(
        "INSERT INTO memory_v2_assertion_payload_digests (
            payload_rowid, assertion_id, fact_id, owner_kind, project_id, content_digest
         )
         SELECT rowid, assertion_id, fact_id, owner_kind, project_id, ?1
         FROM memory_v2_assertion_payloads WHERE fact_id = 'fact.v34.0'",
        params![expected_payload_digest(V34_FIXTURE_CONTENTS[0]).as_str()],
    )
    .await
    .expect("seed the partial backfill");
    assert_eq!(
        get_user_version(&conn).await,
        PAYLOAD_DIGEST_STEP_SOURCE_VERSION
    );

    ensure_schema_current_connection(&conn)
        .await
        .expect("an interrupted step must resume");

    assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);
    assert_eq!(digest_rows(&conn).await, expected_digest_rows());
}

#[tokio::test]
async fn a_v34_store_is_refused_read_only_with_the_step_pending_remedy() {
    let (conn, _dir) = create_v34_db_with_payloads().await;

    let error = verify_final_schema_connection(&conn)
        .await
        .expect_err("a read-only verifier must not step the store");

    let message = error.to_string();
    assert!(
        message.contains("payload digest step is pending"),
        "read-only refusal must name the pending step: {message}"
    );
    assert!(
        error.reset_required_context().is_none(),
        "a steppable store must not be reported as reset-required: {message}"
    );
    assert_eq!(
        get_user_version(&conn).await,
        PAYLOAD_DIGEST_STEP_SOURCE_VERSION
    );
    assert!(
        !table_exists(&conn, "memory_v2_assertion_payload_digests").await,
        "read-only verification must not create the digest objects"
    );
}

#[tokio::test]
async fn a_v34_stamp_on_a_store_that_is_not_v34_shaped_is_reset_required() {
    let (conn, _dir) = create_schema_db().await;
    conn.execute_batch(PAYLOAD_DIGEST_OBJECT_DROPS)
        .await
        .expect("drop payload digest objects");
    conn.execute_batch("DROP TABLE memory_v2_assertion_supersession;")
        .await
        .expect("drop an unrelated final-shape table");
    set_user_version(&conn, PAYLOAD_DIGEST_STEP_SOURCE_VERSION).await;

    let error = ensure_schema_current_connection(&conn)
        .await
        .expect_err("the step admits only the exact pre-digest shape");
    assert_eq!(
        error
            .reset_required_context()
            .map(|(authority, _reason)| authority),
        Some("SQLite store")
    );
    assert_eq!(
        get_user_version(&conn).await,
        PAYLOAD_DIGEST_STEP_SOURCE_VERSION
    );
}

#[tokio::test]
async fn deleting_a_payload_drops_its_digest_and_digests_never_update() {
    let (conn, _dir) = create_schema_db().await;
    for (ordinal, content) in V34_FIXTURE_CONTENTS.iter().enumerate() {
        seed_payload(&conn, ordinal, content).await;
    }
    conn.execute_batch(
        "INSERT INTO memory_v2_assertion_payload_digests (
            payload_rowid, assertion_id, fact_id, owner_kind, project_id, content_digest
         )
         SELECT rowid, assertion_id, fact_id, owner_kind, project_id,
                'sha256:0000000000000000000000000000000000000000000000000000000000000000'
         FROM memory_v2_assertion_payloads;",
    )
    .await
    .expect("seed digest rows");
    assert_eq!(digest_rows(&conn).await.len(), V34_FIXTURE_CONTENTS.len());

    let update = conn
        .execute_batch(
            "UPDATE memory_v2_assertion_payload_digests SET content_digest = \
             'sha256:1111111111111111111111111111111111111111111111111111111111111111';",
        )
        .await;
    assert!(
        update
            .expect_err("digest rows are immutable")
            .to_string()
            .contains("immutable")
    );

    conn.execute_batch("DELETE FROM memory_v2_assertion_payloads WHERE fact_id = 'fact.v34.1';")
        .await
        .expect("delete a payload");
    let remaining: Vec<String> = digest_rows(&conn)
        .await
        .into_iter()
        .map(|(fact_id, _digest)| fact_id)
        .collect();
    assert_eq!(
        remaining,
        vec!["fact.v34.0".to_owned(), "fact.v34.2".to_owned()]
    );
}

#[tokio::test]
async fn the_content_digest_lookup_is_an_indexed_point_read() {
    let (conn, _dir) = create_schema_db().await;
    let mut rows = conn
        .query(
            "EXPLAIN QUERY PLAN
             SELECT current_facts.fact_id
             FROM memory_v2_assertion_payload_digests AS digests
             CROSS JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = digests.fact_id
              AND current_facts.owner_kind = digests.owner_kind
              AND current_facts.project_id = digests.project_id
              AND current_facts.active_assertion_id = digests.assertion_id
             CROSS JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE digests.owner_kind = 'profile'
               AND digests.project_id = ''
               AND digests.content_digest = 'sha256:x'
               AND facts.owner_json = '{}'
               AND current_facts.payload_access = 'eligible'
             ORDER BY current_facts.fact_id ASC
             LIMIT 1",
            (),
        )
        .await
        .expect("explain the digest lookup");
    let mut plan = Vec::new();
    while let Some(row) = rows.next().await.expect("read plan row") {
        plan.push(row.get::<String>(3).expect("read plan detail"));
    }
    let plan = plan.join("\n");
    assert!(
        plan.contains("SEARCH digests USING INDEX memory_v2_assertion_payload_digests_lookup"),
        "the digest lookup must drive from the digest index: {plan}"
    );
    assert!(
        !plan.contains("SCAN digests"),
        "the digest lookup must never scan the digest table: {plan}"
    );
}

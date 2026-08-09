use std::{path::Path, sync::Arc};

use rusqlite::params;
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

#[derive(Debug, PartialEq, Eq)]
struct ProposalStoreSnapshot {
    user_version: i64,
    schema_bytes: Vec<u8>,
    rows: Vec<(String, Option<String>, String)>,
}

fn proposal_store_snapshot(path: &Path) -> ProposalStoreSnapshot {
    let raw = rusqlite::Connection::open(path).expect("open proposal snapshot");
    let user_version = raw
        .query_row("PRAGMA user_version", (), |row| row.get(0))
        .expect("read proposal snapshot version");
    let schema_bytes = raw
        .query_row(
            "SELECT CAST(group_concat(sql, char(0)) AS BLOB)
             FROM (
                 SELECT sql
                 FROM sqlite_master
                 WHERE name IN (
                     'memory_v2_proposals',
                     'memory_v2_proposal_transitions',
                     'memory_v2_proposal_current'
                 )
                 ORDER BY type, name
             )",
            (),
            |row| row.get(0),
        )
        .expect("read proposal snapshot schema");
    let mut statement = raw
        .prepare(
            "SELECT current.state, transitions.previous_state, transitions.current_state
             FROM memory_v2_proposal_current AS current
             JOIN memory_v2_proposal_transitions AS transitions
               ON transitions.transition_id = current.last_transition_id
              AND transitions.proposal_id = current.proposal_id
              AND transitions.owner_kind = current.owner_kind
              AND transitions.project_id = current.project_id
             ORDER BY current.proposal_id",
        )
        .expect("prepare proposal snapshot rows");
    let rows = statement
        .query_map((), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query proposal snapshot rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read proposal snapshot rows");
    ProposalStoreSnapshot {
        user_version,
        schema_bytes,
        rows,
    }
}

#[derive(Clone, Copy)]
enum ProposalStateSurface {
    Current,
    TransitionCurrent,
    TransitionPrevious,
}

impl ProposalStateSurface {
    fn label(self) -> &'static str {
        match self {
            Self::Current => "memory_v2_proposal_current.state",
            Self::TransitionCurrent => "memory_v2_proposal_transitions.current_state",
            Self::TransitionPrevious => "memory_v2_proposal_transitions.previous_state",
        }
    }
}

fn seed_proposal_state_fixture(
    path: &Path,
    current_state: &str,
    transition_current_state: &str,
    transition_previous_state: Option<&str>,
) {
    let raw = rusqlite::Connection::open(path).expect("open proposal state fixture");
    raw.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA ignore_check_constraints = ON;",
    )
    .expect("enable exact-shape legacy fixture");
    raw.execute(
        "INSERT INTO memory_v2_proposals(
             proposal_id, owner_kind, project_id, owner_json, idempotency_key,
             request_digest, request_json, evidence_json, submitted_at
         ) VALUES(
             'proposal-fixture', 'profile', '', '{}', 'idempotency-fixture',
             'digest-fixture', '{}', '{}', 1
         )",
        (),
    )
    .expect("seed proposal fixture");
    raw.execute(
        "INSERT INTO memory_v2_proposal_transitions(
             transition_id, proposal_id, owner_kind, project_id,
             previous_state, current_state, transition_json, occurred_at
         ) VALUES(
             'transition-fixture', 'proposal-fixture', 'profile', '',
             ?1, ?2, '{}', 1
         )",
        params![transition_previous_state, transition_current_state],
    )
    .expect("seed proposal transition fixture");
    raw.execute(
        "INSERT INTO memory_v2_proposal_current(
             proposal_id, owner_kind, project_id, state, revision,
             last_transition_id, updated_at
         ) VALUES(
             'proposal-fixture', 'profile', '', ?1, 1,
             'transition-fixture', 1
         )",
        params![current_state],
    )
    .expect("seed current proposal fixture");
    raw.execute_batch("PRAGMA ignore_check_constraints = OFF;")
        .expect("restore proposal fixture constraints");
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
async fn legacy_proposal_states_are_reset_required_on_every_persisted_surface() {
    for legacy_state in ["pending", "pending_approval"] {
        for surface in [
            ProposalStateSurface::Current,
            ProposalStateSurface::TransitionCurrent,
            ProposalStateSurface::TransitionPrevious,
        ] {
            let (conn, dir) = create_schema_db().await;
            let (current_state, transition_current_state, transition_previous_state) = match surface
            {
                ProposalStateSurface::Current => (legacy_state, "applying", None),
                ProposalStateSurface::TransitionCurrent => ("applying", legacy_state, None),
                ProposalStateSurface::TransitionPrevious => {
                    ("applying", "applying", Some(legacy_state))
                }
            };
            let db_path = dir.path().join("test.db");
            seed_proposal_state_fixture(
                &db_path,
                current_state,
                transition_current_state,
                transition_previous_state,
            );
            let before = proposal_store_snapshot(&db_path);

            let error = match ensure_schema_current_connection(&conn).await {
                Ok(()) => panic!("{} accepted {legacy_state}", surface.label()),
                Err(error) => error,
            };
            let (authority, reason) = error
                .reset_required_context()
                .expect("legacy proposal state must return typed reset-required");
            assert_eq!(authority, "SQLite store");
            assert!(
                reason.contains(legacy_state),
                "{} reset reason omitted {legacy_state}: {reason}",
                surface.label()
            );
            assert_eq!(
                proposal_store_snapshot(&db_path),
                before,
                "{} refusal mutated rows, schema bytes, or version",
                surface.label()
            );
        }
    }
}

#[tokio::test]
async fn canonical_proposal_states_remain_admissible_without_mutation() {
    for state in ["applying", "applied", "rejected", "quarantined"] {
        let (conn, dir) = create_schema_db().await;
        let db_path = dir.path().join("test.db");
        seed_proposal_state_fixture(&db_path, state, state, Some(state));
        let before = proposal_store_snapshot(&db_path);

        ensure_schema_current_connection(&conn)
            .await
            .unwrap_or_else(|error| panic!("canonical state {state} was refused: {error}"));

        assert_eq!(
            proposal_store_snapshot(&db_path),
            before,
            "canonical state {state} admission mutated rows, schema bytes, or version"
        );
        assert_eq!(before.user_version, i64::from(SCHEMA_VERSION));
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
/// manifests, without recreating either superseded SQLite projection.
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
        "memory_v2_proposals",
        "memory_v2_proposal_transitions",
        "memory_v2_proposal_current",
        "memory_v2_fact_relations",
    ] {
        assert!(table_exists(&conn, table).await, "missing table {table}");
    }

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

    // Columns the retired upgrade installers used to add are born with the
    // table, and the retired import/backfill surfaces never exist at all.
    assert!(column_exists(&conn, "memory_v2_proposals", "idempotency_key").await);
    assert!(column_exists(&conn, "memory_v2_proposals", "request_digest").await);
    assert!(!column_exists(&conn, "memory_v2_proposal_transitions", "origin").await);
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
        // Plan 39 Task 7 (owner decision 2026-08-07, second): the unread
        // derived-vector storage is deleted, not migrated.
        "memory_v2_banks",
        "memory_v2_bank_dirty",
        "memory_v2_assertion_vectors",
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
        "projection_state",
        "vector_watermark_json",
    ] {
        assert!(
            column_exists(&conn, "memory_v2_current_facts", column).await,
            "missing memory_v2_current_facts.{column}"
        );
    }

    // The automatic fact lifecycle begins durably in `applying` so an
    // interrupted automatic promotion can resume without an approval queue.
    conn.execute(
        "INSERT INTO memory_v2_proposals (
            proposal_id, owner_kind, project_id, owner_json, idempotency_key,
            request_digest, request_json, evidence_json, submitted_at
         ) VALUES ('proposal.fixture', 'profile', '', '{\"kind\":\"profile\"}',
                   'idempotency.fixture', 'digest.fixture', '{}', '{}', 1)",
        (),
    )
    .await
    .expect("fresh proposal schema must accept an automatic submission");
    let applying = conn
        .execute(
            "INSERT INTO memory_v2_proposal_transitions (
                transition_id, proposal_id, owner_kind, project_id, previous_state,
                current_state, transition_json, occurred_at
             ) VALUES ('transition.fixture', 'proposal.fixture', 'profile', '',
                       NULL, 'applying', '{}', 1)",
            (),
        )
        .await;
    assert!(
        applying.is_ok(),
        "a durable applying transition must support automatic promotion"
    );
    assert_eq!(get_user_version(&conn).await, SCHEMA_VERSION);
}

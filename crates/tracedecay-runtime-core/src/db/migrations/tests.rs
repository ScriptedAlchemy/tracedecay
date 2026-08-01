use std::{path::Path, sync::Arc};

use tempfile::TempDir;
use tracedecay_rusqlite_runtime::migration_sql::{
    MigrationSqlError, MigrationSqlWriteAuthority, MigrationSqlWriteIntent,
};

use crate::db::engine::{Connection, TestConnection};
use crate::db::{
    Database, DatabaseAuthority, DatabaseAuthorityRole, TestDatabaseRuntimeMode,
    enter_maintenance_database_scope,
};
use crate::lifecycle_lease::acquire_exclusive_for_profile;

use super::{LATEST_VERSION, create_schema_connection, migrate_connection};

mod fts;
mod memory_v2_v19_v23;
mod pre_v19;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct AllowMigrationWrites;

impl MigrationSqlWriteAuthority for AllowMigrationWrites {
    fn verify(&self, intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
        if intent == MigrationSqlWriteIntent::Vacuum {
            Err(MigrationSqlError::AuthorityDenied(
                "ordinary migration fixture cannot vacuum".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}

/// Creates a database owned by the engine test runtime.
async fn create_raw_db() -> (TestConnection, TempDir) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    let setup = rusqlite::Connection::open(&db_path).expect("open migration fixture");
    setup
        .execute_batch(
            "PRAGMA auto_vacuum = INCREMENTAL;
             PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .expect("failed to apply pragmas");
    drop(setup);
    let conn = TestConnection::open_with_write_authority(&db_path, Arc::new(AllowMigrationWrites));
    (conn, dir)
}

/// Creates a latest-schema database on the engine test runtime.
async fn create_schema_db() -> (TestConnection, TempDir) {
    let (conn, dir) = create_raw_db().await;
    create_schema_connection(&conn)
        .await
        .expect("failed to create latest schema");
    (conn, dir)
}

/// Creates the v10 legacy schema on the engine test runtime.
async fn create_v10_db() -> (TestConnection, TempDir) {
    let (conn, dir) = create_raw_db().await;
    create_v10_schema_for_v11_tests(&conn).await;
    (conn, dir)
}

async fn publish_test_database(path: &Path, mode: TestDatabaseRuntimeMode) -> (Database, bool) {
    let authority =
        DatabaseAuthority::acquire_test(path, "migration test runtime").expect("test authority");
    Database::publish_test_runtime(path, &authority, mode)
        .await
        .expect("publish canonical test runtime")
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

/// Checks whether an index exists in `sqlite_master`.
async fn index_exists(conn: &Connection, index_name: &str) -> bool {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='index' AND name=?1",
            (index_name,),
        )
        .await
        .expect("failed to query sqlite_master");
    rows.next()
        .await
        .expect("failed to read sqlite_master row")
        .is_some()
}

/// Checks whether a trigger exists in `sqlite_master`.
async fn trigger_exists(conn: &Connection, trigger_name: &str) -> bool {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND name=?1",
            (trigger_name,),
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

fn valid_v23_compatibility_bank_vector() -> Vec<u8> {
    let mut vector = vec![0_u8; 8 + 2048 * 4];
    vector[..8].copy_from_slice(&(2048_u64).to_le_bytes());
    vector
}

async fn assert_backfilled_memory_has_vectors_and_banks(
    conn: &Connection,
    category: &str,
    expected_fact_count: i64,
) {
    assert_eq!(
        scalar_i64(
            conn,
            &format!(
                "SELECT COUNT(*) FROM memory_facts
                 WHERE category = '{category}'
                   AND hrr_vector IS NOT NULL
                   AND length(hrr_vector) = 8200
                   AND hrr_algebra = 'amari_fhrr'
                   AND hrr_dim = 2048
                   AND hrr_precision = 'f32'"
            )
        )
        .await,
        expected_fact_count,
        "all backfilled {category} facts should have serialized HRR vectors"
    );
    assert_eq!(
        scalar_i64(
            conn,
            "SELECT COUNT(*) FROM memory_facts
             WHERE hrr_vector IS NULL
                OR length(hrr_vector) != 8200
                OR hrr_algebra != 'amari_fhrr'
                OR hrr_dim != 2048
                OR hrr_precision != 'f32'"
        )
        .await,
        0,
        "v11 migration should leave no backfilled facts missing vectors"
    );
    assert_eq!(
        scalar_i64(
            conn,
            "SELECT COUNT(*) FROM memory_banks
             WHERE bank_name = 'all'
               AND vector IS NOT NULL
               AND length(vector) > 0
               AND hrr_algebra = 'amari_fhrr'
               AND hrr_dim = 2048"
        )
        .await,
        1,
        "v11 migration should build the global memory bank"
    );
    assert_eq!(
        scalar_i64(
            conn,
            &format!(
                "SELECT COUNT(*) FROM memory_banks
                 WHERE bank_name = '{category}'
                   AND vector IS NOT NULL
                   AND length(vector) > 0
                   AND hrr_algebra = 'amari_fhrr'
                   AND hrr_dim = 2048
                   AND fact_count = {expected_fact_count}"
            )
        )
        .await,
        1,
        "v11 migration should build the {category} memory bank"
    );
}

/// Checks whether a column exists on a table via PRAGMA `table_info`.
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

/// Returns the declared `SQLite` type and primary-key ordinal for a column.
async fn column_type_and_pk(conn: &Connection, table: &str, column: &str) -> (String, i64) {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .expect("failed to query table_info");
    while let Some(row) = rows.next().await.expect("failed to read table_info row") {
        let name = row.get::<String>(1).expect("failed to read column name");
        if name == column {
            return (
                row.get::<String>(2).expect("failed to read column type"),
                row.get(5).expect("failed to read primary key ordinal"),
            );
        }
    }
    panic!("{table}.{column} not found");
}

/// Creates the V1 schema (tables, FTS, indexes — no metadata, no complexity columns).
async fn create_v1_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS nodes (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            qualified_name TEXT NOT NULL,
            file_path TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            start_column INTEGER NOT NULL,
            end_column INTEGER NOT NULL,
            docstring TEXT,
            signature TEXT,
            visibility TEXT NOT NULL DEFAULT 'private',
            is_async INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS edges (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source TEXT NOT NULL,
            target TEXT NOT NULL,
            kind TEXT NOT NULL,
            line INTEGER,
            FOREIGN KEY (source) REFERENCES nodes(id) ON DELETE CASCADE,
            FOREIGN KEY (target) REFERENCES nodes(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS files (
            path TEXT PRIMARY KEY,
            content_hash TEXT NOT NULL,
            size INTEGER NOT NULL,
            modified_at INTEGER NOT NULL,
            indexed_at INTEGER NOT NULL,
            node_count INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS unresolved_refs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            from_node_id TEXT NOT NULL,
            reference_name TEXT NOT NULL,
            reference_kind TEXT NOT NULL,
            line INTEGER NOT NULL,
            col INTEGER NOT NULL,
            file_path TEXT NOT NULL,
            FOREIGN KEY (from_node_id) REFERENCES nodes(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS vectors (
            node_id TEXT PRIMARY KEY,
            embedding BLOB NOT NULL,
            model TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
            name, qualified_name, docstring, signature,
            content='nodes', content_rowid='rowid'
        );

        CREATE TRIGGER IF NOT EXISTS nodes_fts_insert AFTER INSERT ON nodes BEGIN
            INSERT INTO nodes_fts(rowid, name, qualified_name, docstring, signature)
            VALUES (NEW.rowid, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
        END;

        CREATE TRIGGER IF NOT EXISTS nodes_fts_delete AFTER DELETE ON nodes BEGIN
            INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, docstring, signature)
            VALUES ('delete', OLD.rowid, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
        END;

        CREATE TRIGGER IF NOT EXISTS nodes_fts_update AFTER UPDATE ON nodes BEGIN
            INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, docstring, signature)
            VALUES ('delete', OLD.rowid, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
            INSERT INTO nodes_fts(rowid, name, qualified_name, docstring, signature)
            VALUES (NEW.rowid, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
        END;

        CREATE INDEX IF NOT EXISTS idx_nodes_kind ON nodes(kind);
        CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
        CREATE INDEX IF NOT EXISTS idx_nodes_qualified_name ON nodes(qualified_name);
        CREATE INDEX IF NOT EXISTS idx_nodes_file_path ON nodes(file_path);
        CREATE INDEX IF NOT EXISTS idx_nodes_file_path_start_line ON nodes(file_path, start_line);
        CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source);
        CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target);
        CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
        CREATE INDEX IF NOT EXISTS idx_edges_source_kind ON edges(source, kind);
        CREATE INDEX IF NOT EXISTS idx_edges_target_kind ON edges(target, kind);
        CREATE INDEX IF NOT EXISTS idx_unresolved_refs_from_node_id ON unresolved_refs(from_node_id);
        CREATE INDEX IF NOT EXISTS idx_unresolved_refs_reference_name ON unresolved_refs(reference_name);
        CREATE INDEX IF NOT EXISTS idx_unresolved_refs_file_path ON unresolved_refs(file_path);",
    )
    .await
    .expect("failed to create v1 schema");
    set_user_version(conn, 1).await;
}

/// Applies the V2 additions on top of V1 (metadata table).
async fn apply_v2(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .await
    .expect("failed to apply v2");
    set_user_version(conn, 2).await;
}

/// Applies the V3 additions on top of V2 (complexity columns).
async fn apply_v3(conn: &Connection) {
    conn.execute_batch(
        "ALTER TABLE nodes ADD COLUMN branches INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN loops INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN returns INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN max_nesting INTEGER NOT NULL DEFAULT 0;",
    )
    .await
    .expect("failed to apply v3");
    set_user_version(conn, 3).await;
}

/// Applies the V4 additions on top of V3 (safety metric columns).
async fn apply_v4(conn: &Connection) {
    conn.execute_batch(
        "ALTER TABLE nodes ADD COLUMN unsafe_blocks INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN unchecked_calls INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN assertions INTEGER NOT NULL DEFAULT 0;",
    )
    .await
    .expect("failed to apply v4");
    set_user_version(conn, 4).await;
}

/// Creates a latest pre-v11 schema with legacy memory tables but no holographic tables.
async fn create_v10_schema_for_v11_tests(conn: &Connection) {
    create_schema_connection(conn)
        .await
        .expect("failed to create baseline schema");
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS memory_facts_fts_insert;
         DROP TRIGGER IF EXISTS memory_facts_fts_delete;
         DROP TRIGGER IF EXISTS memory_facts_fts_update;
         DROP TABLE IF EXISTS memory_facts_fts;
         DROP TABLE IF EXISTS memory_feedback_events;
         DROP TABLE IF EXISTS memory_fact_entities;
         DROP TABLE IF EXISTS memory_banks;
         DROP TABLE IF EXISTS memory_entities;
         DROP TABLE IF EXISTS memory_facts;

         CREATE TABLE IF NOT EXISTS memory_decisions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            reason TEXT,
            created_at INTEGER NOT NULL,
            files TEXT NOT NULL DEFAULT '[]',
            tags TEXT NOT NULL DEFAULT '[]'
         );

         CREATE TABLE IF NOT EXISTS memory_code_areas (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL,
            description TEXT,
            last_touched_at INTEGER NOT NULL,
            touch_count INTEGER NOT NULL DEFAULT 1
         );

         CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_code_areas_path
            ON memory_code_areas(path);
         CREATE INDEX IF NOT EXISTS idx_memory_decisions_created_at
            ON memory_decisions(created_at);",
    )
    .await
    .expect("failed to remove v11 tables");
    set_user_version(conn, 10).await;
}

/// Creates the released v19 PR7 tables that v20 extends in place. Keep this
/// fixture narrow: the forward migration must work without replaying older
/// migration history or starting a legacy-data backfill.
async fn create_v19_memory_schema_for_v20_test(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE retrieval_anchors (
            anchor_id TEXT PRIMARY KEY,
            anchor_json TEXT NOT NULL,
            owner_json TEXT NOT NULL,
            projection_generation TEXT NOT NULL
         );
         CREATE TABLE retrieval_anchor_aliases (
            owner_json TEXT NOT NULL,
            alias_kind TEXT NOT NULL,
            locator_digest TEXT NOT NULL,
            anchor_id TEXT NOT NULL,
            PRIMARY KEY(owner_json, alias_kind, locator_digest),
            UNIQUE(anchor_id, alias_kind, locator_digest),
            FOREIGN KEY(anchor_id) REFERENCES retrieval_anchors(anchor_id)
         );
         INSERT INTO retrieval_anchors(
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES ('anchor.v19', '{}', '{}', 'v19');
         INSERT INTO retrieval_anchor_aliases(
            owner_json, alias_kind, locator_digest, anchor_id
         ) VALUES ('{}', 'fixture', 'digest.v19', 'anchor.v19');

         CREATE TABLE memory_v2_assertions (
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL,
            assertion_header_json TEXT NOT NULL,
            kind_json TEXT NOT NULL,
            payload_reference_json TEXT NOT NULL,
            receipt_json TEXT NOT NULL,
            asserted_at INTEGER NOT NULL,
            actor_id TEXT,
            PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id)
         );
         INSERT INTO memory_v2_assertions(
            assertion_id, fact_id, owner_kind, project_id, owner_json,
            assertion_header_json, kind_json, payload_reference_json,
            receipt_json, asserted_at, actor_id
         ) VALUES(
            'assertion.v19', 'fact.v19', 'profile', '', '{}',
            '{\"payload\":{\"content\":\"v19-header-secret-canary\"}}',
            '{}', '{}', '{}', 100, NULL
         );

         CREATE TABLE memory_v2_backfill_progress (
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL,
            source_store_id TEXT NOT NULL,
            phase TEXT NOT NULL,
            feedback_frontier INTEGER NOT NULL,
            oplog_frontier INTEGER NOT NULL,
            fact_frontier INTEGER NOT NULL,
            feedback_cursor INTEGER NOT NULL DEFAULT 0,
            oplog_cursor INTEGER NOT NULL DEFAULT 0,
            fact_cursor INTEGER NOT NULL DEFAULT 0,
            started_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            cutover_completed_at INTEGER,
            PRIMARY KEY(owner_kind, project_id, source_store_id),
            CHECK(
                (phase = 'cutover_complete' AND cutover_completed_at IS NOT NULL) OR
                (phase <> 'cutover_complete' AND cutover_completed_at IS NULL)
            )
         );
         INSERT INTO memory_v2_backfill_progress(
            owner_kind, project_id, owner_json, source_store_id, phase,
            feedback_frontier, oplog_frontier, fact_frontier,
            feedback_cursor, oplog_cursor, fact_cursor,
            started_at, updated_at, cutover_completed_at
         ) VALUES(
            'profile', '', '{}', 'source.v19', 'cutover_complete',
            3, 4, 5, 3, 4, 5, 100, 100, 100
         );

         CREATE TABLE memory_v2_proposals (
            proposal_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL,
            request_json TEXT NOT NULL,
            evidence_json TEXT NOT NULL,
            submitted_at INTEGER NOT NULL,
            PRIMARY KEY(proposal_id, owner_kind, project_id)
         );
         INSERT INTO memory_v2_proposals(
            proposal_id, owner_kind, project_id, owner_json,
            request_json, evidence_json, submitted_at
         ) VALUES ('proposal.v19', 'profile', '', '{}', '{}', '[]', 100);

         CREATE TABLE memory_v2_proposal_transitions (
            transition_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            transition_id TEXT NOT NULL,
            proposal_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            previous_state TEXT,
            current_state TEXT NOT NULL,
            reviewer_json TEXT,
            validation_json TEXT,
            promoted_fact_id TEXT,
            promoted_assertion_id TEXT,
            promoted_event_id TEXT,
            transition_json TEXT NOT NULL,
            occurred_at INTEGER NOT NULL,
            UNIQUE(transition_id, proposal_id, owner_kind, project_id)
         );
         INSERT INTO memory_v2_proposal_transitions(
            transition_id, proposal_id, owner_kind, project_id,
            previous_state, current_state, reviewer_json, validation_json,
            promoted_fact_id, promoted_assertion_id, promoted_event_id,
            transition_json, occurred_at
         ) VALUES(
            'transition.v19', 'proposal.v19', 'profile', '',
            NULL, 'pending', NULL, NULL, NULL, NULL, NULL, '{}', 100
         );
         CREATE TABLE memory_v2_proposal_current (
            proposal_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            state TEXT NOT NULL,
            revision INTEGER NOT NULL,
            last_transition_id TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(proposal_id, owner_kind, project_id),
            FOREIGN KEY(proposal_id, owner_kind, project_id)
                REFERENCES memory_v2_proposals(proposal_id, owner_kind, project_id),
            FOREIGN KEY(last_transition_id, proposal_id, owner_kind, project_id)
                REFERENCES memory_v2_proposal_transitions(
                    transition_id, proposal_id, owner_kind, project_id
                )
         );
         INSERT INTO memory_v2_proposal_current(
            proposal_id, owner_kind, project_id, state, revision,
            last_transition_id, updated_at
         ) VALUES(
            'proposal.v19', 'profile', '', 'pending', 0, 'transition.v19', 100
         );",
    )
    .await
    .expect("failed to create v19 PR7 memory fixture");
    set_user_version(conn, 19).await;
}

#[tokio::test]
async fn empty_database_migrates_atomically_through_the_engine_runtime() {
    let (conn, _dir) = create_raw_db().await;

    assert_eq!(super::get_version(&*conn).await.unwrap(), 0);
    assert!(migrate_connection(&conn).await.unwrap());
    assert_eq!(
        super::get_version(&*conn).await.unwrap(),
        super::LATEST_VERSION
    );
    assert!(!migrate_connection(&conn).await.unwrap());
}

#[tokio::test]
async fn migration_reindex_only_follows_graph_invalidating_versions() {
    assert!(super::graph_reindex_required(0, super::LATEST_VERSION));
    assert!(super::graph_reindex_required(2, 3));
    assert!(super::graph_reindex_required(16, 17));
    assert!(!super::graph_reindex_required(17, super::LATEST_VERSION));
    assert!(!super::graph_reindex_required(23, super::LATEST_VERSION));
    assert!(!super::graph_reindex_required(24, super::LATEST_VERSION));

    let (conn, _dir) = create_schema_db().await;
    conn.execute(
        "DELETE FROM metadata WHERE key = ?1",
        (super::GRAPH_GENERATION_SCHEMA_KEY,),
    )
    .await
    .unwrap();
    set_user_version(&conn, 24).await;

    assert!(migrate_connection(&conn).await.unwrap());
    let mut rows = conn
        .query(
            "SELECT value FROM metadata WHERE key = ?1",
            (super::GRAPH_GENERATION_SCHEMA_KEY,),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("generation stamp");
    assert_eq!(
        row.get::<String>(0).unwrap(),
        super::LATEST_VERSION.to_string()
    );
}

#[tokio::test]
async fn compatible_migration_skips_generation_stamp_without_graph_metadata() {
    let (conn, _dir) = create_raw_db().await;
    create_v19_memory_schema_for_v20_test(&conn).await;
    assert!(!table_exists(&conn, "metadata").await);

    assert!(
        migrate_connection(&conn)
            .await
            .expect("a memory-only database owns no graph generation to stamp")
    );
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert!(
        !table_exists(&conn, "metadata").await,
        "a compatible migration must not invent graph metadata in a memory-only database"
    );
}

#[tokio::test]
async fn interrupted_fresh_schema_rolls_back_ddl_and_version_before_retry() {
    let (conn, _dir) = create_raw_db().await;
    super::configure_fresh_auto_vacuum(&conn, "test interrupted fresh schema")
        .await
        .unwrap();

    let transaction = conn.schema_migration_transaction().await.unwrap();
    super::create_schema_transaction(&transaction)
        .await
        .unwrap();
    assert_eq!(
        super::get_version(&transaction).await.unwrap(),
        super::LATEST_VERSION
    );
    transaction.rollback().await.unwrap();

    assert_eq!(get_user_version(&conn).await, 0);
    assert!(!table_exists(&conn, "nodes").await);

    assert!(migrate_connection(&conn).await.unwrap());
    assert_eq!(get_user_version(&conn).await, super::LATEST_VERSION);
    assert!(column_exists(&conn, "nodes", "branches").await);
    assert!(column_exists(&conn, "nodes", "unsafe_blocks").await);
}

#[tokio::test]
async fn exclusive_maintenance_completes_deferred_auto_vacuum_repair() {
    let (conn, dir) = create_raw_db().await;
    let db_path = dir.path().join("test.db");
    create_schema_connection(&conn).await.unwrap();
    drop(conn);
    let setup = rusqlite::Connection::open(&db_path).unwrap();
    setup
        .execute_batch("PRAGMA auto_vacuum = NONE; VACUUM;")
        .unwrap();
    drop(setup);
    let conn = TestConnection::open_with_write_authority(&db_path, Arc::new(AllowMigrationWrites));
    assert_eq!(super::auto_vacuum_mode(&*conn, "test").await.unwrap(), 0);

    assert!(!migrate_connection(&conn).await.unwrap());
    assert_eq!(super::auto_vacuum_mode(&*conn, "test").await.unwrap(), 0);
    drop(conn);

    let profile_root = dir.path().canonicalize().unwrap();
    let lease = acquire_exclusive_for_profile(&profile_root, "auto-vacuum repair fixture").unwrap();
    let _scope =
        enter_maintenance_database_scope(&lease, &profile_root, "auto-vacuum repair fixture")
            .unwrap();
    let authority = DatabaseAuthority::for_runtime(&db_path, "auto-vacuum repair fixture").unwrap();
    assert_eq!(authority.role(), DatabaseAuthorityRole::Maintenance);
    let (database, _) = Database::publish_maintenance_test_runtime(
        &db_path,
        &authority,
        TestDatabaseRuntimeMode::Existing,
    )
    .await
    .unwrap();

    assert!(
        !super::migrate_with_exclusive_maintenance(database)
            .await
            .unwrap()
    );
    let raw_mode: i64 = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row("PRAGMA auto_vacuum", (), |row| row.get(0))
        .unwrap();
    assert_eq!(raw_mode, 2, "the durable database file was not repaired");
}

/// Creates the V20 current projection shape so V21's additive compatibility
/// fields are tested against an already-dogfooded database.
async fn create_v20_current_projection_for_v21_test(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE memory_v2_facts (
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL,
            identity_json TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY(fact_id, owner_kind, project_id)
         );
         CREATE TABLE memory_v2_lineage_events (
            event_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            event_json TEXT NOT NULL,
            occurred_at INTEGER NOT NULL,
            recorded_at INTEGER NOT NULL,
            UNIQUE(event_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id)
         );
         CREATE TABLE memory_v2_current_facts (
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            payload_access TEXT NOT NULL,
            trust_score REAL,
            active_assertion_id TEXT,
            last_event_id TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(fact_id, owner_kind, project_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(last_event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id)
         );
         INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES ('fact.v20', 'profile', '', '{}', '{}', 100);
         INSERT INTO memory_v2_lineage_events(
            event_id, fact_id, owner_kind, project_id, event_json, occurred_at, recorded_at
         ) VALUES ('event.v20', 'fact.v20', 'profile', '', '{}', 110, 110);
         INSERT INTO memory_v2_current_facts(
            fact_id, owner_kind, project_id, payload_access, trust_score,
            active_assertion_id, last_event_id, updated_at
         ) VALUES ('fact.v20', 'profile', '', 'eligible', 0.5, NULL, 'event.v20', 110);",
    )
    .await
    .expect("failed to create v20 current projection fixture");
    set_user_version(conn, 20).await;
}

/// Builds the committed V21 shape without V22 compatibility additions.
async fn create_v21_current_projection_for_v22_test(conn: &Connection) {
    create_v20_current_projection_for_v21_test(conn).await;
    conn.execute_batch(
        "ALTER TABLE memory_v2_current_facts
             ADD COLUMN retrieval_count INTEGER NOT NULL DEFAULT 0 CHECK(retrieval_count >= 0);
         ALTER TABLE memory_v2_current_facts
             ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0 CHECK(access_count >= 0);
         ALTER TABLE memory_v2_current_facts
             ADD COLUMN helpful_count INTEGER NOT NULL DEFAULT 0 CHECK(helpful_count >= 0);
         ALTER TABLE memory_v2_current_facts
             ADD COLUMN unhelpful_count INTEGER NOT NULL DEFAULT 0 CHECK(unhelpful_count >= 0);
         ALTER TABLE memory_v2_current_facts ADD COLUMN last_retrieved_at INTEGER;
         ALTER TABLE memory_v2_current_facts ADD COLUMN last_recalled_at INTEGER;
         ALTER TABLE memory_v2_current_facts ADD COLUMN last_feedback_at INTEGER;
         ALTER TABLE memory_v2_current_facts
             ADD COLUMN projection_state TEXT NOT NULL DEFAULT 'unavailable'
                 CHECK(projection_state IN ('ready', 'rebuilding', 'stale', 'unavailable'));
         ALTER TABLE memory_v2_current_facts
             ADD COLUMN vector_watermark_json TEXT
                 CHECK(vector_watermark_json IS NULL OR json_valid(vector_watermark_json));",
    )
    .await
    .expect("failed to create V21 current projection fixture");
    conn.execute_batch(
        "CREATE TABLE memory_v2_backfill_progress (
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL,
            source_store_id TEXT NOT NULL,
            feedback_frontier INTEGER NOT NULL,
            feedback_cursor INTEGER NOT NULL
         );
         INSERT INTO memory_v2_backfill_progress(
            owner_kind, project_id, owner_json, source_store_id,
            feedback_frontier, feedback_cursor
         ) VALUES('profile', '', '{}', 'legacy-memory-v1', 11, 11);",
    )
    .await
    .expect("failed to create completed V21 feedback-backfill fixture");
    set_user_version(conn, 21).await;
}

/// Reads the column names of `table` via PRAGMA `table_info`.
async fn column_names(conn: &Connection, table: &str) -> Vec<String> {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .expect("failed to read table_info");
    let mut names = Vec::new();
    while let Some(row) = rows.next().await.expect("failed to iterate table_info") {
        names.push(row.get::<String>(1).expect("failed to read column name"));
    }
    names
}

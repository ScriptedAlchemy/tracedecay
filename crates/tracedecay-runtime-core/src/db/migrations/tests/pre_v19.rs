//! Pre-v19 schema-version migration coverage plus general create/open tests.

use super::*;

/// `create_schema` on a fresh database sets `user_version` to latest and creates all tables.
#[tokio::test]
async fn test_create_schema_fresh_db() {
    let (conn, _dir) = create_raw_db().await;

    create_schema_connection(&conn)
        .await
        .expect("create_schema should succeed");

    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert!(table_exists(&conn, "external_source_states_v1").await);
    assert_eq!(
        scalar_i64(&conn, "PRAGMA auto_vacuum").await,
        2,
        "fresh databases should use incremental auto_vacuum for page reclamation"
    );
    assert!(table_exists(&conn, "nodes").await);
    assert!(table_exists(&conn, "edges").await);
    assert!(table_exists(&conn, "files").await);
    assert!(table_exists(&conn, "unresolved_refs").await);
    assert!(table_exists(&conn, "vectors").await);
    assert!(table_exists(&conn, "metadata").await);
    assert!(table_exists(&conn, "nodes_fts").await);
    assert!(!table_exists(&conn, "memory_decisions").await);
    assert!(!table_exists(&conn, "memory_code_areas").await);
    assert!(table_exists(&conn, "memory_facts").await);
    assert!(table_exists(&conn, "memory_entities").await);
    assert!(table_exists(&conn, "memory_fact_entities").await);
    assert!(table_exists(&conn, "memory_banks").await);
    assert!(table_exists(&conn, "memory_bank_dirty").await);
    assert!(table_exists(&conn, "memory_feedback_events").await);
    assert!(table_exists(&conn, "memory_fact_relations").await);
    assert!(table_exists(&conn, "memory_facts_fts").await);
    assert!(table_exists(&conn, "memory_v2_facts").await);
    assert!(table_exists(&conn, "memory_v2_assertions").await);
    assert!(table_exists(&conn, "memory_v2_assertion_payloads").await);
    assert!(table_exists(&conn, "memory_v2_evidence").await);
    assert!(table_exists(&conn, "memory_v2_lineage_events").await);
    assert!(table_exists(&conn, "retrieval_anchors").await);
    assert!(table_exists(&conn, "memory_v2_legacy_map").await);
    assert!(table_exists(&conn, "memory_v2_backfill_progress").await);
    assert!(table_exists(&conn, "memory_v2_proposals").await);
    assert!(table_exists(&conn, "memory_v2_proposal_transitions").await);
    assert!(table_exists(&conn, "memory_v2_proposal_current").await);
    assert!(table_exists(&conn, "memory_v2_compatibility_operation_receipts").await);
    assert!(table_exists(&conn, "memory_v2_legacy_feedback_event_map").await);
    assert!(table_exists(&conn, "memory_v2_feedback_history").await);
    assert!(table_exists(&conn, "memory_v2_feedback_history_repair_progress").await);
    assert!(table_exists(&conn, "memory_v2_fact_relations").await);
    assert!(table_exists(&conn, "memory_v2_compatibility_banks").await);
    assert!(table_exists(&conn, "memory_v2_compatibility_bank_dirty").await);
    assert!(table_exists(&conn, "redundancy_pairs").await);
    assert!(index_exists(&conn, "idx_redundancy_pairs_node_b").await);
    assert!(column_exists(&conn, "memory_v2_current_facts", "retrieval_count").await);
    assert!(column_exists(&conn, "memory_v2_current_facts", "access_count").await);
    assert!(column_exists(&conn, "memory_v2_current_facts", "helpful_count").await);
    assert!(column_exists(&conn, "memory_v2_current_facts", "unhelpful_count").await);
    assert!(column_exists(&conn, "memory_v2_current_facts", "projection_state").await);
    assert!(column_exists(&conn, "memory_v2_current_facts", "vector_watermark_json").await);
    assert!(column_exists(&conn, "memory_v2_fact_relations", "provenance_json").await);
    assert!(index_exists(&conn, "idx_memory_v2_current_compatibility_search").await);
    assert!(index_exists(&conn, "idx_memory_v2_current_projection_state").await);
    assert!(index_exists(&conn, "idx_memory_v2_compatibility_receipts_fact").await);
    assert!(index_exists(&conn, "idx_memory_v2_feedback_history_repair_pending").await);
    assert!(index_exists(&conn, "idx_memory_v2_fact_relations_source").await);
    assert!(index_exists(&conn, "idx_memory_v2_fact_relations_target").await);
    assert!(index_exists(&conn, "idx_memory_v2_compatibility_banks_owner").await);
    assert!(index_exists(&conn, "idx_memory_v2_compatibility_bank_dirty_owner").await);
}

/// `create_schema` is idempotent — calling it twice does not error.
#[tokio::test]
async fn test_create_schema_idempotent() {
    let (conn, _dir) = create_raw_db().await;

    create_schema_connection(&conn)
        .await
        .expect("first create_schema should succeed");
    create_schema_connection(&conn)
        .await
        .expect("second create_schema should succeed");

    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
}

#[tokio::test]
async fn v8_to_v9_creates_exact_objects_and_backfills_contains_edges() {
    let (conn, _dir) = create_raw_db().await;
    crate::db::migrations::migrate_test_connection_to_version(&conn, 8)
        .await
        .expect("build real v8 schema");
    conn.execute_batch(
        "INSERT INTO nodes (
            id, kind, name, qualified_name, file_path,
            start_line, end_line, start_column, end_column, updated_at
         ) VALUES
            ('parent', 'module', 'parent', 'parent', 'src/lib.rs', 1, 10, 0, 0, 1),
            ('child', 'function', 'child', 'parent::child', 'src/lib.rs', 2, 4, 0, 0, 1);
         INSERT INTO edges (source, target, kind, line)
         VALUES ('parent', 'child', 'contains', 2);",
    )
    .await
    .expect("seed v8 contains relationship");

    crate::db::migrations::migrate_test_connection_to_version(&conn, 9)
        .await
        .expect("migrate real v8 schema to v9");

    assert_eq!(get_user_version(&conn).await, 9);
    assert!(table_exists(&conn, "read_cache").await);
    assert!(index_exists(&conn, "idx_read_cache_session").await);
    assert_eq!(
        column_type_and_pk(&conn, "nodes", "parent_id").await,
        ("TEXT".to_string(), 0)
    );
    assert!(index_exists(&conn, "idx_nodes_parent_id").await);
    let mut rows = conn
        .query("SELECT parent_id FROM nodes WHERE id = 'child'", ())
        .await
        .expect("query migrated child");
    let row = rows
        .next()
        .await
        .expect("read migrated child")
        .expect("migrated child exists");
    assert_eq!(
        row.get::<Option<String>>(0).expect("read parent id"),
        Some("parent".to_owned())
    );
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM edges WHERE kind = 'contains'").await,
        0
    );
}

#[tokio::test]
async fn v8_to_v9_rolls_back_created_objects_when_backfill_fails() {
    let (conn, _dir) = create_raw_db().await;
    crate::db::migrations::migrate_test_connection_to_version(&conn, 8)
        .await
        .expect("build real v8 schema");
    conn.execute("DROP TABLE edges", ())
        .await
        .expect("remove required v8 backfill source");

    let error = crate::db::migrations::migrate_test_connection_to_version(&conn, 9)
        .await
        .expect_err("missing required v8 state must fail closed");

    assert!(
        error.to_string().contains("backfill parent_id"),
        "unexpected migration error: {error}"
    );
    assert_eq!(get_user_version(&conn).await, 8);
    assert!(!table_exists(&conn, "read_cache").await);
    assert!(!column_exists(&conn, "nodes", "parent_id").await);
    assert!(!index_exists(&conn, "idx_read_cache_session").await);
    assert!(!index_exists(&conn, "idx_nodes_parent_id").await);
}

#[tokio::test]
async fn v8_to_v9_rejects_exact_preadded_parent_column_before_mutation() {
    let (conn, _dir) = create_raw_db().await;
    crate::db::migrations::migrate_test_connection_to_version(&conn, 8)
        .await
        .expect("build real v8 schema");
    conn.execute("ALTER TABLE nodes ADD COLUMN parent_id TEXT", ())
        .await
        .expect("corrupt v8 schema with pre-added v9 column");

    let error = crate::db::migrations::migrate_test_connection_to_version(&conn, 9)
        .await
        .expect_err("malformed v8 schema must fail closed");

    assert!(
        error.to_string().contains("parent_id"),
        "unexpected migration error: {error}"
    );
    assert_eq!(
        get_user_version(&conn).await,
        8,
        "failed v9 migration must not publish its version"
    );
    assert!(
        !table_exists(&conn, "read_cache").await,
        "V9 admission must reject before creating read_cache"
    );
    assert!(column_exists(&conn, "nodes", "parent_id").await);
}

#[tokio::test]
async fn v8_to_v9_rejects_exact_precreated_read_cache_and_preserves_v8() {
    let (conn, _dir) = create_raw_db().await;
    crate::db::migrations::migrate_test_connection_to_version(&conn, 8)
        .await
        .expect("build real v8 schema");
    conn.execute(crate::db::migrations::V9_READ_CACHE_TABLE_SQL, ())
        .await
        .expect("plant exact-looking v9 table");
    conn.execute(
        "INSERT INTO read_cache VALUES (
            'project', 'session', 'src/lib.rs', 1, 'lines',
            'args', 'digest', X'01', 1, 1
        )",
        (),
    )
    .await
    .expect("plant sentinel row");

    let error = crate::db::migrations::migrate_test_connection_to_version(&conn, 9)
        .await
        .expect_err("every precreated v9 table must fail closed");

    assert!(
        error.to_string().contains("read_cache"),
        "unexpected migration error: {error}"
    );
    assert_eq!(get_user_version(&conn).await, 8);
    assert!(!column_exists(&conn, "nodes", "parent_id").await);
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM read_cache").await,
        1,
        "failed admission must preserve the original v8 database"
    );
}

#[tokio::test]
async fn v8_to_v9_rejects_exact_precreated_read_cache_index_before_mutation() {
    let (conn, _dir) = create_raw_db().await;
    crate::db::migrations::migrate_test_connection_to_version(&conn, 8)
        .await
        .expect("build real v8 schema");
    conn.execute(crate::db::migrations::V9_READ_CACHE_TABLE_SQL, ())
        .await
        .expect("plant exact-looking v9 table");
    conn.execute(crate::db::migrations::V9_READ_CACHE_SESSION_INDEX_SQL, ())
        .await
        .expect("plant exact-looking read_cache index");
    conn.execute(
        "INSERT INTO read_cache VALUES (
            'project', 'session', 'src/lib.rs', 1, 'lines',
            'args', 'digest', X'01', 1, 1
        )",
        (),
    )
    .await
    .expect("plant sentinel row");

    let error = crate::db::migrations::migrate_test_connection_to_version(&conn, 9)
        .await
        .expect_err("every precreated V9 index must fail closed");

    assert!(
        error.to_string().contains("idx_read_cache_session"),
        "unexpected migration error: {error}"
    );
    assert_eq!(get_user_version(&conn).await, 8);
    assert!(!column_exists(&conn, "nodes", "parent_id").await);
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM read_cache").await,
        1,
        "failed admission must preserve the original V8 database"
    );
    assert!(index_exists(&conn, "idx_read_cache_session").await);
}

/// migrate returns false when already at the latest version.
#[tokio::test]
async fn test_migrate_already_latest_returns_false() {
    let (conn, _dir) = create_schema_db().await;

    let migrated = migrate_connection(&conn)
        .await
        .expect("migrate should succeed");

    assert!(
        !migrated,
        "migrate should return false when already at latest"
    );
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
}

#[tokio::test]
async fn test_migrate_rejects_schema_newer_than_supported() {
    let (conn, _dir) = create_schema_db().await;
    set_user_version(&conn, LATEST_VERSION + 1).await;

    let error = migrate_connection(&conn)
        .await
        .expect_err("future schema versions must be rejected");

    assert!(
        error
            .to_string()
            .contains(&format!("newer than supported v{LATEST_VERSION}"))
    );
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION + 1);
}

/// migrate from v0 (completely empty database) applies all migrations to latest.
#[tokio::test]
async fn test_migrate_from_v0() {
    let (conn, _dir) = create_raw_db().await;

    // user_version defaults to 0 on a fresh database
    assert_eq!(get_user_version(&conn).await, 0);

    let migrated = migrate_connection(&conn)
        .await
        .expect("migrate from v0 should succeed");

    assert!(
        migrated,
        "migrate should return true when migrations were applied"
    );
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);

    // All expected tables should exist
    assert!(table_exists(&conn, "nodes").await);
    assert!(table_exists(&conn, "edges").await);
    assert!(table_exists(&conn, "files").await);
    assert!(table_exists(&conn, "unresolved_refs").await);
    assert!(table_exists(&conn, "vectors").await);
    assert!(table_exists(&conn, "metadata").await);
    assert!(table_exists(&conn, "nodes_fts").await);

    // V3 complexity columns should exist
    assert!(column_exists(&conn, "nodes", "branches").await);
    assert!(column_exists(&conn, "nodes", "loops").await);
    assert!(column_exists(&conn, "nodes", "returns").await);
    assert!(column_exists(&conn, "nodes", "max_nesting").await);

    // V4 safety columns should exist
    assert!(column_exists(&conn, "nodes", "unsafe_blocks").await);
    assert!(column_exists(&conn, "nodes", "unchecked_calls").await);
    assert!(column_exists(&conn, "nodes", "assertions").await);

    // V5 unique index should exist
    assert!(index_exists(&conn, "idx_edges_unique").await);
}

/// migrate from v1 (tables exist, no metadata, no complexity columns) to v5.
#[tokio::test]
async fn test_migrate_from_v1() {
    let (conn, _dir) = create_raw_db().await;
    create_v1_schema(&conn).await;

    assert_eq!(get_user_version(&conn).await, 1);
    assert!(!table_exists(&conn, "metadata").await);
    assert!(!column_exists(&conn, "nodes", "branches").await);

    let migrated = migrate_connection(&conn)
        .await
        .expect("migrate from v1 should succeed");

    assert!(migrated);
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);

    // V2: metadata table
    assert!(table_exists(&conn, "metadata").await);

    // V3: complexity columns
    assert!(column_exists(&conn, "nodes", "branches").await);
    assert!(column_exists(&conn, "nodes", "loops").await);
    assert!(column_exists(&conn, "nodes", "returns").await);
    assert!(column_exists(&conn, "nodes", "max_nesting").await);

    // V4: safety columns
    assert!(column_exists(&conn, "nodes", "unsafe_blocks").await);
    assert!(column_exists(&conn, "nodes", "unchecked_calls").await);
    assert!(column_exists(&conn, "nodes", "assertions").await);

    // V5: unique index
    assert!(index_exists(&conn, "idx_edges_unique").await);
}

/// migrate from v2 (has metadata, no complexity columns) to v5.
#[tokio::test]
async fn test_migrate_from_v2() {
    let (conn, _dir) = create_raw_db().await;
    create_v1_schema(&conn).await;
    apply_v2(&conn).await;

    assert_eq!(get_user_version(&conn).await, 2);
    assert!(table_exists(&conn, "metadata").await);
    assert!(!column_exists(&conn, "nodes", "branches").await);

    let migrated = migrate_connection(&conn)
        .await
        .expect("migrate from v2 should succeed");

    assert!(migrated);
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);

    // V3 columns
    assert!(column_exists(&conn, "nodes", "branches").await);
    assert!(column_exists(&conn, "nodes", "max_nesting").await);

    // V4 columns
    assert!(column_exists(&conn, "nodes", "unsafe_blocks").await);

    // V5 unique index
    assert!(index_exists(&conn, "idx_edges_unique").await);
}

/// migrate from v3 (has complexity columns, no safety columns) to v5.
#[tokio::test]
async fn test_migrate_from_v3() {
    let (conn, _dir) = create_raw_db().await;
    create_v1_schema(&conn).await;
    apply_v2(&conn).await;
    apply_v3(&conn).await;

    assert_eq!(get_user_version(&conn).await, 3);
    assert!(column_exists(&conn, "nodes", "branches").await);
    assert!(!column_exists(&conn, "nodes", "unsafe_blocks").await);

    let migrated = migrate_connection(&conn)
        .await
        .expect("migrate from v3 should succeed");

    assert!(migrated);
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);

    // V4 columns
    assert!(column_exists(&conn, "nodes", "unsafe_blocks").await);
    assert!(column_exists(&conn, "nodes", "unchecked_calls").await);
    assert!(column_exists(&conn, "nodes", "assertions").await);

    // V5 unique index
    assert!(index_exists(&conn, "idx_edges_unique").await);
}

/// migrate from v4 (has all columns, no edge dedup) to v5.
#[tokio::test]
async fn test_migrate_from_v4() {
    let (conn, _dir) = create_raw_db().await;
    create_v1_schema(&conn).await;
    apply_v2(&conn).await;
    apply_v3(&conn).await;
    apply_v4(&conn).await;

    assert_eq!(get_user_version(&conn).await, 4);
    assert!(!index_exists(&conn, "idx_edges_unique").await);

    let migrated = migrate_connection(&conn)
        .await
        .expect("migrate from v4 should succeed");

    assert!(migrated);
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);

    assert!(index_exists(&conn, "idx_edges_unique").await);
}

/// V5 migration actually deduplicates edge rows.
#[tokio::test]
async fn test_v5_deduplicates_edges() {
    let (conn, _dir) = create_raw_db().await;
    create_v1_schema(&conn).await;
    apply_v2(&conn).await;
    apply_v3(&conn).await;
    apply_v4(&conn).await;

    // Insert a node so foreign keys are satisfied
    conn.execute(
        "INSERT INTO nodes (id, kind, name, qualified_name, file_path, start_line, end_line, start_column, end_column, visibility, updated_at, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions) VALUES ('n1', 'function', 'foo', 'crate::foo', 'src/lib.rs', 1, 10, 0, 1, 'pub', 1000, 0, 0, 0, 0, 0, 0, 0)",
        (),
    )
    .await
    .expect("failed to insert node n1");

    conn.execute(
        "INSERT INTO nodes (id, kind, name, qualified_name, file_path, start_line, end_line, start_column, end_column, visibility, updated_at, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions) VALUES ('n2', 'function', 'bar', 'crate::bar', 'src/lib.rs', 11, 20, 0, 1, 'pub', 1000, 0, 0, 0, 0, 0, 0, 0)",
        (),
    )
    .await
    .expect("failed to insert node n2");

    // Insert duplicate edges (same source, target, kind, line)
    for _ in 0..5 {
        conn.execute(
            "INSERT INTO edges (source, target, kind, line) VALUES ('n1', 'n2', 'calls', 5)",
            (),
        )
        .await
        .expect("failed to insert duplicate edge");
    }

    // Also insert an edge with NULL line (duplicated)
    for _ in 0..3 {
        conn.execute(
            "INSERT INTO edges (source, target, kind, line) VALUES ('n1', 'n2', 'uses', NULL)",
            (),
        )
        .await
        .expect("failed to insert duplicate NULL-line edge");
    }

    // Verify duplicates exist before migration
    {
        let mut rows = conn
            .query("SELECT COUNT(*) FROM edges", ())
            .await
            .expect("failed to count edges");
        let row = rows
            .next()
            .await
            .expect("failed to read row")
            .expect("should have row");
        let count_before: i64 = row.get(0).expect("failed to read count");
        assert_eq!(
            count_before, 8,
            "should have 8 rows (5 + 3 duplicates) before migration"
        );
    }

    // Run migration (v4 -> v5)
    let migrated = migrate_connection(&conn)
        .await
        .expect("migrate from v4 should succeed");
    assert!(migrated);

    // After dedup, should have exactly 2 distinct edges
    let mut rows = conn
        .query("SELECT COUNT(*) FROM edges", ())
        .await
        .expect("failed to count edges after migration");
    let row = rows
        .next()
        .await
        .expect("failed to read row")
        .expect("should have row");
    let count_after: i64 = row.get(0).expect("failed to read count");
    assert_eq!(
        count_after, 2,
        "v5 migration should deduplicate to 2 distinct edges"
    );
}

/// After full migration from v0, all expected indexes exist.
#[tokio::test]
async fn test_indexes_exist_after_full_migration() {
    let (conn, _dir) = create_raw_db().await;

    migrate_connection(&conn)
        .await
        .expect("migrate from v0 should succeed");

    // Node indexes
    assert!(index_exists(&conn, "idx_nodes_kind").await);
    assert!(index_exists(&conn, "idx_nodes_name").await);
    assert!(index_exists(&conn, "idx_nodes_qualified_name").await);
    assert!(index_exists(&conn, "idx_nodes_file_path").await);
    assert!(index_exists(&conn, "idx_nodes_file_path_start_line").await);

    // Edge indexes
    assert!(index_exists(&conn, "idx_edges_source").await);
    assert!(index_exists(&conn, "idx_edges_target").await);
    assert!(index_exists(&conn, "idx_edges_kind").await);
    assert!(index_exists(&conn, "idx_edges_source_kind").await);
    assert!(index_exists(&conn, "idx_edges_target_kind").await);
    assert!(index_exists(&conn, "idx_edges_unique").await);

    // Unresolved refs indexes
    assert!(index_exists(&conn, "idx_unresolved_refs_from_node_id").await);
    assert!(index_exists(&conn, "idx_unresolved_refs_reference_name").await);
    assert!(index_exists(&conn, "idx_unresolved_refs_file_path").await);
}

/// Registered initialize mode creates a database at the latest schema version.
#[tokio::test]
async fn test_database_initialize_creates_latest_version() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("init_test.db");

    let (db, _migrated) =
        publish_test_database(&db_path, TestDatabaseRuntimeMode::Initialize).await;

    assert_eq!(get_user_version(db.conn()).await, LATEST_VERSION);
}

/// Rejoining an already-current registered runtime does not re-migrate.
#[tokio::test]
async fn test_database_open_no_migration_needed() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("open_test.db");

    // Seed and rejoin through the same retained canonical runtime. The second
    // facade must not reopen the physical database.
    let (_seed, _) = publish_test_database(&db_path, TestDatabaseRuntimeMode::Initialize).await;

    // Rejoin the same database — should not migrate.
    let (_db2, migrated) = publish_test_database(&db_path, TestDatabaseRuntimeMode::Existing).await;

    assert!(
        !migrated,
        "opening an already-current database should not trigger migration"
    );
}

/// Registered existing mode migrates a staged v1 database to the latest schema.
#[tokio::test]
async fn test_database_open_migrates_v1_to_latest() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("open_v1_test.db");

    // Create a v1 fixture on one engine-owned runtime, then release that
    // physical owner before opening through the public registered facade.
    {
        let setup = rusqlite::Connection::open(&db_path).expect("open v1 fixture");
        setup
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = ON;",
            )
            .expect("failed to apply pragmas");
        drop(setup);
        let conn = TestConnection::open(&db_path);
        create_v1_schema(&conn).await;
    }

    // Publish the staged legacy input into the typed runtime, which should
    // detect v1 and migrate to latest.
    let (db, migrated) = publish_test_database(&db_path, TestDatabaseRuntimeMode::Existing).await;

    assert!(migrated, "opening a v1 database should trigger migration");

    assert_eq!(get_user_version(db.conn()).await, LATEST_VERSION);
}

/// After `create_schema`, all v5 columns on nodes exist.
#[tokio::test]
async fn test_create_schema_has_all_node_columns() {
    let (conn, _dir) = create_schema_db().await;

    let expected_columns = [
        "id",
        "kind",
        "name",
        "qualified_name",
        "file_path",
        "start_line",
        "end_line",
        "start_column",
        "end_column",
        "docstring",
        "signature",
        "visibility",
        "is_async",
        "branches",
        "loops",
        "returns",
        "max_nesting",
        "unsafe_blocks",
        "unchecked_calls",
        "assertions",
        "updated_at",
        "attrs_start_line",
    ];
    for col in &expected_columns {
        assert!(
            column_exists(&conn, "nodes", col).await,
            "nodes table should have column '{col}' after create_schema"
        );
    }
}

/// V5 unique index prevents duplicate edge insertion.
#[tokio::test]
async fn test_v5_unique_index_prevents_duplicates() {
    let (conn, _dir) = create_schema_db().await;

    // Insert nodes for FK
    conn.execute(
        "INSERT INTO nodes (id, kind, name, qualified_name, file_path, start_line, end_line, start_column, end_column, visibility, updated_at, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions) VALUES ('a', 'function', 'a', 'crate::a', 'src/lib.rs', 1, 5, 0, 1, 'pub', 1000, 0, 0, 0, 0, 0, 0, 0)",
        (),
    )
    .await
    .expect("failed to insert node a");

    conn.execute(
        "INSERT INTO nodes (id, kind, name, qualified_name, file_path, start_line, end_line, start_column, end_column, visibility, updated_at, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions) VALUES ('b', 'function', 'b', 'crate::b', 'src/lib.rs', 6, 10, 0, 1, 'pub', 1000, 0, 0, 0, 0, 0, 0, 0)",
        (),
    )
    .await
    .expect("failed to insert node b");

    // First edge insertion should succeed
    conn.execute(
        "INSERT INTO edges (source, target, kind, line) VALUES ('a', 'b', 'calls', 3)",
        (),
    )
    .await
    .expect("first edge insert should succeed");

    // Duplicate insertion should fail due to unique index
    let result = conn
        .execute(
            "INSERT INTO edges (source, target, kind, line) VALUES ('a', 'b', 'calls', 3)",
            (),
        )
        .await;

    assert!(
        result.is_err(),
        "inserting a duplicate edge should fail with the v5 unique index"
    );
}

#[tokio::test]
async fn test_latest_schema_omits_legacy_memory_tables() {
    let (conn, _dir) = create_schema_db().await;

    assert!(!table_exists(&conn, "memory_decisions").await);
    assert!(!table_exists(&conn, "memory_code_areas").await);
    assert!(!table_exists(&conn, "memory_decisions_fts").await);
    assert!(table_exists(&conn, "memory_facts").await);
    assert!(table_exists(&conn, "memory_entities").await);
}

#[tokio::test]
async fn test_v7_to_latest_upgrade_path() {
    // Build a genuine v7 schema by running the real migrations up to v7, rather
    // than relabelling a latest-version database. A latest schema already
    // carries v9+ objects (e.g. `nodes.parent_id`), which the fail-closed v9
    // admission correctly rejects as precreated — so only a true historical v7
    // fixture actually exercises the v7 → latest upgrade path.
    let (conn, _dir) = create_raw_db().await;
    crate::db::migrations::migrate_test_connection_to_version(&conn, 7)
        .await
        .expect("build real v7 schema");
    assert_eq!(get_user_version(&conn).await, 7);

    let did_migrate = migrate_connection(&conn).await.unwrap();
    assert!(did_migrate, "expected migrate() to return true");

    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name IN \
             ('memory_decisions','memory_code_areas','memory_decisions_fts','read_cache') ORDER BY name",
            (),
        )
        .await
        .unwrap();
    let mut names = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        names.push(row.get::<String>(0).unwrap());
    }
    assert_eq!(names, vec!["read_cache"]);
}

/// V9 adds the `read_cache` table used by `tracedecay_read`.
#[tokio::test]
async fn test_migrate_v9_adds_read_cache() {
    let (conn, _dir) = create_raw_db().await;
    migrate_connection(&conn)
        .await
        .expect("migrate should succeed");

    assert!(
        table_exists(&conn, "read_cache").await,
        "v9 migration should create the read_cache table"
    );
    assert!(
        index_exists(&conn, "idx_read_cache_session").await,
        "v9 migration should create idx_read_cache_session"
    );
}

/// V16 adds the `redundancy_pairs` cache table on a database that predates it.
#[tokio::test]
async fn test_migrate_v16_adds_redundancy_pairs() {
    let (conn, _dir) = create_schema_db().await;

    // Rewind past v16: drop the table and step the version back to v15.
    conn.execute("DROP TABLE IF EXISTS redundancy_pairs", ())
        .await
        .expect("failed to drop redundancy_pairs for rewind");
    set_user_version(&conn, 15).await;
    assert!(!table_exists(&conn, "redundancy_pairs").await);

    let migrated = migrate_connection(&conn)
        .await
        .expect("v16 migration should apply");

    assert!(migrated, "expected migrate() to run the v16 addition");
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert!(
        table_exists(&conn, "redundancy_pairs").await,
        "v16 migration should create the redundancy_pairs table"
    );
    assert!(
        index_exists(&conn, "idx_redundancy_pairs_node_b").await,
        "v16 migration should create idx_redundancy_pairs_node_b"
    );
    assert_eq!(
        column_type_and_pk(&conn, "redundancy_pairs", "node_a_id").await,
        ("TEXT".to_string(), 1)
    );
    assert_eq!(
        column_type_and_pk(&conn, "redundancy_pairs", "node_b_id").await,
        ("TEXT".to_string(), 2)
    );
}

#[tokio::test]
async fn test_migrate_v18_preserves_memory_and_adds_bounded_relations() {
    let (conn, _dir) = create_schema_db().await;
    conn.execute(
        "INSERT INTO memory_facts (content, category, created_at, updated_at)
         VALUES ('preserved fact', 'project', 11, 11)",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO memory_entities (name, normalized_name, created_at, updated_at)
         VALUES ('Preserved', 'preserved', 12, 12)",
        (),
    )
    .await
    .unwrap();
    conn.execute("DROP TABLE memory_fact_relations", ())
        .await
        .unwrap();
    conn.execute("ALTER TABLE memory_entities DROP COLUMN updated_at", ())
        .await
        .unwrap();
    set_user_version(&conn, 17).await;

    assert!(migrate_connection(&conn).await.unwrap());

    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert!(table_exists(&conn, "memory_fact_relations").await);
    assert!(index_exists(&conn, "idx_memory_fact_relations_target").await);
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_facts").await,
        1
    );
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_entities").await,
        1
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT updated_at FROM memory_entities WHERE normalized_name = 'preserved'"
        )
        .await,
        12
    );
}

#[tokio::test]
async fn test_v11_create_schema_has_holographic_memory_schema() {
    let (conn, _dir) = create_schema_db().await;

    let mut rows = conn
        .query(
            "SELECT name FROM pragma_table_info('memory_facts') ORDER BY cid",
            (),
        )
        .await
        .unwrap();
    let mut cols = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        cols.push(row.get::<String>(0).unwrap());
    }
    assert_eq!(
        cols,
        vec![
            "fact_id",
            "content",
            "category",
            "tags",
            "trust_score",
            "retrieval_count",
            "access_count",
            "helpful_count",
            "unhelpful_count",
            "created_at",
            "updated_at",
            "last_retrieved_at",
            "last_recalled_at",
            "last_feedback_at",
            "source",
            "metadata",
            "hrr_vector",
            "hrr_algebra",
            "hrr_dim",
            "hrr_precision",
        ]
    );
    assert_eq!(
        column_type_and_pk(&conn, "memory_facts", "fact_id").await,
        ("INTEGER".to_string(), 1)
    );
    assert_eq!(
        column_type_and_pk(&conn, "memory_entities", "entity_id").await,
        ("INTEGER".to_string(), 1)
    );
    assert_eq!(
        column_type_and_pk(&conn, "memory_banks", "bank_id").await,
        ("INTEGER".to_string(), 1)
    );
    assert_eq!(
        column_type_and_pk(&conn, "memory_fact_entities", "fact_id").await,
        ("INTEGER".to_string(), 1)
    );
    assert_eq!(
        column_type_and_pk(&conn, "memory_fact_entities", "entity_id").await,
        ("INTEGER".to_string(), 2)
    );
    assert_eq!(
        column_type_and_pk(&conn, "memory_feedback_events", "fact_id").await,
        ("INTEGER".to_string(), 0)
    );

    for table in [
        "memory_entities",
        "memory_fact_entities",
        "memory_banks",
        "memory_feedback_events",
        "memory_facts_fts",
    ] {
        assert!(table_exists(&conn, table).await, "{table} should exist");
    }

    for index in [
        "idx_memory_facts_category",
        "idx_memory_facts_updated_at",
        "idx_memory_entities_type",
        "idx_memory_fact_entities_entity_id",
        "idx_memory_feedback_events_fact_id",
    ] {
        assert!(index_exists(&conn, index).await, "{index} should exist");
    }

    for trigger in [
        "memory_facts_fts_insert",
        "memory_facts_fts_delete",
        "memory_facts_fts_update",
    ] {
        assert!(
            trigger_exists(&conn, trigger).await,
            "{trigger} should exist"
        );
    }

    conn.execute(
        "INSERT INTO memory_facts (content, category) VALUES ('Default values matter', 'test')",
        (),
    )
    .await
    .expect("minimal memory_facts insert should use defaults");
    let fact_id = scalar_i64(&conn, "SELECT fact_id FROM memory_facts").await;
    assert!(fact_id > 0);

    let mut rows = conn
        .query(
            "SELECT tags, trust_score, retrieval_count, helpful_count, unhelpful_count, source, metadata, hrr_algebra, hrr_dim, hrr_precision FROM memory_facts WHERE fact_id=?1",
            (fact_id,),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "[]");
    assert!((row.get::<f64>(1).unwrap() - 0.5).abs() <= f64::EPSILON);
    assert_eq!(row.get::<i64>(2).unwrap(), 0);
    assert_eq!(row.get::<i64>(3).unwrap(), 0);
    assert_eq!(row.get::<i64>(4).unwrap(), 0);
    assert_eq!(row.get::<String>(5).unwrap(), "manual");
    assert_eq!(row.get::<String>(6).unwrap(), "{}");
    assert_eq!(row.get::<String>(7).unwrap(), "amari_fhrr");
    assert_eq!(row.get::<i64>(8).unwrap(), 2048);
    assert_eq!(row.get::<String>(9).unwrap(), "f32");
}

#[tokio::test]
async fn test_v10_to_v11_backfills_and_drops_legacy_memory_tables() {
    let (conn, _dir) = create_v10_db().await;

    assert_eq!(get_user_version(&conn).await, 10);
    assert!(table_exists(&conn, "memory_decisions").await);
    assert!(table_exists(&conn, "memory_code_areas").await);
    assert!(!table_exists(&conn, "memory_facts").await);

    let did_migrate = migrate_connection(&conn)
        .await
        .expect("v10 to v11 should migrate");

    assert!(did_migrate);
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert!(!table_exists(&conn, "memory_decisions").await);
    assert!(!table_exists(&conn, "memory_code_areas").await);
    assert!(table_exists(&conn, "memory_facts").await);
    assert!(table_exists(&conn, "memory_entities").await);
    assert!(table_exists(&conn, "memory_fact_entities").await);
    assert!(table_exists(&conn, "memory_banks").await);
    assert!(table_exists(&conn, "memory_bank_dirty").await);
    assert!(table_exists(&conn, "memory_feedback_events").await);
    assert!(table_exists(&conn, "memory_facts_fts").await);
}

#[tokio::test]
async fn test_v11_database_migrates_to_monotonic_v12() {
    let (conn, _dir) = create_schema_db().await;
    set_user_version(&conn, 11).await;

    let did_migrate = migrate_connection(&conn)
        .await
        .expect("v11 to v12 should migrate");

    assert!(did_migrate);
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert!(table_exists(&conn, "memory_bank_dirty").await);
}

#[tokio::test]
async fn test_v11_feedback_events_enforce_action_and_cascade_with_facts() {
    let (conn, _dir) = create_schema_db().await;

    conn.execute(
        "INSERT INTO memory_facts (content, category) VALUES ('Feedback fact', 'test')",
        (),
    )
    .await
    .expect("failed to insert memory fact");
    let fact_id = scalar_i64(&conn, "SELECT fact_id FROM memory_facts").await;
    conn.execute(
        "INSERT INTO memory_feedback_events (fact_id, action, trust_delta, old_trust, new_trust, note)
         VALUES (?1, 'helpful', 0.1, 0.5, 0.6, 'worked')",
        (fact_id,),
    )
    .await
    .expect("valid feedback action should insert");

    let invalid = conn
        .execute(
            "INSERT INTO memory_feedback_events (fact_id, action, trust_delta, old_trust, new_trust)
             VALUES (?1, 'neutral', 0.0, 0.5, 0.5)",
            (fact_id,),
        )
        .await;
    assert!(invalid.is_err(), "invalid feedback action should fail");

    let mut rows = conn
        .query(
            "SELECT source FROM memory_feedback_events WHERE fact_id=?1",
            (fact_id,),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "mcp");

    conn.execute("DELETE FROM memory_facts WHERE fact_id=?1", (fact_id,))
        .await
        .expect("deleting memory fact should cascade");
    assert_eq!(
        {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM memory_feedback_events WHERE fact_id=?1",
                    (fact_id,),
                )
                .await
                .unwrap();
            let row = rows.next().await.unwrap().unwrap();
            row.get::<i64>(0).unwrap()
        },
        0
    );
}

#[tokio::test]
async fn test_v11_backfills_legacy_memory_decisions_as_facts() {
    let (conn, _dir) = create_v10_db().await;
    conn.execute(
        "INSERT INTO memory_decisions (text, reason, created_at, files, tags)
         VALUES ('Prefer native SQLite migrations', 'Keeps install path simple', 1234, '[\"src/db/migrations.rs\"]', '[\"db\",\"memory\"]')",
        (),
    )
    .await
    .expect("failed to insert legacy decision");

    migrate_connection(&conn)
        .await
        .expect("v11 migration should backfill");

    let mut rows = conn
        .query(
            "SELECT fact_id, content, tags, metadata FROM memory_facts WHERE category='decision'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let fact_id = row.get::<i64>(0).unwrap();
    let content = row.get::<String>(1).unwrap();
    let tags = row.get::<String>(2).unwrap();
    let metadata = row.get::<String>(3).unwrap();

    assert!(fact_id > 0);
    assert!(content.contains("Prefer native SQLite migrations"));
    assert!(content.contains("Keeps install path simple"));
    assert_eq!(tags, "[\"db\",\"memory\"]");
    assert!(!metadata.contains("legacy-decision-"));
    assert!(metadata.contains("holographic_memory_backfill_v1"));
    assert!(metadata.contains("memory_decisions"));
    assert!(metadata.contains("\"legacy_id\":1"));
    assert!(metadata.contains("\"decision_text\":\"Prefer native SQLite migrations\""));
    assert!(metadata.contains("src/db/migrations.rs"));
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*)
             FROM memory_fact_entities fe
             JOIN memory_entities e ON e.entity_id = fe.entity_id
             WHERE fe.fact_id = 1
               AND e.normalized_name IN ('src/db/migrations.rs', 'db', 'memory')"
        )
        .await,
        3
    );
    assert_backfilled_memory_has_vectors_and_banks(&conn, "decision", 1).await;
}

#[tokio::test]
async fn test_v11_backfills_legacy_memory_code_areas_as_facts() {
    let (conn, _dir) = create_v10_db().await;
    conn.execute(
        "INSERT INTO memory_code_areas (path, description, last_touched_at, touch_count)
         VALUES ('src/db/migrations.rs', 'Schema migration code', 5678, 3)",
        (),
    )
    .await
    .expect("failed to insert legacy code area");

    migrate_connection(&conn)
        .await
        .expect("v11 migration should backfill");

    let mut rows = conn
        .query(
            "SELECT fact_id, content, tags, metadata FROM memory_facts WHERE category='code_area'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let fact_id = row.get::<i64>(0).unwrap();
    let content = row.get::<String>(1).unwrap();
    let tags = row.get::<String>(2).unwrap();
    let metadata = row.get::<String>(3).unwrap();

    assert!(fact_id > 0);
    assert!(content.contains("src/db/migrations.rs"));
    assert!(content.contains("Schema migration code"));
    assert!(tags.contains("code_area"));
    assert!(tags.contains("src/db/migrations.rs"));
    assert!(!metadata.contains("legacy-code-area-"));
    assert!(metadata.contains("holographic_memory_backfill_v1"));
    assert!(metadata.contains("memory_code_areas"));
    assert!(metadata.contains("\"legacy_id\":1"));
    assert!(metadata.contains("touch_count"));
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*)
             FROM memory_fact_entities fe
             JOIN memory_entities e ON e.entity_id = fe.entity_id
             WHERE fe.fact_id = 1
               AND e.normalized_name = 'src/db/migrations.rs'"
        )
        .await,
        1
    );
    assert_backfilled_memory_has_vectors_and_banks(&conn, "code_area", 1).await;
}

#[tokio::test]
async fn test_v11_backfill_is_idempotent_when_migration_reruns() {
    let (conn, _dir) = create_v10_db().await;
    conn.execute(
        "INSERT INTO memory_decisions (text, reason, created_at, tags)
         VALUES ('Avoid duplicate facts', 'Content has a unique constraint', 1000, '[\"dedupe\"]')",
        (),
    )
    .await
    .expect("failed to insert legacy decision");
    conn.execute(
        "INSERT INTO memory_code_areas (path, description, last_touched_at)
         VALUES ('src/memory.rs', 'Legacy memory facade', 1000)",
        (),
    )
    .await
    .expect("failed to insert legacy code area");

    migrate_connection(&conn)
        .await
        .expect("first v11 migration should succeed");
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_facts").await,
        2
    );

    set_user_version(&conn, 10).await;
    migrate_connection(&conn)
        .await
        .expect("rerunning v11 migration should succeed");

    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_facts").await,
        2
    );
}

#[tokio::test]
async fn test_v11_backfill_handles_malformed_and_blank_legacy_json() {
    let (conn, _dir) = create_v10_db().await;
    conn.execute(
        "INSERT INTO memory_decisions (text, reason, created_at, files, tags)
         VALUES ('Bad JSON is normalized', '', 1000, '[invalid json', 'not-an-array')",
        (),
    )
    .await
    .expect("failed to insert bad-json legacy decision");
    conn.execute(
        "INSERT INTO memory_code_areas (path, description, last_touched_at, touch_count)
         VALUES ('src/blank.rs', '', 1001, 1)",
        (),
    )
    .await
    .expect("failed to insert blank legacy code area");

    migrate_connection(&conn)
        .await
        .expect("v11 migration should tolerate malformed legacy JSON");

    let mut rows = conn
        .query(
            "SELECT content, tags, metadata FROM memory_facts WHERE category='decision'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let content = row.get::<String>(0).unwrap();
    let tags = row.get::<String>(1).unwrap();
    let metadata = row.get::<String>(2).unwrap();
    assert!(content.contains("Bad JSON is normalized"));
    assert!(!content.contains("Reason:"));
    assert_eq!(tags, "[]");
    assert!(metadata.contains("\"files\":[]"));
    assert!(metadata.contains("\"tags\":[]"));
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*)
             FROM memory_fact_entities fe
             JOIN memory_facts f ON f.fact_id = fe.fact_id
             WHERE f.category = 'decision'"
        )
        .await,
        0
    );

    let mut rows = conn
        .query(
            "SELECT content FROM memory_facts WHERE category='code_area'",
            (),
        )
        .await
        .unwrap();
    let content = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    assert!(content.contains("src/blank.rs"));
    assert!(!content.contains("\n\n\n"));
}

#[tokio::test]
async fn test_v11_backfill_preserves_duplicate_legacy_content() {
    let (conn, _dir) = create_v10_db().await;
    for tag in ["rust", "performance"] {
        conn.execute(
            "INSERT INTO memory_decisions (text, reason, created_at, files, tags)
             VALUES ('Use Rust', 'same reason', 1000, '[]', json_array(?1))",
            (tag,),
        )
        .await
        .expect("failed to insert duplicate legacy decision");
    }

    migrate_connection(&conn)
        .await
        .expect("v11 migration should backfill");

    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts WHERE category='decision'"
        )
        .await,
        2
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(DISTINCT content) FROM memory_facts WHERE category='decision'"
        )
        .await,
        2
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*)
             FROM memory_fact_entities fe
             JOIN memory_entities e ON e.entity_id = fe.entity_id
             WHERE e.normalized_name IN ('rust', 'performance')"
        )
        .await,
        2
    );
}

/// v13 archive-column cleanup must handle the odd dev-DB state where the
/// abandoned archive revision left `superseded_by` as a generated column
/// referencing `merged_into`: `SQLite` refuses to drop `merged_into` while the
/// generated column still references it, so the migration has to drop the
/// dependent column first. Regression test for the "no such column" failure.
#[tokio::test]
async fn test_v13_drops_archive_columns_with_generated_column_dependency() {
    let (conn, _dir) = create_schema_db().await;

    // Recreate the abandoned archive-revision shape, with superseded_by as a
    // VIRTUAL generated column that references merged_into.
    conn.execute_batch(
        "ALTER TABLE memory_facts ADD COLUMN state TEXT NOT NULL DEFAULT 'active';
         ALTER TABLE memory_facts ADD COLUMN archived_at INTEGER;
         ALTER TABLE memory_facts ADD COLUMN archived_reason TEXT;
         ALTER TABLE memory_facts ADD COLUMN merged_into INTEGER;
         ALTER TABLE memory_facts ADD COLUMN superseded_by INTEGER
             GENERATED ALWAYS AS (merged_into) VIRTUAL;
         CREATE INDEX IF NOT EXISTS idx_memory_facts_state
             ON memory_facts(state);",
    )
    .await
    .expect("failed to seed archive-revision columns");
    conn.execute(
        "INSERT INTO memory_facts (content, category) VALUES ('Archived-era fact', 'general')",
        (),
    )
    .await
    .expect("failed to insert fixture fact");
    set_user_version(&conn, 12).await;

    let migrated = migrate_connection(&conn)
        .await
        .expect("v13 must drop archive columns even with a generated-column dependency");
    assert!(migrated, "expected migrate() to run the v13 cleanup");
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);

    let columns = column_names(&conn, "memory_facts").await;
    for col in [
        "state",
        "archived_at",
        "archived_reason",
        "merged_into",
        "superseded_by",
    ] {
        assert!(
            !columns.iter().any(|c| c == col),
            "archive column `{col}` must be dropped by v13; remaining: {columns:?}"
        );
    }
    // The data row survives the column drops.
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_facts").await,
        1
    );
}

/// v14 adds the access-tracking columns (`access_count`, `last_recalled_at`)
/// and the `memory_oplog` table to databases stuck at the v13 shape, and is
/// idempotent for databases that already carry both (fresh schema, or a
/// re-run after a partial upgrade).
#[tokio::test]
async fn test_v14_adds_access_tracking_and_oplog() {
    let (conn, _dir) = create_schema_db().await;

    // Rewind to the v13 shape: no access columns, no oplog table.
    conn.execute_batch(
        "ALTER TABLE memory_facts DROP COLUMN access_count;
         ALTER TABLE memory_facts DROP COLUMN last_recalled_at;
         DROP TABLE memory_oplog;
         DROP INDEX IF EXISTS idx_memory_oplog_ts;",
    )
    .await
    .expect("failed to rewind to the v13 shape");
    conn.execute(
        "INSERT INTO memory_facts (content, category) VALUES ('Pre-v14 fact', 'general')",
        (),
    )
    .await
    .expect("failed to insert fixture fact");
    set_user_version(&conn, 13).await;

    let migrated = migrate_connection(&conn)
        .await
        .expect("v14 must apply cleanly");
    assert!(migrated, "expected migrate() to run the v14 additions");
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);

    let columns = column_names(&conn, "memory_facts").await;
    for col in ["access_count", "last_recalled_at"] {
        assert!(
            columns.iter().any(|c| c == col),
            "v14 must add `{col}`; present: {columns:?}"
        );
    }
    // Pre-existing rows pick up the defaults.
    assert_eq!(
        scalar_i64(&conn, "SELECT access_count FROM memory_facts LIMIT 1").await,
        0
    );
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_oplog").await,
        0
    );

    // Idempotence: re-running v14 against the already-upgraded shape must
    // not fail or duplicate anything.
    set_user_version(&conn, 13).await;
    let migrated_again = migrate_connection(&conn)
        .await
        .expect("v14 must be idempotent on an already-upgraded schema");
    assert!(migrated_again);
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_facts").await,
        1
    );
}

#[tokio::test]
async fn test_v15_compacts_legacy_f64_vectors_without_open_time_vacuum() {
    let (conn, dir) = create_schema_db().await;
    let db_path = dir.path().join("test.db");
    let legacy_vector = vec![0.0_f64; crate::memory::encoding::HolographicEncoder::DIMENSIONS];
    let legacy_bytes = bincode::serialize(&legacy_vector).unwrap();
    assert_eq!(legacy_bytes.len(), 16_392);

    conn.execute("ALTER TABLE memory_facts DROP COLUMN hrr_precision", ())
        .await
        .expect("failed to rewind hrr_precision column");
    conn.execute(
        "INSERT INTO memory_facts (content, category, hrr_vector, hrr_algebra, hrr_dim)
         VALUES ('Pre-v15 compact me', 'general', ?1, 'amari_fhrr', 2048)",
        (legacy_bytes,),
    )
    .await
    .expect("failed to seed legacy f64 vector");
    set_user_version(&conn, 14).await;
    drop(conn);
    let setup = rusqlite::Connection::open(&db_path).expect("open legacy auto-vacuum fixture");
    setup
        .execute_batch("PRAGMA auto_vacuum = NONE; VACUUM;")
        .expect("failed to simulate a legacy database without incremental auto-vacuum");
    drop(setup);
    let conn = TestConnection::open(&db_path);

    let migrated = migrate_connection(&conn)
        .await
        .expect("v15 must compact legacy vectors");
    assert!(migrated);
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert_eq!(
        scalar_i64(&conn, "PRAGMA auto_vacuum").await,
        0,
        "ordinary migration must defer whole-file auto_vacuum repair"
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts
             WHERE hrr_precision = 'f32' AND length(hrr_vector) = 8200"
        )
        .await,
        1,
        "v15 should backfill legacy f64 blobs into compact f32 blobs"
    );

    set_user_version(&conn, 14).await;
    let migrated_again = migrate_connection(&conn)
        .await
        .expect("v15 should be idempotent on compacted rows");
    assert!(migrated_again);
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts
             WHERE hrr_precision = 'f32' AND length(hrr_vector) = 8200"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn test_latest_open_defers_incremental_vacuum_repair() {
    let (conn, dir) = create_schema_db().await;
    let db_path = dir.path().join("test.db");
    set_user_version(&conn, LATEST_VERSION).await;
    drop(conn);
    let setup = rusqlite::Connection::open(&db_path).expect("open pre-repair auto-vacuum fixture");
    setup
        .execute_batch(
            "PRAGMA auto_vacuum = NONE;
             VACUUM;",
        )
        .expect("failed to simulate pre-repair auto_vacuum mode");
    drop(setup);
    let conn = TestConnection::open(&db_path);
    assert_eq!(
        scalar_i64(&conn, "PRAGMA auto_vacuum").await,
        0,
        "fixture should start as an already-latest database without incremental auto_vacuum"
    );

    let migrated = migrate_connection(&conn)
        .await
        .expect("latest-version open should defer incremental auto_vacuum repair");

    assert!(
        !migrated,
        "auto_vacuum repair should not report a schema migration"
    );
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert_eq!(
        scalar_i64(&conn, "PRAGMA auto_vacuum").await,
        0,
        "ordinary open must not run a whole-file VACUUM"
    );
}

#[tokio::test]
async fn test_v25_adds_external_source_state_without_touching_existing_rows() {
    let (conn, _dir) = create_schema_db().await;
    conn.execute(
        "INSERT INTO metadata(key, value) VALUES('migration.fixture', 'retained')",
        (),
    )
    .await
    .unwrap();
    conn.execute("DROP TABLE external_source_states_v1", ())
        .await
        .unwrap();
    set_user_version(&conn, 24).await;

    assert!(migrate_connection(&conn).await.unwrap());
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert!(table_exists(&conn, "external_source_states_v1").await);
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM metadata
             WHERE key = 'migration.fixture' AND value = 'retained'"
        )
        .await,
        1
    );
}

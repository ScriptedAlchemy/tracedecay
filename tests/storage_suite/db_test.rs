#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use tracedecay::db::Database;
use tracedecay::types::*;

use crate::support;
use crate::support::sample_node;

/// Helper: create an empty latest-schema temp database and return
/// (Database, TempDir). Seeded from the cached template rather than running
/// `Database::initialize` per test; `test_initialize_creates_database` still
/// covers the real initialize path.
async fn setup_db() -> (Database, TempDir) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    support::seed_latest_graph_db(&db_path).await;
    let (db, migrated) = crate::common::open_test_database(&db_path)
        .await
        .expect("failed to open template database");
    assert!(!migrated, "template database should not require migration");
    (db, dir)
}

#[tokio::test]
async fn test_initialize_creates_database() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("subdir").join("code_graph.db");
    let (_db, _) = crate::common::initialize_test_database(&db_path)
        .await
        .expect("failed to initialize database");
    assert!(
        db_path.exists(),
        "database file should exist after initialize"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_open_read_only_reads_existing_database_without_write_pragmas() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("code_graph.db");
    let (db, _) = crate::common::initialize_test_database(&db_path)
        .await
        .expect("failed to initialize database");
    db.insert_node(&sample_node("node-1", "process_data", "src/main.rs"))
        .await
        .expect("failed to insert node");
    db.checkpoint()
        .await
        .expect("failed to checkpoint database");
    db.close();
    let mut permissions = std::fs::metadata(&db_path)
        .expect("failed to stat database")
        .permissions();
    permissions.set_mode(0o444);
    std::fs::set_permissions(&db_path, permissions).expect("failed to mark database readonly");

    let (db, migrated) = crate::common::open_test_database_read_only(&db_path)
        .await
        .expect("readonly database should open");
    let stats = db
        .get_stats()
        .await
        .expect("readonly stats should be available");

    assert!(!migrated);
    assert_eq!(stats.node_count, 1);
}

#[tokio::test]
async fn test_insert_and_get_node() {
    let (db, _dir) = setup_db().await;
    let node = sample_node("node-1", "process_data", "src/main.rs");

    db.insert_node(&node).await.expect("failed to insert node");

    let fetched = db
        .get_node_by_id("node-1")
        .await
        .expect("failed to get node")
        .expect("node should exist");

    assert_eq!(fetched.id, "node-1");
    assert_eq!(fetched.name, "process_data");
    assert_eq!(fetched.kind, NodeKind::Function);
    assert_eq!(fetched.qualified_name, "crate::process_data");
    assert_eq!(fetched.file_path, "src/main.rs");
    assert_eq!(fetched.start_line, 1);
    assert_eq!(fetched.end_line, 10);
    assert_eq!(fetched.signature, Some("fn process_data()".to_string()));
    assert_eq!(
        fetched.docstring,
        Some("Documentation for process_data".to_string())
    );
    assert_eq!(fetched.visibility, Visibility::Pub);
    assert!(!fetched.is_async);
    assert_eq!(fetched.updated_at, 1000);
}

#[tokio::test]
async fn test_insert_and_get_edge() {
    let (db, _dir) = setup_db().await;
    let node_a = sample_node("node-a", "caller", "src/lib.rs");
    let node_b = sample_node("node-b", "callee", "src/lib.rs");

    db.insert_node(&node_a)
        .await
        .expect("failed to insert node a");
    db.insert_node(&node_b)
        .await
        .expect("failed to insert node b");

    let edge = Edge {
        source: "node-a".to_string(),
        target: "node-b".to_string(),
        kind: EdgeKind::Calls,
        line: Some(5),
    };
    db.insert_edge(&edge).await.expect("failed to insert edge");

    // Outgoing from node-a
    let outgoing = db
        .get_outgoing_edges("node-a", &[])
        .await
        .expect("failed to get outgoing edges");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].source, "node-a");
    assert_eq!(outgoing[0].target, "node-b");
    assert_eq!(outgoing[0].kind, EdgeKind::Calls);
    assert_eq!(outgoing[0].line, Some(5));

    // Incoming to node-b
    let incoming = db
        .get_incoming_edges("node-b", &[])
        .await
        .expect("failed to get incoming edges");
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].source, "node-a");

    // Filter by kind — should match
    let filtered = db
        .get_outgoing_edges("node-a", &[EdgeKind::Calls])
        .await
        .expect("failed to get filtered edges");
    assert_eq!(filtered.len(), 1);

    // Filter by wrong kind — should be empty
    let empty = db
        .get_outgoing_edges("node-a", &[EdgeKind::Uses])
        .await
        .expect("failed to get filtered edges");
    assert!(empty.is_empty());
}

#[tokio::test]
async fn test_upsert_file() {
    let (db, _dir) = setup_db().await;

    let file = FileRecord {
        path: "src/main.rs".to_string(),
        content_hash: "abc123".to_string(),
        size: 4096,
        modified_at: 1000,
        indexed_at: 2000,
        node_count: 5,
    };

    db.upsert_file(&file).await.expect("failed to upsert file");

    let fetched = db
        .get_file("src/main.rs")
        .await
        .expect("failed to get file")
        .expect("file should exist");

    assert_eq!(fetched.path, "src/main.rs");
    assert_eq!(fetched.content_hash, "abc123");
    assert_eq!(fetched.size, 4096);
    assert_eq!(fetched.modified_at, 1000);
    assert_eq!(fetched.indexed_at, 2000);
    assert_eq!(fetched.node_count, 5);

    // Upsert again with different hash — should replace
    let updated_file = FileRecord {
        path: "src/main.rs".to_string(),
        content_hash: "def456".to_string(),
        size: 8192,
        modified_at: 3000,
        indexed_at: 4000,
        node_count: 10,
    };
    db.upsert_file(&updated_file)
        .await
        .expect("failed to upsert file");

    let fetched2 = db
        .get_file("src/main.rs")
        .await
        .expect("failed to get file")
        .expect("file should exist");
    assert_eq!(fetched2.content_hash, "def456");
    assert_eq!(fetched2.size, 8192);
}

#[tokio::test]
async fn test_fts_search() {
    let (db, _dir) = setup_db().await;

    let node = sample_node("fts-node", "process_request", "src/handler.rs");
    db.insert_node(&node).await.expect("failed to insert node");

    let results = db
        .search_nodes("process", 10)
        .await
        .expect("failed to search nodes");
    assert!(
        !results.is_empty(),
        "FTS search for 'process' should find 'process_request'"
    );
    assert_eq!(results[0].node.id, "fts-node");
    assert!(results[0].score > 0.0);
}

#[tokio::test]
async fn test_get_stats() {
    let (db, _dir) = setup_db().await;

    let node = sample_node("stats-node", "my_func", "src/lib.rs");
    db.insert_node(&node).await.expect("failed to insert node");

    let stats = db.get_stats().await.expect("failed to get stats");
    assert_eq!(stats.node_count, 1);
    assert_eq!(stats.edge_count, 0);
    assert_eq!(stats.file_count, 0);
    assert_eq!(
        stats.nodes_by_kind.get("function"),
        Some(&1),
        "should have 1 function node"
    );
    assert!(stats.db_size_bytes > 0);
}

#[tokio::test]
async fn test_delete_nodes_by_file() {
    let (db, _dir) = setup_db().await;

    let node1 = sample_node("del-1", "func_a", "src/target.rs");
    let node2 = sample_node("del-2", "func_b", "src/target.rs");
    let node_other = sample_node("del-3", "func_c", "src/other.rs");

    db.insert_nodes(&[node1, node2, node_other])
        .await
        .expect("failed to insert nodes");

    // Insert an edge between the target nodes
    let edge = Edge {
        source: "del-1".to_string(),
        target: "del-2".to_string(),
        kind: EdgeKind::Calls,
        line: None,
    };
    db.insert_edge(&edge).await.expect("failed to insert edge");

    // Delete nodes for src/target.rs
    db.delete_nodes_by_file("src/target.rs")
        .await
        .expect("failed to delete nodes by file");

    // Verify they are gone
    let nodes = db
        .get_nodes_by_file("src/target.rs")
        .await
        .expect("failed to get nodes by file");
    assert!(nodes.is_empty(), "nodes for target.rs should be deleted");

    // Verify the other file's node is still there
    let other_nodes = db
        .get_nodes_by_file("src/other.rs")
        .await
        .expect("failed to get nodes by file");
    assert_eq!(other_nodes.len(), 1);
    assert_eq!(other_nodes[0].id, "del-3");
}

#[tokio::test]
async fn test_unresolved_refs() {
    let (db, _dir) = setup_db().await;

    // Insert a node first (FK constraint)
    let node = sample_node("ref-node", "my_func", "src/lib.rs");
    db.insert_node(&node).await.expect("failed to insert node");

    let uref = UnresolvedRef {
        from_node_id: "ref-node".to_string(),
        reference_name: "HashMap".to_string(),
        reference_kind: EdgeKind::Uses,
        line: 10,
        column: 5,
        file_path: "src/lib.rs".to_string(),
    };

    db.insert_unresolved_ref(&uref)
        .await
        .expect("failed to insert unresolved ref");

    let refs = db
        .get_unresolved_refs()
        .await
        .expect("failed to get unresolved refs");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].from_node_id, "ref-node");
    assert_eq!(refs[0].reference_name, "HashMap");
    assert_eq!(refs[0].reference_kind, EdgeKind::Uses);
    assert_eq!(refs[0].line, 10);
    assert_eq!(refs[0].column, 5);
    assert_eq!(refs[0].file_path, "src/lib.rs");

    // Clear and verify
    db.clear_unresolved_refs()
        .await
        .expect("failed to clear unresolved refs");
    let refs_after = db
        .get_unresolved_refs()
        .await
        .expect("failed to get unresolved refs");
    assert!(refs_after.is_empty());
}

/// A first index of a large repository leaves far more unresolved references
/// than the `SQLite` runtime will materialize for a single query, and the
/// runtime rejects an oversized query outright instead of truncating it.
/// Reading them back must page, or branch sync fails with
/// "migration SQL query materialization exceeded its limit".
#[tokio::test]
async fn unresolved_refs_read_back_beyond_the_runtime_query_limit() {
    /// The `SQLite` runtime refuses a single query over this many rows.
    const RUNTIME_QUERY_ROW_LIMIT: u32 = 10_000;
    const REFS: u32 = RUNTIME_QUERY_ROW_LIMIT + 1;

    let (db, _dir) = setup_db().await;
    let node = sample_node("paged-ref-node", "my_func", "src/lib.rs");
    db.insert_node(&node).await.expect("failed to insert node");

    let refs: Vec<UnresolvedRef> = (0..REFS)
        .map(|index| UnresolvedRef {
            from_node_id: "paged-ref-node".to_string(),
            reference_name: format!("target_{index:05}"),
            reference_kind: EdgeKind::Calls,
            line: index,
            column: 0,
            file_path: "src/lib.rs".to_string(),
        })
        .collect();
    db.insert_unresolved_refs(&refs)
        .await
        .expect("failed to insert unresolved refs");

    let read_back = db
        .get_unresolved_refs()
        .await
        .expect("a paged scan must not exceed the runtime materialization limit");

    assert_eq!(read_back.len(), refs.len());
    assert_eq!(
        read_back.first().map(|uref| uref.reference_name.as_str()),
        Some("target_00000")
    );
    assert_eq!(
        read_back.last().map(|uref| uref.reference_name.as_str()),
        Some("target_10000")
    );
}

#[tokio::test]
async fn test_batch_insert_nodes() {
    let (db, _dir) = setup_db().await;

    let nodes: Vec<Node> = (0..10)
        .map(|i| sample_node(&format!("batch-{i}"), &format!("func_{i}"), "src/batch.rs"))
        .collect();

    db.insert_nodes(&nodes)
        .await
        .expect("failed to batch insert nodes");

    let fetched = db
        .get_nodes_by_file("src/batch.rs")
        .await
        .expect("failed to get nodes by file");
    assert_eq!(fetched.len(), 10);
}

#[tokio::test]
async fn test_clear() {
    let (db, _dir) = setup_db().await;

    let node = sample_node("clear-1", "func", "src/lib.rs");
    db.insert_node(&node).await.expect("failed to insert node");

    let file = FileRecord {
        path: "src/lib.rs".to_string(),
        content_hash: "hash".to_string(),
        size: 100,
        modified_at: 1000,
        indexed_at: 2000,
        node_count: 1,
    };
    db.upsert_file(&file).await.expect("failed to upsert file");

    db.clear().await.expect("failed to clear database");

    let stats = db.get_stats().await.expect("failed to get stats");
    assert_eq!(stats.node_count, 0);
    assert_eq!(stats.edge_count, 0);
    assert_eq!(stats.file_count, 0);
}

#[tokio::test]
async fn test_get_node_not_found() {
    let (db, _dir) = setup_db().await;
    let result = db
        .get_node_by_id("nonexistent")
        .await
        .expect("query should not fail");
    assert!(result.is_none());
}

#[tokio::test]
async fn test_database_size() {
    let (db, _dir) = setup_db().await;
    let size = db.size().await.expect("size should not fail");
    assert!(size > 0, "database should have non-zero size");
}

// ---------------------------------------------------------------------------
// Migration v7: attrs_start_line column add + backfill
// ---------------------------------------------------------------------------
//
// Builds a v6-shaped nodes table directly (no attrs_start_line column), inserts
// rows with various start_line values, runs the migration runner, and verifies
// the column now exists with values backfilled from start_line.

#[tokio::test]
async fn test_migrate_v7_adds_and_backfills_attrs_start_line() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("v6.db");

    // Build the v6-shaped fixture offline (no attrs_start_line column) before
    // publishing its one canonical runtime.
    let conn = rusqlite::Connection::open(&db_path).expect("open v6 fixture");

    conn.execute_batch(
        "CREATE TABLE nodes (
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
            branches INTEGER NOT NULL DEFAULT 0,
            loops INTEGER NOT NULL DEFAULT 0,
            returns INTEGER NOT NULL DEFAULT 0,
            max_nesting INTEGER NOT NULL DEFAULT 0,
            unsafe_blocks INTEGER NOT NULL DEFAULT 0,
            unchecked_calls INTEGER NOT NULL DEFAULT 0,
            assertions INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL
        );
         PRAGMA user_version = 6;",
    )
    .expect("v6 schema setup");

    // Two rows: one with a normal start_line, one a file root with start_line=0.
    conn.execute(
        "INSERT INTO nodes (id, kind, name, qualified_name, file_path,
                            start_line, end_line, start_column, end_column, updated_at)
         VALUES ('a', 'function', 'foo', 'crate::foo', 'src/lib.rs', 42, 50, 0, 1, 1000)",
        [],
    )
    .expect("insert row a");
    conn.execute(
        "INSERT INTO nodes (id, kind, name, qualified_name, file_path,
                            start_line, end_line, start_column, end_column, updated_at)
         VALUES ('b', 'file', 'src/lib.rs', 'src/lib.rs', 'src/lib.rs', 0, 100, 0, 0, 1000)",
        [],
    )
    .expect("insert row b");
    drop(conn);

    // Publishing the existing fixture runs pending migrations on that exact
    // registered attachment.
    let (db, migrated) = crate::common::open_test_database(&db_path)
        .await
        .expect("open and migrate fixture");
    assert!(migrated, "expected v7 migration to run");

    // The migrated store must have crossed the v7 boundary. Do not pin this
    // regression test to an unrelated future latest version.
    let version = db
        .query_scalar_i64("read migrated schema version", "PRAGMA user_version")
        .await
        .expect("read version");
    assert!(version >= 7);

    // attrs_start_line is backfilled from start_line for both rows.
    // Row a: start_line=42 -> attrs_start_line=42.
    // Row b: start_line=0  -> attrs_start_line stays 0 (file root, consistent).
    assert_eq!(
        db.query_scalar_i64(
            "verify migrated row a",
            "SELECT COUNT(*) FROM nodes
             WHERE id = 'a' AND start_line = 42 AND attrs_start_line = 42",
        )
        .await
        .expect("inspect row a"),
        1,
        "attrs_start_line should backfill from start_line"
    );
    assert_eq!(
        db.query_scalar_i64(
            "verify migrated row b",
            "SELECT COUNT(*) FROM nodes
             WHERE id = 'b' AND start_line = 0 AND attrs_start_line = 0",
        )
        .await
        .expect("inspect row b"),
        1
    );

    // Inserting a fresh row with an explicit attrs_start_line works post-migration.
    db.execute_write(
        "insert post-migration row",
        "INSERT INTO nodes (id, kind, name, qualified_name, file_path,
                            start_line, end_line, start_column, end_column, updated_at,
                            attrs_start_line)
         VALUES ('c', 'function', 'bar', 'crate::bar', 'src/lib.rs', 60, 70, 0, 1, 2000, 55)",
        (),
    )
    .await
    .expect("insert row c");
    let attrs_start_line = db
        .query_scalar_i64(
            "read post-migration attrs_start_line",
            "SELECT attrs_start_line FROM nodes WHERE id = 'c'",
        )
        .await
        .expect("select c");
    assert_eq!(attrs_start_line, 55);
}

#[tokio::test]
async fn test_migrate_is_idempotent_at_latest() {
    // After Database::initialize creates the latest schema, calling migrate
    // again must be a no-op (returns None) — guards against accidental
    // re-runs of v7's ALTER TABLE on an already-migrated DB.
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("idem.db");
    let (db, _) = crate::common::initialize_test_database(&db_path)
        .await
        .expect("initialize");
    let migrated = tracedecay::db::migrations::migrate(&db)
        .await
        .expect("migrate");
    assert!(migrated.is_none(), "second migrate should be a no-op");
}

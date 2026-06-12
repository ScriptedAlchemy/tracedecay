use libsql::{Builder, Connection, Database as LibsqlDatabase};
use tempfile::TempDir;
use tokensave::db::migrations::{create_schema, migrate};
use tokensave::db::Database;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Creates a raw libsql database in a temp directory.
/// Returns (Connection, Database, TempDir) — all three must stay alive.
async fn create_raw_db() -> (Connection, LibsqlDatabase, TempDir) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    let db = Builder::new_local(&db_path)
        .build()
        .await
        .expect("failed to build libsql database");
    let conn = db.connect().expect("failed to connect");
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )
    .await
    .expect("failed to apply pragmas");
    (conn, db, dir)
}

/// Sets PRAGMA user_version on the connection.
async fn set_user_version(conn: &Connection, version: u32) {
    conn.execute(&format!("PRAGMA user_version = {version}"), ())
        .await
        .expect("failed to set user_version");
}

/// Reads PRAGMA user_version from the connection.
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

/// Checks whether a table exists in sqlite_master.
async fn table_exists(conn: &Connection, table_name: &str) -> bool {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            libsql::params![table_name],
        )
        .await
        .expect("failed to query sqlite_master");
    rows.next()
        .await
        .expect("failed to read sqlite_master row")
        .is_some()
}

/// Checks whether an index exists in sqlite_master.
async fn index_exists(conn: &Connection, index_name: &str) -> bool {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='index' AND name=?1",
            libsql::params![index_name],
        )
        .await
        .expect("failed to query sqlite_master");
    rows.next()
        .await
        .expect("failed to read sqlite_master row")
        .is_some()
}

/// Checks whether a trigger exists in sqlite_master.
async fn trigger_exists(conn: &Connection, trigger_name: &str) -> bool {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND name=?1",
            libsql::params![trigger_name],
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
                   AND length(hrr_vector) > 0
                   AND hrr_algebra = 'amari_fhrr'
                   AND hrr_dim = 2048"
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
             WHERE hrr_vector IS NULL OR hrr_algebra != 'amari_fhrr' OR hrr_dim != 2048"
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

/// Checks whether a column exists on a table via PRAGMA table_info.
async fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .expect("failed to query table_info");
    while let Some(row) = rows.next().await.expect("failed to read table_info row") {
        let name: String = row
            .get_str(1)
            .expect("failed to read column name")
            .to_string();
        if name == column {
            return true;
        }
    }
    false
}

/// Returns the declared SQLite type and primary-key ordinal for a column.
async fn column_type_and_pk(conn: &Connection, table: &str, column: &str) -> (String, i64) {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .expect("failed to query table_info");
    while let Some(row) = rows.next().await.expect("failed to read table_info row") {
        let name = row
            .get_str(1)
            .expect("failed to read column name")
            .to_string();
        if name == column {
            return (
                row.get_str(2)
                    .expect("failed to read column type")
                    .to_string(),
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
    create_schema(conn)
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// create_schema on a fresh database sets user_version to latest and creates all tables.
#[tokio::test]
async fn test_create_schema_fresh_db() {
    let (conn, _db, _dir) = create_raw_db().await;

    create_schema(&conn)
        .await
        .expect("create_schema should succeed");

    assert_eq!(get_user_version(&conn).await, 14);
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
    assert!(table_exists(&conn, "memory_facts_fts").await);
}

/// create_schema is idempotent — calling it twice does not error.
#[tokio::test]
async fn test_create_schema_idempotent() {
    let (conn, _db, _dir) = create_raw_db().await;

    create_schema(&conn)
        .await
        .expect("first create_schema should succeed");
    create_schema(&conn)
        .await
        .expect("second create_schema should succeed");

    assert_eq!(get_user_version(&conn).await, 14);
}

/// migrate returns false when already at the latest version.
#[tokio::test]
async fn test_migrate_already_latest_returns_false() {
    let (conn, _db, _dir) = create_raw_db().await;

    create_schema(&conn)
        .await
        .expect("create_schema should succeed");

    let migrated = migrate(&conn).await.expect("migrate should succeed");

    assert!(
        !migrated,
        "migrate should return false when already at latest"
    );
    assert_eq!(get_user_version(&conn).await, 14);
}

/// migrate from v0 (completely empty database) applies all migrations to latest.
#[tokio::test]
async fn test_migrate_from_v0() {
    let (conn, _db, _dir) = create_raw_db().await;

    // user_version defaults to 0 on a fresh database
    assert_eq!(get_user_version(&conn).await, 0);

    let migrated = migrate(&conn)
        .await
        .expect("migrate from v0 should succeed");

    assert!(
        migrated,
        "migrate should return true when migrations were applied"
    );
    assert_eq!(get_user_version(&conn).await, 14);

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
    let (conn, _db, _dir) = create_raw_db().await;
    create_v1_schema(&conn).await;

    assert_eq!(get_user_version(&conn).await, 1);
    assert!(!table_exists(&conn, "metadata").await);
    assert!(!column_exists(&conn, "nodes", "branches").await);

    let migrated = migrate(&conn)
        .await
        .expect("migrate from v1 should succeed");

    assert!(migrated);
    assert_eq!(get_user_version(&conn).await, 14);

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
    let (conn, _db, _dir) = create_raw_db().await;
    create_v1_schema(&conn).await;
    apply_v2(&conn).await;

    assert_eq!(get_user_version(&conn).await, 2);
    assert!(table_exists(&conn, "metadata").await);
    assert!(!column_exists(&conn, "nodes", "branches").await);

    let migrated = migrate(&conn)
        .await
        .expect("migrate from v2 should succeed");

    assert!(migrated);
    assert_eq!(get_user_version(&conn).await, 14);

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
    let (conn, _db, _dir) = create_raw_db().await;
    create_v1_schema(&conn).await;
    apply_v2(&conn).await;
    apply_v3(&conn).await;

    assert_eq!(get_user_version(&conn).await, 3);
    assert!(column_exists(&conn, "nodes", "branches").await);
    assert!(!column_exists(&conn, "nodes", "unsafe_blocks").await);

    let migrated = migrate(&conn)
        .await
        .expect("migrate from v3 should succeed");

    assert!(migrated);
    assert_eq!(get_user_version(&conn).await, 14);

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
    let (conn, _db, _dir) = create_raw_db().await;
    create_v1_schema(&conn).await;
    apply_v2(&conn).await;
    apply_v3(&conn).await;
    apply_v4(&conn).await;

    assert_eq!(get_user_version(&conn).await, 4);
    assert!(!index_exists(&conn, "idx_edges_unique").await);

    let migrated = migrate(&conn)
        .await
        .expect("migrate from v4 should succeed");

    assert!(migrated);
    assert_eq!(get_user_version(&conn).await, 14);

    assert!(index_exists(&conn, "idx_edges_unique").await);
}

/// V5 migration actually deduplicates edge rows.
#[tokio::test]
async fn test_v5_deduplicates_edges() {
    let (conn, _db, _dir) = create_raw_db().await;
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
    let migrated = migrate(&conn)
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
    let (conn, _db, _dir) = create_raw_db().await;

    migrate(&conn)
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

/// Database::initialize creates a database at the latest schema version.
#[tokio::test]
async fn test_database_initialize_creates_latest_version() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("init_test.db");

    let (db, _migrated) = Database::initialize(&db_path)
        .await
        .expect("Database::initialize should succeed");

    assert_eq!(get_user_version(db.conn()).await, 14);
}

/// Database::open on an already-current database does not re-migrate.
#[tokio::test]
async fn test_database_open_no_migration_needed() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("open_test.db");

    // Initialize creates a database at the latest schema version
    let (db, _) = Database::initialize(&db_path)
        .await
        .expect("Database::initialize should succeed");
    db.close();

    // Open the same database — should not migrate
    let (_db2, migrated) = Database::open(&db_path)
        .await
        .expect("Database::open should succeed");

    assert!(
        !migrated,
        "opening an already-current database should not trigger migration"
    );
}

/// Database::open on a v1 database migrates to the latest schema version.
#[tokio::test]
async fn test_database_open_migrates_v1_to_latest() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("open_v1_test.db");

    // Create a raw v1 database
    {
        let raw_db = Builder::new_local(&db_path)
            .build()
            .await
            .expect("failed to build libsql database");
        let conn = raw_db.connect().expect("failed to connect");
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;",
        )
        .await
        .expect("failed to apply pragmas");
        create_v1_schema(&conn).await;
    }

    // Open via Database::open — should detect v1 and migrate to latest
    let (db, migrated) = Database::open(&db_path)
        .await
        .expect("Database::open should succeed");

    assert!(migrated, "opening a v1 database should trigger migration");

    assert_eq!(get_user_version(db.conn()).await, 14);
}

/// After create_schema, all v5 columns on nodes exist.
#[tokio::test]
async fn test_create_schema_has_all_node_columns() {
    let (conn, _db, _dir) = create_raw_db().await;
    create_schema(&conn)
        .await
        .expect("create_schema should succeed");

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
    let (conn, _db, _dir) = create_raw_db().await;
    create_schema(&conn)
        .await
        .expect("create_schema should succeed");

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

/// FTS triggers exist after migration from v0.
#[tokio::test]
async fn test_fts_triggers_exist_after_migration() {
    let (conn, _db, _dir) = create_raw_db().await;

    migrate(&conn)
        .await
        .expect("migrate from v0 should succeed");

    let triggers = ["nodes_fts_insert", "nodes_fts_delete", "nodes_fts_update"];
    for trigger in &triggers {
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type='trigger' AND name=?1",
                libsql::params![*trigger],
            )
            .await
            .expect("failed to query sqlite_master for trigger");
        assert!(
            rows.next()
                .await
                .expect("failed to read trigger row")
                .is_some(),
            "trigger '{trigger}' should exist after migration"
        );
    }
}

#[tokio::test]
async fn test_latest_schema_omits_legacy_memory_tables() {
    let (conn, _db, _dir) = create_raw_db().await;
    create_schema(&conn).await.unwrap();

    assert!(!table_exists(&conn, "memory_decisions").await);
    assert!(!table_exists(&conn, "memory_code_areas").await);
    assert!(!table_exists(&conn, "memory_decisions_fts").await);
    assert!(table_exists(&conn, "memory_facts").await);
    assert!(table_exists(&conn, "memory_entities").await);
}

#[tokio::test]
async fn test_v7_to_latest_upgrade_path() {
    let (conn, _db, _dir) = create_raw_db().await;

    create_schema(&conn).await.unwrap();
    conn.execute("PRAGMA user_version = 7", ()).await.unwrap();
    // Drop the v8+ tables to simulate a true v7 starting state
    conn.execute("DROP TABLE IF EXISTS memory_decisions_fts", ())
        .await
        .unwrap();
    conn.execute("DROP TABLE IF EXISTS memory_decisions", ())
        .await
        .unwrap();
    conn.execute("DROP TABLE IF EXISTS memory_code_areas", ())
        .await
        .unwrap();
    conn.execute("DROP TABLE IF EXISTS read_cache", ())
        .await
        .unwrap();

    let did_migrate = migrate(&conn).await.unwrap();
    assert!(did_migrate, "expected migrate() to return true");

    assert_eq!(get_user_version(&conn).await, 14);

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

/// V9 adds the `read_cache` table used by `tokensave_read`.
#[tokio::test]
async fn test_migrate_v9_adds_read_cache() {
    let (conn, _db, _dir) = create_raw_db().await;
    migrate(&conn).await.expect("migrate should succeed");

    assert!(
        table_exists(&conn, "read_cache").await,
        "v9 migration should create the read_cache table"
    );
    assert!(
        index_exists(&conn, "idx_read_cache_session").await,
        "v9 migration should create idx_read_cache_session"
    );
}

#[tokio::test]
async fn test_v11_create_schema_has_holographic_memory_schema() {
    let (conn, _db, _dir) = create_raw_db().await;
    create_schema(&conn)
        .await
        .expect("create_schema should succeed");

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
            "SELECT tags, trust_score, retrieval_count, helpful_count, unhelpful_count, source, metadata, hrr_algebra, hrr_dim FROM memory_facts WHERE fact_id=?1",
            libsql::params![fact_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "[]");
    assert_eq!(row.get::<f64>(1).unwrap(), 0.5);
    assert_eq!(row.get::<i64>(2).unwrap(), 0);
    assert_eq!(row.get::<i64>(3).unwrap(), 0);
    assert_eq!(row.get::<i64>(4).unwrap(), 0);
    assert_eq!(row.get::<String>(5).unwrap(), "manual");
    assert_eq!(row.get::<String>(6).unwrap(), "{}");
    assert_eq!(row.get::<String>(7).unwrap(), "amari_fhrr");
    assert_eq!(row.get::<i64>(8).unwrap(), 2048);
}

#[tokio::test]
async fn test_v10_to_v11_backfills_and_drops_legacy_memory_tables() {
    let (conn, _db, _dir) = create_raw_db().await;
    create_v10_schema_for_v11_tests(&conn).await;

    assert_eq!(get_user_version(&conn).await, 10);
    assert!(table_exists(&conn, "memory_decisions").await);
    assert!(table_exists(&conn, "memory_code_areas").await);
    assert!(!table_exists(&conn, "memory_facts").await);

    let did_migrate = migrate(&conn).await.expect("v10 to v11 should migrate");

    assert!(did_migrate);
    assert_eq!(get_user_version(&conn).await, 14);
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
    let (conn, _db, _dir) = create_raw_db().await;
    create_schema(&conn).await.unwrap();
    set_user_version(&conn, 11).await;

    let did_migrate = migrate(&conn).await.expect("v11 to v12 should migrate");

    assert!(did_migrate);
    assert_eq!(get_user_version(&conn).await, 14);
    assert!(table_exists(&conn, "memory_bank_dirty").await);
}

#[tokio::test]
async fn test_v11_feedback_events_enforce_action_and_cascade_with_facts() {
    let (conn, _db, _dir) = create_raw_db().await;
    create_schema(&conn).await.unwrap();

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
        libsql::params![fact_id],
    )
    .await
    .expect("valid feedback action should insert");

    let invalid = conn
        .execute(
            "INSERT INTO memory_feedback_events (fact_id, action, trust_delta, old_trust, new_trust)
             VALUES (?1, 'neutral', 0.0, 0.5, 0.5)",
            libsql::params![fact_id],
        )
        .await;
    assert!(invalid.is_err(), "invalid feedback action should fail");

    let mut rows = conn
        .query(
            "SELECT source FROM memory_feedback_events WHERE fact_id=?1",
            libsql::params![fact_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "mcp");

    conn.execute(
        "DELETE FROM memory_facts WHERE fact_id=?1",
        libsql::params![fact_id],
    )
    .await
    .expect("deleting memory fact should cascade");
    assert_eq!(
        {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM memory_feedback_events WHERE fact_id=?1",
                    libsql::params![fact_id],
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
async fn test_v11_memory_facts_fts_triggers_track_insert_update_delete() {
    let (conn, _db, _dir) = create_raw_db().await;
    create_schema(&conn).await.unwrap();

    conn.execute(
        "INSERT INTO memory_facts (content, category, tags)
         VALUES ('Use orbital retrieval for context', 'test', '[\"retrieval\"]')",
        (),
    )
    .await
    .expect("failed to insert memory fact");
    let fact_id = scalar_i64(&conn, "SELECT fact_id FROM memory_facts").await;
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts_fts WHERE memory_facts_fts MATCH 'orbital'"
        )
        .await,
        1
    );

    conn.execute(
        "UPDATE memory_facts SET content='Use semantic banana storage', tags='[\"banana\"]' WHERE fact_id=?1",
        libsql::params![fact_id],
    )
    .await
    .expect("failed to update memory fact");
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts_fts WHERE memory_facts_fts MATCH 'orbital'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts_fts WHERE memory_facts_fts MATCH 'banana'"
        )
        .await,
        1
    );

    conn.execute(
        "DELETE FROM memory_facts WHERE fact_id=?1",
        libsql::params![fact_id],
    )
    .await
    .expect("failed to delete memory fact");
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_facts_fts WHERE memory_facts_fts MATCH 'banana'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn test_v11_backfills_legacy_memory_decisions_as_facts() {
    let (conn, _db, _dir) = create_raw_db().await;
    create_v10_schema_for_v11_tests(&conn).await;
    conn.execute(
        "INSERT INTO memory_decisions (text, reason, created_at, files, tags)
         VALUES ('Prefer libsql migrations', 'Keeps install path simple', 1234, '[\"src/db/migrations.rs\"]', '[\"db\",\"memory\"]')",
        (),
    )
    .await
    .expect("failed to insert legacy decision");

    migrate(&conn).await.expect("v11 migration should backfill");

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
    assert!(content.contains("Prefer libsql migrations"));
    assert!(content.contains("Keeps install path simple"));
    assert_eq!(tags, "[\"db\",\"memory\"]");
    assert!(!metadata.contains("legacy-decision-"));
    assert!(metadata.contains("holographic_memory_backfill_v1"));
    assert!(metadata.contains("memory_decisions"));
    assert!(metadata.contains("\"legacy_id\":1"));
    assert!(metadata.contains("\"decision_text\":\"Prefer libsql migrations\""));
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
    let (conn, _db, _dir) = create_raw_db().await;
    create_v10_schema_for_v11_tests(&conn).await;
    conn.execute(
        "INSERT INTO memory_code_areas (path, description, last_touched_at, touch_count)
         VALUES ('src/db/migrations.rs', 'Schema migration code', 5678, 3)",
        (),
    )
    .await
    .expect("failed to insert legacy code area");

    migrate(&conn).await.expect("v11 migration should backfill");

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
    let (conn, _db, _dir) = create_raw_db().await;
    create_v10_schema_for_v11_tests(&conn).await;
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

    migrate(&conn)
        .await
        .expect("first v11 migration should succeed");
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_facts").await,
        2
    );

    set_user_version(&conn, 10).await;
    migrate(&conn)
        .await
        .expect("rerunning v11 migration should succeed");

    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_facts").await,
        2
    );
}

#[tokio::test]
async fn test_v11_backfill_handles_malformed_and_blank_legacy_json() {
    let (conn, _db, _dir) = create_raw_db().await;
    create_v10_schema_for_v11_tests(&conn).await;
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

    migrate(&conn)
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
    let (conn, _db, _dir) = create_raw_db().await;
    create_v10_schema_for_v11_tests(&conn).await;
    for tag in ["rust", "performance"] {
        conn.execute(
            "INSERT INTO memory_decisions (text, reason, created_at, files, tags)
             VALUES ('Use Rust', 'same reason', 1000, '[]', json_array(?1))",
            libsql::params![tag],
        )
        .await
        .expect("failed to insert duplicate legacy decision");
    }

    migrate(&conn).await.expect("v11 migration should backfill");

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

/// Reads the column names of `table` via PRAGMA table_info.
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

/// v13 archive-column cleanup must handle the odd dev-DB state where the
/// abandoned archive revision left `superseded_by` as a generated column
/// referencing `merged_into`: SQLite refuses to drop `merged_into` while the
/// generated column still references it, so the migration has to drop the
/// dependent column first. Regression test for the "no such column" failure.
#[tokio::test]
async fn test_v13_drops_archive_columns_with_generated_column_dependency() {
    let (conn, _db, _dir) = create_raw_db().await;
    create_schema(&conn).await.unwrap();

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
        "INSERT INTO memory_facts (content, category) VALUES ('Archived-era fact', 'test')",
        (),
    )
    .await
    .expect("failed to insert fixture fact");
    set_user_version(&conn, 12).await;

    let migrated = migrate(&conn)
        .await
        .expect("v13 must drop archive columns even with a generated-column dependency");
    assert!(migrated, "expected migrate() to run the v13 cleanup");
    assert_eq!(get_user_version(&conn).await, 14);

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
    let (conn, _db, _dir) = create_raw_db().await;
    create_schema(&conn).await.unwrap();

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

    let migrated = migrate(&conn).await.expect("v14 must apply cleanly");
    assert!(migrated, "expected migrate() to run the v14 additions");
    assert_eq!(get_user_version(&conn).await, 14);

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
    let migrated_again = migrate(&conn)
        .await
        .expect("v14 must be idempotent on an already-upgraded schema");
    assert!(migrated_again);
    assert_eq!(get_user_version(&conn).await, 14);
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_facts").await,
        1
    );
}

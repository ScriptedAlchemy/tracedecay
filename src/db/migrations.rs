// Rust guideline compliant 2025-10-17
//! Sequential schema migrations for the tracedecay database.
//!
//! Each migration is a function that takes a connection and applies DDL
//! statements. Migrations run inside an EXCLUSIVE transaction so that
//! concurrent processes (e.g. a post-commit hook and an MCP server)
//! cannot corrupt the schema.
//!
//! The current schema version is stored in `PRAGMA user_version`, which
//! is an atomic integer built into `SQLite`. No extra table is needed.

use libsql::Connection;

use crate::errors::{Result, TraceDecayError};
use crate::memory::store::MemoryStore;

/// The highest migration version defined in this file. Bump this and add a
/// new entry to `run_migration` whenever the schema changes.
const LATEST_VERSION: u32 = 13;

/// Reads the current schema version from `PRAGMA user_version`.
async fn get_version(conn: &Connection) -> Result<u32> {
    let mut rows =
        conn.query("PRAGMA user_version", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to read user_version: {e}"),
                operation: "get_version".to_string(),
            })?;
    let row = rows.next().await.map_err(|e| TraceDecayError::Database {
        message: format!("failed to read user_version row: {e}"),
        operation: "get_version".to_string(),
    })?;
    match row {
        Some(r) => {
            let v: i64 = r.get(0).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read user_version value: {e}"),
                operation: "get_version".to_string(),
            })?;
            Ok(v as u32)
        }
        None => Ok(0),
    }
}

/// Sets the schema version via `PRAGMA user_version`.
///
/// PRAGMA statements cannot be parameterised, so we format the value
/// directly. This is safe because `version` is a u32.
async fn set_version(conn: &Connection, version: u32) -> Result<()> {
    conn.execute(&format!("PRAGMA user_version = {version}"), ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to set user_version: {e}"),
            operation: "set_version".to_string(),
        })?;
    Ok(())
}

/// Creates the complete latest schema from scratch for a brand-new database.
/// This avoids running v0→v1→…→v6 migrations sequentially.
pub async fn create_schema(conn: &Connection) -> Result<()> {
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
            branches INTEGER NOT NULL DEFAULT 0,
            loops INTEGER NOT NULL DEFAULT 0,
            returns INTEGER NOT NULL DEFAULT 0,
            max_nesting INTEGER NOT NULL DEFAULT 0,
            unsafe_blocks INTEGER NOT NULL DEFAULT 0,
            unchecked_calls INTEGER NOT NULL DEFAULT 0,
            assertions INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL,
            attrs_start_line INTEGER NOT NULL DEFAULT 0,
            parent_id TEXT
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

        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
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

        CREATE INDEX IF NOT EXISTS idx_edges_source_kind ON edges(source, kind);
        CREATE INDEX IF NOT EXISTS idx_edges_target_kind ON edges(target, kind);
        CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_unique
            ON edges(source, target, kind, COALESCE(line, -1));

        CREATE INDEX IF NOT EXISTS idx_unresolved_refs_from_node_id ON unresolved_refs(from_node_id);
        CREATE INDEX IF NOT EXISTS idx_unresolved_refs_reference_name ON unresolved_refs(reference_name);
        CREATE INDEX IF NOT EXISTS idx_unresolved_refs_file_path ON unresolved_refs(file_path);

        CREATE INDEX IF NOT EXISTS idx_nodes_lower_name ON nodes(lower(name));
        CREATE INDEX IF NOT EXISTS idx_nodes_parent_id ON nodes(parent_id);

        CREATE TABLE IF NOT EXISTS node_fingerprints (
            node_id TEXT PRIMARY KEY,
            ast_hash TEXT NOT NULL,
            cfg_hash TEXT NOT NULL,
            call_seq_hash TEXT NOT NULL,
            shingles TEXT NOT NULL,
            body_tokens INTEGER NOT NULL,
            source_hash TEXT NOT NULL,
            FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_node_fingerprints_ast ON node_fingerprints(ast_hash);
        CREATE INDEX IF NOT EXISTS idx_node_fingerprints_size ON node_fingerprints(body_tokens);

        CREATE TABLE IF NOT EXISTS read_cache (
            project_id   TEXT NOT NULL,
            session_id   TEXT NOT NULL,
            file_path    TEXT NOT NULL,
            mtime_ns     INTEGER NOT NULL,
            mode         TEXT NOT NULL,
            args_hash    TEXT NOT NULL,
            digest       TEXT NOT NULL,
            body         BLOB NOT NULL,
            token_count  INTEGER NOT NULL,
            created_at   INTEGER NOT NULL,
            PRIMARY KEY (project_id, session_id, file_path, mode, args_hash)
        );

        CREATE INDEX IF NOT EXISTS idx_read_cache_session
            ON read_cache(session_id, created_at);",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("failed to create schema: {e}"),
        operation: "create_schema".to_string(),
    })?;

    create_holographic_memory_schema(conn, "create_schema").await?;
    set_version(conn, LATEST_VERSION).await?;
    Ok(())
}

/// Runs all pending migrations up to `LATEST_VERSION`.
///
/// Acquires an EXCLUSIVE transaction to prevent concurrent writers from
/// interleaving schema changes. Each migration is applied and the version
/// is bumped inside the same transaction.
/// Returns `true` if any migrations were applied, `false` if already up-to-date.
pub async fn migrate(conn: &Connection) -> Result<bool> {
    let current = get_version(conn).await?;
    debug_assert!(
        current <= LATEST_VERSION,
        "database version {current} is ahead of code version {LATEST_VERSION}"
    );
    if current >= LATEST_VERSION {
        return Ok(false);
    }

    eprintln!("[tracedecay] migrating database schema v{current} → v{LATEST_VERSION}…");

    // BEGIN EXCLUSIVE blocks other writers (including other MCP servers or
    // post-commit hooks) until we COMMIT. Readers using WAL mode are not
    // blocked.
    conn.execute("BEGIN EXCLUSIVE", ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to acquire exclusive lock: {e}"),
            operation: "migrate".to_string(),
        })?;

    // Re-read inside the lock in case another process migrated between our
    // check and the lock acquisition.
    let current = get_version(conn).await?;

    let result = run_migrations(conn, current).await;

    match result {
        Ok(()) => {
            conn.execute("COMMIT", ())
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to commit migrations: {e}"),
                    operation: "migrate".to_string(),
                })?;
            Ok(true)
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(e)
        }
    }
}

/// Applies migrations sequentially from `current` up to `LATEST_VERSION`.
async fn run_migrations(conn: &Connection, current: u32) -> Result<()> {
    debug_assert!(
        current < LATEST_VERSION,
        "run_migrations called when already at latest version"
    );
    for version in (current + 1)..=LATEST_VERSION {
        run_migration(conn, version).await?;
        set_version(conn, version).await?;
    }
    Ok(())
}

/// Dispatches a single migration by version number.
async fn run_migration(conn: &Connection, version: u32) -> Result<()> {
    match version {
        1 => migrate_v1(conn).await,
        2 => migrate_v2(conn).await,
        3 => migrate_v3(conn).await,
        4 => migrate_v4(conn).await,
        5 => migrate_v5(conn).await,
        6 => migrate_v6(conn).await,
        7 => migrate_v7(conn).await,
        8 => migrate_v8(conn).await,
        9 => migrate_v9(conn).await,
        10 => migrate_v10(conn).await,
        11 => migrate_v11(conn).await,
        12 => migrate_v12(conn).await,
        13 => migrate_v13(conn).await,
        _ => Err(TraceDecayError::Database {
            message: format!("unknown migration version: {version}"),
            operation: "run_migration".to_string(),
        }),
    }
}

/// Compatibility marker after v12 was exposed on the PR stack.
///
/// The dirty-bank schema now lives in the folded v11/fresh schema, but existing
/// databases may already carry `user_version = 12`. Keep the version monotonic
/// so later schema work can safely use v13 instead of reusing an exposed number.
#[allow(clippy::unused_async)] // keeps the migration dispatch uniform
async fn migrate_v12(_conn: &Connection) -> Result<()> {
    Ok(())
}

/// v13: Cleanup marker for the (never-shipped) fact-archive schema.
///
/// An uncommitted revision of v13 briefly added archive columns (`state`,
/// `archived_at`, `archived_reason`, `merged_into`, `superseded_by`) to
/// `memory_facts`. Curation now hard-deletes losing facts instead, so this
/// migration drops those columns from any local development database that
/// ran the earlier revision, and is a no-op everywhere else.
async fn migrate_v13(conn: &Connection) -> Result<()> {
    // table_xinfo, not table_info: the earlier revision could have left
    // `superseded_by` as a GENERATED column, which plain table_info hides —
    // and a skipped drop then breaks dropping the column it references.
    let existing: std::collections::HashSet<String> = {
        let mut rows = conn
            .query("PRAGMA table_xinfo(memory_facts)", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("v13: failed to read table_xinfo: {e}"),
                operation: "migrate_v13".to_string(),
            })?;
        let mut names = std::collections::HashSet::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("v13: failed to iterate table_xinfo: {e}"),
            operation: "migrate_v13".to_string(),
        })? {
            let name: String = row.get(1).map_err(|e| TraceDecayError::Database {
                message: format!("v13: failed to read column name: {e}"),
                operation: "migrate_v13".to_string(),
            })?;
            names.insert(name);
        }
        names
    };

    // The index must go before its column can be dropped.
    conn.execute("DROP INDEX IF EXISTS idx_memory_facts_state", ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v13: failed to drop state index: {e}"),
            operation: "migrate_v13".to_string(),
        })?;

    // Drop in REVERSE order of how the abandoned revision added them: a
    // later-added column can be a generated column referencing an earlier
    // one (e.g. `superseded_by` GENERATED ALWAYS AS (... merged_into ...)),
    // and SQLite refuses to drop a column while a generated column still
    // references it ("no such column" / "error in generated column").
    for col in [
        "superseded_by",
        "merged_into",
        "archived_reason",
        "archived_at",
        "state",
    ] {
        if existing.contains(col) {
            conn.execute(&format!("ALTER TABLE memory_facts DROP COLUMN {col}"), ())
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("v13: failed to drop column {col}: {e}"),
                    operation: "migrate_v13".to_string(),
                })?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V1: initial schema
// ---------------------------------------------------------------------------

/// Creates all core tables, FTS index, triggers, and indexes.
async fn migrate_v1(conn: &Connection) -> Result<()> {
    // Tables
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
        );",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v1: failed to create tables: {e}"),
        operation: "migrate_v1".to_string(),
    })?;

    // FTS5
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
            name,
            qualified_name,
            docstring,
            signature,
            content='nodes',
            content_rowid='rowid'
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
        END;",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v1: failed to create FTS: {e}"),
        operation: "migrate_v1".to_string(),
    })?;

    // Indexes
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_nodes_kind ON nodes(kind);
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
    .map_err(|e| TraceDecayError::Database {
        message: format!("v1: failed to create indexes: {e}"),
        operation: "migrate_v1".to_string(),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V2: metadata table
// ---------------------------------------------------------------------------

/// Adds the key-value metadata table for persistent counters.
async fn migrate_v2(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        (),
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v2: failed to create metadata table: {e}"),
        operation: "migrate_v2".to_string(),
    })?;

    // Drop the legacy schema_versions table if it exists.
    conn.execute("DROP TABLE IF EXISTS schema_versions", ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v2: failed to drop schema_versions: {e}"),
            operation: "migrate_v2".to_string(),
        })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V3: complexity metric columns on nodes
// ---------------------------------------------------------------------------

/// Adds branches, loops, returns, and `max_nesting` columns to the nodes table.
async fn migrate_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE nodes ADD COLUMN branches INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN loops INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN returns INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN max_nesting INTEGER NOT NULL DEFAULT 0;",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v3: failed to add complexity columns: {e}"),
        operation: "migrate_v3".to_string(),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V4: unsafe_blocks, unchecked_calls, assertions columns on nodes
// ---------------------------------------------------------------------------

/// Adds `unsafe_blocks`, `unchecked_calls`, and assertions columns to the nodes table.
async fn migrate_v4(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE nodes ADD COLUMN unsafe_blocks INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN unchecked_calls INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN assertions INTEGER NOT NULL DEFAULT 0;",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v4: failed to add safety metric columns: {e}"),
        operation: "migrate_v4".to_string(),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V5: deduplicate edges and add UNIQUE index
// ---------------------------------------------------------------------------

/// Removes duplicate edges accumulated by repeated reference resolution
/// during incremental syncs, then adds a UNIQUE index to prevent future
/// duplicates. See: <https://github.com/…/issues/5>
async fn migrate_v5(conn: &Connection) -> Result<()> {
    // Rebuild the edges table keeping only distinct rows. We use a temp
    // table + swap because DELETE with a self-join subquery can be very
    // slow on large tables (the reporter had 13.9 M edges).
    conn.execute_batch(
        "CREATE TABLE edges_dedup (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source TEXT NOT NULL,
            target TEXT NOT NULL,
            kind TEXT NOT NULL,
            line INTEGER,
            FOREIGN KEY (source) REFERENCES nodes(id) ON DELETE CASCADE,
            FOREIGN KEY (target) REFERENCES nodes(id) ON DELETE CASCADE
        );

        INSERT INTO edges_dedup (source, target, kind, line)
        SELECT DISTINCT source, target, kind, line FROM edges;

        DROP TABLE edges;
        ALTER TABLE edges_dedup RENAME TO edges;

        CREATE INDEX idx_edges_source ON edges(source);
        CREATE INDEX idx_edges_target ON edges(target);
        CREATE INDEX idx_edges_kind ON edges(kind);
        CREATE INDEX idx_edges_source_kind ON edges(source, kind);
        CREATE INDEX idx_edges_target_kind ON edges(target, kind);
        CREATE UNIQUE INDEX idx_edges_unique
            ON edges(source, target, kind, COALESCE(line, -1));",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v5: failed to deduplicate edges: {e}"),
        operation: "migrate_v5".to_string(),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V6: expression index on lower(name) for case-insensitive lookups
// ---------------------------------------------------------------------------

/// Adds an expression index on `lower(name)` so that case-insensitive queries
/// and LIKE fallbacks avoid full table scans on large codebases.
async fn migrate_v6(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_nodes_lower_name ON nodes(lower(name))",
        (),
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v6: failed to create lower(name) index: {e}"),
        operation: "migrate_v6".to_string(),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V7: attrs_start_line column for full-span item lookups
// ---------------------------------------------------------------------------

/// Adds `attrs_start_line` to the nodes table. This column captures the first
/// line of an item's leading doc-comment / attribute block, so that consumers
/// (refactoring tools, code movers) can select an item's full span including
/// its documentation rather than guessing where the leading attrs start.
///
/// Existing rows are backfilled with `start_line` so behaviour is preserved
/// for nodes indexed before this migration.
async fn migrate_v7(conn: &Connection) -> Result<()> {
    conn.execute(
        "ALTER TABLE nodes ADD COLUMN attrs_start_line INTEGER NOT NULL DEFAULT 0",
        (),
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v7: failed to add attrs_start_line column: {e}"),
        operation: "migrate_v7".to_string(),
    })?;

    conn.execute(
        "UPDATE nodes SET attrs_start_line = start_line WHERE attrs_start_line = 0",
        (),
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v7: failed to backfill attrs_start_line: {e}"),
        operation: "migrate_v7".to_string(),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V8: cross-session memory tables (decisions, code areas)
// ---------------------------------------------------------------------------

/// Adds tables for persistent agent memory: `memory_decisions` records
/// architecture / design choices with optional reason and tags;
/// `memory_code_areas` tracks paths the agent has worked in. An FTS5 mirror
/// over `memory_decisions.text` and `memory_decisions.reason` supported the
/// legacy decision-recall implementation before v11 backfilled and dropped
/// these tables.
async fn migrate_v8(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_decisions (
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
            ON memory_decisions(created_at);

        CREATE VIRTUAL TABLE IF NOT EXISTS memory_decisions_fts USING fts5(
            text, reason,
            content='memory_decisions', content_rowid='id'
        );

        CREATE TRIGGER IF NOT EXISTS memory_decisions_fts_insert
            AFTER INSERT ON memory_decisions BEGIN
                INSERT INTO memory_decisions_fts(rowid, text, reason)
                VALUES (NEW.id, NEW.text, NEW.reason);
            END;

        CREATE TRIGGER IF NOT EXISTS memory_decisions_fts_delete
            AFTER DELETE ON memory_decisions BEGIN
                INSERT INTO memory_decisions_fts(memory_decisions_fts, rowid, text, reason)
                VALUES ('delete', OLD.id, OLD.text, OLD.reason);
            END;

        CREATE TRIGGER IF NOT EXISTS memory_decisions_fts_update
            AFTER UPDATE ON memory_decisions BEGIN
                INSERT INTO memory_decisions_fts(memory_decisions_fts, rowid, text, reason)
                VALUES ('delete', OLD.id, OLD.text, OLD.reason);
                INSERT INTO memory_decisions_fts(rowid, text, reason)
                VALUES (NEW.id, NEW.text, NEW.reason);
            END;",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v8: failed to create memory tables: {e}"),
        operation: "migrate_v8".to_string(),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V9: read cache + parent_id denormalization
// ---------------------------------------------------------------------------

/// Two changes:
///
/// 1. Creates the `read_cache` table used by `tracedecay_read` to serve
///    unchanged files as a tiny stub across sessions.
/// 2. Denormalizes `Contains` edges onto a new `nodes.parent_id` column.
///    The column is backfilled from existing `Contains` rows, then those
///    rows are deleted. After v9, the truth for "who contains node X" is
///    `nodes.parent_id`, not the edges table — readers should prefer it.
async fn migrate_v9(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS read_cache (
            project_id   TEXT NOT NULL,
            session_id   TEXT NOT NULL,
            file_path    TEXT NOT NULL,
            mtime_ns     INTEGER NOT NULL,
            mode         TEXT NOT NULL,
            args_hash    TEXT NOT NULL,
            digest       TEXT NOT NULL,
            body         BLOB NOT NULL,
            token_count  INTEGER NOT NULL,
            created_at   INTEGER NOT NULL,
            PRIMARY KEY (project_id, session_id, file_path, mode, args_hash)
        );

        CREATE INDEX IF NOT EXISTS idx_read_cache_session
            ON read_cache(session_id, created_at);",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v9: failed to create read_cache table: {e}"),
        operation: "migrate_v9".to_string(),
    })?;

    // ALTER TABLE has no IF NOT EXISTS for columns in SQLite. Probe
    // PRAGMA table_info first — fresh installs already include parent_id
    // from create_schema, and the test harness exercises that path by
    // resetting user_version to a pre-v9 value.
    let has_parent_id = {
        let mut rows = conn
            .query("PRAGMA table_info(nodes)", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("v9: failed to probe nodes columns: {e}"),
                operation: "migrate_v9".to_string(),
            })?;
        let mut found = false;
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("v9: failed to read table_info row: {e}"),
            operation: "migrate_v9".to_string(),
        })? {
            if let Ok(name) = row.get::<String>(1) {
                if name == "parent_id" {
                    found = true;
                    break;
                }
            }
        }
        found
    };

    if !has_parent_id {
        conn.execute("ALTER TABLE nodes ADD COLUMN parent_id TEXT", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("v9: failed to add parent_id column: {e}"),
                operation: "migrate_v9".to_string(),
            })?;
    }

    // Backfill parent_id from existing Contains edges, then drop those
    // rows. Gate on the edges table actually existing — tests seed
    // partial schemas and a real install always has it (migrate_v1).
    let has_edges_table = {
        let mut rows = conn
            .query(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='edges'",
                (),
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("v9: failed to probe sqlite_master: {e}"),
                operation: "migrate_v9".to_string(),
            })?;
        rows.next()
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("v9: failed to read sqlite_master row: {e}"),
                operation: "migrate_v9".to_string(),
            })?
            .is_some()
    };

    if has_edges_table {
        // When a node has multiple incoming Contains rows (legacy data
        // anomaly), the first matching row wins — subsequent rows are
        // noise the new schema does not preserve.
        conn.execute(
            "UPDATE nodes SET parent_id = (
                SELECT source FROM edges
                WHERE edges.target = nodes.id AND edges.kind = 'contains'
                LIMIT 1
            )",
            (),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v9: failed to backfill parent_id from contains edges: {e}"),
            operation: "migrate_v9".to_string(),
        })?;

        conn.execute("DELETE FROM edges WHERE kind = 'contains'", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("v9: failed to drop contains edges: {e}"),
                operation: "migrate_v9".to_string(),
            })?;
    }

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_nodes_parent_id ON nodes(parent_id)",
        (),
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v9: failed to create idx_nodes_parent_id: {e}"),
        operation: "migrate_v9".to_string(),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V10: node_fingerprints (issue #83 — tracedecay_redundancy)
// ---------------------------------------------------------------------------

/// Creates the `node_fingerprints` table used by `tracedecay_redundancy` to
/// detect AST-isomorphic, control-flow-equivalent, and token-similar
/// function/method duplicates. Populated lazily on first redundancy query
/// and invalidated by `source_hash` mismatch.
async fn migrate_v10(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS node_fingerprints (
            node_id TEXT PRIMARY KEY,
            ast_hash TEXT NOT NULL,
            cfg_hash TEXT NOT NULL,
            call_seq_hash TEXT NOT NULL,
            shingles TEXT NOT NULL,
            body_tokens INTEGER NOT NULL,
            source_hash TEXT NOT NULL,
            FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_node_fingerprints_ast ON node_fingerprints(ast_hash);
        CREATE INDEX IF NOT EXISTS idx_node_fingerprints_size ON node_fingerprints(body_tokens);",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v10: failed to create node_fingerprints table: {e}"),
        operation: "migrate_v10".to_string(),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Migration V11: holographic memory active schema
// ---------------------------------------------------------------------------

/// Creates the active holographic-memory tables alongside the legacy memory
/// tables. Legacy data is preserved and copied into `memory_facts`.
async fn migrate_v11(conn: &Connection) -> Result<()> {
    create_holographic_memory_schema(conn, "migrate_v11").await?;
    if legacy_memory_tables_exist(conn).await? {
        backfill_legacy_memory_as_facts(conn).await?;
        backfill_holographic_memory_vectors_and_banks(conn).await?;
    }
    Ok(())
}

async fn backfill_holographic_memory_vectors_and_banks(conn: &Connection) -> Result<()> {
    let store = MemoryStore::new(conn);
    loop {
        let updated = store.compute_missing_vectors(500).await?;
        if updated == 0 {
            break;
        }
    }
    store.rebuild_all_banks().await?;
    Ok(())
}

async fn legacy_memory_tables_exist(conn: &Connection) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type='table'
               AND name IN ('memory_decisions', 'memory_code_areas')",
            (),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("migrate_v11: failed to probe legacy memory tables: {e}"),
            operation: "migrate_v11".to_string(),
        })?;
    let row = rows
        .next()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("migrate_v11: failed to read legacy table probe: {e}"),
            operation: "migrate_v11".to_string(),
        })?
        .ok_or_else(|| TraceDecayError::Database {
            message: "migrate_v11: legacy table probe returned no rows".to_string(),
            operation: "migrate_v11".to_string(),
        })?;
    let count: i64 = row.get(0).map_err(|e| TraceDecayError::Database {
        message: format!("migrate_v11: failed to read legacy table count: {e}"),
        operation: "migrate_v11".to_string(),
    })?;
    Ok(count > 0)
}

async fn create_holographic_memory_schema(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_facts (
            fact_id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL UNIQUE,
            category TEXT NOT NULL DEFAULT 'general',
            tags TEXT NOT NULL DEFAULT '[]',
            trust_score REAL NOT NULL DEFAULT 0.5,
            retrieval_count INTEGER NOT NULL DEFAULT 0,
            helpful_count INTEGER NOT NULL DEFAULT 0,
            unhelpful_count INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0,
            last_retrieved_at INTEGER,
            last_feedback_at INTEGER,
            source TEXT NOT NULL DEFAULT 'manual',
            metadata TEXT NOT NULL DEFAULT '{}',
            hrr_vector BLOB,
            hrr_algebra TEXT NOT NULL DEFAULT 'amari_fhrr',
            hrr_dim INTEGER NOT NULL DEFAULT 2048
        );

        CREATE TABLE IF NOT EXISTS memory_entities (
            entity_id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            normalized_name TEXT NOT NULL UNIQUE,
            entity_type TEXT NOT NULL DEFAULT 'unknown',
            aliases TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS memory_fact_entities (
            fact_id INTEGER NOT NULL,
            entity_id INTEGER NOT NULL,
            PRIMARY KEY (fact_id, entity_id),
            FOREIGN KEY (fact_id) REFERENCES memory_facts(fact_id) ON DELETE CASCADE,
            FOREIGN KEY (entity_id) REFERENCES memory_entities(entity_id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS memory_banks (
            bank_id INTEGER PRIMARY KEY AUTOINCREMENT,
            bank_name TEXT NOT NULL UNIQUE,
            vector BLOB NOT NULL,
            hrr_algebra TEXT NOT NULL DEFAULT 'amari_fhrr',
            hrr_dim INTEGER NOT NULL DEFAULT 2048,
            fact_count INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS memory_bank_dirty (
            bank_name TEXT PRIMARY KEY,
            updated_at INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS memory_feedback_events (
            event_id INTEGER PRIMARY KEY AUTOINCREMENT,
            fact_id INTEGER NOT NULL,
            action TEXT NOT NULL CHECK (action IN ('helpful', 'unhelpful')),
            trust_delta REAL NOT NULL,
            old_trust REAL NOT NULL,
            new_trust REAL NOT NULL,
            created_at INTEGER NOT NULL DEFAULT 0,
            source TEXT NOT NULL DEFAULT 'mcp',
            note TEXT,
            FOREIGN KEY (fact_id) REFERENCES memory_facts(fact_id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_memory_facts_category
            ON memory_facts(category);
        CREATE INDEX IF NOT EXISTS idx_memory_facts_updated_at
            ON memory_facts(updated_at);
        CREATE INDEX IF NOT EXISTS idx_memory_facts_trust_score
            ON memory_facts(trust_score);
        CREATE INDEX IF NOT EXISTS idx_memory_facts_source
            ON memory_facts(source);
        CREATE INDEX IF NOT EXISTS idx_memory_entities_type
            ON memory_entities(entity_type);
        CREATE INDEX IF NOT EXISTS idx_memory_fact_entities_entity_id
            ON memory_fact_entities(entity_id);
        CREATE INDEX IF NOT EXISTS idx_memory_banks_updated_at
            ON memory_banks(updated_at);
        CREATE INDEX IF NOT EXISTS idx_memory_feedback_events_fact_id
            ON memory_feedback_events(fact_id);
        CREATE INDEX IF NOT EXISTS idx_memory_feedback_events_created_at
            ON memory_feedback_events(created_at);

        CREATE VIRTUAL TABLE IF NOT EXISTS memory_facts_fts USING fts5(
            content, tags,
            content='memory_facts', content_rowid='rowid'
        );

        CREATE TRIGGER IF NOT EXISTS memory_facts_fts_insert
            AFTER INSERT ON memory_facts BEGIN
                INSERT INTO memory_facts_fts(rowid, content, tags)
                VALUES (NEW.rowid, NEW.content, NEW.tags);
            END;

        CREATE TRIGGER IF NOT EXISTS memory_facts_fts_delete
            AFTER DELETE ON memory_facts BEGIN
                INSERT INTO memory_facts_fts(memory_facts_fts, rowid, content, tags)
                VALUES ('delete', OLD.rowid, OLD.content, OLD.tags);
            END;

        CREATE TRIGGER IF NOT EXISTS memory_facts_fts_update
            AFTER UPDATE OF content, tags ON memory_facts BEGIN
                INSERT INTO memory_facts_fts(memory_facts_fts, rowid, content, tags)
                VALUES ('delete', OLD.rowid, OLD.content, OLD.tags);
                INSERT INTO memory_facts_fts(rowid, content, tags)
                VALUES (NEW.rowid, NEW.content, NEW.tags);
            END;",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("{operation}: failed to create holographic memory schema: {e}"),
        operation: operation.to_string(),
    })?;

    Ok(())
}

async fn backfill_legacy_memory_as_facts(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "WITH normalized_decisions AS (
            SELECT
                id,
                text,
                reason,
                created_at,
                CASE
                    WHEN json_valid(COALESCE(NULLIF(trim(files), ''), '[]'))
                     AND json_type(COALESCE(NULLIF(trim(files), ''), '[]')) = 'array'
                    THEN COALESCE(NULLIF(trim(files), ''), '[]')
                    ELSE '[]'
                END AS safe_files,
                CASE
                    WHEN json_valid(COALESCE(NULLIF(trim(tags), ''), '[]'))
                     AND json_type(COALESCE(NULLIF(trim(tags), ''), '[]')) = 'array'
                    THEN COALESCE(NULLIF(trim(tags), ''), '[]')
                    ELSE '[]'
                END AS safe_tags
            FROM memory_decisions
        )
        INSERT OR IGNORE INTO memory_facts (
            content,
            category,
            tags,
            created_at,
            updated_at,
            source,
            metadata
        )
        SELECT
            CASE
                WHEN reason IS NULL OR length(trim(reason)) = 0 THEN text
                ELSE text || char(10) || char(10) || 'Reason: ' || reason
            END || char(10) || char(10) || 'Legacy decision id: ' || id,
            'decision',
            safe_tags,
            created_at,
            created_at,
            'legacy_memory_decisions',
            json_object(
                'holographic_memory_backfill_v1', 1,
                'legacy_table', 'memory_decisions',
                'legacy_id', id,
                'decision_text', text,
                'reason', COALESCE(reason, ''),
                'files', json(safe_files),
                'tags', json(safe_tags)
            )
        FROM normalized_decisions;

        WITH normalized_code_areas AS (
            SELECT id, path, description, last_touched_at, touch_count
            FROM memory_code_areas
        )
        INSERT OR IGNORE INTO memory_facts (
            content,
            category,
            tags,
            created_at,
            updated_at,
            source,
            metadata
        )
        SELECT
            CASE
                WHEN description IS NULL OR length(trim(description)) = 0 THEN path
                ELSE path || char(10) || char(10) || description
            END || char(10) || char(10) || 'Legacy code area id: ' || id,
            'code_area',
            json_array('code_area', path),
            last_touched_at,
            last_touched_at,
            'legacy_memory_code_areas',
            json_object(
                'holographic_memory_backfill_v1', 1,
                'legacy_table', 'memory_code_areas',
                'legacy_id', id,
                'path', path,
                'description', COALESCE(description, ''),
                'last_touched_at', last_touched_at,
                'touch_count', touch_count
            )
        FROM normalized_code_areas;",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("migrate_v11: failed to backfill legacy memory: {e}"),
        operation: "migrate_v11".to_string(),
    })?;

    conn.execute_batch(
        "WITH normalized_decisions AS (
            SELECT
                id,
                created_at,
                CASE
                    WHEN json_valid(COALESCE(NULLIF(trim(files), ''), '[]'))
                     AND json_type(COALESCE(NULLIF(trim(files), ''), '[]')) = 'array'
                    THEN COALESCE(NULLIF(trim(files), ''), '[]')
                    ELSE '[]'
                END AS safe_files,
                CASE
                    WHEN json_valid(COALESCE(NULLIF(trim(tags), ''), '[]'))
                     AND json_type(COALESCE(NULLIF(trim(tags), ''), '[]')) = 'array'
                    THEN COALESCE(NULLIF(trim(tags), ''), '[]')
                    ELSE '[]'
                END AS safe_tags
            FROM memory_decisions
        )
        INSERT OR IGNORE INTO memory_entities (name, normalized_name, entity_type, created_at)
        SELECT DISTINCT value, lower(value), 'legacy_file', created_at
        FROM normalized_decisions, json_each(safe_files)
        WHERE trim(value) != '';

        WITH normalized_decisions AS (
            SELECT
                id,
                created_at,
                CASE
                    WHEN json_valid(COALESCE(NULLIF(trim(tags), ''), '[]'))
                     AND json_type(COALESCE(NULLIF(trim(tags), ''), '[]')) = 'array'
                    THEN COALESCE(NULLIF(trim(tags), ''), '[]')
                    ELSE '[]'
                END AS safe_tags
            FROM memory_decisions
        )
        INSERT OR IGNORE INTO memory_entities (name, normalized_name, entity_type, created_at)
        SELECT DISTINCT value, lower(value), 'legacy_tag', created_at
        FROM normalized_decisions, json_each(safe_tags)
        WHERE trim(value) != '';

        INSERT OR IGNORE INTO memory_entities (name, normalized_name, entity_type, created_at)
        SELECT DISTINCT path, lower(path), 'legacy_path', last_touched_at
        FROM memory_code_areas
        WHERE trim(path) != '';

        WITH normalized_decisions AS (
            SELECT
                id,
                CASE
                    WHEN json_valid(COALESCE(NULLIF(trim(files), ''), '[]'))
                     AND json_type(COALESCE(NULLIF(trim(files), ''), '[]')) = 'array'
                    THEN COALESCE(NULLIF(trim(files), ''), '[]')
                    ELSE '[]'
                END AS safe_files
            FROM memory_decisions
        )
        INSERT OR IGNORE INTO memory_fact_entities (fact_id, entity_id)
        SELECT f.fact_id, e.entity_id
        FROM normalized_decisions d
        JOIN memory_facts f
          ON f.source = 'legacy_memory_decisions'
         AND json_extract(f.metadata, '$.legacy_id') = d.id
        JOIN json_each(d.safe_files) file_entity
        JOIN memory_entities e ON e.normalized_name = lower(file_entity.value)
        WHERE trim(file_entity.value) != '';

        WITH normalized_decisions AS (
            SELECT
                id,
                CASE
                    WHEN json_valid(COALESCE(NULLIF(trim(tags), ''), '[]'))
                     AND json_type(COALESCE(NULLIF(trim(tags), ''), '[]')) = 'array'
                    THEN COALESCE(NULLIF(trim(tags), ''), '[]')
                    ELSE '[]'
                END AS safe_tags
            FROM memory_decisions
        )
        INSERT OR IGNORE INTO memory_fact_entities (fact_id, entity_id)
        SELECT f.fact_id, e.entity_id
        FROM normalized_decisions d
        JOIN memory_facts f
          ON f.source = 'legacy_memory_decisions'
         AND json_extract(f.metadata, '$.legacy_id') = d.id
        JOIN json_each(d.safe_tags) tag_entity
        JOIN memory_entities e ON e.normalized_name = lower(tag_entity.value)
        WHERE trim(tag_entity.value) != '';

        INSERT OR IGNORE INTO memory_fact_entities (fact_id, entity_id)
        SELECT f.fact_id, e.entity_id
        FROM memory_code_areas c
        JOIN memory_facts f
          ON f.source = 'legacy_memory_code_areas'
         AND json_extract(f.metadata, '$.legacy_id') = c.id
        JOIN memory_entities e ON e.normalized_name = lower(c.path)
        WHERE trim(c.path) != '';",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("migrate_v11: failed to link legacy memory entities: {e}"),
        operation: "migrate_v11".to_string(),
    })?;

    conn.execute_batch(
        "DROP TRIGGER IF EXISTS memory_decisions_fts_insert;
         DROP TRIGGER IF EXISTS memory_decisions_fts_delete;
         DROP TRIGGER IF EXISTS memory_decisions_fts_update;
         DROP TABLE IF EXISTS memory_decisions_fts;
         DROP TABLE IF EXISTS memory_code_areas;
         DROP TABLE IF EXISTS memory_decisions;",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("migrate_v11: failed to drop legacy memory tables: {e}"),
        operation: "migrate_v11".to_string(),
    })?;

    Ok(())
}

// Rust guideline compliant 2025-10-17
//! Schema creation for the tracedecay database.
//!
//! This binary creates every store at one final schema shape and never steps an
//! older shape forward. `PRAGMA user_version` records that shape as an atomic
//! integer built into `SQLite`; a store carrying any other value was written by
//! an incompatible binary and is refused at open with a fresh-start remedy.

use crate::db::engine::{Connection, Executor, QueryExecutor, Transaction};
use crate::errors::{Result, TraceDecayError};

/// The one schema shape this binary creates and accepts. It is an identity
/// stamp, not a ladder rung: a store at any other version is refused.
pub const SCHEMA_VERSION: u32 = 25;

/// Metadata stamp for the extraction generation currently published in the
/// core graph tables.
pub const GRAPH_GENERATION_SCHEMA_KEY: &str = "graph_generation_schema_version";

/// Reads the current schema version from `PRAGMA user_version`.
async fn get_version(conn: &impl QueryExecutor) -> Result<u32> {
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
async fn set_version(conn: &impl Executor, version: u32) -> Result<()> {
    conn.execute(&format!("PRAGMA user_version = {version}"), ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to set user_version: {e}"),
            operation: "set_version".to_string(),
        })?;
    Ok(())
}
/// Configures incremental auto-vacuum for a brand-new database before any
/// schema-shaping pragmas or tables are created.
pub async fn configure_fresh_auto_vacuum(conn: &Connection, operation: &str) -> Result<()> {
    conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("{operation}: failed to configure fresh auto_vacuum: {e}"),
            operation: operation.to_string(),
        })?;
    Ok(())
}

/// Creates the complete schema from scratch for a brand-new database and
/// stamps [`SCHEMA_VERSION`]. This is the only way a store comes into
/// existence: there is no stepwise path to this shape.
pub async fn create_schema(database: &crate::db::Database) -> Result<()> {
    let writer = database.writer_connection("create schema").await?;
    create_schema_connection(writer.engine_connection()).await
}

/// Creates the schema on an already-open connection. This is the door the
/// store runtime uses when it initializes a brand-new shard.
pub async fn create_schema_connection(conn: &Connection) -> Result<()> {
    // Fresh databases only need the pragma before tables are created.
    configure_fresh_auto_vacuum(conn, "create_schema").await?;

    let transaction = conn
        .authorized_long_lease_transaction()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to acquire fresh-schema writer lock: {e}"),
            operation: "create_schema".to_string(),
        })?;
    let result = create_schema_transaction(&transaction).await;
    match result {
        Ok(()) => transaction
            .commit()
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to commit fresh schema: {e}"),
                operation: "create_schema".to_string(),
            }),
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

async fn create_schema_transaction(conn: &Transaction) -> Result<()> {
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
            -- Nullable and no default: a real value (including a legitimate 0 for
            -- an item documented at the very top of a file) is written by every
            -- extractor, and SQL NULL is reserved as the honest unset marker so
            -- that a stored 0 is never mistaken for a defaulted/unknown value.
            -- See row_to_node in db/rows.rs for the read-side contract.
            attrs_start_line INTEGER,
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

    conn.execute_batch(REDUNDANCY_PAIRS_SCHEMA)
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to create redundancy_pairs schema: {e}"),
            operation: "create_schema".to_string(),
        })?;

    create_holographic_memory_schema(conn, "create_schema").await?;
    super::memory_v2::create_schema(conn, "create_schema").await?;
    super::memory_v2::install_v22_fresh_schema(conn, "create_schema").await?;
    super::memory_v2::install_v23_fresh_schema(conn, "create_schema").await?;
    super::evidence_assembly::install_evidence_assembly_schema(conn, "create_schema").await?;
    super::external_source::install_external_source_schema(conn, "create_schema").await?;
    set_version(conn, SCHEMA_VERSION).await?;
    Ok(())
}
/// Reports whether the file already carries user schema objects.
///
/// A brand-new file has `user_version = 0` and no objects at all. That is not a
/// store at an older shape; it is an empty file this binary may create into.
async fn store_has_objects(conn: &impl QueryExecutor) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'
             LIMIT 1",
            (),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to probe sqlite_master for existing schema: {e}"),
            operation: "ensure_schema_current".to_string(),
        })?;
    Ok(rows
        .next()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to read sqlite_master probe row: {e}"),
            operation: "ensure_schema_current".to_string(),
        })?
        .is_some())
}

fn unsupported_schema_version(current: u32) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!(
            "database schema v{current} is not the v{SCHEMA_VERSION} shape this binary creates; \
             this store was created by an incompatible binary and cannot be upgraded in place. \
             Remove the store directory and let this binary create a fresh one."
        ),
        operation: "ensure_schema_current".to_string(),
    }
}

/// Verifies an opened store carries the schema this binary creates, creating it
/// when the file is still empty.
///
/// This binary has no upgrade ladder: a store stamped with any other version is
/// refused with the fresh-start remedy rather than stepped forward.
pub async fn ensure_schema_current(database: &crate::db::Database) -> Result<()> {
    let writer = database.writer_connection("ensure schema current").await?;
    ensure_schema_current_connection(writer.engine_connection()).await
}

pub(crate) async fn ensure_schema_current_connection(conn: &Connection) -> Result<()> {
    let current = get_version(conn).await?;
    if current == SCHEMA_VERSION {
        return Ok(());
    }
    if current == 0 && !store_has_objects(conn).await? {
        return create_schema_connection(conn).await;
    }
    Err(unsupported_schema_version(current))
}

/// Compatibility alias for `crates/tracedecay-migrate`, which still names the
/// schema door `migrate`. It performs no migration: see
/// [`ensure_schema_current`].
pub async fn migrate(database: &crate::db::Database) -> Result<()> {
    ensure_schema_current(database).await
}

/// Connection-level compatibility alias. See [`migrate`].
#[cfg(any(test, feature = "test-helpers"))]
pub async fn migrate_connection(conn: &Connection) -> Result<()> {
    ensure_schema_current_connection(conn).await
}

async fn create_memory_fact_relations_schema(conn: &impl Executor, operation: &str) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_fact_relations (
            source_fact_id INTEGER NOT NULL,
            target_fact_id INTEGER NOT NULL,
            relation TEXT NOT NULL CHECK (
                relation IN ('supports', 'contradicts', 'supersedes', 'derived_from')
            ),
            confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
            source TEXT NOT NULL,
            metadata TEXT NOT NULL DEFAULT '{}',
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (source_fact_id, target_fact_id, relation),
            CHECK (source_fact_id != target_fact_id),
            FOREIGN KEY (source_fact_id) REFERENCES memory_facts(fact_id) ON DELETE CASCADE,
            FOREIGN KEY (target_fact_id) REFERENCES memory_facts(fact_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_memory_fact_relations_source
            ON memory_fact_relations(source_fact_id);
        CREATE INDEX IF NOT EXISTS idx_memory_fact_relations_target
            ON memory_fact_relations(target_fact_id);
        CREATE INDEX IF NOT EXISTS idx_memory_fact_relations_kind
            ON memory_fact_relations(relation);",
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("{operation}: failed to create memory fact relations: {e}"),
        operation: operation.to_string(),
    })?;
    Ok(())
}

/// Freshness-validated cache of `tracedecay_redundancy` duplicate pairs,
/// installed by the schema-creation path.
const REDUNDANCY_PAIRS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS redundancy_pairs (
        node_a_id TEXT NOT NULL,
        node_b_id TEXT NOT NULL,
        source_hash_a TEXT NOT NULL,
        source_hash_b TEXT NOT NULL,
        ranking_score REAL NOT NULL,
        similarity REAL NOT NULL,
        vector_cosine REAL NOT NULL,
        overlap_kind TEXT NOT NULL,
        severity TEXT NOT NULL,
        generic_helper_downranked INTEGER NOT NULL,
        computed_at INTEGER NOT NULL,
        PRIMARY KEY (node_a_id, node_b_id),
        FOREIGN KEY (node_a_id) REFERENCES nodes(id) ON DELETE CASCADE,
        FOREIGN KEY (node_b_id) REFERENCES nodes(id) ON DELETE CASCADE
    );

    CREATE INDEX IF NOT EXISTS idx_redundancy_pairs_node_b ON redundancy_pairs(node_b_id);";

/// Append-only audit log of memory mutations (add/update/remove/feedback and
/// curation applies). `detail_json` never carries fact content beyond what
/// the op needs — deletes record a content hash, not the content.
const MEMORY_OPLOG_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS memory_oplog (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts INTEGER NOT NULL DEFAULT 0,
        op TEXT NOT NULL,
        fact_id INTEGER,
        detail_json TEXT NOT NULL DEFAULT '{}'
    );

    CREATE INDEX IF NOT EXISTS idx_memory_oplog_ts ON memory_oplog(ts);";

// ---------------------------------------------------------------------------
// Migration V1: initial schema
// ---------------------------------------------------------------------------

async fn create_holographic_memory_schema(conn: &impl Executor, operation: &str) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_facts (
            fact_id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL UNIQUE,
            category TEXT NOT NULL DEFAULT 'general',
            tags TEXT NOT NULL DEFAULT '[]',
            trust_score REAL NOT NULL DEFAULT 0.5,
            retrieval_count INTEGER NOT NULL DEFAULT 0,
            access_count INTEGER NOT NULL DEFAULT 0,
            helpful_count INTEGER NOT NULL DEFAULT 0,
            unhelpful_count INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0,
            last_retrieved_at INTEGER,
            last_recalled_at INTEGER,
            last_feedback_at INTEGER,
            source TEXT NOT NULL DEFAULT 'manual',
            metadata TEXT NOT NULL DEFAULT '{}',
            hrr_vector BLOB,
            hrr_algebra TEXT NOT NULL DEFAULT 'amari_fhrr',
            hrr_dim INTEGER NOT NULL DEFAULT 2048,
            hrr_precision TEXT NOT NULL DEFAULT 'f32'
        );

        CREATE TABLE IF NOT EXISTS memory_entities (
            entity_id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            normalized_name TEXT NOT NULL UNIQUE,
            entity_type TEXT NOT NULL DEFAULT 'unknown',
            aliases TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0
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

    conn.execute_batch(MEMORY_OPLOG_SCHEMA)
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("{operation}: failed to create memory oplog schema: {e}"),
            operation: operation.to_string(),
        })?;

    create_memory_fact_relations_schema(conn, operation).await?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

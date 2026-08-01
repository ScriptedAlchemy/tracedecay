// Rust guideline compliant 2025-10-17
//! Sequential schema migrations for the tracedecay database.
//!
//! Each migration is a function that takes a connection and applies DDL
//! statements. Migrations run inside the runtime's immediate transaction on
//! its single writer lane so concurrent clients cannot interleave schema work.
//!
//! The current schema version is stored in `PRAGMA user_version`, which
//! is an atomic integer built into `SQLite`. No extra table is needed.

use crate::db::engine::{Connection, Executor, QueryExecutor, Transaction, params};
use crate::errors::{Result, TraceDecayError};
use crate::memory::store::MemoryStore;

/// The highest migration version defined in this file. Bump this and add a
/// new entry to `run_migration` whenever the schema changes.
pub const LATEST_VERSION: u32 = 25;

/// Metadata stamp for the extraction generation currently published in the
/// core graph tables.
pub const GRAPH_GENERATION_SCHEMA_KEY: &str = "graph_generation_schema_version";

/// Migration versions whose resulting graph cannot be trusted without
/// re-extracting source files.
///
/// V3/V4/V7 add extractor-populated node fields, while V17 marks the
/// attrs-start-line semantic correction that historical rows cannot recover.
/// V1 is the initial graph generation. Other migrations either preserve and
/// transform graph rows in place (V5/V9), add indexes, or add auxiliary
/// memory/evidence tables.
pub(crate) const GRAPH_INVALIDATING_VERSIONS: &[u32] = &[1, 3, 4, 7, 17];

pub fn graph_reindex_required(from: u32, to: u32) -> bool {
    GRAPH_INVALIDATING_VERSIONS
        .iter()
        .any(|version| from < *version && *version <= to)
}

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

async fn auto_vacuum_mode(conn: &impl QueryExecutor, operation: &str) -> Result<i64> {
    let mut rows =
        conn.query("PRAGMA auto_vacuum", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("{operation}: failed to read auto_vacuum: {e}"),
                operation: operation.to_string(),
            })?;
    let row = rows
        .next()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("{operation}: failed to read auto_vacuum row: {e}"),
            operation: operation.to_string(),
        })?
        .ok_or_else(|| TraceDecayError::Database {
            message: format!("{operation}: auto_vacuum returned no rows"),
            operation: operation.to_string(),
        })?;
    row.get(0).map_err(|e| TraceDecayError::Database {
        message: format!("{operation}: failed to read auto_vacuum value: {e}"),
        operation: operation.to_string(),
    })
}

async fn repair_incremental_auto_vacuum(
    conn: &Connection,
    operation: &str,
    exclusive_maintenance: bool,
) -> Result<()> {
    let mode = auto_vacuum_mode(conn, operation).await?;
    if mode == 2 || !exclusive_maintenance {
        return Ok(());
    }

    conn.repair_incremental_auto_vacuum()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("{operation}: failed to rebuild database for auto_vacuum: {e}"),
            operation: operation.to_string(),
        })?;
    if auto_vacuum_mode(conn, operation).await? != 2 {
        return Err(TraceDecayError::Database {
            message: format!("{operation}: auto_vacuum repair did not enable incremental mode"),
            operation: operation.to_string(),
        });
    }
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

/// Creates the complete latest schema from scratch for a brand-new database.
/// This avoids running v0→v1→…→v6 migrations sequentially.
pub async fn create_schema(database: &crate::db::Database) -> Result<()> {
    let writer = database.writer_connection("create schema").await?;
    create_schema_connection(writer.engine_connection()).await
}

pub(crate) async fn create_schema_connection(conn: &Connection) -> Result<()> {
    // Fresh databases only need the pragma before tables are created.
    configure_fresh_auto_vacuum(conn, "create_schema").await?;

    let transaction =
        conn.schema_migration_transaction()
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
    set_version(conn, LATEST_VERSION).await?;
    Ok(())
}

/// Runs all pending migrations up to `LATEST_VERSION`.
///
/// Acquires the runtime's immediate transaction on its single writer lane.
/// Each migration is applied and the version is bumped inside that same
/// transaction.
/// Returns the pre-migration version when migrations were applied.
pub async fn migrate(database: &crate::db::Database) -> Result<Option<u32>> {
    let writer = database.writer_connection("migrate schema").await?;
    migrate_inner(writer.engine_connection(), false).await
}

/// Internal registered-runtime entry point. Public callers migrate the
/// authority-bound [`crate::db::Database`] facade instead of naming the
/// private engine connection.
#[cfg(test)]
pub async fn migrate_connection(conn: &Connection) -> Result<bool> {
    Ok(migrate_inner(conn, false).await?.is_some())
}

/// Runs schema migrations and completes any whole-file auto-vacuum repair.
///
/// Callers must hold exclusive maintenance authority. Ordinary opens use
/// [`migrate`] so startup latency never grows with the database file size. The
/// maintenance runtime is consumed because a whole-file rebuild invalidates
/// reader connections opened against the previous file image.
#[cfg(test)]
pub async fn migrate_with_exclusive_maintenance(database: crate::db::Database) -> Result<bool> {
    let result = {
        let writer = database
            .writer_connection("migrate schema under exclusive maintenance")
            .await?;
        Ok(migrate_inner(writer.engine_connection(), true)
            .await?
            .is_some())
    };
    database.close();
    result
}

async fn migrate_inner(conn: &Connection, exclusive_maintenance: bool) -> Result<Option<u32>> {
    let current = get_version(conn).await?;
    if current > LATEST_VERSION {
        return Err(TraceDecayError::Database {
            message: format!(
                "database schema v{current} is newer than supported v{LATEST_VERSION}"
            ),
            operation: "migrate".to_string(),
        });
    }
    if current == LATEST_VERSION {
        repair_incremental_auto_vacuum(conn, "migrate", exclusive_maintenance).await?;
        return Ok(None);
    }

    eprintln!("[tracedecay] migrating database schema v{current} → v{LATEST_VERSION}…");

    // The runtime owns one writer lane. Beginning an immediate transaction
    // reserves that lane before the version is re-read and keeps every schema
    // change plus its version bump atomic without raw transaction-control SQL.
    let transaction =
        conn.schema_migration_transaction()
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to acquire migration writer lock: {e}"),
                operation: "migrate".to_string(),
            })?;

    // Re-read inside the lock in case another process migrated between our
    // check and the lock acquisition.
    let current = get_version(&transaction).await?;
    if current > LATEST_VERSION {
        let rollback = transaction.rollback().await;
        let rollback_context = rollback
            .err()
            .map_or(String::new(), |error| format!("; rollback failed: {error}"));
        return Err(TraceDecayError::Database {
            message: format!(
                "database schema v{current} is newer than supported v{LATEST_VERSION}{rollback_context}"
            ),
            operation: "migrate".to_string(),
        });
    }
    if current == LATEST_VERSION {
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to release migration writer lock: {error}"),
                operation: "migrate".to_string(),
            })?;
        repair_incremental_auto_vacuum(conn, "migrate", exclusive_maintenance).await?;
        return Ok(None);
    }

    let result = run_migrations(&transaction, current).await;

    match result {
        Ok(()) => {
            if !graph_reindex_required(current, LATEST_VERSION) {
                carry_graph_generation_stamp(&transaction).await?;
            }
            transaction
                .commit()
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to commit migrations: {e}"),
                    operation: "migrate".to_string(),
                })?;
            repair_incremental_auto_vacuum(conn, "migrate", exclusive_maintenance).await?;
            Ok(Some(current))
        }
        Err(e) => {
            let _ = transaction.rollback().await;
            Err(e)
        }
    }
}

/// Stamps the published graph generation after a migration chain that left the
/// graph rows trustworthy, so a later open does not mistake a missing stamp for
/// a lost generation.
///
/// Only databases that own graph tables carry `metadata`; it arrives with V2 or
/// with the fresh schema. A memory-only database legitimately starts above that
/// version without the table and has no graph generation to stamp, so probe
/// before writing instead of assuming every migrated database owns the graph.
async fn carry_graph_generation_stamp(conn: &Transaction) -> Result<()> {
    let owns_graph_metadata = {
        let mut rows = conn
            .query(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='metadata'",
                (),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to probe sqlite_master for graph metadata: {error}"),
                operation: "migrate".to_string(),
            })?;
        rows.next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read graph metadata probe row: {error}"),
                operation: "migrate".to_string(),
            })?
            .is_some()
    };
    if !owns_graph_metadata {
        return Ok(());
    }

    conn.execute(
        "INSERT INTO metadata (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (GRAPH_GENERATION_SCHEMA_KEY, LATEST_VERSION.to_string()),
    )
    .await
    .map_err(|error| TraceDecayError::Database {
        message: format!("failed to carry graph generation through compatible migration: {error}"),
        operation: "migrate".to_string(),
    })?;
    Ok(())
}

/// Applies migrations sequentially from `current` up to `LATEST_VERSION`.
async fn run_migrations(conn: &Transaction, current: u32) -> Result<()> {
    debug_assert!(
        current < LATEST_VERSION,
        "run_migrations called when already at latest version"
    );
    run_migrations_through(conn, current, LATEST_VERSION).await
}

async fn run_migrations_through(conn: &Transaction, current: u32, target: u32) -> Result<()> {
    for version in (current + 1)..=target {
        run_migration(conn, version).await?;
        set_version(conn, version).await?;
    }
    Ok(())
}

#[cfg(test)]
pub async fn migrate_test_connection_to_version(conn: &Connection, target: u32) -> Result<()> {
    if target > LATEST_VERSION {
        return Err(TraceDecayError::Database {
            message: format!(
                "test migration target v{target} is newer than supported v{LATEST_VERSION}"
            ),
            operation: "migrate_test_connection_to_version".to_owned(),
        });
    }
    let transaction =
        conn.schema_migration_transaction()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to acquire test migration writer lock: {error}"),
                operation: "migrate_test_connection_to_version".to_owned(),
            })?;
    let current = get_version(&transaction).await?;
    if current > target {
        let _ = transaction.rollback().await;
        return Err(TraceDecayError::Database {
            message: format!("database schema v{current} is newer than test target v{target}"),
            operation: "migrate_test_connection_to_version".to_owned(),
        });
    }
    if let Err(error) = run_migrations_through(&transaction, current, target).await {
        let _ = transaction.rollback().await;
        return Err(error);
    }
    transaction
        .commit()
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to commit test migrations: {error}"),
            operation: "migrate_test_connection_to_version".to_owned(),
        })
}

/// Dispatches a single migration by version number.
async fn run_migration(conn: &Transaction, version: u32) -> Result<()> {
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
        14 => migrate_v14(conn).await,
        15 => migrate_v15(conn).await,
        16 => migrate_v16(conn).await,
        17 => migrate_v17(conn).await,
        18 => migrate_v18(conn).await,
        19 => migrate_v19(conn).await,
        20 => migrate_v20(conn).await,
        21 => migrate_v21(conn).await,
        22 => migrate_v22(conn).await,
        23 => migrate_v23(conn).await,
        24 => migrate_v24(conn).await,
        25 => migrate_v25(conn).await,
        _ => Err(TraceDecayError::Database {
            message: format!("unknown migration version: {version}"),
            operation: "run_migration".to_string(),
        }),
    }
}

/// v19: additive typed fact identity, immutable assertion/evidence/lineage,
/// purgeable payloads, and resumable legacy projection backfill state.
async fn migrate_v19(conn: &Transaction) -> Result<()> {
    super::memory_v2::create_schema(conn, "migrate_v19").await
}

/// v20: completes the PR7 owner-bound memory contract for databases that
/// already received the additive v19 tables. It atomically rebuilds the
/// proposal transition projection for the relaxed applied-transition contract;
/// legacy projection backfill and cutover stay daemon-authorized runtime actions.
async fn migrate_v20(conn: &Transaction) -> Result<()> {
    super::memory_v2::upgrade_v20_schema(conn, "migrate_v20").await
}

/// v21: adds durable compatibility telemetry and projection/vector lifecycle
/// fields to current facts. Aggregate status and per-call repair receipts stay
/// derived by the daemon-authorized compatibility authority.
async fn migrate_v21(conn: &Transaction) -> Result<()> {
    super::memory_v2::upgrade_v21_schema(conn, "migrate_v21").await
}

/// v22: adds retry receipts, feedback-history/numeric-event parity, constrained
/// baseline fact relations, and the final proposal-state projection. All V22
/// data is owner-bound and does not retain a compatibility payload snapshot.
async fn migrate_v22(conn: &Transaction) -> Result<()> {
    super::memory_v2::upgrade_v22_schema(conn, "migrate_v22").await
}

/// v23: upgrades durable V22 relation provenance/parity and adds owner-keyed
/// compatibility-bank projections without reopening V20/V21 schema scope.
async fn migrate_v23(conn: &Transaction) -> Result<()> {
    super::memory_v2::upgrade_v23_schema(conn, "migrate_v23").await
}

/// v24: adds the payload-free Plan 13 evidence assembly ledger. The tables
/// retain immutable source membership, producer order, publication receipts,
/// and replay keys in the existing project database.
async fn migrate_v24(conn: &Transaction) -> Result<()> {
    super::evidence_assembly::install_evidence_assembly_schema(conn, "migrate_v24").await
}

/// v25: persists the canonical owner-bound external-source reducer state.
async fn migrate_v25(conn: &Transaction) -> Result<()> {
    super::external_source::install_external_source_schema(conn, "migrate_v25").await
}

/// Compatibility marker after v12 was exposed on the PR stack.
///
/// The dirty-bank schema now lives in the folded v11/fresh schema, but existing
/// databases may already carry `user_version = 12`. Keep the version monotonic
/// so later schema work can safely use v13 instead of reusing an exposed number.
#[allow(clippy::unused_async)] // keeps the migration dispatch uniform
async fn migrate_v12(_conn: &Transaction) -> Result<()> {
    Ok(())
}

/// v13: Cleanup marker for the (never-shipped) fact-archive schema.
///
/// An uncommitted revision of v13 briefly added archive columns (`state`,
/// `archived_at`, `archived_reason`, `merged_into`, `superseded_by`) to
/// `memory_facts`. Curation now hard-deletes losing facts instead, so this
/// migration drops those columns from any local development database that
/// ran the earlier revision, and is a no-op everywhere else.
async fn migrate_v13(conn: &Transaction) -> Result<()> {
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

/// v14: Memory-lifecycle additions — fact access tracking and the memory
/// operation log.
///
/// Adds `access_count` / `last_recalled_at` to `memory_facts` (bumped only
/// when a recall search RETURNS a fact, unlike `retrieval_count`, which also
/// counts probe/list scans) and creates `memory_oplog`, an append-only audit
/// of memory mutations. Idempotent: columns are probed before ALTER and the
/// table/index use IF NOT EXISTS, so databases created from the fresh schema
/// (which already includes both) pass through unchanged.
async fn migrate_v14(conn: &Transaction) -> Result<()> {
    let existing: std::collections::HashSet<String> = {
        let mut rows = conn
            .query("PRAGMA table_info(memory_facts)", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("v14: failed to read table_info: {e}"),
                operation: "migrate_v14".to_string(),
            })?;
        let mut names = std::collections::HashSet::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("v14: failed to iterate table_info: {e}"),
            operation: "migrate_v14".to_string(),
        })? {
            let name: String = row.get(1).map_err(|e| TraceDecayError::Database {
                message: format!("v14: failed to read column name: {e}"),
                operation: "migrate_v14".to_string(),
            })?;
            names.insert(name);
        }
        names
    };

    for (column, ddl) in [
        (
            "access_count",
            "ALTER TABLE memory_facts ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "last_recalled_at",
            "ALTER TABLE memory_facts ADD COLUMN last_recalled_at INTEGER",
        ),
    ] {
        if !existing.contains(column) {
            conn.execute(ddl, ())
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("v14: failed to add column {column}: {e}"),
                    operation: "migrate_v14".to_string(),
                })?;
        }
    }

    conn.execute_batch(MEMORY_OPLOG_SCHEMA)
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v14: failed to create memory_oplog: {e}"),
            operation: "migrate_v14".to_string(),
        })?;
    Ok(())
}

/// v15: compact memory vectors and reclaim pages.
///
/// Adds `hrr_precision` so f64 legacy blobs can be identified and re-encoded
/// once as f32 blobs. The default remains f32 for fresh writes; existing
/// non-compact blobs are marked f64 before the shared vector repair path runs.
async fn migrate_v15(conn: &Transaction) -> Result<()> {
    let mut has_precision = false;
    let mut rows = conn
        .query("PRAGMA table_info(memory_facts)", ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v15: failed to read memory_facts columns: {e}"),
            operation: "migrate_v15".to_string(),
        })?;
    while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
        message: format!("v15: failed to iterate memory_facts columns: {e}"),
        operation: "migrate_v15".to_string(),
    })? {
        let name: String = row.get(1).map_err(|e| TraceDecayError::Database {
            message: format!("v15: failed to read memory_facts column name: {e}"),
            operation: "migrate_v15".to_string(),
        })?;
        has_precision |= name == "hrr_precision";
    }

    if !has_precision {
        conn.execute(
            "ALTER TABLE memory_facts ADD COLUMN hrr_precision TEXT NOT NULL DEFAULT 'f32'",
            (),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v15: failed to add hrr_precision: {e}"),
            operation: "migrate_v15".to_string(),
        })?;
    }

    conn.execute(
        "UPDATE memory_facts
         SET hrr_precision = 'f64'
         WHERE hrr_vector IS NOT NULL
           AND length(hrr_vector) != ?1",
        params![crate::memory::encoding::HolographicEncoder::SERIALIZED_F32_BYTES as i64],
    )
    .await
    .map_err(|e| TraceDecayError::Database {
        message: format!("v15: failed to mark legacy vector precision: {e}"),
        operation: "migrate_v15".to_string(),
    })?;

    backfill_holographic_memory_vectors_and_banks(conn).await?;
    Ok(())
}

/// v16: `redundancy_pairs` — a freshness-validated cache of the duplicate pairs
/// computed by `tracedecay_redundancy`.
///
/// Lets other surfaces (diagnose near-duplicate enrichment, the dashboard,
/// future tools) read the last-known duplicates by an indexed lookup instead
/// of re-running the token-window scan. Rows are keyed on the canonical
/// `(node_a_id, node_b_id)` orientation the scan already emits (a < b by
/// `(file_path, start_line, id)`), and carry the `source_hash` of each side so
/// a reader can join against `node_fingerprints` and discard any row whose
/// stored hash no longer matches the current cached fingerprint. `ON DELETE
/// CASCADE` reclaims rows when either node is deleted, so orphan cleanup is
/// automatic and no explicit sweep is needed. Idempotent via `IF NOT EXISTS`.
async fn migrate_v16(conn: &Transaction) -> Result<()> {
    conn.execute_batch(REDUNDANCY_PAIRS_SCHEMA)
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v16: failed to create redundancy_pairs table: {e}"),
            operation: "migrate_v16".to_string(),
        })?;
    Ok(())
}

/// v17: semantic marker for the `attrs_start_line` sentinel fix.
///
/// The read path used to treat a stored `attrs_start_line = 0` as "unset" and
/// substitute `start_line`. But 0 is a *legitimate* value: an item whose
/// doc-comment / attribute block starts at the very top of a file (row 0) —
/// e.g. `/// doc\nfn foo() {}` extracts as `attrs_start_line=0`, `start_line=1`.
/// The conflation collapsed that to `attrs_start_line=1` after a DB round-trip,
/// so symbol-aware editing lost the leading doc block for first-in-file items.
///
/// The new scheme: the stored integer (including 0) is trusted verbatim; SQL
/// NULL is the only "unset" marker, and readers fall back to `start_line` for
/// NULL alone. Fresh databases now create the column as nullable with no
/// default (see `create_schema`).
///
/// This migration intentionally rewrites no data:
/// - Rows whose legitimate 0 was already overwritten by the v7 backfill
///   (`SET attrs_start_line = start_line WHERE attrs_start_line = 0`) are
///   indistinguishable from correct rows and cannot be recovered here; they
///   self-heal the next time their file is re-indexed and the true value is
///   written back.
/// - After v7, any remaining `attrs_start_line = 0` row necessarily has
///   `start_line = 0` (file-root nodes), for which 0 is already the correct
///   real value — mapping it to NULL would read back identically.
/// - The legacy `NOT NULL DEFAULT 0` column constraint on migrated databases
///   is left in place: `SQLite` cannot relax a column constraint without
///   rebuilding the table, and rebuilding `nodes` inside this exclusive
///   transaction (where `PRAGMA foreign_keys` cannot be toggled) would
///   cascade-delete every child table. The constraint is harmless — all
///   writers supply an explicit value, so the DEFAULT can never manufacture a
///   false 0, and NULL is only ever *read*, never written, on such databases.
#[allow(clippy::unused_async)] // keeps the migration dispatch uniform
async fn migrate_v17(_conn: &Transaction) -> Result<()> {
    Ok(())
}

/// v18: bounded, project-local semantic relationships between memory facts.
///
/// Relations deliberately use a closed vocabulary. Entity timestamps support
/// optimistic grooming without creating a global entity identity system.
async fn migrate_v18(conn: &Transaction) -> Result<()> {
    let mut has_updated_at = false;
    let mut rows = conn
        .query("PRAGMA table_info(memory_entities)", ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v18: failed to read memory_entities columns: {e}"),
            operation: "migrate_v18".to_string(),
        })?;
    while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
        message: format!("v18: failed to iterate memory_entities columns: {e}"),
        operation: "migrate_v18".to_string(),
    })? {
        let name: String = row.get(1).map_err(|e| TraceDecayError::Database {
            message: format!("v18: failed to read memory_entities column name: {e}"),
            operation: "migrate_v18".to_string(),
        })?;
        has_updated_at |= name == "updated_at";
    }
    if !has_updated_at {
        conn.execute(
            "ALTER TABLE memory_entities ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0",
            (),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v18: failed to add memory_entities.updated_at: {e}"),
            operation: "migrate_v18".to_string(),
        })?;
        conn.execute(
            "UPDATE memory_entities SET updated_at = created_at WHERE updated_at = 0",
            (),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v18: failed to backfill memory_entities.updated_at: {e}"),
            operation: "migrate_v18".to_string(),
        })?;
    }
    create_memory_fact_relations_schema(conn, "migrate_v18").await
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

/// Freshness-validated cache of `tracedecay_redundancy` duplicate pairs. Shared
/// verbatim by the fresh-schema path (`create_schema`) and the v16 migration so
/// the two cannot drift. See [`migrate_v16`] for the column contract.
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

/// Creates all core tables, FTS index, triggers, and indexes.
async fn migrate_v1(conn: &Transaction) -> Result<()> {
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
async fn migrate_v2(conn: &Transaction) -> Result<()> {
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
async fn migrate_v3(conn: &Transaction) -> Result<()> {
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
async fn migrate_v4(conn: &Transaction) -> Result<()> {
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
async fn migrate_v5(conn: &Transaction) -> Result<()> {
    // Rebuild the edges table keeping only distinct rows. We use a temp
    // table + swap because DELETE with a self-join subquery can be very
    // slow on large tables (the reporter had 13.9 M edges).
    conn.execute_schema_batch_step(
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
async fn migrate_v6(conn: &Transaction) -> Result<()> {
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
///
/// NOTE (reconciliation): this backfill (`SET attrs_start_line = start_line
/// WHERE attrs_start_line = 0`) treated a stored `0` as "unset". That conflates
/// a *legitimate* 0 — an item documented at the very top of a file — with the
/// column default, and irreversibly overwrites such rows with `start_line`. The
/// read path in [`crate::db::rows`] no longer makes that mistake: it trusts the
/// stored integer (including 0) and only falls back to `start_line` for a SQL
/// NULL / absent column. The fresh schema (`create_schema`) now declares the
/// column nullable so 0 and "unset" are distinct going forward. Rows whose
/// legitimate 0 was already destroyed by this historical backfill cannot be
/// recovered here — they self-heal the next time the file is re-indexed and the
/// true `attrs_start_line` is written back. The DDL below is intentionally left
/// as-is (not rewritten) to keep migration history stable for databases that
/// have already applied it.
async fn migrate_v7(conn: &Transaction) -> Result<()> {
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
async fn migrate_v8(conn: &Transaction) -> Result<()> {
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
///
/// A V8 database must contain none of the V9 table, column, or index objects.
/// Admission rejects even exact-looking objects before any V8 data is changed;
/// the surrounding migration transaction then keeps the schema and
/// `user_version` at V8 on every rejection.
async fn migrate_v9(conn: &Transaction) -> Result<()> {
    reject_precreated_v9_objects(conn).await?;
    create_and_verify_v9_read_cache(conn).await?;

    // V9 and its user_version publication execute in one immediate
    // transaction. A valid V8 source therefore always lacks this column;
    // accepting it would silently bless a malformed or partially edited V8.
    conn.execute("ALTER TABLE nodes ADD COLUMN parent_id TEXT", ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v9: failed to add parent_id column: {e}"),
            operation: "migrate_v9".to_string(),
        })?;
    verify_created_v9_parent_id_column(conn).await?;

    // V1 creates edges, so its absence at V8 is corruption and must fail
    // closed. When a node has multiple incoming Contains rows (legacy data
    // anomaly), the first matching row wins; later rows are not preserved.
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

    create_and_verify_schema_object(
        conn,
        "index",
        "idx_nodes_parent_id",
        V9_NODES_PARENT_INDEX_SQL,
    )
    .await?;

    Ok(())
}

const V9_READ_CACHE_TABLE_SQL: &str = "CREATE TABLE read_cache (
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
        )";

const V9_READ_CACHE_SESSION_INDEX_SQL: &str =
    "CREATE INDEX idx_read_cache_session ON read_cache(session_id, created_at)";

const V9_NODES_PARENT_INDEX_SQL: &str = "CREATE INDEX idx_nodes_parent_id ON nodes(parent_id)";

async fn reject_precreated_v9_objects(conn: &Transaction) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT description
             FROM (
                 SELECT
                     type AS sort_type,
                     name AS sort_name,
                     type || ' ' || quote(name) || ' on ' || quote(tbl_name) AS description
                 FROM sqlite_master
                 WHERE name IN ('read_cache', 'idx_read_cache_session', 'idx_nodes_parent_id')
                    OR tbl_name = 'read_cache'
                 UNION ALL
                 SELECT
                     'column' AS sort_type,
                     'parent_id' AS sort_name,
                     'column ''nodes.parent_id''' AS description
                 FROM pragma_table_info('nodes')
                 WHERE name = 'parent_id'
             )
             ORDER BY sort_type, sort_name
             LIMIT 1",
            (),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v9: failed to inspect V8 schema objects: {e}"),
            operation: "migrate_v9".to_string(),
        })?;
    let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
        message: format!("v9: failed to read V8 schema object: {e}"),
        operation: "migrate_v9".to_string(),
    })?
    else {
        return Ok(());
    };
    let precreated = row
        .get::<String>(0)
        .map_err(|e| TraceDecayError::Database {
            message: format!("v9: failed to decode V8 schema object: {e}"),
            operation: "migrate_v9".to_string(),
        })?;
    Err(TraceDecayError::Database {
        message: format!("v9: V8 admission rejected precreated V9 object: {precreated}"),
        operation: "migrate_v9".to_string(),
    })
}

async fn create_and_verify_v9_read_cache(conn: &Transaction) -> Result<()> {
    create_and_verify_schema_object(conn, "table", "read_cache", V9_READ_CACHE_TABLE_SQL).await?;
    create_and_verify_schema_object(
        conn,
        "index",
        "idx_read_cache_session",
        V9_READ_CACHE_SESSION_INDEX_SQL,
    )
    .await
}

async fn verify_created_v9_parent_id_column(conn: &Transaction) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT 1
             FROM pragma_table_info('nodes')
             WHERE name = 'parent_id'
               AND upper(type) = 'TEXT'
               AND \"notnull\" = 0
               AND dflt_value IS NULL
               AND pk = 0",
            (),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v9: failed to verify created nodes.parent_id column: {e}"),
            operation: "migrate_v9".to_string(),
        })?;
    let matches_contract = rows
        .next()
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v9: failed to read created nodes.parent_id contract: {e}"),
            operation: "migrate_v9".to_string(),
        })?
        .is_some();
    if matches_contract {
        Ok(())
    } else {
        Err(TraceDecayError::Database {
            message: "v9: created nodes.parent_id column does not match the exact contract"
                .to_string(),
            operation: "migrate_v9".to_string(),
        })
    }
}

async fn create_and_verify_schema_object(
    conn: &Transaction,
    object_type: &str,
    name: &str,
    expected_sql: &str,
) -> Result<()> {
    conn.execute(expected_sql, ())
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v9: failed to create {object_type} '{name}': {e}"),
            operation: "migrate_v9".to_string(),
        })?;
    let sql = schema_object_sql(conn, object_type, name)
        .await?
        .ok_or_else(|| TraceDecayError::Database {
            message: format!("v9: {object_type} '{name}' missing after create"),
            operation: "migrate_v9".to_string(),
        })?;
    if normalize_schema_sql(&sql) != normalize_schema_sql(expected_sql) {
        return Err(TraceDecayError::Database {
            message: format!(
                "v9: created {object_type} '{name}' SQL does not match the exact contract"
            ),
            operation: "migrate_v9".to_string(),
        });
    }
    Ok(())
}

async fn schema_object_sql(
    conn: &Transaction,
    object_type: &str,
    name: &str,
) -> Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master WHERE type = ?1 AND name = ?2",
            (object_type, name),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("v9: failed to probe sqlite_master for {object_type} '{name}': {e}"),
            operation: "migrate_v9".to_string(),
        })?;
    let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
        message: format!("v9: failed to read sqlite_master row for {object_type} '{name}': {e}"),
        operation: "migrate_v9".to_string(),
    })?
    else {
        return Ok(None);
    };
    Ok(Some(row.get::<String>(0).map_err(|e| {
        TraceDecayError::Database {
            message: format!(
                "v9: failed to decode sqlite_master sql for {object_type} '{name}': {e}"
            ),
            operation: "migrate_v9".to_string(),
        }
    })?))
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

// ---------------------------------------------------------------------------
// Migration V10: node_fingerprints (issue #83 — tracedecay_redundancy)
// ---------------------------------------------------------------------------

/// Creates the `node_fingerprints` table used by `tracedecay_redundancy` to
/// detect AST-isomorphic, control-flow-equivalent, and token-similar
/// function/method duplicates. Populated lazily on first redundancy query
/// and invalidated by `source_hash` mismatch.
async fn migrate_v10(conn: &Transaction) -> Result<()> {
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
async fn migrate_v11(conn: &Transaction) -> Result<()> {
    create_holographic_memory_schema(conn, "migrate_v11").await?;
    if legacy_memory_tables_exist(conn).await? {
        backfill_legacy_memory_as_facts(conn).await?;
        backfill_holographic_memory_vectors_and_banks(conn).await?;
    }
    Ok(())
}

async fn backfill_holographic_memory_vectors_and_banks(conn: &Transaction) -> Result<()> {
    let store = MemoryStore::new_engine_transaction(conn);
    loop {
        let updated = store.compute_missing_vectors(500).await?;
        if updated == 0 {
            break;
        }
    }
    store.rebuild_all_banks().await?;
    Ok(())
}

async fn legacy_memory_tables_exist(conn: &Transaction) -> Result<bool> {
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

async fn backfill_legacy_memory_as_facts(conn: &Transaction) -> Result<()> {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

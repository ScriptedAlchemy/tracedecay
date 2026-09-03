//! Schema creation for the tracedecay database.
//!
//! This binary creates every store at one final schema shape and never steps an
//! older shape forward. `PRAGMA user_version` records that shape as an atomic
//! integer built into `SQLite`; a store carrying any other value was written by
//! an incompatible binary and is refused at open with a fresh-start remedy.

use crate::db::connection::DatabaseEngineWriteConnection;
use crate::db::engine::{Connection, Executor, QueryExecutor, params};
use tracedecay_domain::errors::{Result, TraceDecayError};

mod final_shape;

const ROOT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS metadata (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

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
        ON read_cache(session_id, created_at);";

/// The one schema shape this binary creates and accepts. It is an identity
/// stamp, not a ladder rung: a store at any other version is refused.
///
/// Code topology lives only in the verified Grafeo generation. Exact memory
/// content, provenance, trust, retention, and feedback live only in the
/// canonical `memory_v2_*` tables; derived vectors are re-created from that
/// content. A current-stamped store containing a retired projection fails
/// closed before interpretation.
pub const SCHEMA_VERSION: u32 = 35;

/// The one prior shape this binary steps forward in place: v34 is v35 minus
/// the persisted payload-digest objects (#834). Every other stamp is still
/// refused with the fresh-start remedy.
pub const PAYLOAD_DIGEST_STEP_SOURCE_VERSION: u32 = 34;

/// Metadata key journaling the v34 -> v35 backfill receipt.
pub const PAYLOAD_DIGEST_BACKFILL_RECEIPT_KEY: &str = "memory_v2.payload_digest_backfill.v35";

/// Payload rows fingerprinted per short backfill write.
const PAYLOAD_DIGEST_BACKFILL_CHUNK_ROWS: usize = 512;

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
    let connection = writer.engine_connection();
    create_schema_engine_connection(&connection).await
}

async fn create_schema_engine_connection(conn: &DatabaseEngineWriteConnection) -> Result<()> {
    conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("create_schema: failed to configure fresh auto_vacuum: {error}"),
            operation: "create_schema".to_owned(),
        })?;
    let transaction = conn
        .authorized_long_lease_transaction()
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to acquire fresh-schema writer lock: {error}"),
            operation: "create_schema".to_owned(),
        })?;
    let result = create_schema_transaction(&transaction).await;
    match result {
        Ok(()) => transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to commit fresh schema: {error}"),
                operation: "create_schema".to_owned(),
            }),
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
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

async fn create_schema_transaction(conn: &(impl Executor + Sync)) -> Result<()> {
    conn.execute_batch(ROOT_SCHEMA)
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to create schema: {e}"),
            operation: "create_schema".to_string(),
        })?;

    super::memory_v2::create_schema(conn, "create_schema").await?;
    super::evidence_assembly::install_evidence_assembly_schema(conn, "create_schema").await?;
    super::external_source::install_external_source_schema(conn, "create_schema").await?;
    conn.execute_batch(tracedecay_rusqlite_runtime::repository::GRAPH_PUBLICATION_SCHEMA_V1)
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to create graph publication schema: {e}"),
            operation: "create_schema".to_string(),
        })?;
    conn.execute_batch(tracedecay_rusqlite_runtime::repository::SEMANTIC_VECTOR_STAGING_SCHEMA)
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to create semantic vector staging schema: {e}"),
            operation: "create_schema".to_string(),
        })?;
    conn.execute_batch(tracedecay_rusqlite_runtime::handoff::HANDOFF_OPEN_SCHEMA_V1)
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to create handoff-open schema: {e}"),
            operation: "create_schema".to_string(),
        })?;
    final_shape::require_exact_final_shape(conn).await?;
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

async fn retired_sqlite_projection_object(conn: &impl QueryExecutor) -> Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master
             WHERE type IN ('table', 'view', 'trigger')
               AND (
                   name IN (
                       'nodes', 'edges', 'files', 'unresolved_refs',
                       'node_fingerprints', 'redundancy_pairs',
                       'memory_facts', 'memory_entities',
                       'memory_fact_entities', 'memory_feedback_events',
                       'memory_oplog', 'memory_fact_relations',
                       'memory_banks', 'memory_bank_dirty',
                       'memory_v2_banks', 'memory_v2_bank_dirty',
                       'memory_v2_fact_relations',
                       'memory_v2_assertion_vectors',
                       'memory_v2_legacy_map', 'memory_v2_legacy_quarantine',
                       'memory_v2_backfill_progress',
                       'memory_v2_legacy_proposal_map',
                       'memory_v2_proposals',
                       'memory_v2_proposal_transitions',
                       'memory_v2_proposal_current',
                       'memory_v2_legacy_feedback_event_map',
                       'memory_v2_feedback_history_repair_progress',
                       'memory_v2_compatibility_operation_receipts',
                       'memory_v2_compatibility_banks',
                       'memory_v2_compatibility_bank_dirty'
                   )
                   OR name GLOB 'nodes_fts*'
                   OR name GLOB 'memory_facts_fts*'
               )
             ORDER BY name
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to probe for retired SQLite projection objects: {error}"),
            operation: "ensure_schema_current".to_owned(),
        })?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to read retired SQLite projection object probe: {error}"),
            operation: "ensure_schema_current".to_owned(),
        })?
    else {
        return Ok(None);
    };
    row.get::<String>(0)
        .map(Some)
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to decode retired SQLite projection object name: {error}"),
            operation: "ensure_schema_current".to_owned(),
        })
}

fn unsupported_schema_version(current: u32) -> TraceDecayError {
    TraceDecayError::reset_required(
        "SQLite store",
        format!(
            "database schema v{current} is not the v{SCHEMA_VERSION} shape this binary creates; \
             this store was created by an incompatible binary and cannot be upgraded in place. \
             Remove the store directory and let this binary create a fresh one."
        ),
    )
}

/// Verifies an opened store carries the schema this binary creates, creating it
/// when the file is still empty.
///
/// This binary has no upgrade ladder: a store stamped with any other version is
/// refused with the fresh-start remedy rather than stepped forward.
pub async fn ensure_schema_current(database: &crate::db::Database) -> Result<()> {
    let writer = database.writer_connection("ensure schema current").await?;
    let connection = writer.engine_connection();
    ensure_schema_current_engine_connection(&connection).await
}

async fn ensure_schema_current_engine_connection(
    conn: &DatabaseEngineWriteConnection,
) -> Result<()> {
    let current = get_version(conn).await?;
    if current == 0 && !store_has_objects(conn).await? {
        return create_schema_engine_connection(conn).await;
    }
    if current == PAYLOAD_DIGEST_STEP_SOURCE_VERSION {
        step_payload_digests(conn).await?;
    }
    verify_final_schema_connection(conn).await
}

/// Steps a v34 store to v35: creates the payload-digest objects (idempotent)
/// and fingerprints every existing payload in bounded chunks, each its own
/// short write, so the writer is released between chunks and an interrupted
/// run resumes from the rows still missing a digest. The stamp moves only
/// after the last chunk and the receipt are durable.
async fn step_payload_digests(conn: &DatabaseEngineWriteConnection) -> Result<()> {
    const OPERATION: &str = "step_payload_digests";
    final_shape::require_final_shape_except_payload_digests(conn).await?;
    conn.execute_batch(super::memory_v2::PAYLOAD_DIGESTS_SCHEMA)
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to create payload digest objects: {error}"),
            operation: OPERATION.to_owned(),
        })?;
    let mut cursor: i64 = 0;
    let mut backfilled: u64 = 0;
    loop {
        let chunk = payload_digest_backfill_chunk(conn, cursor).await?;
        let Some(last_rowid) = chunk.last().map(|row| row.rowid) else {
            break;
        };
        for row in &chunk {
            let digest = payload_content_digest(&row.content);
            conn.execute(
                "INSERT OR IGNORE INTO memory_v2_assertion_payload_digests(
                    payload_rowid, assertion_id, fact_id, owner_kind, project_id, content_digest
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    row.rowid,
                    row.assertion_id.as_str(),
                    row.fact_id.as_str(),
                    row.owner_kind.as_str(),
                    row.project_id.as_str(),
                    digest.as_str(),
                ],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to backfill payload digest: {error}"),
                operation: OPERATION.to_owned(),
            })?;
            backfilled += 1;
        }
        cursor = last_rowid;
    }
    let receipt = serde_json::json!({
        "from_version": PAYLOAD_DIGEST_STEP_SOURCE_VERSION,
        "to_version": SCHEMA_VERSION,
        "backfilled_rows": backfilled,
        "chunk_rows": PAYLOAD_DIGEST_BACKFILL_CHUNK_ROWS,
    });
    conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
        params![PAYLOAD_DIGEST_BACKFILL_RECEIPT_KEY, receipt.to_string()],
    )
    .await
    .map_err(|error| TraceDecayError::Database {
        message: format!("failed to journal payload digest backfill receipt: {error}"),
        operation: OPERATION.to_owned(),
    })?;
    set_version(conn, SCHEMA_VERSION).await
}

struct PayloadDigestBackfillRow {
    rowid: i64,
    assertion_id: String,
    fact_id: String,
    owner_kind: String,
    project_id: String,
    content: String,
}

async fn payload_digest_backfill_chunk(
    conn: &impl QueryExecutor,
    after_rowid: i64,
) -> Result<Vec<PayloadDigestBackfillRow>> {
    const OPERATION: &str = "step_payload_digests";
    let map = |error: String| TraceDecayError::Database {
        message: error,
        operation: OPERATION.to_owned(),
    };
    let mut rows = conn
        .query(
            "SELECT payloads.rowid, payloads.assertion_id, payloads.fact_id,
                    payloads.owner_kind, payloads.project_id, payloads.content
             FROM memory_v2_assertion_payloads AS payloads
             LEFT JOIN memory_v2_assertion_payload_digests AS digests
               ON digests.payload_rowid = payloads.rowid
             WHERE digests.payload_rowid IS NULL AND payloads.rowid > ?1
             ORDER BY payloads.rowid ASC
             LIMIT ?2",
            params![
                after_rowid,
                i64::try_from(PAYLOAD_DIGEST_BACKFILL_CHUNK_ROWS).unwrap_or(i64::MAX)
            ],
        )
        .await
        .map_err(|error| {
            map(format!(
                "failed to read payload digest backfill chunk: {error}"
            ))
        })?;
    let mut chunk = Vec::with_capacity(PAYLOAD_DIGEST_BACKFILL_CHUNK_ROWS);
    while let Some(row) = rows.next().await.map_err(|error| {
        map(format!(
            "failed to read payload digest backfill row: {error}"
        ))
    })? {
        let column = |index: i32| -> Result<String> {
            row.get::<String>(index)
                .map_err(|error| map(format!("failed to read backfill column {index}: {error}")))
        };
        chunk.push(PayloadDigestBackfillRow {
            rowid: row
                .get::<i64>(0)
                .map_err(|error| map(format!("failed to read backfill rowid: {error}")))?,
            assertion_id: column(1)?,
            fact_id: column(2)?,
            owner_kind: column(3)?,
            project_id: column(4)?,
            content: column(5)?,
        });
    }
    Ok(chunk)
}

/// Byte-for-byte the digest `store::memory::crud::content_digest` derives for
/// a payload's `content`: `sha256:` plus lowercase hex.
fn payload_content_digest(content: &str) -> String {
    use sha2::Digest as _;
    tracedecay_domain::canonical_text::encode_tagged_lowercase_hex(
        "sha256:",
        &sha2::Sha256::digest(content.as_bytes()),
    )
}

/// Verifies that an already-existing store has the one exact final shape this
/// binary accepts. This query-only authority intentionally cannot initialize a
/// fresh file, so read-only mounts cannot change persisted state.
pub(crate) async fn verify_final_schema_connection(conn: &impl QueryExecutor) -> Result<()> {
    let current = get_version(conn).await?;
    if current == PAYLOAD_DIGEST_STEP_SOURCE_VERSION {
        // A read-only mount may not step the store; the message names the
        // writer-side remedy instead of the fresh-start reset.
        return Err(TraceDecayError::Database {
            message: format!(
                "database schema v{current} is one step behind v{SCHEMA_VERSION}: \
                 the payload digest step is pending and runs the next time a writer \
                 opens this store; retry after that open instead of resetting the store"
            ),
            operation: "verify_final_schema".to_owned(),
        });
    }
    if current != SCHEMA_VERSION {
        return Err(unsupported_schema_version(current));
    }
    if let Some(object) = retired_sqlite_projection_object(conn).await? {
        return Err(TraceDecayError::reset_required(
            "SQLite store",
            format!(
                "database schema v{current} still contains retired SQLite projection object \
                 '{object}'; remove the store directory and let this binary create the exact \
                 relational shape"
            ),
        ));
    }
    final_shape::require_exact_final_shape(conn).await?;
    Ok(())
}

#[cfg(test)]
pub(crate) async fn ensure_schema_current_connection(conn: &Connection) -> Result<()> {
    let current = get_version(conn).await?;
    if current == 0 && !store_has_objects(conn).await? {
        return create_schema_connection(conn).await;
    }
    if current == PAYLOAD_DIGEST_STEP_SOURCE_VERSION {
        step_payload_digests_connection(conn).await?;
    }
    verify_final_schema_connection(conn).await
}

/// Test-runtime twin of [`step_payload_digests`] on a plain connection.
#[cfg(test)]
async fn step_payload_digests_connection(conn: &Connection) -> Result<()> {
    const OPERATION: &str = "step_payload_digests";
    final_shape::require_final_shape_except_payload_digests(conn).await?;
    conn.execute_batch(super::memory_v2::PAYLOAD_DIGESTS_SCHEMA)
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to create payload digest objects: {error}"),
            operation: OPERATION.to_owned(),
        })?;
    let mut cursor: i64 = 0;
    let mut backfilled: u64 = 0;
    loop {
        let chunk = payload_digest_backfill_chunk(conn, cursor).await?;
        let Some(last_rowid) = chunk.last().map(|row| row.rowid) else {
            break;
        };
        for row in &chunk {
            let digest = payload_content_digest(&row.content);
            conn.execute(
                "INSERT OR IGNORE INTO memory_v2_assertion_payload_digests(
                    payload_rowid, assertion_id, fact_id, owner_kind, project_id, content_digest
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    row.rowid,
                    row.assertion_id.as_str(),
                    row.fact_id.as_str(),
                    row.owner_kind.as_str(),
                    row.project_id.as_str(),
                    digest.as_str(),
                ],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to backfill payload digest: {error}"),
                operation: OPERATION.to_owned(),
            })?;
            backfilled += 1;
        }
        cursor = last_rowid;
    }
    let receipt = serde_json::json!({
        "from_version": PAYLOAD_DIGEST_STEP_SOURCE_VERSION,
        "to_version": SCHEMA_VERSION,
        "backfilled_rows": backfilled,
        "chunk_rows": PAYLOAD_DIGEST_BACKFILL_CHUNK_ROWS,
    });
    conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
        params![PAYLOAD_DIGEST_BACKFILL_RECEIPT_KEY, receipt.to_string()],
    )
    .await
    .map_err(|error| TraceDecayError::Database {
        message: format!("failed to journal payload digest backfill receipt: {error}"),
        operation: OPERATION.to_owned(),
    })?;
    set_version(conn, SCHEMA_VERSION).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

use libsql::{params, Connection};

use super::{raw, LcmError, LcmRawMessage, LcmStorageKind};

use super::util;

pub const LCM_SCHEMA_VERSION: i64 = 5;

const MIGRATION_NAME: &str = "lcm";
const TRUNCATION_MARKER: &str = "\n[truncated by tracedecay]";

/// Raw-message FTS structure (schema v3): index only `index_text`, matching
/// hermes-lcm `build_message_fts_spec` (store.py:173-204), which indexes
/// nothing but the message content column. Earlier schemas also indexed
/// `role` and `metadata_json`, so an unqualified MATCH over-matched rows via
/// role names or metadata text. Role and source filtering happen as plain
/// SQL predicates on `lcm_raw_messages`, never through the FTS index.
const RAW_FTS_DDL: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS lcm_raw_messages_fts USING fts5(
        index_text,
        content='lcm_raw_messages',
        content_rowid='store_id'
    );
    CREATE TRIGGER IF NOT EXISTS lcm_raw_messages_fts_insert
        AFTER INSERT ON lcm_raw_messages BEGIN
            INSERT INTO lcm_raw_messages_fts(rowid, index_text)
            VALUES (NEW.store_id, NEW.index_text);
        END;
    CREATE TRIGGER IF NOT EXISTS lcm_raw_messages_fts_delete
        AFTER DELETE ON lcm_raw_messages BEGIN
            INSERT INTO lcm_raw_messages_fts(lcm_raw_messages_fts, rowid, index_text)
            VALUES ('delete', OLD.store_id, OLD.index_text);
        END;
    CREATE TRIGGER IF NOT EXISTS lcm_raw_messages_fts_update
        AFTER UPDATE ON lcm_raw_messages BEGIN
            INSERT INTO lcm_raw_messages_fts(lcm_raw_messages_fts, rowid, index_text)
            VALUES ('delete', OLD.store_id, OLD.index_text);
            INSERT INTO lcm_raw_messages_fts(rowid, index_text)
            VALUES (NEW.store_id, NEW.index_text);
        END;";

/// Returns whether the raw-message FTS table and triggers already use the
/// v3 content-only structure. Pre-v3 objects mention `metadata_json` in
/// their DDL; a missing table counts as current here because presence is
/// checked separately (doctor) or guaranteed (migration runs the DDL).
pub(crate) async fn raw_fts_structure_is_current(conn: &Connection) -> Option<bool> {
    let stale = util::fetch_i64(
        conn,
        "SELECT COUNT(*) FROM sqlite_master
         WHERE name IN ('lcm_raw_messages_fts',
                        'lcm_raw_messages_fts_insert',
                        'lcm_raw_messages_fts_delete',
                        'lcm_raw_messages_fts_update')
           AND sql LIKE '%metadata_json%'",
        (),
        "raw FTS structure query returned no rows",
    )
    .await
    .ok()?;
    Some(stale == 0)
}

/// Drops any existing raw-message FTS table/triggers (old or new shape),
/// recreates the v3 content-only structure, and repopulates the index from
/// `lcm_raw_messages` via the FTS5 `'rebuild'` command. Used by the schema
/// migration and the doctor repair path; idempotent and data-preserving
/// because the index is derived entirely from the content table.
pub(crate) async fn rebuild_raw_fts(conn: &Connection) -> Option<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS lcm_raw_messages_fts_insert;
         DROP TRIGGER IF EXISTS lcm_raw_messages_fts_delete;
         DROP TRIGGER IF EXISTS lcm_raw_messages_fts_update;
         DROP TABLE IF EXISTS lcm_raw_messages_fts;",
    )
    .await
    .ok()?;
    conn.execute_batch(RAW_FTS_DDL).await.ok()?;
    conn.execute(
        "INSERT INTO lcm_raw_messages_fts(lcm_raw_messages_fts) VALUES('rebuild')",
        (),
    )
    .await
    .ok()?;
    Some(())
}

pub(crate) async fn ensure_lcm_schema(conn: &Connection) -> Result<(), LcmError> {
    // Mirrors hermes-lcm `run_versioned_migrations`: version steps are
    // monotonic, so a database written by a newer release is left untouched
    // (no marker downgrade, no carry-forward re-run against newer data).
    if schema_version(conn)
        .await
        .is_some_and(|version| version >= LCM_SCHEMA_VERSION)
    {
        return Ok(());
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_schema_migrations (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            applied_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE TABLE IF NOT EXISTS lcm_raw_messages (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            store_id INTEGER PRIMARY KEY AUTOINCREMENT,
            role TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            timestamp INTEGER,
            content TEXT,
            content_hash TEXT NOT NULL,
            storage_kind TEXT NOT NULL CHECK(storage_kind IN ('inline', 'external')),
            payload_ref TEXT,
            snippet_text TEXT NOT NULL,
            index_text TEXT NOT NULL,
            legacy_source INTEGER NOT NULL DEFAULT 0,
            legacy_truncated INTEGER NOT NULL DEFAULT 0,
            metadata_json TEXT,
            UNIQUE(provider, message_id),
            FOREIGN KEY(provider, session_id)
                REFERENCES sessions(provider, session_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_lcm_raw_session_order
            ON lcm_raw_messages(provider, session_id, store_id);
        -- Schema v4: the dashboard session view filters by session_id alone
        -- (no provider), which the (provider, session_id, …) index cannot
        -- serve; without this index every session click full-scans the
        -- text-heavy table three times (count, token estimate, page).
        CREATE INDEX IF NOT EXISTS idx_lcm_raw_session_id
            ON lcm_raw_messages(session_id);
        CREATE TABLE IF NOT EXISTS lcm_external_payloads (
            payload_ref TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            message_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            byte_count INTEGER NOT NULL,
            char_count INTEGER NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            metadata_json TEXT,
            UNIQUE(provider, message_id, payload_ref),
            FOREIGN KEY(provider, session_id)
                REFERENCES sessions(provider, session_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_lcm_external_payloads_owner
            ON lcm_external_payloads(provider, session_id);
        CREATE TABLE IF NOT EXISTS lcm_gc_marks (
            payload_ref TEXT PRIMARY KEY,
            state TEXT NOT NULL CHECK(state IN ('unreferenced', 'missing')),
            first_seen_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE TABLE IF NOT EXISTS lcm_gc_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS lcm_summary_nodes (
            node_id TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            conversation_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            depth INTEGER NOT NULL,
            summary_text TEXT NOT NULL,
            summary_hash TEXT NOT NULL,
            summary_token_count INTEGER NOT NULL,
            source_token_count INTEGER NOT NULL,
            source_time_start INTEGER,
            source_time_end INTEGER,
            expand_hint TEXT,
            metadata_json TEXT,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            FOREIGN KEY(provider, session_id)
                REFERENCES sessions(provider, session_id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS lcm_summary_sources (
            node_id TEXT NOT NULL,
            source_kind TEXT NOT NULL CHECK(source_kind IN ('raw_message', 'summary_node')),
            source_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            PRIMARY KEY(node_id, ordinal),
            FOREIGN KEY(node_id) REFERENCES lcm_summary_nodes(node_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_lcm_summary_nodes_session_depth_time
            ON lcm_summary_nodes(
                provider, session_id, depth, source_time_start, source_time_end, created_at
            );
        CREATE INDEX IF NOT EXISTS idx_lcm_summary_sources_source
            ON lcm_summary_sources(source_kind, source_id);
        CREATE TABLE IF NOT EXISTS lcm_lifecycle_state (
            provider TEXT NOT NULL,
            conversation_id TEXT NOT NULL,
            current_session_id TEXT NOT NULL,
            last_finalized_session_id TEXT,
            current_frontier_store_id INTEGER,
            last_finalized_frontier_store_id INTEGER,
            rollover_at INTEGER,
            reset_at INTEGER,
            maintenance_at INTEGER,
            boundary_skip_at INTEGER,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            PRIMARY KEY(provider, conversation_id)
        );
        CREATE TABLE IF NOT EXISTS lcm_maintenance_debt (
            provider TEXT NOT NULL,
            conversation_id TEXT NOT NULL,
            debt_id TEXT NOT NULL,
            debt_kind TEXT NOT NULL,
            from_store_id INTEGER,
            to_store_id INTEGER,
            metadata_json TEXT,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            PRIMARY KEY(provider, conversation_id, debt_id),
            FOREIGN KEY(provider, conversation_id)
                REFERENCES lcm_lifecycle_state(provider, conversation_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_lcm_maintenance_debt_kind
            ON lcm_maintenance_debt(provider, debt_kind, created_at);
        CREATE VIRTUAL TABLE IF NOT EXISTS lcm_summary_nodes_fts USING fts5(
            summary_text, expand_hint, metadata_json,
            content='lcm_summary_nodes',
            content_rowid='rowid'
        );
        CREATE TRIGGER IF NOT EXISTS lcm_summary_nodes_fts_insert
            AFTER INSERT ON lcm_summary_nodes BEGIN
                INSERT INTO lcm_summary_nodes_fts(rowid, summary_text, expand_hint, metadata_json)
                VALUES (NEW.rowid, NEW.summary_text, NEW.expand_hint, NEW.metadata_json);
            END;
        CREATE TRIGGER IF NOT EXISTS lcm_summary_nodes_fts_delete
            AFTER DELETE ON lcm_summary_nodes BEGIN
                INSERT INTO lcm_summary_nodes_fts(
                    lcm_summary_nodes_fts, rowid, summary_text, expand_hint, metadata_json
                )
                VALUES ('delete', OLD.rowid, OLD.summary_text, OLD.expand_hint, OLD.metadata_json);
            END;
        CREATE TRIGGER IF NOT EXISTS lcm_summary_nodes_fts_update
            AFTER UPDATE ON lcm_summary_nodes BEGIN
                INSERT INTO lcm_summary_nodes_fts(
                    lcm_summary_nodes_fts, rowid, summary_text, expand_hint, metadata_json
                )
                VALUES ('delete', OLD.rowid, OLD.summary_text, OLD.expand_hint, OLD.metadata_json);
                INSERT INTO lcm_summary_nodes_fts(rowid, summary_text, expand_hint, metadata_json)
                VALUES (NEW.rowid, NEW.summary_text, NEW.expand_hint, NEW.metadata_json);
            END;",
    )
    .await?;

    // Schema v3: the raw-message FTS index dropped the role and
    // metadata_json columns (see RAW_FTS_DDL). The rebuild is gated on the
    // stored structure so later version bumps (e.g. the v4 index above)
    // don't re-pay a full FTS rebuild; a retry after a partially applied
    // earlier run still converges because the index is fully derived from
    // lcm_raw_messages. A fresh store has no FTS objects at all (they are
    // created by the rebuild, not the DDL batch above), so presence is
    // checked too — `raw_fts_structure_is_current` deliberately counts a
    // missing table as current.
    let fts_exists = util::fetch_i64(
        conn,
        "SELECT COUNT(*) FROM sqlite_master WHERE name = 'lcm_raw_messages_fts'",
        (),
        "raw FTS presence query returned no rows",
    )
    .await?
        > 0;
    if !fts_exists
        || !raw_fts_structure_is_current(conn)
            .await
            .ok_or_else(|| LcmError::Db("raw FTS structure check failed".to_string()))?
    {
        rebuild_raw_fts(conn)
            .await
            .ok_or_else(|| LcmError::Db("raw FTS rebuild failed".to_string()))?;
    }

    // Schema v2: lifecycle rows gained the compression-boundary cooldown
    // marker. Databases created before the column existed need the ALTER;
    // the error is ignored when the column is already present.
    let _ = conn
        .execute(
            "ALTER TABLE lcm_lifecycle_state ADD COLUMN boundary_skip_at INTEGER",
            (),
        )
        .await;

    carry_forward_legacy_messages(conn).await?;
    conn.execute(
        "INSERT INTO session_schema_migrations(name, version)
         VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET
            version = excluded.version,
            applied_at = unixepoch()",
        params![MIGRATION_NAME, LCM_SCHEMA_VERSION],
    )
    .await?;
    Ok(())
}

pub(crate) async fn schema_version(conn: &Connection) -> Option<i64> {
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            params![MIGRATION_NAME],
        )
        .await
        .ok()?;
    rows.next().await.ok()??.get(0).ok()
}

#[allow(dead_code)] // Foundation slice: consumed by follow-up GC/reporting cards.
pub(crate) async fn get_gc_meta(conn: &Connection, key: &str) -> Result<Option<String>, LcmError> {
    let mut rows = conn
        .query("SELECT value FROM lcm_gc_meta WHERE key = ?1", params![key])
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

#[allow(dead_code)] // Foundation slice: consumed by follow-up GC/reporting cards.
pub(crate) async fn set_gc_meta(conn: &Connection, key: &str, value: &str) -> Result<(), LcmError> {
    conn.execute(
        "INSERT OR REPLACE INTO lcm_gc_meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .await?;
    Ok(())
}

#[allow(dead_code)] // Foundation slice: consumed by follow-up GC/reporting cards.
pub(crate) async fn clear_gc_meta(conn: &Connection, key: &str) -> Result<(), LcmError> {
    conn.execute("DELETE FROM lcm_gc_meta WHERE key = ?1", params![key])
        .await?;
    Ok(())
}

pub(crate) async fn load_raw_message(
    conn: &Connection,
    provider: &str,
    message_id: &str,
) -> Option<LcmRawMessage> {
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, store_id, role, ordinal,
                    timestamp, content, content_hash, storage_kind, payload_ref,
                    legacy_source, legacy_truncated, metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1 AND message_id = ?2",
            params![provider, message_id],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let storage_kind_text: String = row.get(9).ok()?;
    let content: Option<String> = row.get(7).ok()?;
    Some(LcmRawMessage {
        provider: row.get(0).ok()?,
        message_id: row.get(1).ok()?,
        session_id: row.get(2).ok()?,
        store_id: row.get(3).ok()?,
        role: row.get(4).ok()?,
        ordinal: row.get(5).ok()?,
        timestamp: row.get(6).ok()?,
        content: content.unwrap_or_default(),
        content_hash: row.get(8).ok()?,
        storage_kind: LcmStorageKind::from_db(&storage_kind_text)?,
        payload_ref: row.get(10).ok()?,
        legacy_source: row.get::<i64>(11).unwrap_or(0) != 0,
        legacy_truncated: row.get::<i64>(12).unwrap_or(0) != 0,
        metadata_json: row.get(13).ok()?,
    })
}

async fn carry_forward_legacy_messages(conn: &Connection) -> Result<(), LcmError> {
    conn.execute("BEGIN IMMEDIATE", ()).await?;
    let carry_forward = carry_forward_legacy_messages_in_transaction(conn).await;
    match carry_forward {
        Ok(()) => {
            if let Err(err) = conn.execute("COMMIT", ()).await {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(err.into())
            } else {
                Ok(())
            }
        }
        Err(err) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(err)
        }
    }
}

async fn carry_forward_legacy_messages_in_transaction(conn: &Connection) -> Result<(), LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, role, timestamp, ordinal,
                    text, metadata_json
             FROM session_messages
             ORDER BY provider, session_id, ordinal, message_id",
            (),
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let provider: String = row.get(0)?;
        let message_id: String = row.get(1)?;
        let session_id: String = row.get(2)?;
        let role: String = row.get(3)?;
        let timestamp: Option<i64> = row.get(4)?;
        let ordinal: i64 = row.get(5)?;
        let content: String = row.get(6)?;
        let metadata_json: Option<String> = row.get(7)?;
        let legacy_truncated = content.contains(TRUNCATION_MARKER);
        let content_hash = raw::sha256_hex(&content);
        let snippet_text = raw::derived_text_for_snippet(&content);
        let index_text = raw::derived_text_for_index(&content);

        conn.execute(
            "INSERT OR IGNORE INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, snippet_text,
                index_text, legacy_source, legacy_truncated, metadata_json
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11, 1, ?12, ?13)",
            params![
                provider.as_str(),
                message_id.as_str(),
                session_id.as_str(),
                role.as_str(),
                ordinal,
                util::opt_i64(timestamp),
                content.as_str(),
                content_hash.as_str(),
                LcmStorageKind::Inline.as_str(),
                snippet_text.as_str(),
                index_text.as_str(),
                i64::from(legacy_truncated),
                util::opt_text(metadata_json.as_deref()),
            ],
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use libsql::Builder;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn ensure_lcm_schema_errors_and_rolls_back_failed_legacy_carry_forward(
    ) -> Result<(), String> {
        let tmp = TempDir::new().map_err(|err| err.to_string())?;
        let db_path = tmp.path().join("sessions.db");
        let db = Builder::new_local(&db_path)
            .build()
            .await
            .map_err(|err| err.to_string())?;
        let conn = db.connect().map_err(|err| err.to_string())?;
        conn.execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
                title TEXT,
                started_at INTEGER,
                ended_at INTEGER,
                transcript_path TEXT,
                metadata_json TEXT,
                PRIMARY KEY(provider, session_id)
            );
            CREATE TABLE session_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                timestamp INTEGER,
                ordinal INTEGER NOT NULL,
                text TEXT NOT NULL,
                kind TEXT,
                model TEXT,
                tool_names TEXT,
                source_path TEXT,
                source_offset INTEGER,
                metadata_json TEXT,
                PRIMARY KEY(provider, message_id)
            );
            CREATE TABLE lcm_raw_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                store_id INTEGER PRIMARY KEY AUTOINCREMENT,
                role TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                timestamp INTEGER,
                content TEXT,
                content_hash TEXT NOT NULL,
                storage_kind TEXT NOT NULL CHECK(storage_kind IN ('inline', 'external')),
                payload_ref TEXT,
                snippet_text TEXT NOT NULL,
                index_text TEXT NOT NULL,
                legacy_source INTEGER NOT NULL DEFAULT 0,
                legacy_truncated INTEGER NOT NULL DEFAULT 0,
                metadata_json TEXT,
                UNIQUE(provider, message_id)
            );
            CREATE TRIGGER lcm_raw_messages_fail_second
            BEFORE INSERT ON lcm_raw_messages
            WHEN NEW.message_id = 'legacy-message-2'
            BEGIN
                SELECT RAISE(ABORT, 'legacy carry-forward insert failed');
            END;
            INSERT INTO sessions(provider, session_id, project_key, project_path)
            VALUES ('cursor', 'legacy-session', '/tmp/project', '/tmp/project');
            INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
            VALUES
              ('cursor', 'legacy-message-1', 'legacy-session', 'assistant', 1, 'legacy one'),
              ('cursor', 'legacy-message-2', 'legacy-session', 'assistant', 2, 'legacy two');",
        )
        .await
        .map_err(|err| err.to_string())?;

        let Err(err) = ensure_lcm_schema(&conn).await else {
            return Err("failed carry-forward insert should propagate".to_string());
        };
        assert!(matches!(err, LcmError::Db(_)));
        assert_eq!(
            util::fetch_i64(
                &conn,
                "SELECT COUNT(*) FROM lcm_raw_messages",
                (),
                "raw count",
            )
            .await
            .map_err(|err| err.to_string())?,
            0
        );
        assert_eq!(
            util::fetch_i64(
                &conn,
                "SELECT COUNT(*) FROM session_schema_migrations WHERE name = 'lcm'",
                (),
                "migration marker count",
            )
            .await
            .map_err(|err| err.to_string())?,
            0
        );
        Ok(())
    }
}

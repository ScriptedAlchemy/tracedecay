use crate::application::session::compatibility::projected_content_hash;
#[cfg(test)]
use crate::db::engine::{Connection, TransactionBehavior};
use crate::db::engine::{Executor, QueryExecutor, params};

use super::{LcmError, LcmRawMessage, LcmStorageKind, raw};

#[cfg(test)]
use super::util;

pub const LCM_SCHEMA_VERSION: i64 = 7;

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

/// Returns whether the raw-message FTS table and all three synchronization
/// triggers use the v3 content-only contracts.
pub async fn raw_fts_structure_is_current(
    conn: &(impl QueryExecutor + ?Sized),
) -> Option<bool> {
    let mut rows = conn
        .query(
            "SELECT type, name, tbl_name, COALESCE(sql, '')
             FROM sqlite_master
             WHERE name IN ('lcm_raw_messages_fts',
                            'lcm_raw_messages_fts_insert',
                            'lcm_raw_messages_fts_delete',
                            'lcm_raw_messages_fts_update')",
            (),
        )
        .await
        .ok()?;
    let mut table_current = false;
    let mut insert_current = false;
    let mut delete_current = false;
    let mut update_current = false;
    while let Some(row) = rows.next().await.ok()? {
        let object_type: String = row.get(0).ok()?;
        let name: String = row.get(1).ok()?;
        let table_name: String = row.get(2).ok()?;
        let sql: String = row.get(3).ok()?;
        let sql = compact_sql(&sql);
        match name.as_str() {
            "lcm_raw_messages_fts" => {
                table_current = object_type == "table"
                    && sql.contains(
                        "usingfts5(index_text,content='lcm_raw_messages',content_rowid='store_id')",
                    );
            }
            "lcm_raw_messages_fts_insert" => {
                insert_current = object_type == "trigger"
                    && table_name == "lcm_raw_messages"
                    && sql.contains("afterinsertonlcm_raw_messagesbegin")
                    && sql.contains(
                        "insertintolcm_raw_messages_fts(rowid,index_text)\
                         values(new.store_id,new.index_text)",
                    );
            }
            "lcm_raw_messages_fts_delete" => {
                delete_current = object_type == "trigger"
                    && table_name == "lcm_raw_messages"
                    && sql.contains("afterdeleteonlcm_raw_messagesbegin")
                    && sql.contains(
                        "insertintolcm_raw_messages_fts\
                         (lcm_raw_messages_fts,rowid,index_text)\
                         values('delete',old.store_id,old.index_text)",
                    );
            }
            "lcm_raw_messages_fts_update" => {
                update_current = object_type == "trigger"
                    && table_name == "lcm_raw_messages"
                    && sql.contains("afterupdateonlcm_raw_messagesbegin")
                    && sql.contains(
                        "insertintolcm_raw_messages_fts\
                         (lcm_raw_messages_fts,rowid,index_text)\
                         values('delete',old.store_id,old.index_text)",
                    )
                    && sql.contains(
                        "insertintolcm_raw_messages_fts(rowid,index_text)\
                         values(new.store_id,new.index_text)",
                    );
            }
            _ => {}
        }
    }
    Some(table_current && insert_current && delete_current && update_current)
}

fn compact_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Drops any existing raw-message FTS table/triggers (old or new shape),
/// recreates the v3 content-only structure, and repopulates the index from
/// `lcm_raw_messages` via the FTS5 `'rebuild'` command. Used by the schema
/// migration and the doctor repair path; idempotent and data-preserving
/// because the index is derived entirely from the content table.
pub async fn rebuild_raw_fts(conn: &(impl Executor + ?Sized)) -> Option<()> {
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

/// Test-only convenience wrapper: production schema creation runs through
/// [`ensure_lcm_schema_in_transaction`] inside the callers' own transactions.
#[cfg(test)]
pub async fn ensure_lcm_schema(conn: &Connection) -> Result<(), LcmError> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    match ensure_lcm_schema_in_transaction(&transaction).await {
        Ok(()) => transaction.commit().await.map_err(Into::into),
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(LcmError::Db(format!(
                "{error}; rollback after LCM schema migration failed: {rollback_error}"
            ))),
        },
    }
}

pub async fn ensure_lcm_schema_in_transaction(
    conn: &(impl Executor + ?Sized),
) -> Result<(), LcmError> {
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
        -- Schema v7: eligibility is an indexed expression rather than a
        -- partial-index predicate, allowing the pending queue to constrain
        -- it without forcing an index or scanning JSON at query time.
        DROP INDEX IF EXISTS idx_lcm_summary_nodes_codex_pending_session_order;
        DROP INDEX IF EXISTS idx_lcm_summary_nodes_codex_pending_root_order;
        CREATE INDEX idx_lcm_summary_nodes_codex_pending_session_order
            ON lcm_summary_nodes(
                session_id,
                (CASE
                    WHEN json_valid(metadata_json) THEN
                        json_extract(metadata_json, '$.source') = 'codex_context_compacted'
                        AND COALESCE(
                              json_extract(metadata_json, '$.tracedecay_summary_source'),
                              ''
                            ) <> 'codex_app_server'
                    ELSE 0
                 END),
                depth DESC,
                created_at DESC,
                node_id
            )
            WHERE provider = 'codex';
        CREATE INDEX idx_lcm_summary_nodes_codex_pending_root_order
            ON lcm_summary_nodes(
                (CASE
                    WHEN json_valid(metadata_json) THEN
                        json_extract(metadata_json, '$.source') = 'codex_context_compacted'
                        AND COALESCE(
                              json_extract(metadata_json, '$.tracedecay_summary_source'),
                              ''
                            ) <> 'codex_app_server'
                    ELSE 0
                 END),
                created_at DESC,
                depth DESC,
                node_id,
                session_id
            )
            WHERE provider = 'codex';
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
    // don't re-pay a full FTS rebuild; missing or malformed synchronization
    // objects are stale because they can silently desynchronize the index.
    if !raw_fts_structure_is_current(conn)
        .await
        .ok_or_else(|| LcmError::Db("raw FTS structure check failed".to_string()))?
    {
        rebuild_raw_fts(conn)
            .await
            .ok_or_else(|| LcmError::Db("raw FTS rebuild failed".to_string()))?;
    }

    // Schema v2: lifecycle rows gained the compression-boundary cooldown
    // marker. Probe first so the expected duplicate-column case is avoided,
    // while every real ALTER failure aborts before the v7 marker is written.
    let boundary_skip_at_exists = fetch_i64(
        conn,
        "SELECT COUNT(*) FROM pragma_table_xinfo('lcm_lifecycle_state')
         WHERE name = 'boundary_skip_at'",
        "boundary_skip_at column query returned no rows",
    )
    .await?
        > 0;
    if !boundary_skip_at_exists {
        conn.execute(
            "ALTER TABLE lcm_lifecycle_state ADD COLUMN boundary_skip_at INTEGER",
            (),
        )
        .await?;
    }

    carry_forward_legacy_messages_in_transaction(conn).await?;
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

pub async fn schema_version(conn: &(impl QueryExecutor + ?Sized)) -> Option<i64> {
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            params![MIGRATION_NAME],
        )
        .await
        .ok()?;
    rows.next().await.ok()??.get(0).ok()
}

pub async fn get_gc_meta(
    conn: &(impl QueryExecutor + ?Sized),
    key: &str,
) -> Result<Option<String>, LcmError> {
    let mut rows = conn
        .query("SELECT value FROM lcm_gc_meta WHERE key = ?1", params![key])
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

pub async fn set_gc_meta(
    conn: &(impl Executor + ?Sized),
    key: &str,
    value: &str,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT OR REPLACE INTO lcm_gc_meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .await?;
    Ok(())
}

pub async fn clear_gc_meta(
    conn: &(impl Executor + ?Sized),
    key: &str,
) -> Result<(), LcmError> {
    conn.execute("DELETE FROM lcm_gc_meta WHERE key = ?1", params![key])
        .await?;
    Ok(())
}

pub async fn load_raw_message(
    conn: &(impl QueryExecutor + ?Sized),
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

async fn carry_forward_legacy_messages_in_transaction(
    conn: &(impl Executor + ?Sized),
) -> Result<(), LcmError> {
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
        let content_hash = projected_content_hash(&content);
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
                timestamp,
                content.as_str(),
                content_hash.as_str(),
                LcmStorageKind::Inline.as_str(),
                snippet_text.as_str(),
                index_text.as_str(),
                i64::from(legacy_truncated),
                metadata_json.as_deref(),
            ],
        )
        .await?;
    }
    Ok(())
}

async fn fetch_i64(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    empty_message: &str,
) -> Result<i64, LcmError> {
    let mut rows = conn.query(sql, ()).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db(empty_message.to_string()))?;
    Ok(row.get(0)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::engine::TestConnection;

    #[tokio::test]
    async fn raw_fts_currency_requires_table_and_every_trigger_contract() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let conn = TestConnection::open(&temp.path().join("sessions.db"));
        conn.execute_batch(
            "CREATE TABLE lcm_raw_messages (
                store_id INTEGER PRIMARY KEY,
                index_text TEXT NOT NULL
            );",
        )
        .await
        .map_err(|error| error.to_string())?;
        rebuild_raw_fts(&*conn)
            .await
            .ok_or_else(|| "initial raw FTS rebuild failed".to_string())?;
        assert_eq!(raw_fts_structure_is_current(&*conn).await, Some(true));

        for trigger in [
            "lcm_raw_messages_fts_insert",
            "lcm_raw_messages_fts_delete",
            "lcm_raw_messages_fts_update",
        ] {
            conn.execute_batch(&format!("DROP TRIGGER {trigger}"))
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(
                raw_fts_structure_is_current(&*conn).await,
                Some(false),
                "missing {trigger} was accepted as current"
            );
            rebuild_raw_fts(&*conn)
                .await
                .ok_or_else(|| format!("raw FTS rebuild failed after dropping {trigger}"))?;
            assert_eq!(raw_fts_structure_is_current(&*conn).await, Some(true));
        }

        conn.execute_batch(
            "DROP TRIGGER lcm_raw_messages_fts_update;
             CREATE TRIGGER lcm_raw_messages_fts_update
                 AFTER UPDATE ON lcm_raw_messages BEGIN
                     SELECT 1;
                 END;",
        )
        .await
        .map_err(|error| error.to_string())?;
        assert_eq!(
            raw_fts_structure_is_current(&*conn).await,
            Some(false),
            "malformed update trigger was accepted as current"
        );

        conn.execute_batch("DROP TABLE lcm_raw_messages_fts")
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            raw_fts_structure_is_current(&*conn).await,
            Some(false),
            "missing raw FTS table was accepted as current"
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_boundary_skip_column_upgrade_does_not_publish_v7_marker() -> Result<(), String>
    {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let conn = TestConnection::open(&temp.path().join("sessions.db"));
        conn.execute_batch(
            "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
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
                metadata_json TEXT,
                PRIMARY KEY(provider, message_id)
            );
            CREATE TABLE session_schema_migrations (
                name TEXT PRIMARY KEY,
                version INTEGER NOT NULL,
                applied_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            INSERT INTO session_schema_migrations(name, version, applied_at)
            VALUES ('lcm', 6, 123);
            CREATE VIRTUAL TABLE lcm_lifecycle_state USING fts5(
                provider,
                conversation_id,
                current_session_id
            );",
        )
        .await
        .map_err(|error| error.to_string())?;

        let error = ensure_lcm_schema(&conn)
            .await
            .expect_err("unsupported lifecycle ALTER should fail the migration");
        assert!(matches!(error, LcmError::Db(_)));
        assert_eq!(
            util::fetch_i64(
                &*conn,
                "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
                (),
                "migration marker version",
            )
            .await
            .map_err(|error| error.to_string())?,
            6
        );
        assert_eq!(
            util::fetch_i64(
                &*conn,
                "SELECT COUNT(*) FROM pragma_table_xinfo('lcm_lifecycle_state')
                 WHERE name = 'boundary_skip_at'",
                (),
                "boundary column count",
            )
            .await
            .map_err(|error| error.to_string())?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn ensure_lcm_schema_errors_and_rolls_back_failed_legacy_carry_forward()
    -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let conn = TestConnection::open(&temp.path().join("sessions.db"));
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
            CREATE TABLE session_schema_migrations (
                name TEXT PRIMARY KEY,
                version INTEGER NOT NULL,
                applied_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            INSERT INTO session_schema_migrations(name, version, applied_at)
            VALUES ('lcm', 2, 123);
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

        let schema_before = sqlite_schema_fingerprint(&conn).await?;
        let Err(err) = ensure_lcm_schema(&conn).await else {
            return Err("failed carry-forward insert should propagate".to_string());
        };
        assert!(matches!(err, LcmError::Db(_)));
        assert_eq!(sqlite_schema_fingerprint(&conn).await?, schema_before);
        assert_eq!(
            util::fetch_i64(
                &*conn,
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
                &*conn,
                "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
                (),
                "migration marker version",
            )
            .await
            .map_err(|err| err.to_string())?,
            2
        );
        assert_eq!(
            util::fetch_i64(
                &*conn,
                "SELECT applied_at FROM session_schema_migrations WHERE name = 'lcm'",
                (),
                "migration marker applied_at",
            )
            .await
            .map_err(|err| err.to_string())?,
            123
        );
        assert_eq!(
            util::fetch_i64(
                &*conn,
                "SELECT COUNT(*) FROM session_messages",
                (),
                "legacy message count",
            )
            .await
            .map_err(|err| err.to_string())?,
            2
        );
        Ok(())
    }

    async fn sqlite_schema_fingerprint(
        conn: &Connection,
    ) -> Result<Vec<(String, String, String)>, String> {
        let mut rows = conn
            .query(
                "SELECT type, name, COALESCE(sql, '')
                 FROM sqlite_master
                 WHERE name = 'session_schema_migrations'
                    OR name LIKE 'lcm_%'
                 ORDER BY type, name",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut fingerprint = Vec::new();
        while let Some(row) = rows.next().await.map_err(|error| error.to_string())? {
            fingerprint.push((
                row.get(0).map_err(|error| error.to_string())?,
                row.get(1).map_err(|error| error.to_string())?,
                row.get(2).map_err(|error| error.to_string())?,
            ));
        }
        Ok(fingerprint)
    }
}

#[cfg(test)]
use crate::compatibility::projected_content_hash;
#[cfg(test)]
use tracedecay_runtime_core::db::engine::{Connection, TransactionBehavior};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::{LcmError, LcmRawMessage, raw};

#[cfg(test)]
use super::util;

pub const LCM_SCHEMA_VERSION: i64 = 8;

const MIGRATION_NAME: &str = "lcm";

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
pub async fn raw_fts_structure_is_current(conn: &(impl QueryExecutor + ?Sized)) -> Option<bool> {
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
    match stored_schema_version(conn).await? {
        Some(LCM_SCHEMA_VERSION) => return Ok(()),
        Some(found_version) => {
            return Err(LcmError::ProfileResetRequired {
                found_version: Some(found_version),
                required_version: LCM_SCHEMA_VERSION,
            });
        }
        None if lcm_schema_objects_exist(conn).await?
            || legacy_session_content_exists(conn).await? =>
        {
            return Err(LcmError::ProfileResetRequired {
                found_version: None,
                required_version: LCM_SCHEMA_VERSION,
            });
        }
        None => {}
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
    conn.execute_batch(RAW_FTS_DDL).await?;

    conn.execute(
        "INSERT INTO session_schema_migrations(name, version) VALUES (?1, ?2)",
        params![MIGRATION_NAME, LCM_SCHEMA_VERSION],
    )
    .await?;
    Ok(())
}

pub async fn schema_version(conn: &(impl QueryExecutor + ?Sized)) -> Option<i64> {
    stored_schema_version(conn).await.ok().flatten()
}

async fn stored_schema_version(
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<Option<i64>, LcmError> {
    if !schema_object_exists(conn, "session_schema_migrations").await? {
        return Ok(None);
    }
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            params![MIGRATION_NAME],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

async fn lcm_schema_objects_exist(conn: &(impl QueryExecutor + ?Sized)) -> Result<bool, LcmError> {
    let mut rows = conn
        .query(
            "SELECT EXISTS(
                SELECT 1
                FROM sqlite_master
                WHERE name LIKE 'lcm\\_%' ESCAPE '\\'
            )",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("LCM schema object probe returned no rows".to_owned()))?;
    Ok(row.get::<i64>(0)? != 0)
}

async fn legacy_session_content_exists(
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<bool, LcmError> {
    if !schema_object_exists(conn, "session_messages").await? {
        return Ok(false);
    }
    let mut rows = conn
        .query("SELECT EXISTS(SELECT 1 FROM session_messages)", ())
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("legacy session content probe returned no rows".to_owned()))?;
    Ok(row.get::<i64>(0)? != 0)
}

async fn schema_object_exists(
    conn: &(impl QueryExecutor + ?Sized),
    name: &str,
) -> Result<bool, LcmError> {
    let mut rows = conn
        .query(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1)",
            params![name],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("schema object probe returned no rows".to_owned()))?;
    Ok(row.get::<i64>(0)? != 0)
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

pub async fn clear_gc_meta(conn: &(impl Executor + ?Sized), key: &str) -> Result<(), LcmError> {
    conn.execute("DELETE FROM lcm_gc_meta WHERE key = ?1", params![key])
        .await?;
    Ok(())
}

pub async fn load_raw_message(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    message_id: &str,
) -> Result<Option<LcmRawMessage>, LcmError> {
    let sql = format!(
        "SELECT {}
         FROM lcm_raw_messages
         WHERE provider = ?1 AND message_id = ?2
         ORDER BY store_id
         LIMIT 2",
        raw::RAW_MESSAGE_SELECT_COLUMNS
    );
    let mut rows = conn.query(&sql, params![provider, message_id]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let message = raw::verified_raw_message_from_row(&row)?;
    if rows.next().await?.is_some() {
        return Err(LcmError::Db(
            "duplicate raw messages for provider/message identity".to_string(),
        ));
    }
    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_runtime_core::db::engine::TestConnection;
    use tracedecay_runtime_core::privacy::sanitize_lcm_payload_text;

    async fn lcm_reader_test_connection() -> Result<(tempfile::TempDir, TestConnection), String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let conn = TestConnection::open(&temp.path().join("sessions.db"));
        conn.execute_batch(
            "CREATE TABLE lcm_raw_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                store_id INTEGER PRIMARY KEY AUTOINCREMENT,
                role TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                timestamp INTEGER,
                content TEXT,
                content_hash TEXT NOT NULL,
                storage_kind TEXT NOT NULL,
                payload_ref TEXT,
                snippet_text TEXT NOT NULL,
                index_text TEXT NOT NULL,
                legacy_source INTEGER NOT NULL DEFAULT 0,
                legacy_truncated INTEGER NOT NULL DEFAULT 0,
                metadata_json TEXT,
                UNIQUE(provider, message_id)
            );",
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok((temp, conn))
    }

    async fn insert_reader_test_message(
        conn: &TestConnection,
        content: &str,
        storage_kind: &str,
        metadata_json: Option<&str>,
    ) -> Result<(), String> {
        let content_hash = projected_content_hash(content);
        conn.execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, content,
                content_hash, storage_kind, snippet_text, index_text, metadata_json
             ) VALUES (
                'cursor', 'message-1', 'session-1', 'user', 1, ?1,
                ?2, ?3, ?1, ?1, ?4
             )",
            params![content, content_hash, storage_kind, metadata_json],
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }

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
    async fn incompatible_profile_requires_reset_without_mutating_schema() -> Result<(), String> {
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

        let schema_before = sqlite_schema_fingerprint(&conn).await?;
        let error = ensure_lcm_schema(&conn)
            .await
            .expect_err("an incompatible profile must require an explicit reset");
        assert!(matches!(
            error,
            LcmError::ProfileResetRequired {
                found_version: Some(6),
                required_version: LCM_SCHEMA_VERSION,
            }
        ));
        assert_eq!(sqlite_schema_fingerprint(&conn).await?, schema_before);
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
    async fn fresh_schema_never_carries_forward_legacy_session_content() -> Result<(), String> {
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
            );",
        )
        .await
        .map_err(|err| err.to_string())?;

        ensure_lcm_schema(&conn)
            .await
            .map_err(|error| error.to_string())?;
        conn.execute_batch(
            "INSERT INTO sessions(provider, session_id, project_key, project_path)
             VALUES ('cursor', 'legacy-session', '/tmp/project', '/tmp/project');
             INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
             VALUES
               ('cursor', 'legacy-message-1', 'legacy-session', 'assistant', 1, 'legacy one'),
               ('cursor', 'legacy-message-2', 'legacy-session', 'assistant', 2, 'legacy two');",
        )
        .await
        .map_err(|error| error.to_string())?;
        ensure_lcm_schema(&conn)
            .await
            .map_err(|error| error.to_string())?;
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
            LCM_SCHEMA_VERSION
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

    #[tokio::test]
    async fn raw_message_load_propagates_poisoned_storage_kind() -> Result<(), String> {
        let (_temp, conn) = lcm_reader_test_connection().await?;
        insert_reader_test_message(&conn, "safe content", "poisoned", None).await?;

        let error = load_raw_message(&*conn, "cursor", "message-1")
            .await
            .expect_err("poisoned storage kind must not collapse to absence");

        assert!(matches!(error, LcmError::Db(message) if message.contains("invalid storage_kind")));
        Ok(())
    }

    #[tokio::test]
    async fn raw_message_load_rejects_mismatched_sanitization_receipt() -> Result<(), String> {
        let (_temp, conn) = lcm_reader_test_connection().await?;
        let sanitization = sanitize_lcm_payload_text("receipt-bound content")
            .map_err(|error| error.to_string())?;
        let metadata = serde_json::json!({
            "ingest_protection": {
                "sanitization_receipt": sanitization.receipt()
            }
        })
        .to_string();
        insert_reader_test_message(&conn, "tampered content", "inline", Some(&metadata)).await?;

        let error = load_raw_message(&*conn, "cursor", "message-1")
            .await
            .expect_err("receipt mismatch must not return a raw row");

        assert!(matches!(error, LcmError::Db(message) if message.contains("does not match")));
        Ok(())
    }

    #[tokio::test]
    async fn raw_message_load_propagates_database_failure() -> Result<(), String> {
        let (_temp, conn) = lcm_reader_test_connection().await?;
        conn.execute_batch("DROP TABLE lcm_raw_messages")
            .await
            .map_err(|error| error.to_string())?;

        let error = load_raw_message(&*conn, "cursor", "message-1")
            .await
            .expect_err("database failure must not collapse to absence");

        assert!(matches!(error, LcmError::Db(_)));
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

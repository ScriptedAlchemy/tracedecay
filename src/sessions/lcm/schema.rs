use libsql::{params, Connection, Value};

use super::{raw, LcmRawMessage, LcmStorageKind};

pub const LCM_SCHEMA_VERSION: i64 = 1;

const MIGRATION_NAME: &str = "lcm";
const LEGACY_TRUNCATION_MARKER: &str = "\n[truncated by tokensave]";

pub(crate) async fn ensure_lcm_schema(conn: &Connection) -> Option<()> {
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
        CREATE VIRTUAL TABLE IF NOT EXISTS lcm_raw_messages_fts USING fts5(
            index_text, role, metadata_json,
            content='lcm_raw_messages',
            content_rowid='store_id'
        );
        CREATE TRIGGER IF NOT EXISTS lcm_raw_messages_fts_insert
            AFTER INSERT ON lcm_raw_messages BEGIN
                INSERT INTO lcm_raw_messages_fts(rowid, index_text, role, metadata_json)
                VALUES (NEW.store_id, NEW.index_text, NEW.role, NEW.metadata_json);
            END;
        CREATE TRIGGER IF NOT EXISTS lcm_raw_messages_fts_delete
            AFTER DELETE ON lcm_raw_messages BEGIN
                INSERT INTO lcm_raw_messages_fts(
                    lcm_raw_messages_fts, rowid, index_text, role, metadata_json
                )
                VALUES ('delete', OLD.store_id, OLD.index_text, OLD.role, OLD.metadata_json);
            END;
        CREATE TRIGGER IF NOT EXISTS lcm_raw_messages_fts_update
            AFTER UPDATE ON lcm_raw_messages BEGIN
                INSERT INTO lcm_raw_messages_fts(
                    lcm_raw_messages_fts, rowid, index_text, role, metadata_json
                )
                VALUES ('delete', OLD.store_id, OLD.index_text, OLD.role, OLD.metadata_json);
                INSERT INTO lcm_raw_messages_fts(rowid, index_text, role, metadata_json)
                VALUES (NEW.store_id, NEW.index_text, NEW.role, NEW.metadata_json);
            END;",
    )
    .await
    .ok()?;

    carry_forward_legacy_messages(conn).await?;
    conn.execute(
        "INSERT INTO session_schema_migrations(name, version)
         VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET
            version = excluded.version,
            applied_at = unixepoch()",
        params![MIGRATION_NAME, LCM_SCHEMA_VERSION],
    )
    .await
    .ok()?;
    Some(())
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

async fn carry_forward_legacy_messages(conn: &Connection) -> Option<()> {
    let mut rows = conn
        .query(
            "SELECT provider, message_id, session_id, role, timestamp, ordinal,
                    text, metadata_json
             FROM session_messages
             ORDER BY provider, session_id, ordinal, message_id",
            (),
        )
        .await
        .ok()?;
    while let Some(row) = rows.next().await.ok()? {
        let provider: String = row.get(0).ok()?;
        let message_id: String = row.get(1).ok()?;
        let session_id: String = row.get(2).ok()?;
        let role: String = row.get(3).ok()?;
        let timestamp: Option<i64> = row.get(4).ok()?;
        let ordinal: i64 = row.get(5).ok()?;
        let content: String = row.get(6).ok()?;
        let metadata_json: Option<String> = row.get(7).ok()?;
        let legacy_truncated = content.contains(LEGACY_TRUNCATION_MARKER);
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
                opt_i64(timestamp),
                content.as_str(),
                content_hash.as_str(),
                LcmStorageKind::Inline.as_str(),
                snippet_text.as_str(),
                index_text.as_str(),
                if legacy_truncated { 1_i64 } else { 0_i64 },
                opt_text(metadata_json.as_deref()),
            ],
        )
        .await
        .ok()?;
    }
    Some(())
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.to_string()))
}

fn opt_i64(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}

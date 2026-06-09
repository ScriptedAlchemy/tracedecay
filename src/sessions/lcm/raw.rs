use libsql::{params, Connection, Value};
use sha2::{Digest, Sha256};

use crate::sessions::SessionMessageRecord;

use super::{LcmStorageKind, DERIVED_TRUNCATION_MARKER, MAX_DERIVED_TEXT_CHARS};

pub fn derived_text_for_index(raw: &str) -> String {
    if raw.chars().count() <= MAX_DERIVED_TEXT_CHARS {
        return raw.to_string();
    }

    let marker_chars = DERIVED_TRUNCATION_MARKER.chars().count();
    let budget = MAX_DERIVED_TEXT_CHARS.saturating_sub(marker_chars);
    let mut derived = raw.chars().take(budget).collect::<String>();
    derived.push_str(DERIVED_TRUNCATION_MARKER);
    derived
}

pub(crate) async fn upsert_raw_message(conn: &Connection, message: &SessionMessageRecord) -> bool {
    let derived = derived_text_for_index(&message.text);
    let content_hash = sha256_hex(&message.text);
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp,
            content, content_hash, storage_kind, payload_ref, snippet_text,
            index_text, legacy_source, legacy_truncated, metadata_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?10, 0, 0, ?11)
         ON CONFLICT(provider, message_id) DO UPDATE SET
            session_id = excluded.session_id,
            role = excluded.role,
            ordinal = excluded.ordinal,
            timestamp = excluded.timestamp,
            content = excluded.content,
            content_hash = excluded.content_hash,
            storage_kind = excluded.storage_kind,
            payload_ref = excluded.payload_ref,
            snippet_text = excluded.snippet_text,
            index_text = excluded.index_text,
            legacy_source = 0,
            legacy_truncated = 0,
            metadata_json = excluded.metadata_json",
        params![
            message.provider.as_str(),
            message.message_id.as_str(),
            message.session_id.as_str(),
            message.role.as_str(),
            message.ordinal,
            opt_i64(message.timestamp),
            message.text.as_str(),
            content_hash.as_str(),
            LcmStorageKind::Inline.as_str(),
            derived.as_str(),
            opt_text(message.metadata_json.as_deref()),
        ],
    )
    .await
    .is_ok()
}

pub(crate) fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.to_string()))
}

fn opt_i64(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}

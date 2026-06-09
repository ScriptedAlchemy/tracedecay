use std::path::Path;

use libsql::{params, Connection, Value};
use sha2::{Digest, Sha256};

use crate::sessions::SessionMessageRecord;

use super::{
    payload, security, LcmError, LcmPayloadRef, LcmStorageKind, DERIVED_TRUNCATION_MARKER,
    MAX_DERIVED_SNIPPET_CHARS, MAX_DERIVED_TEXT_CHARS,
};

pub(crate) struct RawMessageUpsert {
    pub projection_text: String,
    pub projection_metadata_json: Option<String>,
}

pub fn derived_text_for_index(raw: &str) -> String {
    derived_text_with_cap(raw, MAX_DERIVED_TEXT_CHARS)
}

pub(crate) fn derived_text_for_snippet(raw: &str) -> String {
    derived_text_with_cap(raw, MAX_DERIVED_SNIPPET_CHARS)
}

fn externalized_payload_placeholder(payload_ref: &super::LcmPayloadRef) -> String {
    let hash_prefix = payload_ref
        .content_hash
        .get(..12)
        .unwrap_or(payload_ref.content_hash.as_str());
    format!(
        "[externalized payload: {}, ref={}, bytes={}, sha256={}]",
        payload_ref.kind, payload_ref.payload_ref, payload_ref.byte_count, hash_prefix
    )
}

fn derived_text_with_cap(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() <= max_chars {
        return raw.to_string();
    }

    let marker_chars = DERIVED_TRUNCATION_MARKER.chars().count();
    let budget = max_chars.saturating_sub(marker_chars);
    let mut derived = raw.chars().take(budget).collect::<String>();
    derived.push_str(DERIVED_TRUNCATION_MARKER);
    derived
}

pub(crate) async fn upsert_raw_message(conn: &Connection, message: &SessionMessageRecord) -> bool {
    let snippet = derived_text_for_snippet(&message.text);
    let index = derived_text_for_index(&message.text);
    let content_hash = sha256_hex(&message.text);
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp,
            content, content_hash, storage_kind, payload_ref, snippet_text,
            index_text, legacy_source, legacy_truncated, metadata_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11, 0, 0, ?12)
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
            snippet.as_str(),
            index.as_str(),
            opt_text(message.metadata_json.as_deref()),
        ],
    )
    .await
    .is_ok()
}

fn externalized_payload_metadata(payload_ref: &LcmPayloadRef) -> String {
    serde_json::json!({
        "external_payload": true,
        "payload_ref": payload_ref.payload_ref,
        "kind": payload_ref.kind,
        "byte_count": payload_ref.byte_count,
        "char_count": payload_ref.char_count,
        "sha256": payload_ref.content_hash,
    })
    .to_string()
}

pub(crate) async fn upsert_raw_message_with_payload(
    conn: &Connection,
    storage_root: &Path,
    message: &SessionMessageRecord,
) -> Result<RawMessageUpsert, LcmError> {
    if !security::should_externalize(&message.role, message.kind.as_deref(), &message.text) {
        let projection_text = derived_text_for_index(&message.text);
        return if upsert_raw_message(conn, message).await {
            Ok(RawMessageUpsert {
                projection_text,
                projection_metadata_json: message.metadata_json.clone(),
            })
        } else {
            Err(LcmError::Db(
                "failed to upsert inline raw message".to_string(),
            ))
        };
    }

    let kind = message.kind.as_deref().unwrap_or("message");
    let payload_ref = payload::write_external_payload(
        storage_root,
        &message.provider,
        &message.session_id,
        &message.message_id,
        kind,
        &message.text,
        message.metadata_json.clone(),
    )?;
    payload::upsert_payload_metadata(conn, &payload_ref).await?;

    let placeholder = externalized_payload_placeholder(&payload_ref);
    let metadata_json = externalized_payload_metadata(&payload_ref);
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp,
            content, content_hash, storage_kind, payload_ref, snippet_text,
            index_text, legacy_source, legacy_truncated, metadata_json
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11, 0, 0, ?12)
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
            payload_ref.content_hash.as_str(),
            LcmStorageKind::External.as_str(),
            payload_ref.payload_ref.as_str(),
            placeholder.as_str(),
            placeholder.as_str(),
            metadata_json.as_str(),
        ],
    )
    .await
    .map_err(|err| LcmError::Db(err.to_string()))?;
    Ok(RawMessageUpsert {
        projection_text: placeholder,
        projection_metadata_json: Some(metadata_json),
    })
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

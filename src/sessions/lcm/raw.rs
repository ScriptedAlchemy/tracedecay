use std::path::Path;

use libsql::{params, Connection, Value};
use serde_json::{json, Map, Value as JsonValue};
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

#[derive(Default)]
struct IngestProtection {
    nested_external_payloads: usize,
    redacted: bool,
    lossy: bool,
    redaction_patterns: Vec<String>,
    quarantine_reason: Option<String>,
    quarantine_kind: Option<String>,
}

struct PreparedMessage {
    text: String,
    metadata_json: Option<String>,
    external_kind: Option<String>,
    protection: IngestProtection,
}

struct IngestConfig {
    sensitive_patterns_enabled: bool,
    sensitive_patterns: Vec<String>,
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

async fn upsert_inline_raw_message(
    conn: &Connection,
    message: &SessionMessageRecord,
    text: &str,
    metadata_json: Option<&str>,
) -> bool {
    let snippet = derived_text_for_snippet(text);
    let index = derived_text_for_index(text);
    let content_hash = sha256_hex(text);
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
            text,
            content_hash.as_str(),
            LcmStorageKind::Inline.as_str(),
            snippet.as_str(),
            index.as_str(),
            opt_text(metadata_json),
        ],
    )
    .await
    .is_ok()
}

fn externalized_payload_metadata(
    payload_ref: &LcmPayloadRef,
    protection: &IngestProtection,
) -> String {
    let mut metadata = json!({
        "external_payload": true,
        "payload_ref": payload_ref.payload_ref,
        "kind": payload_ref.kind,
        "byte_count": payload_ref.byte_count,
        "char_count": payload_ref.char_count,
        "sha256": payload_ref.content_hash,
    });
    add_ingest_protection_metadata(&mut metadata, protection);
    metadata.to_string()
}

pub(crate) async fn upsert_raw_message_with_payload(
    conn: &Connection,
    storage_root: &Path,
    message: &SessionMessageRecord,
) -> Result<RawMessageUpsert, LcmError> {
    let prepared = prepare_message(conn, storage_root, message).await?;
    if !security::should_externalize(&message.role, message.kind.as_deref(), &prepared.text) {
        let projection_text = derived_text_for_index(&prepared.text);
        return if upsert_inline_raw_message(
            conn,
            message,
            &prepared.text,
            prepared.metadata_json.as_deref(),
        )
        .await
        {
            Ok(RawMessageUpsert {
                projection_text,
                projection_metadata_json: prepared.metadata_json,
            })
        } else {
            Err(LcmError::Db(
                "failed to upsert inline raw message".to_string(),
            ))
        };
    }

    let kind = prepared
        .external_kind
        .as_deref()
        .or(message.kind.as_deref())
        .unwrap_or("message");
    let payload_ref = payload::write_external_payload(
        storage_root,
        &message.provider,
        &message.session_id,
        &message.message_id,
        kind,
        &prepared.text,
        payload_metadata_json(&prepared.protection),
    )?;
    payload::upsert_payload_metadata(conn, &payload_ref).await?;

    let placeholder = externalized_payload_placeholder(&payload_ref);
    let metadata_json = externalized_payload_metadata(&payload_ref, &prepared.protection);
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

async fn prepare_message(
    conn: &Connection,
    storage_root: &Path,
    message: &SessionMessageRecord,
) -> Result<PreparedMessage, LcmError> {
    let config = ingest_config(message.metadata_json.as_deref());
    let mut protection = IngestProtection::default();
    let redacted = redact_sensitive_text(&message.text, &config);
    let mut text = redacted.text;
    if redacted.redacted {
        protection.redacted = true;
        protection.lossy = true;
        protection.redaction_patterns = redacted.patterns;
    }

    if let Ok(mut value) = serde_json::from_str::<JsonValue>(&text) {
        if matches!(value, JsonValue::Object(_) | JsonValue::Array(_)) {
            let mut nested_payloads = Vec::new();
            protect_json_media_payloads(
                &mut value,
                storage_root,
                message,
                "content",
                &mut nested_payloads,
            )?;
            if !nested_payloads.is_empty() {
                for payload_ref in &nested_payloads {
                    payload::upsert_payload_metadata(conn, payload_ref).await?;
                }
                protection.nested_external_payloads = nested_payloads.len();
                text = serde_json::to_string(&value)
                    .map_err(|err| LcmError::Db(format!("json protection failed: {err}")))?;
            }
        }
    }

    if let Some(reason) = security::quarantine_reason(&message.role, message.kind.as_deref(), &text)
    {
        protection.quarantine_reason = Some(reason.to_string());
        protection.quarantine_kind = Some("quarantined_assistant_output".to_string());
    }

    let external_kind = protection.quarantine_kind.clone();
    let metadata_json = protected_metadata_json(message.metadata_json.as_deref(), &protection);
    Ok(PreparedMessage {
        text,
        metadata_json,
        external_kind,
        protection,
    })
}

fn protect_json_media_payloads(
    value: &mut JsonValue,
    storage_root: &Path,
    message: &SessionMessageRecord,
    field_path: &str,
    payloads: &mut Vec<LcmPayloadRef>,
) -> Result<(), LcmError> {
    match value {
        JsonValue::Object(map) => {
            for (key, child) in map.iter_mut() {
                let child_path = format!("{field_path}.{key}");
                protect_json_media_payloads(child, storage_root, message, &child_path, payloads)?;
            }
        }
        JsonValue::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                let child_path = format!("{field_path}[{index}]");
                protect_json_media_payloads(child, storage_root, message, &child_path, payloads)?;
            }
        }
        JsonValue::String(text) if security::contains_media_payload(text) => {
            let metadata_json = Some(
                json!({
                    "ingest_payload": true,
                    "field_path": field_path,
                    "lossless": true,
                })
                .to_string(),
            );
            let payload_ref = payload::write_external_payload(
                storage_root,
                &message.provider,
                &message.session_id,
                &message.message_id,
                "ingest_payload",
                text,
                metadata_json,
            )?;
            let placeholder = ingest_payload_placeholder(&payload_ref, &message.role, field_path);
            *text = placeholder;
            payloads.push(payload_ref);
        }
        _ => {}
    }
    Ok(())
}

fn ingest_payload_placeholder(payload_ref: &LcmPayloadRef, role: &str, field_path: &str) -> String {
    format!(
        "[Externalized LCM ingest payload: kind={}; role={}; field={}; chars={}; bytes={}; ref={}]",
        safe_placeholder_metadata(&payload_ref.kind),
        safe_placeholder_metadata(role),
        safe_placeholder_metadata(field_path),
        payload_ref.char_count,
        payload_ref.byte_count,
        payload_ref.payload_ref
    )
}

fn safe_placeholder_metadata(value: &str) -> String {
    let safe = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | ':' | '/' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(120)
        .collect::<String>();
    if safe.is_empty() {
        "?".to_string()
    } else {
        safe
    }
}

struct RedactionOutcome {
    text: String,
    redacted: bool,
    patterns: Vec<String>,
}

fn ingest_config(metadata_json: Option<&str>) -> IngestConfig {
    let mut config = IngestConfig {
        sensitive_patterns_enabled: false,
        sensitive_patterns: vec![
            "api_key".to_string(),
            "bearer_token".to_string(),
            "password_assignment".to_string(),
            "private_key".to_string(),
        ],
    };
    let Some(metadata_json) = metadata_json else {
        return config;
    };
    let Ok(value) = serde_json::from_str::<JsonValue>(metadata_json) else {
        return config;
    };
    let ingest = value
        .get("lcm_ingest")
        .or_else(|| value.get("ingest_protection"))
        .unwrap_or(&value);
    config.sensitive_patterns_enabled = ingest
        .get("sensitive_patterns_enabled")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    if let Some(patterns) = ingest
        .get("sensitive_patterns")
        .and_then(JsonValue::as_array)
    {
        config.sensitive_patterns = patterns
            .iter()
            .filter_map(JsonValue::as_str)
            .map(|value| value.to_ascii_lowercase())
            .collect();
    }
    config
}

fn redact_sensitive_text(text: &str, config: &IngestConfig) -> RedactionOutcome {
    if !config.sensitive_patterns_enabled {
        return RedactionOutcome {
            text: text.to_string(),
            redacted: false,
            patterns: Vec::new(),
        };
    }
    let mut protected = text.to_string();
    let mut patterns = Vec::new();
    if config
        .sensitive_patterns
        .iter()
        .any(|pattern| pattern == "api_key" || pattern == "all" || pattern == "default")
    {
        let next = redact_assignments(
            &protected,
            &[
                "api_key",
                "api-key",
                "api token",
                "api_token",
                "access_token",
                "access-token",
                "secret_key",
                "secret-key",
                "client_secret",
                "client-secret",
            ],
            "api_key",
            12,
        );
        if next != protected {
            protected = next;
            patterns.push("api_key".to_string());
        }
    }
    if config
        .sensitive_patterns
        .iter()
        .any(|pattern| pattern == "bearer_token" || pattern == "all" || pattern == "default")
    {
        let next = redact_bearer_tokens(&protected);
        if next != protected {
            protected = next;
            patterns.push("bearer_token".to_string());
        }
    }
    if config
        .sensitive_patterns
        .iter()
        .any(|pattern| pattern == "password_assignment" || pattern == "all" || pattern == "default")
    {
        let next = redact_assignments(
            &protected,
            &["password", "passwd", "pwd", "passphrase"],
            "password_assignment",
            6,
        );
        if next != protected {
            protected = next;
            patterns.push("password_assignment".to_string());
        }
    }
    patterns.sort();
    patterns.dedup();
    RedactionOutcome {
        redacted: protected != text,
        text: protected,
        patterns,
    }
}

fn redact_assignments(
    text: &str,
    keys: &[&str],
    pattern_name: &str,
    min_secret_len: usize,
) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::new();
    let mut cursor = 0usize;
    while cursor < text.len() {
        let Some((key_start, key_len)) = find_next_key(&lower, cursor, keys) else {
            out.push_str(&text[cursor..]);
            break;
        };
        let mut pos = key_start + key_len;
        pos = skip_chars(text, pos, |ch| {
            ch.is_whitespace() || matches!(ch, '"' | '\'')
        });
        if !text[pos..]
            .chars()
            .next()
            .map(|ch| matches!(ch, '=' | ':'))
            .unwrap_or(false)
        {
            out.push_str(&text[cursor..pos.min(text.len())]);
            cursor = pos.min(text.len());
            continue;
        }
        pos += 1;
        pos = skip_chars(text, pos, |ch| {
            ch.is_whitespace() || matches!(ch, '"' | '\'')
        });
        let secret_start = pos;
        pos = skip_chars(text, pos, |ch| {
            !ch.is_whitespace() && !matches!(ch, ',' | '"' | '\'' | ']' | '}')
        });
        let secret = &text[secret_start..pos];
        if secret.chars().count() < min_secret_len
            || secret.starts_with("[LCM sensitive redaction:")
        {
            out.push_str(&text[cursor..pos]);
            cursor = pos;
            continue;
        }
        out.push_str(&text[cursor..secret_start]);
        out.push_str(&sensitive_placeholder(pattern_name, secret));
        cursor = pos;
    }
    out
}

fn redact_bearer_tokens(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::new();
    let mut cursor = 0usize;
    while let Some(relative) = lower[cursor..].find("bearer ") {
        let start = cursor + relative;
        let secret_start = start + "bearer ".len();
        let secret_end = skip_chars(text, secret_start, |ch| {
            !ch.is_whitespace() && !matches!(ch, ',' | '"' | '\'' | ']' | '}')
        });
        let secret = &text[secret_start..secret_end];
        if secret.chars().count() < 12 {
            out.push_str(&text[cursor..secret_end]);
        } else {
            out.push_str(&text[cursor..secret_start]);
            out.push_str(&sensitive_placeholder("bearer_token", secret));
        }
        cursor = secret_end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn find_next_key(lower: &str, cursor: usize, keys: &[&str]) -> Option<(usize, usize)> {
    keys.iter()
        .filter_map(|key| {
            lower[cursor..]
                .find(key)
                .map(|idx| (cursor + idx, key.len()))
        })
        .min_by_key(|(idx, _)| *idx)
}

fn skip_chars(text: &str, mut pos: usize, predicate: impl Fn(char) -> bool) -> usize {
    while pos < text.len() {
        let Some(ch) = text[pos..].chars().next() else {
            break;
        };
        if !predicate(ch) {
            break;
        }
        pos += ch.len_utf8();
    }
    pos
}

fn sensitive_placeholder(pattern_name: &str, secret: &str) -> String {
    let mut parts = vec![format!(
        "[LCM sensitive redaction: name={}; chars={}; bytes={}",
        safe_placeholder_metadata(pattern_name),
        secret.chars().count(),
        secret.len()
    )];
    if pattern_name != "password_assignment" {
        let digest = sha256_hex(secret);
        parts.push(format!("sha256={}", &digest[..16]));
    }
    format!("{}]", parts.join("; "))
}

fn protected_metadata_json(
    original: Option<&str>,
    protection: &IngestProtection,
) -> Option<String> {
    if !has_ingest_protection_metadata(protection) {
        return original.map(str::to_string);
    }
    let mut metadata = original
        .and_then(|text| serde_json::from_str::<JsonValue>(text).ok())
        .filter(JsonValue::is_object)
        .unwrap_or_else(|| JsonValue::Object(Map::new()));
    add_ingest_protection_metadata(&mut metadata, protection);
    Some(metadata.to_string())
}

fn payload_metadata_json(protection: &IngestProtection) -> Option<String> {
    if !has_ingest_protection_metadata(protection) {
        return None;
    }
    let mut metadata = JsonValue::Object(Map::new());
    add_ingest_protection_metadata(&mut metadata, protection);
    Some(metadata.to_string())
}

fn has_ingest_protection_metadata(protection: &IngestProtection) -> bool {
    protection.nested_external_payloads > 0
        || protection.redacted
        || protection.lossy
        || protection.quarantine_reason.is_some()
}

fn add_ingest_protection_metadata(metadata: &mut JsonValue, protection: &IngestProtection) {
    if !has_ingest_protection_metadata(protection) {
        return;
    }
    let mut ingest = Map::new();
    if protection.nested_external_payloads > 0 {
        ingest.insert(
            "nested_external_payloads".to_string(),
            json!(protection.nested_external_payloads),
        );
    }
    if protection.redacted {
        ingest.insert("redacted".to_string(), json!(true));
        ingest.insert(
            "redaction_patterns".to_string(),
            json!(protection.redaction_patterns),
        );
    }
    if protection.lossy {
        ingest.insert("lossy".to_string(), json!(true));
    }
    if let Some(reason) = protection.quarantine_reason.as_deref() {
        ingest.insert("reason".to_string(), json!(reason));
    }
    if let Some(kind) = protection.quarantine_kind.as_deref() {
        ingest.insert("kind".to_string(), json!(kind));
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert("ingest_protection".to_string(), JsonValue::Object(ingest));
    }
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

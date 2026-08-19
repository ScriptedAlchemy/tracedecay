use std::path::Path;

use serde_json::{Map, Value as JsonValue, json};
use tracedecay_domain::{ComponentVersion, SanitizationReceiptV1, SanitizerDispositionV1};

pub use crate::retrieval_content::derived_text_for_index;
pub use crate::retrieval_content::derived_text_for_snippet;
use crate::retrieval_content::projected_content_hash;
use crate::runtime::SessionMessageRecord;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_runtime_core::privacy::{
    LCM_PAYLOAD_SANITIZER_VERSION_V1, LcmPayloadSanitizationV1, PrivacyDetectorV1,
    bind_sanitized_lcm_payload_text, quarantine_lcm_payload_text, sanitize_lcm_payload_text,
    sanitize_provider_metadata_json, verify_sanitized_json_payload,
};

use super::{
    LcmError, LcmPayloadRef, LcmRawMessage, LcmRawMessageMetadata, LcmStorageKind, payload,
    security,
};

pub const RAW_MESSAGE_SELECT_COLUMNS: &str =
    "provider, message_id, session_id, store_id, role, ordinal,
                    timestamp, content, content_hash, storage_kind, payload_ref,
                    snippet_text, legacy_source, legacy_truncated, metadata_json";
pub const RAW_MESSAGE_METADATA_SELECT_COLUMNS: &str =
    "provider, message_id, session_id, store_id, role, ordinal,
                    timestamp, NULL AS content, content_hash, storage_kind, payload_ref,
                    '' AS snippet_text, legacy_source, legacy_truncated, metadata_json";

pub fn raw_message_metadata_from_row(row: &Row) -> Result<LcmRawMessageMetadata, LcmError> {
    let storage_kind_text: String = row.get(9)?;
    let content_hash: String = row.get(8)?;
    let storage_kind = LcmStorageKind::from_db(&storage_kind_text)
        .ok_or_else(|| LcmError::Db(format!("invalid storage_kind: {storage_kind_text}")))?;
    Ok(LcmRawMessageMetadata {
        provider: row.get(0)?,
        message_id: row.get(1)?,
        session_id: row.get(2)?,
        store_id: row.get(3)?,
        role: row.get(4)?,
        ordinal: row.get(5)?,
        timestamp: row.get(6)?,
        content_hash,
        storage_kind,
        payload_ref: row.get(10)?,
        legacy_source: row.get::<i64>(12)? != 0,
        legacy_truncated: row.get::<i64>(13)? != 0,
        metadata_json: row.get(14)?,
    })
}

fn verify_raw_message_receipt(message: &LcmRawMessage) -> Result<(), LcmError> {
    let metadata = message
        .metadata_json
        .as_deref()
        .ok_or_else(|| LcmError::Db("missing LCM sanitization receipt".to_owned()))?;
    let metadata = serde_json::from_str::<JsonValue>(metadata)
        .map_err(|_| LcmError::Db("invalid LCM sanitization metadata".to_owned()))?;
    let receipt = metadata
        .get("ingest_protection")
        .and_then(|protection| protection.get("sanitization_receipt"))
        .ok_or_else(|| LcmError::Db("missing LCM sanitization receipt".to_owned()))?;
    let receipt: SanitizationReceiptV1 = serde_json::from_value(receipt.clone())
        .map_err(|_| LcmError::Db("invalid LCM sanitization receipt".to_owned()))?;
    let expected_revision = ComponentVersion::new(LCM_PAYLOAD_SANITIZER_VERSION_V1)
        .map_err(|_| LcmError::Db("invalid canonical LCM sanitizer revision".to_owned()))?;
    if message.storage_kind == LcmStorageKind::Inline {
        verify_sanitized_json_payload(
            &JsonValue::String(message.content.clone()),
            &receipt,
            &expected_revision,
        )
        .map_err(|error| LcmError::Db(format!("invalid LCM sanitization receipt: {error}")))?;
    } else {
        let quarantined = metadata
            .get("ingest_protection")
            .and_then(|protection| protection.get("kind"))
            .and_then(JsonValue::as_str)
            == Some("quarantined_assistant_output");
        let disposition_is_valid = matches!(
            receipt.disposition(),
            SanitizerDispositionV1::Accepted | SanitizerDispositionV1::Redacted
        ) || (quarantined
            && receipt.disposition() == SanitizerDispositionV1::Quarantined
            && receipt.payload().is_none());
        if receipt.receipt().sanitizer_version() != &expected_revision || !disposition_is_valid {
            return Err(LcmError::Db(
                "invalid LCM sanitization receipt for external payload".to_owned(),
            ));
        }
    }
    Ok(())
}

pub fn verified_raw_message_from_row(row: &Row) -> Result<LcmRawMessage, LcmError> {
    let inline_content: Option<String> = row.get(7)?;
    let snippet_text: String = row.get(11)?;
    let metadata = raw_message_metadata_from_row(row)?;
    let message = match metadata.storage_kind {
        LcmStorageKind::Inline => {
            let inline_content = inline_content.ok_or(LcmError::PayloadIntegrityMismatch)?;
            metadata.with_verified_content(inline_content)?
        }
        // External rows hash the owning payload, while the raw-message
        // projection is the non-secret inline placeholder.
        LcmStorageKind::External => metadata.with_external_placeholder(snippet_text)?,
    };
    verify_raw_message_receipt(&message)?;
    Ok(message)
}

pub async fn load_raw_message_by_identity(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
    message_id: &str,
) -> Result<Option<LcmRawMessage>, LcmError> {
    let sql = format!(
        "SELECT {RAW_MESSAGE_SELECT_COLUMNS}
         FROM lcm_raw_messages
         WHERE provider = ?1 AND session_id = ?2 AND message_id = ?3
         ORDER BY store_id
         LIMIT 2"
    );
    let mut rows = conn
        .query(&sql, params![provider, session_id, message_id])
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let message = verified_raw_message_from_row(&row)?;
    if rows.next().await?.is_some() {
        return Err(LcmError::Db(
            "duplicate raw messages for exact provider/session/message identity".to_string(),
        ));
    }
    Ok(Some(message))
}

pub async fn load_raw_message_by_store_id(
    conn: &(impl QueryExecutor + ?Sized),
    store_id: i64,
) -> Result<LcmRawMessage, LcmError> {
    let sql = format!(
        "SELECT {RAW_MESSAGE_SELECT_COLUMNS}
         FROM lcm_raw_messages
         WHERE store_id = ?1"
    );
    let mut rows = conn.query(&sql, params![store_id]).await?;
    let row = rows
        .next()
        .await?
        .ok_or(LcmError::SummarySourceNotOwnedBySession)?;
    verified_raw_message_from_row(&row)
}

pub struct RawMessageUpsert {
    pub projection_text: String,
    pub projection_metadata_json: Option<String>,
}

struct PreparedMessage {
    text: String,
    metadata_json: Option<String>,
    external_kind: Option<String>,
    sanitization: LcmPayloadSanitizationV1,
    quarantine_receipt: Option<SanitizationReceiptV1>,
    nested_external_payloads: usize,
    quarantine_reason: Option<String>,
    quarantine_kind: Option<String>,
    pending_payload_refs: Vec<LcmPayloadRef>,
}

/// File-side ingest work that must not sit on a SQLite write lease.
///
/// Privacy sanitization and payload externalization are CPU- and IO-bound.
/// Holding an ordinary Immediate transaction across them lets the idle lease
/// expire under load before the first SQL command runs.
pub struct StagedRawMessageIngest {
    prepared: PreparedMessage,
    whole_message: Option<StagedWholeMessage>,
}

struct StagedWholeMessage {
    payload_ref: LcmPayloadRef,
    placeholder: String,
    metadata_json: String,
}

impl PreparedMessage {
    fn receipt(&self) -> &SanitizationReceiptV1 {
        self.quarantine_receipt
            .as_ref()
            .unwrap_or_else(|| self.sanitization.receipt())
    }
}

struct PayloadExternalizer<'a> {
    storage_root: &'a Path,
    rollback: &'a mut payload::PayloadFileRollback,
}

impl PayloadExternalizer<'_> {
    fn write(
        &mut self,
        message: &SessionMessageRecord,
        kind: &str,
        content: &str,
        metadata_json: Option<String>,
    ) -> Result<LcmPayloadRef, LcmError> {
        payload::write_external_payload_tracked(
            self.storage_root,
            payload::ExternalPayloadWrite {
                provider: &message.provider,
                session_id: &message.session_id,
                message_id: &message.message_id,
                kind,
                content,
                metadata_json,
            },
            self.rollback,
        )
    }
}

fn externalized_payload_placeholder(
    payload_ref: &super::LcmPayloadRef,
    field_path: &str,
    quarantine_reason: Option<&str>,
) -> String {
    if let Some(reason) = quarantine_reason {
        return format!(
            "[Externalized LCM ingest payload: assistant output quarantined; kind={}; reason={}; field={}; chars={}; bytes={}; ref={}]",
            safe_placeholder_metadata(&payload_ref.kind),
            safe_placeholder_metadata(reason),
            safe_placeholder_metadata(field_path),
            payload_ref.char_count,
            payload_ref.byte_count,
            payload_ref.payload_ref
        );
    }
    format!(
        "[Externalized LCM ingest payload: kind={}; field={}; chars={}; bytes={}; ref={}]",
        safe_placeholder_metadata(&payload_ref.kind),
        safe_placeholder_metadata(field_path),
        payload_ref.char_count,
        payload_ref.byte_count,
        payload_ref.payload_ref
    )
}

async fn upsert_inline_raw_message(
    conn: &(impl Executor + ?Sized),
    message: &SessionMessageRecord,
    text: &str,
    metadata_json: Option<&str>,
) -> Result<(), LcmError> {
    let snippet = derived_text_for_snippet(text);
    let index = derived_text_for_index(text);
    let content_hash = projected_content_hash(text);
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
            message.timestamp,
            text,
            content_hash.as_str(),
            LcmStorageKind::Inline.as_str(),
            snippet.as_str(),
            index.as_str(),
            metadata_json,
        ],
    )
    .await?;
    Ok(())
}

fn externalized_payload_metadata(
    payload_ref: &LcmPayloadRef,
    prepared: &PreparedMessage,
) -> Result<String, LcmError> {
    let mut metadata = json!({
        "external_payload": true,
        "payload_ref": payload_ref.payload_ref,
        "kind": payload_ref.kind,
        "byte_count": payload_ref.byte_count,
        "char_count": payload_ref.char_count,
        "sha256": payload_ref.content_hash,
    });
    add_sanitization_metadata(&mut metadata, prepared, None)?;
    Ok(metadata.to_string())
}

/// Raw-authority write for one projection-derived message row.
///
/// Observation capture already privacy-sanitized and size-bounded the payload
/// before it became durable, so the projected row stays inline; this binds a
/// fresh content receipt to the stored text so the canonical raw-read
/// authority ([`load_raw_message`]) can hydrate observation-projected
/// messages instead of refusing them as receipt-less rows.
pub async fn upsert_projection_raw_message(
    conn: &(impl Executor + ?Sized),
    message: &SessionMessageRecord,
) -> Result<(), LcmError> {
    // `sanitize_lcm_payload_text` is a pure function of the text: a failure
    // here is a deterministic content refusal, not a storage fault, and must
    // never be retried as one.
    let sanitization = sanitize_lcm_payload_text(&message.text).map_err(|error| {
        LcmError::SanitizationRefused {
            reason: format!("LCM privacy sanitization failed: {error}"),
        }
    })?;
    let mut prepared = PreparedMessage {
        text: sanitization.sanitized_text().to_owned(),
        metadata_json: None,
        external_kind: None,
        sanitization,
        quarantine_receipt: None,
        nested_external_payloads: 0,
        quarantine_reason: None,
        quarantine_kind: None,
        pending_payload_refs: Vec::new(),
    };
    prepared.metadata_json = protected_metadata_json(message.metadata_json.as_deref(), &prepared)?;
    upsert_inline_raw_message(
        conn,
        message,
        &prepared.text,
        prepared.metadata_json.as_deref(),
    )
    .await
}

pub fn stage_raw_message_with_payload_tracked(
    storage_root: &Path,
    message: &SessionMessageRecord,
    rollback: &mut payload::PayloadFileRollback,
) -> Result<StagedRawMessageIngest, LcmError> {
    let mut externalizer = PayloadExternalizer {
        storage_root,
        rollback,
    };
    let prepared = prepare_message(message, &mut externalizer)?;
    if !security::should_externalize(&message.role, message.kind.as_deref(), &prepared.text) {
        return Ok(StagedRawMessageIngest {
            prepared,
            whole_message: None,
        });
    }

    let kind = prepared
        .external_kind
        .as_deref()
        .or(message.kind.as_deref())
        .unwrap_or("message");
    let payload_ref = externalizer.write(
        message,
        kind,
        &prepared.text,
        payload_metadata_json(&prepared)?,
    )?;
    let placeholder = externalized_payload_placeholder(
        &payload_ref,
        "content",
        prepared.quarantine_reason.as_deref(),
    );
    let metadata_json = externalized_payload_metadata(&payload_ref, &prepared)?;
    Ok(StagedRawMessageIngest {
        prepared,
        whole_message: Some(StagedWholeMessage {
            payload_ref,
            placeholder,
            metadata_json,
        }),
    })
}

pub async fn commit_staged_raw_message(
    conn: &(impl Executor + ?Sized),
    message: &SessionMessageRecord,
    staged: StagedRawMessageIngest,
) -> Result<RawMessageUpsert, LcmError> {
    for payload_ref in &staged.prepared.pending_payload_refs {
        payload::upsert_payload_metadata(conn, payload_ref).await?;
    }
    let Some(whole_message) = staged.whole_message else {
        let projection_text = derived_text_for_index(&staged.prepared.text);
        upsert_inline_raw_message(
            conn,
            message,
            &staged.prepared.text,
            staged.prepared.metadata_json.as_deref(),
        )
        .await?;
        return Ok(RawMessageUpsert {
            projection_text,
            projection_metadata_json: staged.prepared.metadata_json,
        });
    };
    payload::upsert_payload_metadata(conn, &whole_message.payload_ref).await?;
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
            message.timestamp,
            whole_message.payload_ref.content_hash.as_str(),
            LcmStorageKind::External.as_str(),
            whole_message.payload_ref.payload_ref.as_str(),
            whole_message.placeholder.as_str(),
            whole_message.placeholder.as_str(),
            whole_message.metadata_json.as_str(),
        ],
    )
    .await?;
    Ok(RawMessageUpsert {
        projection_text: whole_message.placeholder,
        projection_metadata_json: Some(whole_message.metadata_json),
    })
}

pub async fn upsert_raw_message_with_payload_tracked(
    conn: &(impl Executor + ?Sized),
    storage_root: &Path,
    message: &SessionMessageRecord,
    rollback: &mut payload::PayloadFileRollback,
) -> Result<RawMessageUpsert, LcmError> {
    let staged = stage_raw_message_with_payload_tracked(storage_root, message, rollback)?;
    commit_staged_raw_message(conn, message, staged).await
}

/// Applies ingest protection to an arbitrary replay field value (for example
/// active-replay `tool_calls`) using the same redaction and substring media
/// externalization primitives as raw-message ingest.
pub async fn protect_replay_field_value_tracked(
    conn: &(impl Executor + ?Sized),
    storage_root: &Path,
    message: &SessionMessageRecord,
    field_path: &str,
    value: &JsonValue,
    rollback: &mut payload::PayloadFileRollback,
) -> Result<JsonValue, LcmError> {
    let mut externalizer = PayloadExternalizer {
        storage_root,
        rollback,
    };
    let encoded = serde_json::to_string(value)
        .map_err(|error| LcmError::Db(format!("replay privacy encoding failed: {error}")))?;
    let initial = sanitize_lcm_payload_text(&encoded)
        .map_err(|error| LcmError::Db(format!("replay privacy sanitization failed: {error}")))?;
    let mut protected = serde_json::from_str(initial.sanitized_text())
        .map_err(|error| LcmError::Db(format!("replay privacy decoding failed: {error}")))?;

    let mut payloads = Vec::new();
    protect_json_media_payloads(
        &mut protected,
        message,
        field_path,
        &mut payloads,
        &mut externalizer,
    )?;
    for payload_ref in &payloads {
        payload::upsert_payload_metadata(conn, payload_ref).await?;
    }
    let serialized = serde_json::to_string(&protected)
        .map_err(|error| LcmError::Db(format!("replay privacy encoding failed: {error}")))?;
    let sanitized = bind_sanitized_lcm_payload_text(&encoded, &serialized)
        .map_err(|error| LcmError::Db(format!("replay privacy receipt failed: {error}")))?;
    serde_json::from_str(sanitized.sanitized_text())
        .map_err(|error| LcmError::Db(format!("replay privacy decoding failed: {error}")))
}

fn prepare_message(
    message: &SessionMessageRecord,
    externalizer: &mut PayloadExternalizer<'_>,
) -> Result<PreparedMessage, LcmError> {
    let initial = sanitize_lcm_payload_text(&message.text).map_err(|error| {
        LcmError::SanitizationRefused {
            reason: format!("LCM privacy sanitization failed: {error}"),
        }
    })?;
    let mut text = initial.sanitized_text().to_owned();
    let quarantine_reason =
        security::quarantine_reason(&message.role, message.kind.as_deref(), &text)
            .map(str::to_owned);
    let mut pending_payload_refs = Vec::new();

    let mut handled_as_structured = false;
    if quarantine_reason.is_none()
        && let Ok(mut value) = serde_json::from_str::<JsonValue>(&text)
        && matches!(value, JsonValue::Object(_) | JsonValue::Array(_))
    {
        handled_as_structured = true;
        let mut json_changed = false;
        let mut nested_payloads = Vec::new();
        protect_json_media_payloads(
            &mut value,
            message,
            "content",
            &mut nested_payloads,
            externalizer,
        )?;
        if !nested_payloads.is_empty() {
            pending_payload_refs.extend(nested_payloads.iter().cloned());
            json_changed = true;
        }
        if json_changed {
            text = serde_json::to_string(&value)
                .map_err(|err| LcmError::Db(format!("json protection failed: {err}")))?;
        }
    }

    // Hermes `_protect_payload_substrings` (ingest_protection.py:576-614):
    // externalize only the media/base64 spans of plain text, keeping the
    // surrounding text inline and searchable. Whole-message externalization
    // still wins when there is no inline scaffold worth keeping or when a
    // whole-message reason (quarantine, binary-ish, oversized tool output)
    // applies.
    if quarantine_reason.is_none()
        && !handled_as_structured
        && !security::prefers_whole_message_externalization(
            &message.role,
            message.kind.as_deref(),
            &text,
        )
        && has_inline_scaffold_outside_media_spans(&text)
    {
        let mut span_payloads = Vec::new();
        if let Some(protected) =
            replace_media_substrings(&text, message, "content", &mut span_payloads, externalizer)?
        {
            pending_payload_refs.extend(span_payloads.iter().cloned());
            text = protected;
        }
    }

    let nested_external_payloads = pending_payload_refs.len();
    let sanitization = bind_sanitized_lcm_payload_text(&message.text, &text)
        .map_err(|error| LcmError::Db(format!("LCM privacy receipt failed: {error}")))?;
    text = sanitization.sanitized_text().to_owned();
    let quarantine_receipt = quarantine_reason
        .as_ref()
        .map(|_| quarantine_lcm_payload_text(&message.text))
        .transpose()
        .map_err(|error| LcmError::Db(format!("LCM quarantine receipt failed: {error}")))?;
    let quarantine_kind = quarantine_reason
        .as_ref()
        .map(|_| "quarantined_assistant_output".to_owned());
    let mut prepared = PreparedMessage {
        text,
        metadata_json: None,
        external_kind: quarantine_kind.clone(),
        sanitization,
        quarantine_receipt,
        nested_external_payloads,
        quarantine_reason,
        quarantine_kind,
        pending_payload_refs,
    };
    prepared.metadata_json = protected_metadata_json(message.metadata_json.as_deref(), &prepared)?;
    Ok(prepared)
}

fn protect_json_media_payloads(
    value: &mut JsonValue,
    message: &SessionMessageRecord,
    field_path: &str,
    payloads: &mut Vec<LcmPayloadRef>,
    externalizer: &mut PayloadExternalizer<'_>,
) -> Result<(), LcmError> {
    match value {
        JsonValue::Object(map) => {
            let original = std::mem::take(map);
            let mut rebuilt = Map::with_capacity(original.len());
            for (key, mut child) in original {
                let mut replaced_key = None;
                if security::contains_media_payload(&key) {
                    let key_field_path = format!("{field_path}.<key>");
                    if let Some(protected_key) = replace_media_substrings(
                        &key,
                        message,
                        &key_field_path,
                        payloads,
                        externalizer,
                    )? {
                        replaced_key = Some(protected_key);
                    }
                }
                let child_path = if replaced_key.is_none() {
                    format!("{field_path}.{key}")
                } else {
                    format!("{field_path}.<key>")
                };
                protect_json_media_payloads(
                    &mut child,
                    message,
                    &child_path,
                    payloads,
                    externalizer,
                )?;
                let protected_key = replaced_key.unwrap_or(key);
                rebuilt.insert(protected_key, child);
            }
            *map = rebuilt;
        }
        JsonValue::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                let child_path = format!("{field_path}[{index}]");
                protect_json_media_payloads(child, message, &child_path, payloads, externalizer)?;
            }
        }
        JsonValue::String(text) if security::contains_media_payload(text) => {
            // Hermes `_protect_value` applies `_protect_payload_substrings`
            // to nested strings: only the media spans are externalized while
            // surrounding text stays in place.
            if let Some(protected) =
                replace_media_substrings(text, message, field_path, payloads, externalizer)?
            {
                *text = protected;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Returns true when the text holds non-whitespace content outside its
/// media/base64 spans, i.e. there is an inline scaffold worth preserving via
/// substring externalization instead of whole-message externalization.
fn has_inline_scaffold_outside_media_spans(text: &str) -> bool {
    let mut spans = security::data_uri_spans(text);
    spans.extend(security::long_base64_run_spans(text));
    if spans.is_empty() {
        return false;
    }
    spans.sort_unstable();
    let mut cursor = 0usize;
    for (start, end) in spans {
        let start = start.max(cursor);
        if text[cursor..start].chars().any(|ch| !ch.is_whitespace()) {
            return true;
        }
        cursor = cursor.max(end);
    }
    text[cursor..].chars().any(|ch| !ch.is_whitespace())
}

/// Port of hermes-lcm `_protect_payload_substrings`
/// (ingest_protection.py:576-614): pass 1 externalizes data-URI base64 spans,
/// pass 2 externalizes qualifying long base64 runs in the remaining text.
/// Returns `None` when nothing matched.
fn replace_media_substrings(
    text: &str,
    message: &SessionMessageRecord,
    field_path: &str,
    payloads: &mut Vec<LcmPayloadRef>,
    externalizer: &mut PayloadExternalizer<'_>,
) -> Result<Option<String>, LcmError> {
    let data_uri_spans = security::data_uri_spans(text);
    let after_data_uris = if data_uri_spans.is_empty() {
        text.to_string()
    } else {
        externalize_spans(
            text,
            &data_uri_spans,
            message,
            field_path,
            payloads,
            externalizer,
        )?
    };
    let run_spans = security::long_base64_run_spans(&after_data_uris);
    if run_spans.is_empty() {
        return Ok((!data_uri_spans.is_empty()).then_some(after_data_uris));
    }
    let protected = externalize_spans(
        &after_data_uris,
        &run_spans,
        message,
        field_path,
        payloads,
        externalizer,
    )?;
    Ok(Some(protected))
}

fn externalize_spans(
    text: &str,
    spans: &[(usize, usize)],
    message: &SessionMessageRecord,
    field_path: &str,
    payloads: &mut Vec<LcmPayloadRef>,
    externalizer: &mut PayloadExternalizer<'_>,
) -> Result<String, LcmError> {
    let mut protected = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for &(start, end) in spans {
        protected.push_str(&text[cursor..start]);
        let span = &text[start..end];
        let sanitization = sanitize_lcm_payload_text(span)
            .map_err(|error| LcmError::Db(format!("LCM payload sanitization failed: {error}")))?;
        let metadata_json = Some(
            json!({
                "ingest_payload": true,
                "field_path": field_path,
                "sanitization_receipt": sanitization.receipt(),
            })
            .to_string(),
        );
        let payload_ref = externalizer.write(
            message,
            "ingest_payload",
            sanitization.sanitized_text(),
            metadata_json,
        )?;
        protected.push_str(&ingest_payload_placeholder(&payload_ref, field_path));
        payloads.push(payload_ref);
        cursor = end;
    }
    protected.push_str(&text[cursor..]);
    Ok(protected)
}

fn ingest_payload_placeholder(payload_ref: &LcmPayloadRef, field_path: &str) -> String {
    format!(
        "[Externalized LCM ingest payload: kind={}; field={}; chars={}; bytes={}; ref={}]",
        safe_placeholder_metadata(&payload_ref.kind),
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

const MAX_PROVIDER_METADATA_BYTES: u64 = 1_048_576;

/// Returns whether the current detector would change this provider metadata.
///
/// This is the at-rest rescan's change probe for the exact transformation
/// ingest applies through [`protected_metadata_json`]: metadata persisted
/// under older detector rules is dirty when re-sanitizing it under the
/// current rules yields a different document. A document the sanitizer
/// refuses to evaluate is a typed refusal, never implicitly clean.
pub fn provider_metadata_requires_resanitization(
    provider_metadata_json: &str,
) -> Result<bool, LcmError> {
    let original = serde_json::from_str::<JsonValue>(provider_metadata_json).map_err(|error| {
        LcmError::SanitizationRefused {
            reason: format!("stored LCM provider metadata is not valid JSON: {error}"),
        }
    })?;
    let sanitized =
        sanitize_provider_metadata_json(provider_metadata_json, MAX_PROVIDER_METADATA_BYTES)
            .ok_or_else(|| LcmError::SanitizationRefused {
                reason: "LCM metadata sanitization failed".to_owned(),
            })?;
    Ok(sanitized != original)
}

/// Pure function of the provider metadata bytes: every failure is a
/// deterministic content refusal, never an environmental fault.
fn protected_metadata_json(
    original: Option<&str>,
    prepared: &PreparedMessage,
) -> Result<Option<String>, LcmError> {
    let refused = |reason: String| LcmError::SanitizationRefused { reason };
    let mut metadata =
        sanitize_provider_metadata_json(original.unwrap_or("{}"), MAX_PROVIDER_METADATA_BYTES)
            .ok_or_else(|| refused("LCM metadata sanitization failed".to_owned()))?;
    if !metadata.is_object() {
        return Err(refused(
            "LCM metadata sanitization failed: metadata must be a JSON object".to_owned(),
        ));
    }
    let sanitized_metadata = serde_json::to_string(&metadata)
        .map_err(|error| refused(format!("LCM metadata encoding failed: {error}")))?;
    let metadata_sanitization =
        bind_sanitized_lcm_payload_text(original.unwrap_or("{}"), &sanitized_metadata)
            .map_err(|error| refused(format!("LCM metadata receipt failed: {error}")))?;
    add_sanitization_metadata(
        &mut metadata,
        prepared,
        Some(metadata_sanitization.receipt()),
    )?;
    Ok(Some(metadata.to_string()))
}

fn payload_metadata_json(prepared: &PreparedMessage) -> Result<Option<String>, LcmError> {
    let mut metadata = JsonValue::Object(Map::new());
    add_sanitization_metadata(&mut metadata, prepared, None)?;
    Ok(Some(metadata.to_string()))
}

fn add_sanitization_metadata(
    metadata: &mut JsonValue,
    prepared: &PreparedMessage,
    metadata_receipt: Option<&SanitizationReceiptV1>,
) -> Result<(), LcmError> {
    let mut ingest = Map::new();
    ingest.insert(
        "sanitization_receipt".to_owned(),
        serde_json::to_value(prepared.receipt())
            .map_err(|error| LcmError::Db(format!("LCM receipt encoding failed: {error}")))?,
    );
    if let Some(receipt) = metadata_receipt {
        ingest.insert(
            "metadata_sanitization_receipt".to_owned(),
            serde_json::to_value(receipt).map_err(|error| {
                LcmError::Db(format!("LCM metadata receipt encoding failed: {error}"))
            })?,
        );
    }
    if prepared.nested_external_payloads > 0 {
        ingest.insert(
            "nested_external_payloads".to_string(),
            json!(prepared.nested_external_payloads),
        );
    }
    if !prepared.sanitization.findings().is_empty() {
        let mut patterns = prepared
            .sanitization
            .findings()
            .iter()
            .map(|finding| redaction_pattern(finding.detector()))
            .collect::<Vec<_>>();
        patterns.sort_unstable();
        patterns.dedup();
        ingest.insert("redacted".to_string(), json!(true));
        ingest.insert("redaction_patterns".to_string(), json!(patterns));
        ingest.insert("lossy".to_string(), json!(true));
    }
    if let Some(reason) = prepared.quarantine_reason.as_deref() {
        ingest.insert("reason".to_string(), json!(reason));
    }
    if let Some(kind) = prepared.quarantine_kind.as_deref() {
        ingest.insert("kind".to_string(), json!(kind));
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert("ingest_protection".to_string(), JsonValue::Object(ingest));
    }
    Ok(())
}

const fn redaction_pattern(detector: PrivacyDetectorV1) -> &'static str {
    match detector {
        PrivacyDetectorV1::ExactCredential | PrivacyDetectorV1::CredentialAssignment => "api_key",
        PrivacyDetectorV1::BearerToken => "bearer_token",
        PrivacyDetectorV1::PrivateKey => "private_key",
        PrivacyDetectorV1::SensitiveField => "sensitive_field",
        PrivacyDetectorV1::HighEntropyToken => "high_entropy",
        PrivacyDetectorV1::MalformedRecord => "malformed_record",
        PrivacyDetectorV1::RecordSizeLimit => "record_size_limit",
        PrivacyDetectorV1::StructureLimit => "structure_limit",
    }
}

#[cfg(test)]
#[path = "raw/ingest_protection_defaults_tests.rs"]
mod ingest_protection_defaults_tests;

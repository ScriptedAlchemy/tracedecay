use std::path::Path;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay::sessions::lcm::{LcmError, LcmStorageKind, LCM_SCHEMA_VERSION};

mod common;
use common::{
    isolated_lcm_db_path as isolated_db_path, lcm_payload_message as raw_message,
    lcm_payload_session as sample_session, open_lcm_db,
};

async fn raw_snippet_and_index(
    db_path: &std::path::Path,
    provider: &str,
    message_id: &str,
) -> (String, String) {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT snippet_text, index_text
             FROM lcm_raw_messages
             WHERE provider = ?1 AND message_id = ?2",
            libsql::params![provider, message_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    (row.get(0).unwrap(), row.get(1).unwrap())
}

async fn lcm_fts_count(db_path: &std::path::Path, query: &str) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_raw_messages_fts
             WHERE lcm_raw_messages_fts MATCH ?1",
            libsql::params![query],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn raw_metadata_json(
    db_path: &std::path::Path,
    provider: &str,
    message_id: &str,
) -> Option<String> {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT metadata_json
             FROM lcm_raw_messages
             WHERE provider = ?1 AND message_id = ?2",
            libsql::params![provider, message_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hex::encode(hasher.finalize())
}

fn expected_payload_ref(
    provider: &str,
    session_id: &str,
    message_id: &str,
    content: &str,
) -> String {
    let content_hash = sha256_hex(content.as_bytes());
    let owner_hash =
        sha256_hex(format!("{provider}\0{session_id}\0{message_id}\0{content_hash}").as_bytes());
    format!("payload_{owner_hash}.payload")
}

fn externalized_ref_from_placeholder(text: &str) -> String {
    let marker = "ref=";
    let start = text.find(marker).expect("placeholder ref") + marker.len();
    let tail = &text[start..];
    let end = tail.find([']', ',', ';']).unwrap_or(tail.len());
    tail[..end].trim().to_string()
}

#[tokio::test]
async fn externalizes_nested_json_media_payload_without_externalizing_scaffold() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let media_payload = format!(
        "data:image/png;base64,{}",
        "QWxhZGRpbjpvcGVuIHNlc2FtZQ==".repeat(160)
    );
    let content = json!({
        "content": [
            {"type": "text", "text": "keep searchable nested canary"},
            {"type": "image_url", "image_url": {"url": media_payload}},
        ],
        "tool_result": {"mime": "image/png"}
    })
    .to_string();
    let mut message = raw_message("cursor", "nested-media", "session-1", "user", &content);
    message.kind = Some("message".to_string());

    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("nested media payload should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "nested-media")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(raw.content.contains("keep searchable nested canary"));
    assert!(!raw.content.contains("QWxhZGRpbjpvcGVuIHNlc2FtZQ"));
    assert!(raw.content.contains("[Externalized LCM ingest payload:"));
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["ingest_protection"]["nested_external_payloads"], 1);

    let payload_ref = externalized_ref_from_placeholder(&raw.content);
    let expanded = store
        .lcm_expand_payload(
            "cursor",
            "session-1",
            &payload_ref,
            0,
            media_payload.chars().count(),
        )
        .await
        .expect("nested payload should expand with hash and ownership checks");
    assert_eq!(expanded.content, media_payload);
    assert_eq!(lcm_fts_count(&db_path, "nested").await, 1);
    assert_eq!(lcm_fts_count(&db_path, "QWxhZGRpbjpvcGVu").await, 0);
}

// Mirrors hermes-lcm `_protect_payload_substrings`
// (ingest_protection.py:576-614, Hermes test
// `test_ingest_preserves_trailing_text_after_data_uri`): only the data-URI
// span is externalized while surrounding plain text stays inline and
// searchable.
#[tokio::test]
async fn data_uri_substring_externalizes_span_keeping_surrounding_text_searchable() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let media_span = format!(
        "data:application/octet-stream;base64,{}",
        "QUJDRA==".repeat(64)
    );
    let content = format!("substringprefixcanary inspect {media_span} then substringsuffixcanary");
    let mut message = raw_message("cursor", "substring-media", "session-1", "user", &content);
    message.kind = Some("message".to_string());

    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("substring media message should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "substring-media")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(raw.content.starts_with("substringprefixcanary inspect ["));
    assert!(raw.content.ends_with(" then substringsuffixcanary"));
    assert!(raw.content.contains("[Externalized LCM ingest payload:"));
    assert!(!raw.content.contains(";base64,"));
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["ingest_protection"]["nested_external_payloads"], 1);

    assert_eq!(lcm_fts_count(&db_path, "substringprefixcanary").await, 1);
    assert_eq!(lcm_fts_count(&db_path, "substringsuffixcanary").await, 1);
    assert_eq!(lcm_fts_count(&db_path, "QUJDRA").await, 0);

    let payload_ref = externalized_ref_from_placeholder(&raw.content);
    let expanded = store
        .lcm_expand_payload(
            "cursor",
            "session-1",
            &payload_ref,
            0,
            media_span.chars().count(),
        )
        .await
        .expect("substring payload should expand");
    assert_eq!(expanded.content, media_span);
}

// Mirrors hermes-lcm `_protect_payload_substrings` pass 2: a long generic
// base64 run embedded in plain text is externalized as a substring while the
// surrounding log text stays inline.
#[tokio::test]
async fn long_base64_run_substring_externalizes_span_inline() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let run = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".repeat(80);
    let content = format!("buildlogprefixcanary {run} buildlogsuffixcanary");
    let mut message = raw_message("cursor", "substring-base64", "session-1", "user", &content);
    message.kind = Some("message".to_string());

    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("substring base64 message should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "substring-base64")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(raw.content.starts_with("buildlogprefixcanary ["));
    assert!(raw.content.ends_with(" buildlogsuffixcanary"));
    assert!(raw.content.contains("[Externalized LCM ingest payload:"));
    assert!(!raw.content.contains(&run));

    assert_eq!(lcm_fts_count(&db_path, "buildlogprefixcanary").await, 1);
    assert_eq!(lcm_fts_count(&db_path, "buildlogsuffixcanary").await, 1);

    let payload_ref = externalized_ref_from_placeholder(&raw.content);
    let expanded = store
        .lcm_expand_payload("cursor", "session-1", &payload_ref, 0, run.chars().count())
        .await
        .expect("substring payload should expand");
    assert_eq!(expanded.content, run);
}

// When the message body is nothing but the media payload there is no inline
// scaffold worth keeping, so whole-message externalization still applies
// (intentional storage-representation divergence from Hermes; recovery via
// expand is identical).
#[tokio::test]
async fn message_that_is_only_a_media_payload_externalizes_whole_message() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let content = format!(
        "data:application/octet-stream;base64,{}",
        "QUJDRA==".repeat(64)
    );
    let mut message = raw_message("cursor", "whole-media", "session-1", "user", &content);
    message.kind = Some("message".to_string());

    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("whole media message should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "whole-media")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    let payload_ref = raw.payload_ref.as_deref().expect("payload ref");
    let expanded = store
        .lcm_expand_payload(
            "cursor",
            "session-1",
            payload_ref,
            0,
            content.chars().count(),
        )
        .await
        .expect("whole media payload should expand");
    assert_eq!(expanded.content, content);
}

// Mirrors hermes-lcm `_DATA_URI_BASE64_RE` minimum: tiny inline data URIs
// (icons/thumbnails below 256 base64 chars) stay inline and lossless.
#[tokio::test]
async fn tiny_data_uri_stays_inline_and_lossless() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let content = format!(
        "tinyiconcanary data:image/png;base64,{} trailing text",
        "iVBORw0KGgo=".repeat(8)
    );
    let mut message = raw_message("cursor", "tiny-data-uri", "session-1", "user", &content);
    message.kind = Some("message".to_string());

    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("tiny data uri message should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "tiny-data-uri")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert_eq!(raw.content, content);
    assert!(raw.metadata_json.is_none());
    assert_eq!(lcm_fts_count(&db_path, "tinyiconcanary").await, 1);
}

// Mirrors hermes-lcm `_sensitive_pattern_for_key` / `redact_sensitive_value`
// (ingest_protection.py:252-323): when redaction is enabled, JSON values
// under sensitive keys are fully redacted even when the flat assignment scan
// misses them (compact aliases, short secrets).
#[tokio::test]
async fn json_key_sensitive_redaction_covers_compact_aliases_and_short_secrets() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let content = json!({
        "apiToken": "shortkey1",
        "nested": {"client-secret": "tiny66"},
        "auth": {"token": "tok12"},
        "note": "keep jsonkeyredactioncanary",
    })
    .to_string();
    let mut message = raw_message(
        "cursor",
        "json-key-redaction",
        "session-1",
        "user",
        &content,
    );
    message.kind = Some("message".to_string());
    message.metadata_json = Some(
        json!({
            "lcm_ingest": {
                "sensitive_patterns_enabled": true,
                "sensitive_patterns": ["default"]
            }
        })
        .to_string(),
    );

    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("json key redacted message should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "json-key-redaction")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(!raw.content.contains("shortkey1"));
    assert!(!raw.content.contains("tiny66"));
    assert!(!raw.content.contains("tok12"));
    assert!(raw
        .content
        .contains("[LCM sensitive redaction: name=api_key"));
    assert!(raw
        .content
        .contains("[LCM sensitive redaction: name=bearer_token"));
    assert!(raw.content.contains("keep jsonkeyredactioncanary"));

    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["ingest_protection"]["lossy"], true);
    assert_eq!(metadata["ingest_protection"]["redacted"], true);
    let patterns = metadata["ingest_protection"]["redaction_patterns"]
        .as_array()
        .expect("redaction patterns");
    assert!(patterns.contains(&json!("api_key")));
    assert!(patterns.contains(&json!("bearer_token")));

    assert_eq!(lcm_fts_count(&db_path, "shortkey1").await, 0);
    assert_eq!(lcm_fts_count(&db_path, "jsonkeyredactioncanary").await, 1);
}

// Parity with Hermes defaults: the JSON-key walk is opt-in; without the
// config flag the same content stays lossless.
#[tokio::test]
async fn json_key_sensitive_redaction_disabled_by_default_keeps_content() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let content = json!({"apiToken": "shortkey1", "note": "lossless"}).to_string();
    let mut message = raw_message("cursor", "json-key-lossless", "session-1", "user", &content);
    message.kind = Some("message".to_string());

    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("lossless json message should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "json-key-lossless")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.content, content);
    assert!(raw.metadata_json.is_none());
}

#[tokio::test]
async fn sensitive_redaction_is_opt_in_lossy_and_not_indexed() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let secret = "sk-redaction1234567890abcdef";
    let mut message = raw_message(
        "cursor",
        "redacted-secret",
        "session-1",
        "user",
        &format!("api_key={secret} keep searchable redaction canary"),
    );
    message.kind = Some("message".to_string());
    message.metadata_json = Some(
        json!({
            "lcm_ingest": {
                "sensitive_patterns_enabled": true,
                "sensitive_patterns": ["api_key"]
            }
        })
        .to_string(),
    );

    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("redacted message should ingest");
    let raw = db
        .lcm_load_raw_message("cursor", "redacted-secret")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(!raw.content.contains(secret));
    assert!(raw
        .content
        .contains("[LCM sensitive redaction: name=api_key"));
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["ingest_protection"]["lossy"], true);
    assert_eq!(metadata["ingest_protection"]["redacted"], true);
    assert_eq!(
        metadata["ingest_protection"]["redaction_patterns"],
        json!(["api_key"])
    );

    let status = db
        .lcm_status("cursor", Some("session-1"))
        .await
        .expect("status should load");
    assert!(status.redaction.enabled);
    assert_eq!(status.redaction.lossy_records, 1);
    assert_eq!(
        lcm_fts_count(&db_path, "redaction1234567890abcdef").await,
        0
    );
    assert_eq!(lcm_fts_count(&db_path, "redaction").await, 1);
}

#[tokio::test]
async fn quoted_password_assignment_redacts_full_quoted_value() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let secret = "correct horse battery staple";
    let mut message = raw_message(
        "cursor",
        "quoted-password-redaction",
        "session-1",
        "user",
        &format!("password=\"{secret}\" keep quotedpasswordcanary"),
    );
    message.metadata_json = Some(
        json!({
            "lcm_ingest": {
                "sensitive_patterns_enabled": true,
                "sensitive_patterns": ["password_assignment"]
            }
        })
        .to_string(),
    );

    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("quoted password message should ingest");
    let raw = db
        .lcm_load_raw_message("cursor", "quoted-password-redaction")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(!raw.content.contains(secret));
    assert!(raw
        .content
        .contains("[LCM sensitive redaction: name=password_assignment"));
    assert!(raw.content.contains("keep quotedpasswordcanary"));
    assert_eq!(lcm_fts_count(&db_path, "battery").await, 0);
}

#[tokio::test]
async fn api_alias_assignments_redact_apikey_and_apitoken() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let api_key_secret = "aliaskey1234567890";
    let api_token_secret = "aliastoken1234567890";
    let mut message = raw_message(
        "cursor",
        "api-alias-redaction",
        "session-1",
        "user",
        &format!("apikey={api_key_secret} apitoken={api_token_secret} keep aliasredactioncanary"),
    );
    message.metadata_json = Some(
        json!({
            "lcm_ingest": {
                "sensitive_patterns_enabled": true,
                "sensitive_patterns": ["api_key"]
            }
        })
        .to_string(),
    );

    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("api alias message should ingest");
    let raw = db
        .lcm_load_raw_message("cursor", "api-alias-redaction")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(!raw.content.contains(api_key_secret));
    assert!(!raw.content.contains(api_token_secret));
    assert!(raw
        .content
        .contains("[LCM sensitive redaction: name=api_key"));
    assert!(raw.content.contains("keep aliasredactioncanary"));
    assert_eq!(lcm_fts_count(&db_path, "aliaskey1234567890").await, 0);
    assert_eq!(lcm_fts_count(&db_path, "aliastoken1234567890").await, 0);
}

#[tokio::test]
async fn private_key_redaction_is_lossy_and_not_indexed_when_enabled() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let private_key =
        "-----BEGIN PRIVATE KEY-----\nPRIVATEKEYSECRET1234567890\n-----END PRIVATE KEY-----";
    let mut message = raw_message(
        "cursor",
        "redacted-private-key",
        "session-1",
        "user",
        &format!("before\n{private_key}\nafter searchable private key canary"),
    );
    message.kind = Some("message".to_string());
    message.metadata_json = Some(
        json!({
            "lcm_ingest": {
                "sensitive_patterns_enabled": true,
                "sensitive_patterns": ["default"]
            }
        })
        .to_string(),
    );

    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("private key redacted message should ingest");
    let raw = db
        .lcm_load_raw_message("cursor", "redacted-private-key")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(!raw.content.contains("BEGIN PRIVATE KEY"));
    assert!(!raw.content.contains("PRIVATEKEYSECRET1234567890"));
    assert!(raw
        .content
        .contains("[LCM sensitive redaction: name=private_key"));
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["ingest_protection"]["lossy"], true);
    assert_eq!(metadata["ingest_protection"]["redacted"], true);
    assert_eq!(
        metadata["ingest_protection"]["redaction_patterns"],
        json!(["private_key"])
    );
    assert_eq!(
        lcm_fts_count(&db_path, "PRIVATEKEYSECRET1234567890").await,
        0
    );
    assert_eq!(lcm_fts_count(&db_path, "searchable").await, 1);
}

#[tokio::test]
async fn private_key_redaction_disabled_preserves_lossless_content() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let private_key =
        "-----BEGIN PRIVATE KEY-----\nLOSSLESSPRIVATEKEY1234567890\n-----END PRIVATE KEY-----";
    let mut message = raw_message(
        "cursor",
        "lossless-private-key",
        "session-1",
        "user",
        &format!("{private_key}\nlossless private key canary"),
    );
    message.kind = Some("message".to_string());

    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("lossless private key message should ingest");
    let raw = db
        .lcm_load_raw_message("cursor", "lossless-private-key")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(raw.content.contains("BEGIN PRIVATE KEY"));
    assert!(raw.content.contains("LOSSLESSPRIVATEKEY1234567890"));
    assert!(raw.metadata_json.is_none());
    assert_eq!(
        lcm_fts_count(&db_path, "LOSSLESSPRIVATEKEY1234567890").await,
        1
    );
}

#[tokio::test]
async fn repetitive_assistant_output_is_quarantined_without_indexing_body() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let repeated_segment = "LOOP_SEGMENT repeated assistant diagnostic line.\n";
    let body = repeated_segment.repeat(2_000);
    assert!(body.chars().count() > 65_536);
    let mut message = raw_message("cursor", "assistant-loop", "session-1", "assistant", &body);
    message.kind = Some("message".to_string());

    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("repetitive assistant output should ingest");
    let raw = db
        .lcm_load_raw_message("cursor", "assistant-loop")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    let payload_ref = raw.payload_ref.as_deref().expect("payload ref");
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["ingest_protection"]["kind"],
        "quarantined_assistant_output"
    );
    assert_eq!(metadata["ingest_protection"]["reason"], "high_repetition");
    assert!(!raw.content.contains("LOOP_SEGMENT"));
    let (snippet_text, index_text) =
        raw_snippet_and_index(&db_path, "cursor", "assistant-loop").await;
    assert!(
        snippet_text.contains("[Externalized LCM ingest payload: assistant output quarantined;")
    );
    assert!(snippet_text.contains("kind=quarantined_assistant_output;"));
    assert!(snippet_text.contains("reason=high_repetition;"));
    assert_eq!(snippet_text, index_text);
    assert!(!index_text.contains("LOOP_SEGMENT"));
    assert_eq!(lcm_fts_count(&db_path, "LOOP_SEGMENT").await, 0);

    let expanded = store
        .lcm_expand_payload("cursor", "session-1", payload_ref, 0, body.chars().count())
        .await
        .expect("quarantined payload should expand");
    assert_eq!(expanded.content, body);
}

#[tokio::test]
async fn externalizes_large_tool_payload_with_recoverable_ref() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let payload = format!("tool output\n{}", "A".repeat(900_000));
    let message = raw_message("cursor", "tool-1", "session-1", "tool", &payload);
    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("raw ingest should externalize payload");

    let raw = db
        .lcm_load_raw_message("cursor", "tool-1")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    let payload_ref = raw.payload_ref.as_deref().expect("payload ref");
    assert!(payload_ref.ends_with(".payload"));
    assert_eq!(Path::new(payload_ref).file_name().unwrap(), payload_ref);
    assert!(tracedecay::sessions::lcm::payload::validate_payload_ref(payload_ref).is_ok());

    let expanded = store
        .lcm_expand_payload(
            "cursor",
            "session-1",
            payload_ref,
            0,
            payload.chars().count(),
        )
        .await
        .expect("payload should expand");
    assert_eq!(expanded.content, payload);
}

#[tokio::test]
async fn externalized_payload_indexes_placeholder_without_body_text() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let unique_secret = "uniquebodysecretdonotindex";
    let metadata_secret = "uniquemetadatasecretdonotindex";
    let payload = format!("tool output {unique_secret}\n{}", "B".repeat(900_000));
    let mut message = raw_message("cursor", "tool-secret", "session-1", "tool", &payload);
    message.metadata_json = Some(format!(r#"{{"payload_preview":"{metadata_secret}"}}"#));
    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("raw ingest should externalize payload");

    let raw = db
        .lcm_load_raw_message("cursor", "tool-secret")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    let payload_ref = raw.payload_ref.as_deref().expect("payload ref");
    let expanded = store
        .lcm_expand_payload(
            "cursor",
            "session-1",
            payload_ref,
            0,
            payload.chars().count(),
        )
        .await
        .expect("payload should expand");
    assert_eq!(expanded.content, payload);

    let (snippet_text, index_text) = raw_snippet_and_index(&db_path, "cursor", "tool-secret").await;
    assert!(snippet_text.contains("[Externalized LCM ingest payload: kind=tool_result;"));
    assert!(snippet_text.contains("field=content;"));
    assert!(snippet_text.contains(payload_ref));
    assert!(snippet_text.contains("chars="));
    assert!(snippet_text.contains("bytes="));
    assert_eq!(snippet_text, index_text);
    assert!(!snippet_text.contains(unique_secret));
    assert!(!index_text.contains(unique_secret));

    let raw_metadata = raw_metadata_json(&db_path, "cursor", "tool-secret").await;
    assert!(!raw_metadata
        .as_deref()
        .unwrap_or("")
        .contains(metadata_secret));
    assert_eq!(lcm_fts_count(&db_path, "externalized").await, 1);
    assert_eq!(lcm_fts_count(&db_path, unique_secret).await, 0);
    assert_eq!(lcm_fts_count(&db_path, metadata_secret).await, 0);
}

#[tokio::test]
async fn lcm_status_reports_missing_and_unreferenced_payloads_without_previewing_content() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let secret = format!("SUPER_SECRET_PAYLOAD\n{}", "S".repeat(300_000));
    let message = raw_message("cursor", "message-1", "session-1", "tool", &secret);
    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("payload should ingest");
    let payload_ref = db
        .lcm_load_raw_message("cursor", "message-1")
        .await
        .unwrap()
        .payload_ref
        .expect("payload ref");

    let payload_dir = tracedecay::sessions::lcm::payload::payload_dir(&storage_root);
    std::fs::remove_file(payload_dir.join(&payload_ref)).unwrap();
    std::fs::write(payload_dir.join("orphan.payload"), "ORPHAN_PAYLOAD_SECRET").unwrap();

    let status = db
        .lcm_status("cursor", Some("session-1"))
        .await
        .expect("status should load");
    let status_json = serde_json::to_value(&status).unwrap();

    assert_eq!(status_json["schema_version"], LCM_SCHEMA_VERSION);
    assert_eq!(status_json["storage_scope"], "project_local");
    assert_eq!(status_json["raw_message_count"], 1);
    assert_eq!(status_json["summary_node_count"], 0);
    assert_eq!(status_json["external_payload_count"], 1);
    assert_eq!(status_json["missing_payload_count"], 1);
    assert_eq!(status_json["unreferenced_payload_count"], 1);
    assert_eq!(status_json["payload"]["externalized_count"], 1);
    assert_eq!(status_json["payload"]["missing_count"], 1);
    assert_eq!(status_json["payload"]["unreferenced_count"], 1);
    assert_eq!(status_json["payload"]["root_contained"], true);
    assert_eq!(status_json["lifecycle"]["maintenance_debt_count"], 0);
    assert_eq!(status_json["redaction"]["enabled"], false);
    assert_eq!(status_json["redaction"]["lossy_records"], 0);

    let rendered = serde_json::to_string(&status).unwrap();
    assert!(!rendered.contains("SUPER_SECRET_PAYLOAD"));
    assert!(!rendered.contains("ORPHAN_PAYLOAD_SECRET"));
}

#[test]
fn rejects_payload_ref_path_traversal() {
    for bad in [
        "../secret",
        "/tmp/secret",
        "nested/file",
        r"nested\file",
        ".",
        "..",
    ] {
        assert!(
            tracedecay::sessions::lcm::payload::validate_payload_ref(bad).is_err(),
            "{bad} should be rejected"
        );
    }
}

#[tokio::test]
async fn denies_cross_session_payload_expansion() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-a"))
            .await
    );
    assert!(
        db.upsert_session(&sample_session("cursor", "session-b"))
            .await
    );

    let payload = format!("secret tool output\n{}", "S".repeat(300_000));
    let message = raw_message("cursor", "message-a", "session-a", "tool", &payload);
    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("payload should ingest");
    let payload_ref = db
        .lcm_load_raw_message("cursor", "message-a")
        .await
        .unwrap()
        .payload_ref
        .expect("payload ref");

    let denied = store
        .lcm_expand_payload("cursor", "session-b", &payload_ref, 0, 100)
        .await;
    assert!(matches!(denied, Err(LcmError::PayloadNotOwnedBySession)));
}

#[tokio::test]
async fn denies_expansion_after_message_updates_to_new_payload_ref() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let first_payload = format!("first secret tool output\n{}", "F".repeat(300_000));
    let mut message = raw_message(
        "cursor",
        "message-update",
        "session-1",
        "tool",
        &first_payload,
    );
    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("first payload should ingest");
    let first_ref = db
        .lcm_load_raw_message("cursor", "message-update")
        .await
        .unwrap()
        .payload_ref
        .expect("first payload ref");

    let second_payload = format!("second secret tool output\n{}", "G".repeat(300_000));
    message.text = second_payload.clone();
    store
        .ingest_raw_message(&message)
        .await
        .expect("second payload should ingest");
    let second_ref = db
        .lcm_load_raw_message("cursor", "message-update")
        .await
        .unwrap()
        .payload_ref
        .expect("second payload ref");
    assert_ne!(first_ref, second_ref);

    let stale = store
        .lcm_expand_payload("cursor", "session-1", &first_ref, 0, first_payload.len())
        .await;
    assert!(matches!(stale, Err(LcmError::PayloadNotFound)));

    let current = store
        .lcm_expand_payload(
            "cursor",
            "session-1",
            &second_ref,
            0,
            second_payload.chars().count(),
        )
        .await
        .expect("current payload should expand");
    assert_eq!(current.content, second_payload);
}

#[cfg(unix)]
#[tokio::test]
async fn external_payload_write_rejects_preexisting_symlink_ref() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let payload = format!("tool output\n{}", "C".repeat(900_000));
    let payload_ref = expected_payload_ref("cursor", "session-1", "tool-symlink", &payload);
    let payload_dir = tracedecay::sessions::lcm::payload::payload_dir(&storage_root);
    std::fs::create_dir_all(&payload_dir).unwrap();
    let outside_target = tmp.path().join("outside-target.txt");
    std::fs::write(&outside_target, "do not overwrite").unwrap();
    symlink(&outside_target, payload_dir.join(&payload_ref)).unwrap();

    let message = raw_message("cursor", "tool-symlink", "session-1", "tool", &payload);
    let store = db.lcm_store(&storage_root);
    let result = store.ingest_raw_message(&message).await;

    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(&outside_target).unwrap(),
        "do not overwrite"
    );
    assert!(db
        .lcm_load_raw_message("cursor", "tool-symlink")
        .await
        .is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn external_payload_write_rejects_symlinked_payload_directory() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    std::fs::create_dir_all(&storage_root).unwrap();
    let outside_dir = tmp.path().join("outside-payloads");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let payload_dir = tracedecay::sessions::lcm::payload::payload_dir(&storage_root);
    symlink(&outside_dir, &payload_dir).unwrap();

    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let payload = format!("tool output\n{}", "D".repeat(900_000));
    let payload_ref = expected_payload_ref("cursor", "session-1", "tool-dir-symlink", &payload);
    let message = raw_message("cursor", "tool-dir-symlink", "session-1", "tool", &payload);
    let store = db.lcm_store(&storage_root);
    let result = store.ingest_raw_message(&message).await;

    assert!(result.is_err());
    assert!(!outside_dir.join(payload_ref).exists());
    assert!(db
        .lcm_load_raw_message("cursor", "tool-dir-symlink")
        .await
        .is_none());
}

// Substring externalization splices placeholders at byte spans reported by
// the media scanners; multibyte text immediately around the span must survive
// intact (Hermes `_protect_payload_substrings` operates on Python str
// offsets, so unicode scaffold is preserved by construction there).
#[tokio::test]
async fn unicode_scaffold_survives_data_uri_substring_externalization() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let media_span = format!(
        "data:application/octet-stream;base64,{}",
        "QUJDRA==".repeat(64)
    );
    // Multibyte chars border the span on both sides: the closing boundary
    // char (a CJK ideograph) is not base64 alphabet, so the span must end
    // exactly at the `==` padding without splitting the following char.
    let prefix = "日本語のログ🦀 unicodescaffoldcanary ";
    let suffix = "終わり🎈のテキスト";
    let content = format!("{prefix}{media_span}{suffix}");
    let mut message = raw_message("cursor", "unicode-media", "session-1", "user", &content);
    message.kind = Some("message".to_string());

    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("unicode media message should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "unicode-media")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(raw.content.starts_with(prefix));
    assert!(raw.content.ends_with(suffix));
    assert!(raw.content.contains("[Externalized LCM ingest payload:"));
    assert!(!raw.content.contains(";base64,"));
    assert!(!raw.content.contains("QUJDRA"));

    assert_eq!(lcm_fts_count(&db_path, "unicodescaffoldcanary").await, 1);
    assert_eq!(lcm_fts_count(&db_path, "QUJDRA").await, 0);

    let payload_ref = externalized_ref_from_placeholder(&raw.content);
    let expanded = store
        .lcm_expand_payload(
            "cursor",
            "session-1",
            &payload_ref,
            0,
            media_span.chars().count(),
        )
        .await
        .expect("unicode-bounded payload should expand");
    assert_eq!(expanded.content, media_span);
}

// Mirrors hermes-lcm
// `test_sensitive_patterns_redact_before_large_payload_externalization`
// (ingest_protection.py `_protect_value` order): redaction runs before
// externalization, so the externalized payload file on disk must hold the
// redaction placeholder and never the secret.
#[tokio::test]
async fn redaction_applies_before_whole_message_externalization() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let secret = "sk-prequel1234567890abcdef";
    let content = format!(
        "tool output api_key={secret} preexternalredactcanary\n{}",
        "B".repeat(300_000)
    );
    let mut message = raw_message(
        "cursor",
        "redact-then-extern",
        "session-1",
        "tool",
        &content,
    );
    message.metadata_json = Some(
        json!({
            "lcm_ingest": {
                "sensitive_patterns_enabled": true,
                "sensitive_patterns": ["api_key"]
            }
        })
        .to_string(),
    );

    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("oversized redacted tool message should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "redact-then-extern")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    let payload_ref = raw.payload_ref.clone().expect("payload ref");
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["ingest_protection"]["redacted"], true);
    assert_eq!(metadata["ingest_protection"]["lossy"], true);
    assert_eq!(
        metadata["ingest_protection"]["redaction_patterns"],
        json!(["api_key"])
    );

    // The durable payload body was redacted before it ever hit disk.
    let payload_path =
        tracedecay::sessions::lcm::payload::payload_dir(&storage_root).join(&payload_ref);
    let payload_body = std::fs::read_to_string(&payload_path).expect("payload file should exist");
    assert!(!payload_body.contains(secret));
    assert!(payload_body.contains("[LCM sensitive redaction: name=api_key"));
    assert!(payload_body.contains("preexternalredactcanary"));

    // Neither the secret nor the payload body is searchable.
    assert_eq!(lcm_fts_count(&db_path, "prequel1234567890abcdef").await, 0);
    assert_eq!(lcm_fts_count(&db_path, "preexternalredactcanary").await, 0);
}

// Mirrors hermes-lcm `test_ingest_keeps_scanning_after_existing_placeholder_prefix`
// and the placeholder idempotency tests: text that already carries an
// externalized-payload placeholder is not re-externalized, while new media
// spans after it are still protected.
#[tokio::test]
async fn existing_ingest_placeholder_is_not_double_externalized() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );
    let store = db.lcm_store(&storage_root);

    // First ingest produces a real placeholder for the original data URI.
    let first_span = format!(
        "data:application/octet-stream;base64,{}",
        "QUJDRA==".repeat(64)
    );
    let mut first = raw_message(
        "cursor",
        "placeholder-origin",
        "session-1",
        "user",
        &format!("origin scaffold {first_span} tail"),
    );
    first.kind = Some("message".to_string());
    store
        .ingest_raw_message(&first)
        .await
        .expect("first media message should ingest");
    let protected_first = db
        .lcm_load_raw_message("cursor", "placeholder-origin")
        .await
        .expect("first raw message should exist")
        .content;
    let existing_ref = externalized_ref_from_placeholder(&protected_first);

    // A later message replays that placeholder text and appends a new span.
    let second_span = format!(
        "data:application/octet-stream;base64,{}",
        "WFlaQQ==".repeat(64)
    );
    let mut second = raw_message(
        "cursor",
        "placeholder-replay",
        "session-1",
        "user",
        &format!("{protected_first} appended {second_span} done"),
    );
    second.kind = Some("message".to_string());
    second.ordinal = 2;
    store
        .ingest_raw_message(&second)
        .await
        .expect("replayed placeholder message should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "placeholder-replay")
        .await
        .expect("second raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    // The pre-existing placeholder text is preserved verbatim, not wrapped
    // in another placeholder.
    assert!(raw.content.starts_with(&protected_first));
    assert!(raw.content.ends_with(" done"));
    assert!(!raw.content.contains(";base64,"));
    let placeholder_count = raw
        .content
        .matches("[Externalized LCM ingest payload:")
        .count();
    assert_eq!(placeholder_count, 2);
    let new_ref = externalized_ref_from_placeholder(&raw.content[protected_first.len()..]);
    assert_ne!(new_ref, existing_ref);
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["ingest_protection"]["nested_external_payloads"], 1,
        "only the new media span should externalize"
    );

    // Both payloads stay recoverable: the original through its first owner,
    // the new span through the replaying message.
    let original = store
        .lcm_expand_payload(
            "cursor",
            "session-1",
            &existing_ref,
            0,
            first_span.chars().count(),
        )
        .await
        .expect("original payload should still expand");
    assert_eq!(original.content, first_span);
    let appended = store
        .lcm_expand_payload(
            "cursor",
            "session-1",
            &new_ref,
            0,
            second_span.chars().count(),
        )
        .await
        .expect("new span payload should expand");
    assert_eq!(appended.content, second_span);
}

#[tokio::test]
async fn json_key_media_payload_externalizes_key_span_without_whole_message_externalization() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let media_key = format!("data:image/png;base64,{}", "QUJDRA==".repeat(64));
    let mut object = serde_json::Map::new();
    object.insert(media_key.clone(), json!("keep json-key-media canary"));
    let content = Value::Object(object).to_string();
    let mut message = raw_message("cursor", "json-key-media", "session-1", "user", &content);
    message.kind = Some("message".to_string());

    let store = db.lcm_store(&storage_root);
    store
        .ingest_raw_message(&message)
        .await
        .expect("json key media payload should ingest");

    let raw = db
        .lcm_load_raw_message("cursor", "json-key-media")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(!raw.content.contains(";base64,"));
    assert!(raw.content.contains("[Externalized LCM ingest payload:"));
    assert!(raw.content.contains("keep json-key-media canary"));

    let payload_ref = externalized_ref_from_placeholder(&raw.content);
    let expanded = store
        .lcm_expand_payload(
            "cursor",
            "session-1",
            &payload_ref,
            0,
            media_key.chars().count(),
        )
        .await
        .expect("media key payload should expand");
    assert_eq!(expanded.content, media_key);
}

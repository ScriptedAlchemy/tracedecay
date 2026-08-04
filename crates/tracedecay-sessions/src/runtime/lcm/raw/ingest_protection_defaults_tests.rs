use super::{BUILT_IN_SENSITIVE_PATTERNS, IngestProtectionDefaults, ingest_config};
use crate::host_ports::LcmRedactionPolicy;
use crate::runtime::SessionMessageRecord;
use tracedecay_runtime_core::db::engine::TestConnection;

const RAW_MESSAGE_TEST_SCHEMA: &str = "CREATE TABLE lcm_raw_messages (
    store_id INTEGER PRIMARY KEY,
    provider TEXT NOT NULL,
    message_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    timestamp INTEGER,
    content TEXT,
    content_hash TEXT NOT NULL,
    storage_kind TEXT NOT NULL,
    payload_ref TEXT,
    snippet_text TEXT NOT NULL,
    index_text TEXT NOT NULL,
    legacy_source INTEGER NOT NULL,
    legacy_truncated INTEGER NOT NULL,
    metadata_json TEXT
);";

fn profile(enabled: bool, patterns: &[&str]) -> IngestProtectionDefaults {
    IngestProtectionDefaults::from_policy(&LcmRedactionPolicy {
        enabled,
        patterns: patterns
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect(),
    })
}

#[test]
fn default_profile_leaves_redaction_off() {
    let config = ingest_config(None, &IngestProtectionDefaults::default());
    assert!(!config.sensitive_patterns_enabled);
}

#[test]
fn profile_setting_enables_redaction_without_a_metadata_key() {
    let config = ingest_config(None, &profile(true, &[]));
    assert!(config.sensitive_patterns_enabled);
    assert_eq!(config.sensitive_patterns, BUILT_IN_SENSITIVE_PATTERNS);
}

#[test]
fn profile_patterns_restrict_the_redactor_set() {
    let config = ingest_config(None, &profile(true, &["API_KEY"]));
    assert_eq!(config.sensitive_patterns, vec!["api_key".to_string()]);
}

#[test]
fn message_metadata_still_overrides_the_profile_in_both_directions() {
    let off = ingest_config(
        Some(r#"{"lcm_ingest":{"sensitive_patterns_enabled":false}}"#),
        &profile(true, &[]),
    );
    assert!(!off.sensitive_patterns_enabled);
    let on = ingest_config(
        Some(r#"{"lcm_ingest":{"sensitive_patterns_enabled":true}}"#),
        &profile(false, &[]),
    );
    assert!(on.sensitive_patterns_enabled);
}

#[test]
fn enabled_profile_redacts_an_api_key_assignment() {
    let config = ingest_config(None, &profile(true, &[]));
    let outcome = super::redact_sensitive_text("api_key=sk-liveSECRETVALUE123", &config)
        .expect("redaction succeeds");
    assert!(
        !outcome.text.contains("sk-liveSECRETVALUE123"),
        "secret survived redaction: {}",
        outcome.text
    );
}

#[tokio::test]
async fn exact_identity_reader_rejects_tampered_inline_content() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    conn.execute_batch(RAW_MESSAGE_TEST_SCHEMA)
        .await
        .expect("raw message schema");
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp,
            content, content_hash, storage_kind, payload_ref,
            snippet_text, index_text, legacy_source, legacy_truncated
         ) VALUES (
            'cursor', 'message-1', 'session-1', 'assistant', 1, 1,
            'canary-secret', 'not-the-content-hash', 'inline', NULL,
            'canary-secret', 'canary-secret', 0, 0
         )",
        (),
    )
    .await
    .expect("tampered fixture");

    let result =
        super::load_raw_message_by_identity(&*conn, "cursor", "session-1", "message-1").await;
    assert_eq!(result, Err(super::LcmError::PayloadIntegrityMismatch));
}

#[tokio::test]
async fn exact_identity_reader_rejects_missing_inline_content() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    conn.execute_batch(RAW_MESSAGE_TEST_SCHEMA)
        .await
        .expect("raw message schema");
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp,
            content, content_hash, storage_kind, payload_ref,
            snippet_text, index_text, legacy_source, legacy_truncated
         ) VALUES (
            'cursor', 'message-1', 'session-1', 'assistant', 1, 1,
            NULL, 'not-an-empty-content-hash', 'inline', NULL,
            '', '', 0, 0
         )",
        (),
    )
    .await
    .expect("missing-content fixture");

    let result =
        super::load_raw_message_by_identity(&*conn, "cursor", "session-1", "message-1").await;
    assert_eq!(result, Err(super::LcmError::PayloadIntegrityMismatch));
}

#[tokio::test]
async fn inline_upsert_preserves_the_storage_failure_cause() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    let message = SessionMessageRecord {
        provider: "cursor".to_string(),
        message_id: "message-1".to_string(),
        session_id: "session-1".to_string(),
        role: "assistant".to_string(),
        timestamp: Some(1),
        ordinal: 1,
        text: "ordinary inline content".to_string(),
        kind: Some("chat".to_string()),
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: None,
    };
    let storage_root = temp.path().join("storage");
    let mut rollback = super::payload::PayloadFileRollback::begin_cancellation_safe(&storage_root);

    let result = super::upsert_raw_message_with_payload_tracked(
        &*conn,
        &storage_root,
        &message,
        &mut rollback,
    )
    .await;
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("missing raw-message table must fail"),
    };

    assert!(
        error
            .to_string()
            .contains("no such table: lcm_raw_messages"),
        "storage cause was lost: {error}"
    );
}

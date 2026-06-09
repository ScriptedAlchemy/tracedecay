use std::path::Path;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::{LcmError, LcmStorageKind, LCM_SCHEMA_VERSION};
use tokensave::sessions::{SessionMessageRecord, SessionRecord};

fn isolated_db_path(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join(".tokensave").join("sessions.db")
}

async fn open_lcm_db(tmp: &TempDir) -> GlobalDb {
    let db_path = isolated_db_path(tmp);
    GlobalDb::open_at(&db_path).await.expect("session db open")
}

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
    let end = tail.find([']', ',', ';']).unwrap_or_else(|| tail.len());
    tail[..end].trim().to_string()
}

fn sample_session(provider: &str, session_id: &str) -> SessionRecord {
    SessionRecord {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        project_key: "/tmp/project".to_string(),
        project_path: "/tmp/project".to_string(),
        title: Some("LCM payload test".to_string()),
        started_at: Some(1_715_000_000),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

fn raw_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    role: &str,
    text: &str,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: provider.to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        timestamp: Some(1_715_000_030),
        ordinal: 1,
        text: text.to_string(),
        kind: Some("tool_result".to_string()),
        model: Some("test-model".to_string()),
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: None,
    }
}

#[tokio::test]
async fn externalizes_nested_json_media_payload_without_externalizing_scaffold() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tokensave");
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

#[tokio::test]
async fn sensitive_redaction_is_opt_in_lossy_and_not_indexed() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tokensave");
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
    assert_eq!(status.redaction.enabled, true);
    assert_eq!(status.redaction.lossy_records, 1);
    assert_eq!(
        lcm_fts_count(&db_path, "redaction1234567890abcdef").await,
        0
    );
    assert_eq!(lcm_fts_count(&db_path, "redaction").await, 1);
}

#[tokio::test]
async fn repetitive_assistant_output_is_quarantined_without_indexing_body() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tokensave");
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
    assert!(snippet_text.contains("quarantined_assistant_output"));
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
    let storage_root = tmp.path().join(".tokensave");
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
    assert!(tokensave::sessions::lcm::payload::validate_payload_ref(payload_ref).is_ok());

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
    let storage_root = tmp.path().join(".tokensave");
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
    assert!(snippet_text.contains("[externalized payload: tool_result"));
    assert!(snippet_text.contains(payload_ref));
    assert!(snippet_text.contains("bytes="));
    assert!(snippet_text.contains("sha256="));
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
    let storage_root = tmp.path().join(".tokensave");
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

    let payload_dir = tokensave::sessions::lcm::payload::payload_dir(&storage_root);
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
            tokensave::sessions::lcm::payload::validate_payload_ref(bad).is_err(),
            "{bad} should be rejected"
        );
    }
}

#[tokio::test]
async fn denies_cross_session_payload_expansion() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tokensave");
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
    let storage_root = tmp.path().join(".tokensave");
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
    let storage_root = tmp.path().join(".tokensave");
    let db = open_lcm_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session("cursor", "session-1"))
            .await
    );

    let payload = format!("tool output\n{}", "C".repeat(900_000));
    let payload_ref = expected_payload_ref("cursor", "session-1", "tool-symlink", &payload);
    let payload_dir = tokensave::sessions::lcm::payload::payload_dir(&storage_root);
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
    let storage_root = tmp.path().join(".tokensave");
    std::fs::create_dir_all(&storage_root).unwrap();
    let outside_dir = tmp.path().join("outside-payloads");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let payload_dir = tokensave::sessions::lcm::payload::payload_dir(&storage_root);
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

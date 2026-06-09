use std::path::Path;

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::{LcmError, LcmStorageKind};
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
    let payload = format!("tool output {unique_secret}\n{}", "B".repeat(900_000));
    let message = raw_message("cursor", "tool-secret", "session-1", "tool", &payload);
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

    assert_eq!(lcm_fts_count(&db_path, "externalized").await, 1);
    assert_eq!(lcm_fts_count(&db_path, unique_secret).await, 0);
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

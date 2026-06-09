use std::path::Path;

use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::{LcmError, LcmStorageKind};
use tokensave::sessions::{SessionMessageRecord, SessionRecord};

async fn open_lcm_db(tmp: &TempDir) -> GlobalDb {
    let db_path = tmp.path().join(".tokensave").join("sessions.db");
    GlobalDb::open_at(&db_path).await.expect("session db open")
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

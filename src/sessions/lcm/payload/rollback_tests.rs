use libsql::{Builder, Connection, params};
use tempfile::TempDir;

use crate::sessions::SessionMessageRecord;

use super::{LcmStore, PayloadFileRollback, payload_dir, write_external_payload_tracked};

async fn metadata_connection() -> Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "CREATE TABLE lcm_external_payloads (payload_ref TEXT PRIMARY KEY)",
        (),
    )
    .await
    .unwrap();
    conn
}

#[tokio::test]
async fn removes_only_new_unowned_files() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    std::fs::create_dir(&storage_root).unwrap();
    let conn = metadata_connection().await;
    let mut existing_write = PayloadFileRollback::begin(&storage_root);
    let existing = write_external_payload_tracked(
        &storage_root,
        "cursor",
        "session-1",
        "existing",
        "tool_result",
        "existing payload",
        None,
        &mut existing_write,
    )
    .unwrap();
    let mut rollback = PayloadFileRollback::begin(&storage_root);
    let created = write_external_payload_tracked(
        &storage_root,
        "cursor",
        "session-1",
        "created",
        "tool_result",
        "created payload",
        None,
        &mut rollback,
    )
    .unwrap();

    assert_eq!(rollback.rollback(&conn).await.unwrap(), 1);
    assert!(
        payload_dir(&storage_root)
            .join(existing.payload_ref)
            .exists()
    );
    assert!(
        !payload_dir(&storage_root)
            .join(created.payload_ref)
            .exists()
    );
}

#[tokio::test]
async fn preserves_newly_owned_file() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    std::fs::create_dir(&storage_root).unwrap();
    let conn = metadata_connection().await;
    let mut rollback = PayloadFileRollback::begin(&storage_root);
    let created = write_external_payload_tracked(
        &storage_root,
        "claude",
        "session-1",
        "created",
        "tool_result",
        "created payload",
        None,
        &mut rollback,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO lcm_external_payloads (payload_ref) VALUES (?1)",
        params![created.payload_ref.as_str()],
    )
    .await
    .unwrap();

    assert_eq!(rollback.rollback(&conn).await.unwrap(), 0);
    assert!(
        payload_dir(&storage_root)
            .join(created.payload_ref)
            .exists()
    );
}

#[tokio::test]
async fn direct_store_failure_rolls_back_metadata_and_payload_file() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    std::fs::create_dir(&storage_root).unwrap();
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE lcm_external_payloads (
            payload_ref TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            message_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            byte_count INTEGER NOT NULL,
            char_count INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            metadata_json TEXT
        );
        CREATE TABLE lcm_raw_messages (
            store_id INTEGER PRIMARY KEY AUTOINCREMENT,
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
            metadata_json TEXT,
            UNIQUE(provider, message_id)
        );
        CREATE TRIGGER reject_raw_message
        BEFORE INSERT ON lcm_raw_messages
        BEGIN
            SELECT RAISE(ABORT, 'late raw failure');
        END;",
    )
    .await
    .unwrap();
    let message = SessionMessageRecord {
        provider: "cursor".to_string(),
        message_id: "rollback-message".to_string(),
        session_id: "rollback-session".to_string(),
        role: "tool".to_string(),
        timestamp: Some(1),
        ordinal: 1,
        text: "x".repeat(300 * 1024),
        kind: Some("tool_result".to_string()),
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: None,
    };
    let transaction = tokio::sync::Mutex::new(());

    assert!(
        LcmStore::new(&conn, storage_root.clone(), &transaction)
            .ingest_raw_message(&message)
            .await
            .is_err()
    );
    let count: i64 = conn
        .query("SELECT COUNT(*) FROM lcm_external_payloads", ())
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        std::fs::read_dir(payload_dir(&storage_root))
            .unwrap()
            .count(),
        0
    );
}

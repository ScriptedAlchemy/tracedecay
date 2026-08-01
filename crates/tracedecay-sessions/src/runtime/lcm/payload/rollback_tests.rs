use tempfile::TempDir;

use crate::runtime::SessionMessageRecord;
use crate::runtime::lcm::{raw, schema};
use tracedecay_runtime_core::db::engine::{TestConnection, TransactionBehavior};

use super::{
    ExternalPayloadWrite, PayloadFileRollback, payload_dir, write_external_payload_tracked,
};

#[tokio::test]
async fn cancellation_guard_removes_new_file_on_drop() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    std::fs::create_dir(&storage_root).unwrap();
    let mut rollback = PayloadFileRollback::begin_cancellation_safe(&storage_root);
    let created = write_external_payload_tracked(
        &storage_root,
        ExternalPayloadWrite {
            provider: "cursor",
            session_id: "session-1",
            message_id: "created",
            kind: "tool_result",
            content: "created payload",
            metadata_json: None,
        },
        &mut rollback,
    )
    .unwrap();

    let payload_path = payload_dir(&storage_root).join(created.payload_ref);
    assert!(payload_path.exists());
    drop(rollback);
    assert!(!payload_path.exists());
}

#[tokio::test]
async fn disarmed_guard_preserves_committed_file() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    std::fs::create_dir(&storage_root).unwrap();
    let mut rollback = PayloadFileRollback::begin_cancellation_safe(&storage_root);
    let created = write_external_payload_tracked(
        &storage_root,
        ExternalPayloadWrite {
            provider: "claude",
            session_id: "session-1",
            message_id: "created",
            kind: "tool_result",
            content: "created payload",
            metadata_json: None,
        },
        &mut rollback,
    )
    .unwrap();

    let payload_path = payload_dir(&storage_root).join(created.payload_ref);
    rollback.disarm();
    assert!(payload_path.exists());
}

#[tokio::test]
async fn direct_store_failure_rolls_back_metadata_and_payload_file() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tracedecay");
    std::fs::create_dir(&storage_root).unwrap();
    let conn = TestConnection::open(&tmp.path().join("global.db"));
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
            PRIMARY KEY(provider, session_id)
        );
        CREATE TABLE session_messages (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            timestamp INTEGER,
            ordinal INTEGER NOT NULL,
            text TEXT NOT NULL,
            metadata_json TEXT,
            PRIMARY KEY(provider, message_id)
        );",
    )
    .await
    .unwrap();
    schema::ensure_lcm_schema(&conn).await.unwrap();
    conn.execute_batch(
        "INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES ('cursor', 'rollback-session', '/tmp/project', '/tmp/project');
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
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let mut rollback = PayloadFileRollback::begin_cancellation_safe(&storage_root);
    assert!(
        raw::upsert_raw_message_with_payload_tracked(
            &transaction,
            &storage_root,
            &message,
            &mut rollback,
        )
        .await
        .is_err()
    );
    transaction.rollback().await.unwrap();
    drop(rollback);
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

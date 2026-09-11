use tracedecay_runtime_core::db::engine::TestConnection;
use tracedecay_store::SessionMessageRecord;

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

#[tokio::test]
async fn predecessor_range_skips_policy_anchor_roles() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    conn.execute_batch(
        "CREATE TABLE lcm_raw_messages (
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
            metadata_json TEXT,
            UNIQUE(provider, message_id)
        );
        CREATE TABLE lcm_raw_predecessor_ranges (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            from_store_id INTEGER NOT NULL,
            to_store_id INTEGER NOT NULL,
            PRIMARY KEY(provider, message_id)
        );",
    )
    .await
    .expect("predecessor schema");
    let storage_root = temp.path().join("storage");
    let mut rollback = super::payload::PayloadFileRollback::begin_cancellation_safe(&storage_root);
    for (message_id, role, ordinal, text) in [
        (
            "prior-user",
            "user",
            1,
            "keep this conversational predecessor",
        ),
        (
            "compact_boundary:marker",
            "system",
            2,
            "Claude compaction boundary",
        ),
        (
            "compact-summary",
            "user",
            3,
            "exact Claude compact summary wrapper and body",
        ),
    ] {
        let message = SessionMessageRecord {
            provider: "claude".to_string(),
            message_id: message_id.to_string(),
            session_id: "session-1".to_string(),
            role: role.to_string(),
            timestamp: Some(ordinal),
            ordinal,
            text: text.to_string(),
            kind: Some("message".to_string()),
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        };
        super::upsert_raw_message_with_payload_tracked(
            &*conn,
            &storage_root,
            &message,
            &mut rollback,
        )
        .await
        .expect("ingest predecessor fixture");
    }
    let mut rows = conn
        .query(
            "SELECT from_store_id, to_store_id
             FROM lcm_raw_predecessor_ranges
             WHERE provider = 'claude' AND message_id = 'compact-summary'",
            (),
        )
        .await
        .expect("read predecessor range");
    let row = rows
        .next()
        .await
        .expect("advance predecessor range")
        .expect("compact-summary must keep a conversational predecessor interval");
    assert_eq!(row.get::<i64>(0).expect("from"), 1);
    assert_eq!(row.get::<i64>(1).expect("to"), 1);
}

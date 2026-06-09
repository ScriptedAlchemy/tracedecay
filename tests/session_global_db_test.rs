use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::LcmStorageKind;
use tokensave::sessions::{SessionMessageRecord, SessionRecord, SessionSearchScope};

fn isolated_db_path(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join(".tokensave").join("global.db")
}

async fn open_isolated_db(tmp: &TempDir) -> GlobalDb {
    let db_path = isolated_db_path(tmp);
    GlobalDb::open_at(&db_path).await.expect("global db open")
}

async fn raw_message_count(db_path: &std::path::Path, provider: &str, message_id: &str) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_raw_messages
             WHERE provider = ?1 AND message_id = ?2",
            libsql::params![provider, message_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
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

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

fn sample_session(provider: &str, session_id: &str, project_key: &str) -> SessionRecord {
    SessionRecord {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        project_key: project_key.to_string(),
        project_path: "/tmp/project".to_string(),
        title: Some("Initial title".to_string()),
        started_at: Some(1_715_000_000),
        ended_at: None,
        transcript_path: Some("/tmp/project/transcript.jsonl".to_string()),
        metadata_json: Some(r#"{"source":"test"}"#.to_string()),
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

fn sample_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    text: &str,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: provider.to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: "assistant".to_string(),
        timestamp: Some(1_715_000_030),
        ordinal: 1,
        text: text.to_string(),
        kind: Some("message".to_string()),
        model: Some("test-model".to_string()),
        tool_names: Some("tokensave_context,tokensave_search".to_string()),
        source_path: Some("/tmp/project/transcript.jsonl".to_string()),
        source_offset: Some(42),
        metadata_json: Some(r#"{"finish_reason":"stop"}"#.to_string()),
    }
}

#[tokio::test]
async fn global_db_opens_with_session_schema() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;

    assert!(db.get_session("cursor", "missing").await.is_none());
    assert!(db
        .search_session_messages("cursor", None, "not-present", 10)
        .await
        .is_empty());
}

#[tokio::test]
async fn upsert_session_round_trips_and_updates() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let mut session = sample_session("cursor", "session-1", "project-a");

    db.upsert_session(&session).await;
    session.title = Some("Updated title".to_string());
    session.ended_at = Some(1_715_000_900);
    session.metadata_json = Some(r#"{"source":"test","updated":true}"#.to_string());
    db.upsert_session(&session).await;

    let fetched = db
        .get_session("cursor", "session-1")
        .await
        .expect("session should exist");
    assert_eq!(fetched.project_key, "project-a");
    assert_eq!(fetched.title.as_deref(), Some("Updated title"));
    assert_eq!(fetched.ended_at, Some(1_715_000_900));
    assert_eq!(
        fetched.metadata_json.as_deref(),
        Some(r#"{"source":"test","updated":true}"#)
    );
}

#[tokio::test]
async fn upsert_session_message_round_trips_and_updates() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    db.upsert_session(&session).await;

    let mut message = sample_message(
        "cursor",
        "message-1",
        "session-1",
        "Initial answer about parsing transcripts.",
    );
    assert!(db.upsert_session_message(&message).await);
    let updated = format!(
        "Updated answer about parsing transcripts.\n{}::updated-tail",
        "x".repeat(tokensave::sessions::lcm::MAX_DERIVED_TEXT_CHARS * 2)
    );
    message.text = updated.clone();
    message.tool_names = Some("tokensave_context".to_string());
    message.source_offset = Some(99);
    assert!(db.upsert_session_message(&message).await);

    let fetched = db
        .get_session_message("cursor", "message-1")
        .await
        .expect("message should exist");
    assert_eq!(fetched.session_id, "session-1");
    assert!(fetched
        .text
        .starts_with("Updated answer about parsing transcripts."));
    assert!(fetched.text.chars().count() <= tokensave::sessions::lcm::MAX_DERIVED_TEXT_CHARS);
    assert!(fetched
        .text
        .contains(tokensave::sessions::lcm::DERIVED_TRUNCATION_MARKER));
    assert_eq!(fetched.tool_names.as_deref(), Some("tokensave_context"));
    assert_eq!(fetched.source_offset, Some(99));

    assert_eq!(raw_message_count(&db_path, "cursor", "message-1").await, 1);
    let raw = db
        .lcm_load_raw_message("cursor", "message-1")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.content, updated);
    assert_eq!(raw.content_hash, sha256_hex(&updated));

    let (snippet_text, index_text) = raw_snippet_and_index(&db_path, "cursor", "message-1").await;
    assert!(snippet_text.chars().count() <= tokensave::sessions::lcm::MAX_DERIVED_SNIPPET_CHARS);
    assert!(snippet_text.contains(tokensave::sessions::lcm::DERIVED_TRUNCATION_MARKER));
    assert!(index_text.chars().count() <= tokensave::sessions::lcm::MAX_DERIVED_TEXT_CHARS);
    assert!(index_text.contains(tokensave::sessions::lcm::DERIVED_TRUNCATION_MARKER));
}

#[tokio::test]
async fn upsert_session_message_rejects_missing_session_without_orphan_raw() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_isolated_db(&tmp).await;
    let message = sample_message("cursor", "orphan-message", "missing-session", "orphan text");

    assert!(!db.upsert_session_message(&message).await);
    assert!(db
        .get_session_message("cursor", "orphan-message")
        .await
        .is_none());
    assert!(db
        .lcm_load_raw_message("cursor", "orphan-message")
        .await
        .is_none());
    assert_eq!(
        raw_message_count(&db_path, "cursor", "orphan-message").await,
        0
    );
}

#[tokio::test]
async fn upsert_session_message_rolls_back_raw_when_projection_fails() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    assert!(db.upsert_session(&session).await);

    let trigger_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let trigger_conn = trigger_db.connect().unwrap();
    trigger_conn
        .execute_batch(
            "CREATE TRIGGER fail_session_message_projection
             BEFORE INSERT ON session_messages
             BEGIN
                SELECT RAISE(ABORT, 'projection failure');
             END;",
        )
        .await
        .unwrap();

    let message = sample_message(
        "cursor",
        "message-rollback",
        "session-1",
        "raw before failure",
    );
    assert!(!db.upsert_session_message(&message).await);
    assert_eq!(
        raw_message_count(&db_path, "cursor", "message-rollback").await,
        0
    );
    assert!(db
        .lcm_load_raw_message("cursor", "message-rollback")
        .await
        .is_none());
}

#[tokio::test]
async fn upsert_session_message_preserves_oversized_text_losslessly() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    db.upsert_session(&session).await;

    let oversized = format!("{}{}", "x".repeat(300_000), "::lossless-tail");
    let message = sample_message("cursor", "message-1", "session-1", &oversized);
    assert!(db.upsert_session_message(&message).await);

    let compatibility = db
        .get_session_message("cursor", "message-1")
        .await
        .expect("compatibility message should exist");
    assert!(compatibility.text.chars().count() <= tokensave::sessions::lcm::MAX_DERIVED_TEXT_CHARS);
    assert!(compatibility
        .text
        .contains(tokensave::sessions::lcm::DERIVED_TRUNCATION_MARKER));

    let raw = db
        .lcm_load_raw_message("cursor", "message-1")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.content, oversized);
    assert!(raw.content.ends_with("::lossless-tail"));
    assert!(!raw.legacy_source);
    assert!(!raw.legacy_truncated);
}

#[tokio::test]
async fn upsert_session_message_externalizes_tool_payload_without_indexing_body_or_metadata() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let storage_root = tmp.path().join(".tokensave");
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    assert!(db.upsert_session(&session).await);

    let body_secret = "globaldbbodysecretnotindexed";
    let metadata_secret = "globaldbmetadatasecretnotindexed";
    let payload = format!("tool output {body_secret}\n{}", "T".repeat(900_000));
    let mut message = sample_message("cursor", "tool-large", "session-1", &payload);
    message.role = "tool".to_string();
    message.kind = Some("tool_result".to_string());
    message.metadata_json = Some(format!(r#"{{"preview":"{metadata_secret}"}}"#));
    assert!(db.upsert_session_message(&message).await);

    let raw = db
        .lcm_load_raw_message("cursor", "tool-large")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    assert!(raw.content.is_empty());
    assert!(!raw.content.contains(body_secret));
    assert!(!raw
        .metadata_json
        .as_deref()
        .unwrap_or("")
        .contains(metadata_secret));
    let payload_ref = raw.payload_ref.as_deref().expect("payload ref");

    let fetched = db
        .get_session_message("cursor", "tool-large")
        .await
        .expect("projection should exist");
    assert!(fetched.text.chars().count() <= tokensave::sessions::lcm::MAX_DERIVED_TEXT_CHARS);
    assert!(!fetched.text.contains(body_secret));
    assert!(fetched.text.contains("[externalized payload: tool_result"));
    let projection_metadata = fetched.metadata_json.as_deref().unwrap_or("");
    assert!(!projection_metadata.contains(metadata_secret));
    assert!(projection_metadata.contains("\"external_payload\":true"));
    assert!(projection_metadata.contains(payload_ref));

    let (snippet_text, index_text) = raw_snippet_and_index(&db_path, "cursor", "tool-large").await;
    assert!(!snippet_text.contains(body_secret));
    assert!(!index_text.contains(body_secret));
    assert!(!snippet_text.contains(metadata_secret));
    assert!(!index_text.contains(metadata_secret));
    assert_eq!(lcm_fts_count(&db_path, body_secret).await, 0);
    assert_eq!(lcm_fts_count(&db_path, metadata_secret).await, 0);
    assert!(db
        .search_session_messages("cursor", Some("project-a"), body_secret, 10)
        .await
        .is_empty());

    let expanded = db
        .lcm_store(&storage_root)
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
async fn search_session_messages_uses_fts_and_filters_provider_project() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let cursor_a = sample_session("cursor", "cursor-a", "project-a");
    let cursor_b = sample_session("cursor", "cursor-b", "project-b");
    let codex_a = sample_session("codex", "codex-a", "project-a");
    db.upsert_session(&cursor_a).await;
    db.upsert_session(&cursor_b).await;
    db.upsert_session(&codex_a).await;

    db.upsert_session_message(&sample_message(
        "cursor",
        "cursor-msg-a",
        "cursor-a",
        "The billing ingestion plan is ready.",
    ))
    .await;
    db.upsert_session_message(&sample_message(
        "cursor",
        "cursor-msg-b",
        "cursor-b",
        "The billing ingestion plan belongs to another project.",
    ))
    .await;
    db.upsert_session_message(&sample_message(
        "codex",
        "codex-msg-a",
        "codex-a",
        "The billing ingestion plan belongs to another provider.",
    ))
    .await;

    let results = db
        .search_session_messages("cursor", Some("project-a"), "billing", 10)
        .await;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].message.message_id, "cursor-msg-a");
    assert_eq!(results[0].session.project_key, "project-a");
    assert!(results[0].score > 0.0);
}

#[tokio::test]
async fn open_at_upgrades_existing_sessions_table_with_parent_columns() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tokensave").join("global.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let old_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = old_db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
            title TEXT,
            started_at INTEGER,
            ended_at INTEGER,
            transcript_path TEXT,
            metadata_json TEXT,
            PRIMARY KEY(provider, session_id)
        );
        INSERT INTO sessions (
            provider, session_id, project_key, project_path, title, started_at,
            ended_at, transcript_path, metadata_json
        ) VALUES (
            'cursor', 'old-parent', 'project-a', '/tmp/project', 'Old title',
            1715000000, NULL, '/tmp/project/old.jsonl', '{\"source\":\"old\"}'
        );",
    )
    .await
    .unwrap();
    drop(conn);
    drop(old_db);

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    let session = db
        .get_session("cursor", "old-parent")
        .await
        .expect("old row should survive schema upgrade");

    assert_eq!(session.parent_session_id, None);
    assert!(!session.is_subagent);
    assert_eq!(session.agent_id, None);
    assert_eq!(session.parent_tool_use_id, None);

    let child = SessionRecord {
        session_id: "child-agent".to_string(),
        parent_session_id: Some("old-parent".to_string()),
        is_subagent: true,
        agent_id: Some("child-agent".to_string()),
        ..sample_session("cursor", "child-agent", "project-a")
    };
    assert!(db.upsert_session(&child).await);

    let fetched = db
        .get_session("cursor", "child-agent")
        .await
        .expect("child row should round-trip");
    assert_eq!(fetched.parent_session_id.as_deref(), Some("old-parent"));
    assert!(fetched.is_subagent);
    assert_eq!(fetched.agent_id.as_deref(), Some("child-agent"));
}

#[tokio::test]
async fn search_session_messages_filters_parent_and_subagent_scope() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let parent = sample_session("cursor", "parent", "project-a");
    let child = SessionRecord {
        session_id: "agent-worker".to_string(),
        parent_session_id: Some("parent".to_string()),
        is_subagent: true,
        agent_id: Some("worker".to_string()),
        ..sample_session("cursor", "agent-worker", "project-a")
    };
    db.upsert_session(&parent).await;
    db.upsert_session(&child).await;
    db.upsert_session_message(&sample_message(
        "cursor",
        "parent-msg",
        "parent",
        "The orchard dispatch plan is ready.",
    ))
    .await;
    db.upsert_session_message(&sample_message(
        "cursor",
        "child-msg",
        "agent-worker",
        "The orchard dispatch result came from the worker.",
    ))
    .await;

    let all = db
        .search_session_messages("cursor", Some("project-a"), "orchard dispatch", 10)
        .await;
    assert_eq!(all.len(), 2);

    let parents_only = db
        .search_session_messages_filtered(
            "cursor",
            Some("project-a"),
            "orchard dispatch",
            10,
            SessionSearchScope::ParentsOnly,
            None,
        )
        .await;
    assert_eq!(parents_only.len(), 1);
    assert_eq!(parents_only[0].session.session_id, "parent");

    let subagents_only = db
        .search_session_messages_filtered(
            "cursor",
            Some("project-a"),
            "orchard dispatch",
            10,
            SessionSearchScope::SubagentsOnly,
            Some("parent"),
        )
        .await;
    assert_eq!(subagents_only.len(), 1);
    assert_eq!(subagents_only[0].session.session_id, "agent-worker");
    assert_eq!(
        subagents_only[0].session.parent_session_id.as_deref(),
        Some("parent")
    );
}

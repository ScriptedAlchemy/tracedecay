use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::{SessionMessageRecord, SessionRecord};

async fn open_isolated_db(tmp: &TempDir) -> GlobalDb {
    let db_path = tmp.path().join(".tokensave").join("global.db");
    GlobalDb::open_at(&db_path).await.expect("global db open")
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
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    db.upsert_session(&session).await;

    let mut message = sample_message(
        "cursor",
        "message-1",
        "session-1",
        "Initial answer about parsing transcripts.",
    );
    db.upsert_session_message(&message).await;
    message.text = "Updated answer about parsing transcripts.".to_string();
    message.tool_names = Some("tokensave_context".to_string());
    message.source_offset = Some(99);
    db.upsert_session_message(&message).await;

    let fetched = db
        .get_session_message("cursor", "message-1")
        .await
        .expect("message should exist");
    assert_eq!(fetched.session_id, "session-1");
    assert_eq!(fetched.text, "Updated answer about parsing transcripts.");
    assert_eq!(fetched.tool_names.as_deref(), Some("tokensave_context"));
    assert_eq!(fetched.source_offset, Some(99));
}

#[tokio::test]
async fn upsert_session_message_truncates_oversized_text_deterministically() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    db.upsert_session(&session).await;

    let oversized = "x".repeat(300_000);
    let message = sample_message("cursor", "message-1", "session-1", &oversized);
    assert!(db.upsert_session_message(&message).await);

    let fetched = db
        .get_session_message("cursor", "message-1")
        .await
        .expect("message should exist");
    assert!(fetched.text.len() < oversized.len());
    assert!(
        fetched.text.ends_with("[truncated by tokensave]"),
        "truncated message should be explicitly marked"
    );
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

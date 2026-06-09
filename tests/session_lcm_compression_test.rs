use serde_json::json;
use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::{
    LcmCompressionRequest, LcmLifecycleUpdate, LcmLoadSessionRequest, LcmMaintenanceDebt,
    LcmPreflightRequest, LcmSummarizerMode,
};
use tokensave::sessions::{SessionMessageRecord, SessionRecord};

fn isolated_db_path(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join(".tokensave").join("sessions.db")
}

async fn open_lcm_db(tmp: &TempDir) -> GlobalDb {
    GlobalDb::open_at(&isolated_db_path(tmp))
        .await
        .expect("session db open")
}

fn sample_session(provider: &str, session_id: &str) -> SessionRecord {
    SessionRecord {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        project_key: "/tmp/project".to_string(),
        project_path: "/tmp/project".to_string(),
        title: Some("LCM compression test".to_string()),
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
    ordinal: i64,
    text: &str,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: provider.to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: "assistant".to_string(),
        timestamp: Some(1_715_000_000 + ordinal),
        ordinal,
        text: text.to_string(),
        kind: Some("message".to_string()),
        model: Some("test-model".to_string()),
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: None,
    }
}

async fn insert_session(db: &GlobalDb, provider: &str, session_id: &str) {
    assert!(
        db.upsert_session(&sample_session(provider, session_id))
            .await
    );
}

async fn insert_raw_messages(
    db: &GlobalDb,
    provider: &str,
    session_id: &str,
    contents: &[&str],
) -> Vec<i64> {
    insert_session(db, provider, session_id).await;
    let mut store_ids = Vec::new();
    for (idx, content) in contents.iter().enumerate() {
        let message_slug = content.replace(|ch: char| !ch.is_ascii_alphanumeric(), "-");
        let message_id = format!("{session_id}-message-{}-{message_slug}", idx + 1);
        let message = raw_message(provider, &message_id, session_id, (idx + 1) as i64, content);
        assert!(db.upsert_session_message(&message).await);
        let raw = db
            .lcm_load_raw_message(provider, &message_id)
            .await
            .expect("raw message should exist");
        store_ids.push(raw.store_id);
    }
    store_ids
}

fn compress_request(
    provider: &str,
    session_id: &str,
    summarizer: LcmSummarizerMode,
) -> LcmCompressionRequest {
    LcmCompressionRequest {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        messages: Vec::new(),
        current_tokens: Some(1_000),
        focus_topic: None,
        summarizer,
    }
}

#[tokio::test]
async fn lifecycle_frontier_survives_reopen() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = GlobalDb::open_at(&db_path).await.unwrap();

    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "conversation-1".into(),
        current_session_id: "session-1".into(),
        current_frontier_store_id: Some(42),
        last_finalized_session_id: Some("session-0".into()),
        last_finalized_frontier_store_id: Some(40),
        maintenance_debt: vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: 41,
            to_store_id: 42,
        }],
    })
    .await
    .unwrap();
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.unwrap();
    let state = reopened
        .lcm_lifecycle_state("cursor", "conversation-1")
        .await
        .unwrap();
    assert_eq!(state.provider, "cursor");
    assert_eq!(state.conversation_id, "conversation-1");
    assert_eq!(state.current_session_id, "session-1");
    assert_eq!(state.current_frontier_store_id, Some(42));
    assert_eq!(
        state.last_finalized_session_id.as_deref(),
        Some("session-0")
    );
    assert_eq!(state.last_finalized_frontier_store_id, Some(40));
    assert_eq!(
        state.maintenance_debt,
        vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: 41,
            to_store_id: 42,
        }]
    );
}

#[tokio::test]
async fn noop_summarizer_ingests_without_summary_nodes() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;

    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            messages: vec![json!({
                "id": "active-1",
                "role": "user",
                "content": "fresh active message"
            })],
            current_tokens: Some(100),
            focus_topic: None,
            summarizer: LcmSummarizerMode::Noop,
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 0);
    assert_eq!(response.replay_messages.len(), 1);
    assert_eq!(
        response.replay_messages[0]["content"],
        "fresh active message"
    );

    let page = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            after_store_id: None,
            limit: 10,
            role: None,
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 1);

    let status = db.lcm_status("cursor", Some("session-1")).await.unwrap();
    assert_eq!(status.summary_node_count, 0);
}

#[tokio::test]
async fn preflight_can_request_compression_when_ingest_protection_changes_replay() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;

    let response = db
        .lcm_preflight(LcmPreflightRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            messages: vec![json!({
                "id": "protected-1",
                "role": "assistant",
                "content": format!("data:image/png;base64,{}", "A".repeat(100_000))
            })],
            current_tokens: Some(100),
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert!(response.should_compress);
    assert_eq!(response.reason, "ingest_protection_changed_replay");
    assert!(response.replay_messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("[externalized payload"));
}

#[tokio::test]
async fn fake_summarizer_compacts_backlog_and_preserves_fresh_tail() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let response = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "old summary".into(),
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(response.replay_messages.len(), 3);
    assert_eq!(response.replay_messages[0]["role"], "system");
    assert_eq!(response.replay_messages[0]["content"], "old summary");
    assert_eq!(response.replay_messages[1]["content"], "fresh-1");
    assert_eq!(response.replay_messages[2]["content"], "fresh-2");
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[1])
    );

    let summary_node_id = response.summary_nodes[0].node_id.clone();
    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &summary_node_id)
        .await
        .unwrap();
    assert_eq!(expanded.sources.len(), 2);
    assert_eq!(expanded.sources[0].content, "old-1");
    assert_eq!(expanded.sources[1].content, "old-2");
}

#[tokio::test]
async fn provided_summarizer_advances_frontier_consistently() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let first_store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["one", "two", "three", "four", "five"],
    )
    .await;

    let first = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Provided {
                summary_text: "one two three".into(),
                route: Some("test-route".into()),
            },
        ))
        .await
        .unwrap();
    assert_eq!(
        first.frontier.current_frontier_store_id,
        Some(first_store_ids[2])
    );

    let next_store_ids = insert_raw_messages(&db, "cursor", "session-1", &["six", "seven"]).await;
    let second = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Provided {
                summary_text: "four five".into(),
                route: Some("test-route".into()),
            },
        ))
        .await
        .unwrap();

    assert_eq!(second.summary_nodes_created, 1);
    assert_eq!(
        second.frontier.current_frontier_store_id,
        Some(next_store_ids[0].saturating_sub(1))
    );
    let state = db.lcm_lifecycle_state("cursor", "session-1").await.unwrap();
    assert_eq!(
        state.current_frontier_store_id,
        second.frontier.current_frontier_store_id
    );
}

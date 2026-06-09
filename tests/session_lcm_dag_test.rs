use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::{LcmError, LcmSourceRef, LcmStorageKind, LcmSummaryNodeDraft};
use tokensave::sessions::{SessionMessageRecord, SessionRecord};

fn isolated_db_path(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join(".tokensave").join("sessions.db")
}

async fn open_lcm_db(tmp: &TempDir) -> GlobalDb {
    GlobalDb::open_at(&isolated_db_path(tmp))
        .await
        .expect("session db open")
}

async fn summary_table_counts(db_path: &std::path::Path) -> (i64, i64) {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut node_rows = conn
        .query("SELECT COUNT(*) FROM lcm_summary_nodes", ())
        .await
        .unwrap();
    let node_count = node_rows.next().await.unwrap().unwrap().get(0).unwrap();
    let mut source_rows = conn
        .query("SELECT COUNT(*) FROM lcm_summary_sources", ())
        .await
        .unwrap();
    let source_count = source_rows.next().await.unwrap().unwrap().get(0).unwrap();
    (node_count, source_count)
}

async fn summary_fts_count(db_path: &std::path::Path, query: &str) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_summary_nodes_fts
             WHERE lcm_summary_nodes_fts MATCH ?1",
            libsql::params![query],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn sample_session(provider: &str, session_id: &str) -> SessionRecord {
    SessionRecord {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        project_key: "/tmp/project".to_string(),
        project_path: "/tmp/project".to_string(),
        title: Some("LCM DAG test".to_string()),
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
        let message_id = format!("{session_id}-message-{}", idx + 1);
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

async fn insert_external_raw_message(
    db: &GlobalDb,
    tmp: &TempDir,
    provider: &str,
    session_id: &str,
    message_id: &str,
) -> (i64, String) {
    insert_session(db, provider, session_id).await;
    let payload = format!("tool output\n{}", "X".repeat(300_000));
    let mut message = raw_message(provider, message_id, session_id, 1, &payload);
    message.role = "tool".to_string();
    message.kind = Some("tool_result".to_string());

    let storage_root = tmp.path().join(".tokensave");
    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("raw ingest should externalize payload");
    let raw = db
        .lcm_load_raw_message(provider, message_id)
        .await
        .expect("external raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    let payload_ref = raw.payload_ref.clone().expect("payload ref");
    (raw.store_id, payload_ref)
}

fn summary_draft(
    provider: &str,
    session_id: &str,
    depth: i64,
    summary_text: &str,
    source_refs: Vec<LcmSourceRef>,
) -> LcmSummaryNodeDraft {
    LcmSummaryNodeDraft {
        provider: provider.to_string(),
        conversation_id: "conversation-1".to_string(),
        session_id: session_id.to_string(),
        depth,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count: 30,
        summary_token_count: 4,
        source_time_start: Some(1_715_000_000),
        source_time_end: Some(1_715_000_030),
        expand_hint: Some("expand source lineage".to_string()),
        metadata_json: Some(r#"{"topic":"dag"}"#.to_string()),
    }
}

#[tokio::test]
async fn summary_node_preserves_source_lineage_and_expands_sources() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids =
        insert_raw_messages(&db, "cursor", "session-1", &["alpha", "beta", "gamma"]).await;

    let node = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "alpha through gamma",
            store_ids
                .iter()
                .copied()
                .map(|store_id| LcmSourceRef::RawMessage { store_id })
                .collect(),
        ))
        .await
        .expect("summary node insert should succeed");

    assert!(node.node_id.starts_with("sum_"));
    assert_eq!(node.summary_text, "alpha through gamma");
    assert_eq!(node.source_refs.len(), 3);
    assert_eq!(node.summary_token_count, 4);
    assert_eq!(node.source_token_count, 30);
    assert_eq!(node.source_time_start, Some(1_715_000_000));
    assert_eq!(node.source_time_end, Some(1_715_000_030));
    assert_eq!(node.expand_hint.as_deref(), Some("expand source lineage"));
    assert_eq!(node.metadata_json.as_deref(), Some(r#"{"topic":"dag"}"#));

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &node.node_id)
        .await
        .expect("summary node should expand");
    assert_eq!(expanded.summary, node);
    assert_eq!(expanded.sources.len(), 3);
    assert_eq!(
        expanded.sources[0].source_ref,
        LcmSourceRef::RawMessage {
            store_id: store_ids[0]
        }
    );
    assert_eq!(expanded.sources[0].content, "alpha");
    assert_eq!(
        expanded.sources[0].raw_message.as_ref().unwrap().message_id,
        "session-1-message-1"
    );
    assert_eq!(expanded.sources[1].content, "beta");
    assert_eq!(expanded.sources[2].content, "gamma");
}

#[tokio::test]
async fn summary_dag_survives_reopen() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = GlobalDb::open_at(&db_path).await.expect("session db open");
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha", "beta"]).await;
    let node = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "alpha and beta",
            store_ids
                .iter()
                .copied()
                .map(|store_id| LcmSourceRef::RawMessage { store_id })
                .collect(),
        ))
        .await
        .expect("summary node insert should succeed");
    drop(db);

    let reopened = GlobalDb::open_at(&db_path)
        .await
        .expect("session db reopen");
    let expanded = reopened
        .lcm_expand_summary_node("cursor", "session-1", &node.node_id)
        .await
        .expect("summary node should expand after reopen");

    assert_eq!(expanded.summary.node_id, node.node_id);
    assert_eq!(expanded.summary.summary_text, "alpha and beta");
    assert_eq!(expanded.sources.len(), 2);
    assert_eq!(expanded.sources[0].content, "alpha");
    assert_eq!(expanded.sources[1].content, "beta");
}

#[tokio::test]
async fn summary_insert_rejects_missing_raw_source_without_persisting_rows() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;

    let result = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "bad missing raw source",
            vec![LcmSourceRef::RawMessage { store_id: 404 }],
        ))
        .await;

    assert!(matches!(
        result,
        Err(LcmError::SummarySourceNotOwnedBySession)
    ));
    assert_eq!(summary_table_counts(&db_path).await, (0, 0));
}

#[tokio::test]
async fn summary_insert_validates_source_session_ownership_without_persisting_rows() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_one = insert_raw_messages(&db, "cursor", "session-1", &["owned"]).await;
    let session_two = insert_raw_messages(&db, "cursor", "session-2", &["other"]).await;

    let cross_raw = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "bad raw source",
            vec![LcmSourceRef::RawMessage {
                store_id: session_two[0],
            }],
        ))
        .await
        .expect_err("cross-session raw source should be rejected at insert");
    assert!(matches!(
        cross_raw,
        LcmError::SummarySourceNotOwnedBySession
    ));
    assert_eq!(summary_table_counts(&db_path).await, (0, 0));

    let other_child = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-2",
            0,
            "child summary",
            vec![LcmSourceRef::RawMessage {
                store_id: session_two[0],
            }],
        ))
        .await
        .expect("child summary insert should succeed");
    let before_cross_child = summary_table_counts(&db_path).await;
    let cross_child = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            1,
            "bad child summary source",
            vec![
                LcmSourceRef::RawMessage {
                    store_id: session_one[0],
                },
                LcmSourceRef::SummaryNode {
                    node_id: other_child.node_id,
                },
            ],
        ))
        .await
        .expect_err("cross-session child summary source should be rejected at insert");
    assert!(matches!(
        cross_child,
        LcmError::SummarySourceNotOwnedBySession
    ));
    assert_eq!(summary_table_counts(&db_path).await, before_cross_child);
}

#[tokio::test]
async fn summary_expansion_marks_external_raw_sources_without_silent_empty_content() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let (store_id, payload_ref) =
        insert_external_raw_message(&db, &tmp, "cursor", "session-1", "tool-1").await;

    let node = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "summary over externalized tool payload",
            vec![LcmSourceRef::RawMessage { store_id }],
        ))
        .await
        .expect("summary node insert should succeed");

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &node.node_id)
        .await
        .expect("summary node should expand");
    assert_eq!(expanded.sources.len(), 1);
    let source = &expanded.sources[0];
    assert!(!source.content.is_empty());
    assert!(source
        .content
        .contains("[externalized payload: tool_result"));
    assert!(source.content.contains(&payload_ref));
    let raw = source.raw_message.as_ref().expect("raw message source");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    assert_eq!(raw.payload_ref.as_deref(), Some(payload_ref.as_str()));
    assert_eq!(raw.content, source.content);
}

#[tokio::test]
async fn nested_summary_expansion_is_direct_only() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha"]).await;
    let child = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "child summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("child summary insert should succeed");
    let parent = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            1,
            "parent summary",
            vec![LcmSourceRef::SummaryNode {
                node_id: child.node_id.clone(),
            }],
        ))
        .await
        .expect("parent summary insert should succeed");

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &parent.node_id)
        .await
        .expect("parent summary should expand");
    assert_eq!(expanded.sources.len(), 1);
    assert_eq!(expanded.sources[0].content, child.summary_text);
    assert!(expanded.sources[0].raw_message.is_none());
    let expanded_child = expanded.sources[0]
        .summary_node
        .as_ref()
        .expect("direct child summary source");
    assert_eq!(expanded_child.node_id, child.node_id);
    assert_eq!(
        expanded_child.source_refs,
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0]
        }]
    );
}

#[tokio::test]
async fn summary_fts_matches_inserted_summary_text() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha"]).await;
    db.lcm_insert_summary_node(summary_draft(
        "cursor",
        "session-1",
        0,
        "unique summary fts phrase",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    ))
    .await
    .expect("summary node insert should succeed");

    assert_eq!(summary_fts_count(&db_path, "\"unique summary\"").await, 1);
}

#[tokio::test]
async fn summary_node_ids_are_stable_for_identical_drafts() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha"]).await;
    let draft = summary_draft(
        "cursor",
        "session-1",
        0,
        "stable summary",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    );

    let first = db
        .lcm_insert_summary_node(draft.clone())
        .await
        .expect("first summary insert should succeed");
    let second = db
        .lcm_insert_summary_node(draft)
        .await
        .expect("second summary insert should succeed");

    assert_eq!(first.node_id, second.node_id);
}

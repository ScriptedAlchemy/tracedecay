use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::{
    LcmContentSlice, LcmError, LcmExpandRequest, LcmExpandTarget, LcmGrepRequest,
    LcmLifecycleUpdate, LcmLoadSessionRequest, LcmMaintenanceDebt, LcmScope, LcmSourceRef,
    LcmStorageKind, LcmSummaryNodeDraft, LCM_SCHEMA_VERSION, MAX_DERIVED_SNIPPET_CHARS,
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
        title: Some("LCM query test".to_string()),
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
    contents: &[String],
) -> Vec<i64> {
    insert_session(db, provider, session_id).await;
    let mut store_ids = Vec::new();
    for (idx, content) in contents.iter().enumerate() {
        let message_id = format!("{session_id}-message-{:03}", idx + 1);
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

fn summary_draft(
    provider: &str,
    session_id: &str,
    summary_text: &str,
    source_refs: Vec<LcmSourceRef>,
) -> LcmSummaryNodeDraft {
    LcmSummaryNodeDraft {
        provider: provider.to_string(),
        conversation_id: "conversation-1".to_string(),
        session_id: session_id.to_string(),
        depth: 0,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count: 30,
        summary_token_count: 5,
        source_time_start: Some(1_715_000_000),
        source_time_end: Some(1_715_000_030),
        expand_hint: Some("query test summary".to_string()),
        metadata_json: None,
    }
}

#[test]
fn lcm_modules_do_not_depend_on_context_builder_or_memory_fact_store() {
    for path in [
        "src/sessions/lcm/raw.rs",
        "src/sessions/lcm/dag.rs",
        "src/sessions/lcm/query.rs",
        "src/sessions/lcm/compression.rs",
    ] {
        let source = std::fs::read_to_string(path).unwrap();
        assert!(
            !source.contains("ContextBuilder"),
            "{path} must stay independent from codegraph context assembly"
        );
        assert!(
            !source.contains("MemoryCategory"),
            "{path} must stay independent from memory fact categories"
        );
        assert!(
            !source.contains("memory_facts"),
            "{path} must not store LCM summaries in fact memory tables"
        );
    }
}

#[tokio::test]
async fn load_session_returns_ordered_raw_pages_with_stable_cursor() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let contents = (1..=105)
        .map(|idx| format!("message-{idx:03}"))
        .collect::<Vec<_>>();
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &contents).await;

    let first = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            after_store_id: None,
            limit: 500,
            role: None,
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .expect("first page should load");
    assert_eq!(first.messages.len(), 100);
    assert_eq!(first.messages[0].content, "message-001");
    assert_eq!(first.messages[99].content, "message-100");
    assert_eq!(
        first.next_cursor.as_deref(),
        Some(store_ids[99].to_string().as_str())
    );

    let second = db
        .lcm_load_session(LcmLoadSessionRequest {
            after_store_id: Some(store_ids[99]),
            limit: 2,
            ..first.request_for_next()
        })
        .await
        .expect("second page should load");
    assert_eq!(
        second
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["message-101", "message-102"]
    );
    assert_eq!(
        second.next_cursor.as_deref(),
        Some(store_ids[101].to_string().as_str())
    );

    let min_clamped = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            after_store_id: None,
            limit: 0,
            role: None,
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .expect("minimum-clamped page should load");
    assert_eq!(min_clamped.messages.len(), 1);
    assert_eq!(
        min_clamped.next_cursor.as_deref(),
        Some(store_ids[0].to_string().as_str())
    );
}

#[tokio::test]
async fn grep_searches_raw_snippets_and_summary_nodes() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tokensave");
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "raw billing migration details".to_string(),
            "ordinary follow-up".to_string(),
        ],
    )
    .await;

    let external_secret = format!("billing migration secret body {}", "S".repeat(300_000));
    let mut external = raw_message("cursor", "tool-secret", "session-1", 3, &external_secret);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_store(&storage_root)
        .ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");

    db.lcm_insert_summary_node(summary_draft(
        "cursor",
        "session-1",
        "summary for billing migration decisions",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    ))
    .await
    .expect("summary should insert");

    let hits = db
        .lcm_grep(LcmGrepRequest {
            provider: "cursor".into(),
            query: "billing migration".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
            include_summaries: true,
            limit: 10,
        })
        .await
        .expect("grep should succeed");

    assert!(hits.iter().any(|hit| hit.kind == "raw_message"));
    assert!(hits.iter().any(|hit| hit.kind == "summary_node"));
    assert!(hits
        .iter()
        .all(|hit| hit.snippet.chars().count() <= MAX_DERIVED_SNIPPET_CHARS));
    assert!(!hits.iter().any(|hit| hit.snippet.contains("secret body")));
}

#[tokio::test]
async fn grep_tokenizes_punctuation_heavy_path_like_queries() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "The regression lives in src/foo.rs and needs a tokenizer-style query.".to_string(),
            "Another message mentions src and foo but not the extension token.".to_string(),
        ],
    )
    .await;

    let hits = db
        .lcm_grep(LcmGrepRequest {
            provider: "cursor".into(),
            query: "src/foo.rs".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
            include_summaries: false,
            limit: 10,
        })
        .await
        .expect("path-like grep should not miss because punctuation was collapsed");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].store_id, Some(store_ids[0]));
    assert!(hits[0].snippet.contains("src/foo.rs"));
}

#[tokio::test]
async fn grep_quotes_reserved_operator_looking_query_text() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "A literal OR token appears in this transcript.".to_string(),
            "This message deliberately omits the operator word.".to_string(),
        ],
    )
    .await;

    let hits = db
        .lcm_grep(LcmGrepRequest {
            provider: "cursor".into(),
            query: "OR".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
            include_summaries: false,
            limit: 10,
        })
        .await
        .expect("reserved FTS operator text should be treated as literal text");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].store_id, Some(store_ids[0]));
    assert!(hits[0].snippet.contains("OR"));
}

#[tokio::test]
async fn status_reports_schema_frontier_payload_and_debt_counts() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tokensave");
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["alpha".to_string(), "beta".to_string()],
    )
    .await;

    let payload = format!("private payload marker\n{}", "P".repeat(300_000));
    let mut external = raw_message("cursor", "tool-payload", "session-1", 3, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_store(&storage_root)
        .ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");

    db.lcm_insert_summary_node(summary_draft(
        "cursor",
        "session-1",
        "alpha beta summary",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    ))
    .await
    .expect("summary should insert");
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "session-1".into(),
        current_session_id: "session-1".into(),
        current_frontier_store_id: Some(store_ids[1]),
        last_finalized_session_id: Some("session-0".into()),
        last_finalized_frontier_store_id: Some(store_ids[0]),
        maintenance_debt: vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: store_ids[0],
            to_store_id: store_ids[1],
        }],
    })
    .await
    .expect("lifecycle state should update");

    let status = db
        .lcm_status("cursor", Some("session-1"))
        .await
        .expect("status should load");
    assert_eq!(status.schema_version, LCM_SCHEMA_VERSION);
    assert_eq!(status.raw_message_count, 3);
    assert_eq!(status.summary_node_count, 1);
    assert_eq!(status.external_payload_count, 1);
    assert_eq!(status.missing_payload_count, 0);
    assert_eq!(status.maintenance_debt_count, 1);
    assert_eq!(status.lifecycle.lifecycle_state_count, 1);
    assert_eq!(status.lifecycle.frontier_count, 1);
    assert_eq!(status.lifecycle.maintenance_debt_count, 1);
    assert_eq!(
        status.lifecycle.current_session_id.as_deref(),
        Some("session-1")
    );
    assert_eq!(
        status.lifecycle.current_frontier_store_id,
        Some(store_ids[1])
    );
    assert_eq!(
        status.lifecycle.last_finalized_session_id.as_deref(),
        Some("session-0")
    );
    assert_eq!(
        status.lifecycle.last_finalized_frontier_store_id,
        Some(store_ids[0])
    );

    let rendered = serde_json::to_string(&status).unwrap();
    assert!(!rendered.contains("private payload marker"));
}

#[tokio::test]
async fn describe_gives_session_overview_without_full_payload_bodies() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tokensave");
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha".to_string()]).await;
    let payload = format!("describe secret body\n{}", "D".repeat(300_000));
    let mut external = raw_message("cursor", "tool-describe", "session-1", 2, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_store(&storage_root)
        .ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "describe alpha summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let description = db
        .lcm_describe("cursor", "session-1")
        .await
        .expect("description should load");
    assert_eq!(description.provider, "cursor");
    assert_eq!(description.session_id, "session-1");
    assert_eq!(description.raw_message_count, 2);
    assert_eq!(description.summary_node_count, 1);
    assert!(description
        .summary_nodes
        .iter()
        .any(|node| node.node_id == summary.node_id));

    let rendered = serde_json::to_string(&description).unwrap();
    assert!(rendered.contains("tool-describe"));
    assert!(!rendered.contains("describe secret body"));
}

#[tokio::test]
async fn expand_returns_sliced_raw_summary_and_payload_content_with_ranges() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tokensave");
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["0123456789abcdef".to_string()],
    )
    .await;
    let payload = format!("payload-prefix-{}", "Z".repeat(300_000));
    let mut external = raw_message("cursor", "tool-expand", "session-1", 2, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_store(&storage_root)
        .ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");
    let payload_ref = db
        .lcm_load_raw_message("cursor", "tool-expand")
        .await
        .unwrap()
        .payload_ref
        .expect("payload ref");
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "summary expansion body",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let raw = db
        .lcm_expand(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::RawMessage {
                store_id: store_ids[0],
            },
            content_slice: Some(LcmContentSlice {
                offset: 2,
                limit: 4,
            }),
        })
        .await
        .expect("raw should expand");
    assert_eq!(raw.kind, "raw_message");
    assert_eq!(raw.content, "2345");
    assert_eq!(raw.content_range.offset, 2);
    assert_eq!(raw.content_range.returned_chars, 4);
    assert!(raw.content_range.truncated);
    let raw_metadata = raw.raw_message.as_ref().expect("raw metadata");
    assert_eq!(raw_metadata.content, "2345");
    assert_eq!(raw_metadata.content.chars().count(), 4);
    let rendered_raw = serde_json::to_string(&raw).unwrap();
    assert!(!rendered_raw.contains("0123456789abcdef"));

    let summary_expansion = db
        .lcm_expand(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::SummaryNode {
                node_id: summary.node_id.clone(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 8,
                limit: 9,
            }),
        })
        .await
        .expect("summary should expand");
    assert_eq!(summary_expansion.kind, "summary_node");
    assert_eq!(summary_expansion.content, "expansion");
    let summary_metadata = summary_expansion
        .summary_node
        .as_ref()
        .expect("summary metadata");
    assert_eq!(summary_metadata.summary_text, "expansion");
    assert_eq!(summary_metadata.summary_text.chars().count(), 9);
    let rendered_summary = serde_json::to_string(&summary_expansion).unwrap();
    assert!(!rendered_summary.contains("summary expansion body"));
    assert_eq!(summary_expansion.summary_sources.len(), 1);

    let payload_expansion = db
        .lcm_expand(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: "payload-prefix".chars().count(),
            }),
        })
        .await
        .expect("payload should expand");
    assert_eq!(payload_expansion.kind, "external_payload");
    assert_eq!(
        payload_expansion.payload_ref.as_deref(),
        Some(payload_ref.as_str())
    );
    assert_eq!(payload_expansion.content, "payload-prefix");
    assert_eq!(payload_expansion.content_range.offset, 0);
    assert!(payload_expansion.content_range.truncated);

    let raw_external = db
        .lcm_load_raw_message("cursor", "tool-expand")
        .await
        .unwrap();
    assert_eq!(raw_external.storage_kind, LcmStorageKind::External);
    assert!(!payload_expansion.content.contains("ZZZZZZZZZZ"));
}

#[tokio::test]
async fn expand_slices_summary_source_content_and_nested_source_bodies() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let huge_source = format!("source-prefix-{}", "X".repeat(128_000));
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &[huge_source]).await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "summary source slicing regression",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let expansion = db
        .lcm_expand(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::SummaryNode {
                node_id: summary.node_id.clone(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: "source-prefix".chars().count(),
            }),
        })
        .await
        .expect("summary should expand");

    assert_eq!(expansion.summary_sources.len(), 1);
    let source = &expansion.summary_sources[0];
    assert_eq!(source.content, "source-prefix");
    assert!(source.content.chars().count() <= "source-prefix".chars().count());
    let source_range = source.content_range.as_ref().expect("source range");
    assert_eq!(source_range.offset, 0);
    assert_eq!(source_range.limit, "source-prefix".chars().count() as u64);
    assert_eq!(
        source_range.returned_chars,
        "source-prefix".chars().count() as u64
    );
    assert_eq!(source_range.total_chars, 128_014);
    assert!(source_range.truncated);
    let raw_source = source.raw_message.as_ref().expect("raw source metadata");
    assert_eq!(raw_source.store_id, store_ids[0]);
    assert_eq!(raw_source.content, "source-prefix");
    assert_eq!(
        raw_source.content.chars().count(),
        "source-prefix".chars().count()
    );
    assert!(!raw_source.content_hash.is_empty());

    let rendered = serde_json::to_string(&expansion).unwrap();
    assert!(!rendered.contains("XXXXXXXXXX"));
    assert!(rendered.contains("\"content_hash\""));
}

#[tokio::test]
async fn expand_wrapper_denies_cross_session_summary_nodes() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids =
        insert_raw_messages(&db, "cursor", "session-1", &["owned by session one".into()]).await;
    insert_session(&db, "cursor", "session-2").await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "summary belongs to session one",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let err = db
        .lcm_expand(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-2".into(),
            target: LcmExpandTarget::SummaryNode {
                node_id: summary.node_id,
            },
            content_slice: None,
        })
        .await
        .expect_err("wrapper expansion should reject nodes from another session");

    assert_eq!(err, LcmError::SummaryNodeNotFound);
}

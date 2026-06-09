use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeTarget, LcmError, LcmExpandQueryRequest,
    LcmExpandRequest, LcmExpandTarget, LcmGrepRequest, LcmGrepSort, LcmLifecycleUpdate,
    LcmLoadSessionRequest, LcmMaintenanceDebt, LcmScope, LcmSourceRef, LcmStorageKind,
    LcmSummaryNodeDraft, LCM_SCHEMA_VERSION, MAX_DERIVED_SNIPPET_CHARS,
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

#[allow(clippy::too_many_arguments)]
fn raw_message_with_role_source_timestamp(
    provider: &str,
    message_id: &str,
    session_id: &str,
    ordinal: i64,
    role: &str,
    source: &str,
    timestamp: i64,
    text: &str,
) -> SessionMessageRecord {
    let mut message = raw_message(provider, message_id, session_id, ordinal, text);
    message.role = role.to_string();
    message.timestamp = Some(timestamp);
    message.metadata_json = Some(serde_json::json!({"source": source}).to_string());
    message
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
            roles: Vec::new(),
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

    let next_after_store_id = first
        .next_cursor
        .as_deref()
        .and_then(|cursor| cursor.parse::<i64>().ok());
    let second = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            after_store_id: next_after_store_id,
            limit: 2,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
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
            roles: Vec::new(),
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
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
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
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        })
        .await
        .expect("path-like grep should not miss because punctuation was collapsed");

    assert_eq!(hits.len(), 2);
    let hit_ids = hits
        .iter()
        .filter_map(|hit| hit.store_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        hit_ids,
        std::collections::BTreeSet::from([store_ids[0], store_ids[1]])
    );
    assert!(hits
        .iter()
        .any(|hit| hit.store_id == Some(store_ids[0]) && hit.snippet.contains("src/foo.rs")));
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
            query: "\"OR\"".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
            include_summaries: false,
            limit: 10,
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        })
        .await
        .expect("reserved FTS operator text should be treated as literal text");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].store_id, Some(store_ids[0]));
    assert!(hits[0].snippet.contains("OR"));
}

#[tokio::test]
async fn grep_preserves_quoted_phrase_semantics() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "alpha beta phrase canary".to_string(),
            "alpha phrase beta (not adjacent)".to_string(),
        ],
    )
    .await;

    let hits = db
        .lcm_grep(LcmGrepRequest {
            provider: "cursor".into(),
            query: "\"alpha beta\"".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
            include_summaries: false,
            limit: 10,
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        })
        .await
        .expect("quoted phrase grep should preserve phrase matching");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].store_id, Some(store_ids[0]));
}

#[tokio::test]
async fn grep_preserves_boolean_or_semantics() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "apple only phrase".to_string(),
            "banana only phrase".to_string(),
            "neither fruit term".to_string(),
        ],
    )
    .await;

    let hits = db
        .lcm_grep(LcmGrepRequest {
            provider: "cursor".into(),
            query: "apple OR banana".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
            include_summaries: false,
            limit: 10,
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        })
        .await
        .expect("OR query should preserve boolean operator semantics");

    let matched = hits
        .iter()
        .filter_map(|hit| hit.store_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        matched,
        [store_ids[0], store_ids[1]]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
}

#[tokio::test]
async fn grep_cjk_query_uses_like_fallback_substring_matching() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "这是一个柠檬测试用例".to_string(),
            "仅包含苹果关键词".to_string(),
        ],
    )
    .await;

    let hits = db
        .lcm_grep(LcmGrepRequest {
            provider: "cursor".into(),
            query: "柠檬".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
            include_summaries: false,
            limit: 10,
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        })
        .await
        .expect("CJK grep should fall back to LIKE substring matching");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].store_id, Some(store_ids[0]));
    assert!(hits[0].snippet.contains("柠檬"));
}

#[tokio::test]
async fn grep_filters_raw_hits_by_role_source_and_time_and_sorts() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;

    for message in [
        raw_message_with_role_source_timestamp(
            "cursor",
            "old-cli-assistant",
            "session-1",
            1,
            "assistant",
            "cli",
            10,
            "orchard parity old cli assistant",
        ),
        raw_message_with_role_source_timestamp(
            "cursor",
            "new-cli-user",
            "session-1",
            2,
            "user",
            "cli",
            20,
            "orchard parity new cli user",
        ),
        raw_message_with_role_source_timestamp(
            "cursor",
            "new-api-assistant",
            "session-1",
            3,
            "assistant",
            "api",
            30,
            "orchard parity new api assistant",
        ),
    ] {
        assert!(db.upsert_session_message(&message).await);
    }

    let hits = db
        .lcm_grep(LcmGrepRequest {
            provider: "cursor".into(),
            query: "orchard parity".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
            include_summaries: true,
            limit: 10,
            sort: LcmGrepSort::Recency,
            source: Some("cli".into()),
            role: Some("assistant".into()),
            start_time: Some(5),
            end_time: Some(25),
        })
        .await
        .expect("filtered grep should succeed");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_id.as_deref(), Some("old-cli-assistant"));
    assert_eq!(hits[0].kind, "raw_message");
}

#[tokio::test]
async fn load_session_accepts_multiple_roles_and_slices_to_caller_limit() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;

    for message in [
        raw_message_with_role_source_timestamp(
            "cursor",
            "role-user",
            "session-1",
            1,
            "user",
            "cli",
            10,
            "user message content",
        ),
        raw_message_with_role_source_timestamp(
            "cursor",
            "role-tool",
            "session-1",
            2,
            "tool",
            "cli",
            20,
            "tool message content",
        ),
        raw_message_with_role_source_timestamp(
            "cursor",
            "role-assistant",
            "session-1",
            3,
            "assistant",
            "cli",
            30,
            "assistant message content",
        ),
    ] {
        assert!(db.upsert_session_message(&message).await);
    }

    let page = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            after_store_id: None,
            limit: 10,
            roles: vec!["user".into(), "tool".into()],
            start_time: Some(1),
            end_time: Some(25),
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: 12,
            }),
        })
        .await
        .expect("multi-role page should load");

    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["role-user", "role-tool"]
    );
    assert!(page
        .messages
        .iter()
        .all(|message| message.content_range.returned_chars <= 12));
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
        .lcm_describe(LcmDescribeRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmDescribeTarget::Session,
        })
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
async fn describe_node_and_external_payload_return_metadata_without_body_leaks() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tokensave");
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "leaf source alpha body".to_string(),
            "leaf source beta body".to_string(),
        ],
    )
    .await;
    let payload = format!("external describe secret {}", "P".repeat(300_000));
    let mut external = raw_message("cursor", "tool-describe-target", "session-1", 3, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_store(&storage_root)
        .ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");
    let payload_ref = db
        .lcm_load_raw_message("cursor", "tool-describe-target")
        .await
        .unwrap()
        .payload_ref
        .expect("payload ref");

    let leaf = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "leaf summary body must not appear in describe",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("leaf summary should insert");
    let parent = db
        .lcm_insert_summary_node(LcmSummaryNodeDraft {
            depth: 1,
            summary_text: "parent summary body must not appear in describe".to_string(),
            source_refs: vec![
                LcmSourceRef::SummaryNode {
                    node_id: leaf.node_id.clone(),
                },
                LcmSourceRef::RawMessage {
                    store_id: store_ids[1],
                },
            ],
            ..summary_draft("cursor", "session-1", "", Vec::new())
        })
        .await
        .expect("parent summary should insert");

    let node_description = db
        .lcm_describe(LcmDescribeRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmDescribeTarget::SummaryNode {
                node_id: parent.node_id.clone(),
            },
        })
        .await
        .expect("node description should load");
    assert_eq!(node_description.target, "summary_node");
    let node = node_description
        .summary_node
        .as_ref()
        .expect("summary metadata");
    assert_eq!(node.node_id, parent.node_id);
    assert_eq!(node.source_count, 2);
    assert!(node
        .children
        .iter()
        .any(|child| child.node_id.as_deref() == Some(leaf.node_id.as_str())));

    let payload_description = db
        .lcm_describe(LcmDescribeRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmDescribeTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
        })
        .await
        .expect("payload description should load");
    assert_eq!(payload_description.target, "external_payload");
    let payload_meta = payload_description
        .external_payload
        .as_ref()
        .expect("payload metadata");
    assert_eq!(payload_meta.payload_ref, payload_ref);
    assert!(payload_meta
        .content_preview
        .contains(&payload_meta.payload_ref));
    assert!(!payload_meta
        .content_preview
        .contains("external describe secret"));

    let rendered = serde_json::to_string(&(node_description, payload_description)).unwrap();
    assert!(!rendered.contains("parent summary body"));
    assert!(!rendered.contains("leaf summary body"));
    assert!(!rendered.contains("external describe secret"));
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
            source_offset: 0,
            source_limit: None,
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
            source_offset: 0,
            source_limit: None,
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
            source_offset: 0,
            source_limit: None,
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
    // Varied prose-like filler keeps the oversized body inline (no base64
    // runs, no high-repetition quarantine) so this exercises char slicing.
    let filler = (0..12_000)
        .map(|index| format!("filler{index:05}"))
        .collect::<Vec<_>>()
        .join(" ");
    let huge_source = format!("source-prefix-{filler}");
    let huge_source_chars = huge_source.chars().count() as u64;
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
            source_offset: 0,
            source_limit: None,
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
    assert_eq!(source_range.total_chars, huge_source_chars);
    assert!(source_range.truncated);
    assert!(source.content_truncated);
    let raw_source = source.raw_message.as_ref().expect("raw source metadata");
    assert_eq!(raw_source.store_id, store_ids[0]);
    assert_eq!(raw_source.content, "source-prefix");
    assert_eq!(
        raw_source.content.chars().count(),
        "source-prefix".chars().count()
    );
    assert!(!raw_source.content_hash.is_empty());

    let rendered = serde_json::to_string(&expansion).unwrap();
    assert!(!rendered.contains("filler11999"));
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
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect_err("wrapper expansion should reject nodes from another session");

    assert_eq!(err, LcmError::SummaryNodeNotFound);
}

#[tokio::test]
async fn expand_query_returns_no_match_without_synthesis() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["ordinary transcript without the target term".to_string()],
    )
    .await;

    let response = db
        .lcm_expand_query(LcmExpandQueryRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            prompt: "What did we decide about citron?".into(),
            query: Some("citron".into()),
            node_ids: Vec::new(),
            max_results: 5,
            max_tokens: 2000,
            context_max_tokens: 1024,
        })
        .await
        .expect("expand query should succeed");

    assert!(!response.needs_synthesis);
    assert_eq!(
        response.answer.as_deref(),
        Some("No matching LCM context found in the current session.")
    );
    assert!(response.node_ids.is_empty());
    assert!(response.matches.is_empty());
    assert!(response.context_blocks.is_empty());
    assert!(!response.context_truncated);
}

#[tokio::test]
async fn expand_query_selects_summary_and_raw_context_blocks() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "raw orchard migration source detail".to_string(),
            "unrelated follow-up".to_string(),
        ],
    )
    .await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "summary orchard migration decision",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let response = db
        .lcm_expand_query(LcmExpandQueryRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            prompt: "What happened with orchard migration?".into(),
            query: Some("orchard migration".into()),
            node_ids: Vec::new(),
            max_results: 5,
            max_tokens: 512,
            context_max_tokens: 4096,
        })
        .await
        .expect("expand query should assemble context");

    assert!(response.needs_synthesis);
    assert_eq!(response.answer, None);
    assert!(response
        .node_ids
        .iter()
        .any(|node_id| node_id == &summary.node_id));
    assert!(response.matches.iter().any(
        |item| item.kind == "summary_node" && item.node_id.as_deref() == Some(&summary.node_id)
    ));
    assert!(
        response
            .context_blocks
            .iter()
            .any(|block| block.kind == "summary"
                && block.node_id.as_deref() == Some(&summary.node_id))
    );
    assert!(response.context_blocks.iter().any(
        |block| block.kind == "raw_message" && block.content.contains("raw orchard migration")
    ));
    assert!(response
        .synthesis_prompt
        .as_ref()
        .expect("synthesis prompt")
        .user
        .contains("EXPANDED CONTEXT"));
}

#[tokio::test]
async fn expand_query_reports_context_budget_truncation() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let long_source = format!("raw lemon details {}", "L".repeat(4000));
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &[long_source]).await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            &format!("summary lemon budget {}", "S".repeat(4000)),
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let response = db
        .lcm_expand_query(LcmExpandQueryRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            prompt: "Explain lemon budget".into(),
            query: Some("lemon".into()),
            node_ids: vec![summary.node_id.clone()],
            max_results: 1,
            max_tokens: 128,
            context_max_tokens: 64,
        })
        .await
        .expect("expand query should report truncation");

    assert!(response.context_truncated);
    assert!(response.context_budget.used_chars <= 64);
    assert!(response
        .context_blocks
        .iter()
        .any(|block| block.content_range.truncated));
    assert!(!response.context_pagination.is_empty());
    assert!(response
        .context_pagination
        .iter()
        .any(|page| page.has_more && page.node_id.as_deref() == Some(&summary.node_id)));
}

#[tokio::test]
async fn expand_paginates_summary_sources_with_offset_and_limit() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let contents: Vec<String> = (1..=5)
        .map(|index| format!("source body {index}"))
        .collect();
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &contents).await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "paginated summary",
            store_ids
                .iter()
                .map(|store_id| LcmSourceRef::RawMessage {
                    store_id: *store_id,
                })
                .collect(),
        ))
        .await
        .expect("summary should insert");
    let expand_request = |source_offset: usize, source_limit: Option<usize>| LcmExpandRequest {
        provider: "cursor".into(),
        session_id: "session-1".into(),
        target: LcmExpandTarget::SummaryNode {
            node_id: summary.node_id.clone(),
        },
        content_slice: None,
        source_offset,
        source_limit,
    };

    let page = db
        .lcm_expand(expand_request(1, Some(2)))
        .await
        .expect("paginated expand should succeed");
    let returned_store_ids: Vec<i64> = page
        .summary_sources
        .iter()
        .filter_map(|source| source.raw_message.as_ref().map(|raw| raw.store_id))
        .collect();
    assert_eq!(returned_store_ids, vec![store_ids[1], store_ids[2]]);
    let pagination = page.source_pagination.expect("pagination metadata");
    assert_eq!(pagination.source_offset, 1);
    assert_eq!(pagination.source_limit, 2);
    assert_eq!(pagination.returned_sources, 2);
    assert_eq!(pagination.total_sources, 5);
    assert_eq!(pagination.next_source_offset, Some(3));
    assert!(pagination.has_more);
    assert_eq!(pagination.remaining_sources, 2);

    // Resuming from the cursor drains the list; an omitted limit clamps to
    // the remaining sources like hermes-lcm.
    let tail = db
        .lcm_expand(expand_request(3, None))
        .await
        .expect("cursor resume should succeed");
    assert_eq!(tail.summary_sources.len(), 2);
    let tail_pagination = tail.source_pagination.expect("tail pagination");
    assert_eq!(tail_pagination.source_limit, 2);
    assert_eq!(tail_pagination.next_source_offset, None);
    assert!(!tail_pagination.has_more);
    assert_eq!(tail_pagination.remaining_sources, 0);

    // An offset beyond the end clamps to the source count and returns an
    // empty page instead of erroring.
    let beyond = db
        .lcm_expand(expand_request(9, Some(2)))
        .await
        .expect("out-of-range offset should clamp");
    assert!(beyond.summary_sources.is_empty());
    let beyond_pagination = beyond.source_pagination.expect("beyond pagination");
    assert_eq!(beyond_pagination.source_offset, 5);
    assert_eq!(beyond_pagination.returned_sources, 0);
    assert!(!beyond_pagination.has_more);

    // The default request still returns every source with full metadata.
    let full = db
        .lcm_expand(expand_request(0, None))
        .await
        .expect("default expand should succeed");
    assert_eq!(full.summary_sources.len(), 5);
    let full_pagination = full.source_pagination.expect("default pagination");
    assert_eq!(full_pagination.returned_sources, 5);
    assert!(!full_pagination.has_more);
}

#[tokio::test]
async fn expand_allows_cross_session_raw_store_id_with_provenance() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["cross session body".to_string()],
    )
    .await;
    insert_session(&db, "cursor", "session-2").await;
    let raw_request = |provider: &str, session_id: &str| LcmExpandRequest {
        provider: provider.into(),
        session_id: session_id.into(),
        target: LcmExpandTarget::RawMessage {
            store_id: store_ids[0],
        },
        content_slice: None,
        source_offset: 0,
        source_limit: None,
    };

    let cross = db
        .lcm_expand(raw_request("cursor", "session-2"))
        .await
        .expect("cross-session store_id expand should succeed");
    assert_eq!(cross.kind, "raw_message");
    assert_eq!(cross.from_current_session, Some(false));
    assert_eq!(cross.content, "cross session body");
    assert_eq!(
        cross.raw_message.as_ref().expect("raw metadata").session_id,
        "session-1"
    );

    let same = db
        .lcm_expand(raw_request("cursor", "session-1"))
        .await
        .expect("same-session store_id expand should succeed");
    assert_eq!(same.from_current_session, Some(true));
    assert_eq!(same.externalized_note, None);

    // Cross-provider raw rows stay rejected: providers are a TokenSave
    // concept with no hermes-lcm equivalent.
    insert_session(&db, "claude", "session-9").await;
    let err = db
        .lcm_expand(raw_request("claude", "session-9"))
        .await
        .expect_err("cross-provider store_id expand should be rejected");
    assert_eq!(err, LcmError::SummarySourceNotOwnedBySession);
}

#[tokio::test]
async fn expand_cross_session_external_row_can_hydrate_payload_via_two_step_expand() {
    let tmp = TempDir::new().unwrap();
    let storage_root = tmp.path().join(".tokensave");
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;
    insert_session(&db, "cursor", "session-2").await;
    let payload = format!("cross-payload-{}", "Z".repeat(300_000));
    let mut external = raw_message("cursor", "cross-external", "session-1", 1, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_store(&storage_root)
        .ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");
    let raw = db
        .lcm_load_raw_message("cursor", "cross-external")
        .await
        .expect("external raw message should exist");
    let payload_ref = raw.payload_ref.clone().expect("payload ref");

    let cross = db
        .lcm_expand(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-2".into(),
            target: LcmExpandTarget::RawMessage {
                store_id: raw.store_id,
            },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("cross-session external row should expand");
    assert_eq!(cross.from_current_session, Some(false));
    assert_eq!(cross.payload_ref.as_deref(), Some(payload_ref.as_str()));
    assert_eq!(cross.externalized_note, None);
    let rendered = serde_json::to_string(&cross).unwrap();
    assert!(
        !rendered.contains("ZZZZZZZZZZ"),
        "cross-session raw-message expand should stay compact until payload expansion"
    );
    let payload_owner_session_id = cross
        .raw_message
        .as_ref()
        .expect("raw metadata should include owner session")
        .session_id
        .clone();
    let expanded_payload = db
        .lcm_expand(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: payload_owner_session_id,
            target: LcmExpandTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: 128,
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("cross-session payload should hydrate through explicit payload target");
    assert_eq!(expanded_payload.kind, "external_payload");
    assert!(expanded_payload.content.starts_with("cross-payload-"));
    assert_eq!(
        expanded_payload.payload_ref.as_deref(),
        Some(payload_ref.as_str())
    );
}

#[tokio::test]
async fn status_reports_dag_depth_distribution_store_estimate_and_config_defaults() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["alpha beta gamma".to_string(), "delta epsilon".to_string()],
    )
    .await;
    let leaf = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "leaf summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("leaf summary should insert");
    let mut parent = summary_draft(
        "cursor",
        "session-1",
        "condensed parent summary",
        vec![LcmSourceRef::SummaryNode {
            node_id: leaf.node_id.clone(),
        }],
    );
    parent.depth = 1;
    parent.summary_token_count = 3;
    parent.source_token_count = 5;
    db.lcm_insert_summary_node(parent)
        .await
        .expect("parent summary should insert");

    let status = db
        .lcm_status("cursor", Some("session-1"))
        .await
        .expect("status should load");

    assert_eq!(status.store.messages, 2);
    assert_eq!(status.store.estimated_tokens, 5);

    assert_eq!(status.dag.total_nodes, 2);
    assert_eq!(status.dag.total_tokens, 8);
    assert_eq!(status.dag.total_source_tokens, 35);
    assert_eq!(status.dag.compression_ratio, "4.4:1");
    let depth_zero = status.dag.depths.get("d0").expect("depth-0 bucket");
    assert_eq!(depth_zero.count, 1);
    assert_eq!(depth_zero.tokens, 5);
    assert_eq!(depth_zero.source_tokens, 30);
    let depth_one = status.dag.depths.get("d1").expect("depth-1 bucket");
    assert_eq!(depth_one.count, 1);
    assert_eq!(depth_one.tokens, 3);
    assert_eq!(depth_one.source_tokens, 5);

    assert_eq!(status.config.fresh_tail_count, 2);
    assert_eq!(status.config.summary_fan_in, 4);
    assert_eq!(status.config.compression_boundary_cooldown_seconds, 60);

    // An empty scope reports an inert DAG rather than dividing by zero.
    insert_session(&db, "cursor", "session-empty").await;
    let empty = db
        .lcm_status("cursor", Some("session-empty"))
        .await
        .expect("empty status should load");
    assert_eq!(empty.dag.total_nodes, 0);
    assert_eq!(empty.dag.compression_ratio, "0:1");
    assert!(empty.dag.depths.is_empty());
    assert_eq!(empty.store.messages, 0);
    assert_eq!(empty.store.estimated_tokens, 0);
}

#[tokio::test]
async fn status_uses_python_half_even_rounding_for_ratio_ties() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-tie",
        &["alpha".to_string(), "beta".to_string()],
    )
    .await;
    let mut node = summary_draft(
        "cursor",
        "session-tie",
        "ratio tie",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    );
    node.summary_token_count = 4;
    node.source_token_count = 5; // 1.25 -> Python round(..., 1) => 1.2
    db.lcm_insert_summary_node(node).await.unwrap();
    let status = db
        .lcm_status("cursor", Some("session-tie"))
        .await
        .expect("status should load");
    assert_eq!(status.dag.compression_ratio, "1.2:1");
}

// Hermes load_session paging only hands back a resume cursor while more rows
// remain: a final page that exactly fills the limit terminates the cursor, and
// resuming past the last row yields an empty page instead of an error.
#[tokio::test]
async fn load_session_exact_final_page_omits_next_cursor() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let contents = (1..=4)
        .map(|idx| format!("edge-message-{idx}"))
        .collect::<Vec<_>>();
    let store_ids = insert_raw_messages(&db, "cursor", "session-edge", &contents).await;
    let request = |after_store_id: Option<i64>, limit: usize| LcmLoadSessionRequest {
        provider: "cursor".into(),
        session_id: "session-edge".into(),
        after_store_id,
        limit,
        roles: Vec::new(),
        start_time: None,
        end_time: None,
        content_slice: None,
    };

    // The whole session in one exactly-sized page: no resume cursor.
    let exact = db
        .lcm_load_session(request(None, 4))
        .await
        .expect("exact-limit page should load");
    assert_eq!(exact.messages.len(), 4);
    assert_eq!(exact.next_cursor, None);

    // A final page that exactly fills the limit also terminates the cursor.
    let first = db
        .lcm_load_session(request(None, 2))
        .await
        .expect("first page should load");
    assert_eq!(
        first.next_cursor.as_deref(),
        Some(store_ids[1].to_string().as_str())
    );
    let last = db
        .lcm_load_session(request(Some(store_ids[1]), 2))
        .await
        .expect("final page should load");
    assert_eq!(last.messages.len(), 2);
    assert_eq!(last.messages[1].content, "edge-message-4");
    assert_eq!(last.next_cursor, None);

    // Resuming from the last row returns an empty terminal page.
    let drained = db
        .lcm_load_session(request(Some(store_ids[3]), 2))
        .await
        .expect("drained cursor should load");
    assert!(drained.messages.is_empty());
    assert_eq!(drained.next_cursor, None);
}

// A session with no LCM rows is a valid empty state, matching hermes-lcm
// engine behavior on a fresh store: reads return empty results and zeroed
// overviews instead of errors.
#[tokio::test]
async fn empty_session_load_grep_and_describe_return_empty_results() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-empty").await;

    let page = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-empty".into(),
            after_store_id: None,
            limit: 10,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .expect("empty session should load");
    assert!(page.messages.is_empty());
    assert_eq!(page.next_cursor, None);

    let hits = db
        .lcm_grep(LcmGrepRequest {
            provider: "cursor".into(),
            query: "anything".into(),
            scope: LcmScope::Session,
            session_id: Some("session-empty".into()),
            include_summaries: true,
            limit: 10,
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        })
        .await
        .expect("grep on empty session should succeed");
    assert!(hits.is_empty());

    let described = db
        .lcm_describe(LcmDescribeRequest {
            provider: "cursor".into(),
            session_id: "session-empty".into(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .expect("describe on empty session should succeed");
    assert_eq!(described.raw_message_count, 0);
    assert_eq!(described.summary_node_count, 0);
    assert_eq!(described.external_payload_count, 0);
    assert_eq!(described.first_store_id, None);
    assert_eq!(described.last_store_id, None);
    assert!(described.raw_messages.is_empty());
    assert!(described.summary_nodes.is_empty());
}

// Content slices are character offsets, never byte offsets, matching Python
// string slicing in hermes-lcm `lcm_expand`/`lcm_load_session` (text[a:a+n]).
// Multibyte content must slice cleanly with char-based range metadata.
#[tokio::test]
async fn content_slices_use_char_offsets_for_multibyte_content() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    // 9 chars, 17 UTF-8 bytes: byte-based slicing would panic or split chars.
    let content = "αβγδε🦀abc".to_string();
    assert_eq!(content.chars().count(), 9);
    assert_eq!(content.len(), 17);
    let store_ids = insert_raw_messages(&db, "cursor", "session-utf8", &[content]).await;

    let page = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-utf8".into(),
            after_store_id: None,
            limit: 10,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: Some(LcmContentSlice {
                offset: 4,
                limit: 3,
            }),
        })
        .await
        .expect("multibyte slice should load");
    // Python: "αβγδε🦀abc"[4:7] == "ε🦀a"
    assert_eq!(page.messages[0].content, "ε🦀a");
    let range = &page.messages[0].content_range;
    assert_eq!(range.offset, 4);
    assert_eq!(range.returned_chars, 3);
    assert_eq!(range.total_chars, 9);
    assert!(range.truncated);

    let expanded = db
        .lcm_expand(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-utf8".into(),
            target: LcmExpandTarget::RawMessage {
                store_id: store_ids[0],
            },
            content_slice: Some(LcmContentSlice {
                offset: 5,
                limit: 2,
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("multibyte expand should succeed");
    // Python: "αβγδε🦀abc"[5:7] == "🦀a"
    assert_eq!(expanded.content, "🦀a");
    assert_eq!(expanded.content_range.offset, 5);
    assert_eq!(expanded.content_range.returned_chars, 2);
    assert_eq!(expanded.content_range.total_chars, 9);
    assert!(expanded.content_range.truncated);

    // An offset past the end clamps to an empty slice like Python s[99:101].
    let beyond = db
        .lcm_expand(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-utf8".into(),
            target: LcmExpandTarget::RawMessage {
                store_id: store_ids[0],
            },
            content_slice: Some(LcmContentSlice {
                offset: 99,
                limit: 2,
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("out-of-range multibyte slice should clamp");
    assert_eq!(beyond.content, "");
    assert_eq!(beyond.content_range.returned_chars, 0);
    assert_eq!(beyond.content_range.total_chars, 9);
}

// Pins the SQL pushdown of `count_lossy_ingest_records` to the previous
// serde_json semantics: only a JSON boolean `true` under
// `$.ingest_protection.lossy` counts; numeric 1, false, missing keys,
// non-object metadata, invalid JSON, and NULL metadata are all not-lossy.
#[tokio::test]
async fn status_counts_lossy_ingest_records_with_pinned_metadata_semantics() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = GlobalDb::open_at(&db_path).await.expect("session db open");
    insert_session(&db, "cursor", "session-lossy").await;

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    let variants: &[(&str, Option<&str>)] = &[
        ("lossy-true", Some(r#"{"ingest_protection":{"lossy":true}}"#)),
        ("lossy-false", Some(r#"{"ingest_protection":{"lossy":false}}"#)),
        ("lossy-integer", Some(r#"{"ingest_protection":{"lossy":1}}"#)),
        ("missing-key", Some(r#"{"ingest_protection":{}}"#)),
        ("missing-section", Some(r#"{"other":true}"#)),
        ("invalid-json", Some("{not json")),
        ("non-object", Some(r#"[{"ingest_protection":{"lossy":true}}]"#)),
        ("null-metadata", None),
    ];
    for (idx, (message_id, metadata)) in variants.iter().enumerate() {
        let metadata_value = match metadata {
            Some(text) => libsql::Value::Text((*text).to_string()),
            None => libsql::Value::Null,
        };
        conn.execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, snippet_text,
                index_text, legacy_source, legacy_truncated, metadata_json
             )
             VALUES ('cursor', ?1, 'session-lossy', 'assistant', ?2, ?2,
                     'body', 'hash', 'inline', NULL, 'body', 'body', 0, 0, ?3)",
            libsql::params![*message_id, (idx + 1) as i64, metadata_value],
        )
        .await
        .unwrap();
    }

    let status = db
        .lcm_status("cursor", Some("session-lossy"))
        .await
        .expect("status should load");
    assert_eq!(
        status.redaction.lossy_records, 1,
        "only the JSON boolean true row counts as lossy"
    );
    assert!(status.redaction.enabled);
    assert_eq!(status.redaction.legacy_truncated_count, 0);
}

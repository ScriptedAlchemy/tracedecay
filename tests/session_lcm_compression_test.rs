use serde_json::json;
use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::{
    LcmCompressionRequest, LcmLifecycleUpdate, LcmLoadSessionRequest, LcmMaintenanceDebt,
    LcmPreflightRequest, LcmSourceRef, LcmSummarizerMode, LcmSummaryNodeDraft,
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
    raw_message_with_role(provider, message_id, session_id, "assistant", ordinal, text)
}

fn raw_message_with_role(
    provider: &str,
    message_id: &str,
    session_id: &str,
    role: &str,
    ordinal: i64,
    text: &str,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: provider.to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
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

async fn insert_raw_messages_with_roles(
    db: &GlobalDb,
    provider: &str,
    session_id: &str,
    messages: &[(&str, &str)],
) -> Vec<i64> {
    insert_session(db, provider, session_id).await;
    let mut store_ids = Vec::new();
    for (idx, (role, content)) in messages.iter().enumerate() {
        let message_slug = content.replace(|ch: char| !ch.is_ascii_alphanumeric(), "-");
        let message_id = format!("{session_id}-message-{}-{message_slug}", idx + 1);
        let message = raw_message_with_role(
            provider,
            &message_id,
            session_id,
            role,
            (idx + 1) as i64,
            content,
        );
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
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        summarizer,
    }
}

fn limited_compress_request(
    provider: &str,
    session_id: &str,
    summarizer: LcmSummarizerMode,
    leaf_chunk_tokens: Option<i64>,
    max_source_messages: Option<usize>,
    max_assembly_tokens: Option<i64>,
) -> LcmCompressionRequest {
    LcmCompressionRequest {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        messages: Vec::new(),
        current_tokens: Some(1_000),
        focus_topic: None,
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id: None,
        max_assembly_tokens,
        leaf_chunk_tokens,
        max_source_messages,
        summary_fan_in: None,
        summarizer,
    }
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
        conversation_id: session_id.to_string(),
        session_id: session_id.to_string(),
        depth,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count: 20,
        summary_token_count: 3,
        source_time_start: Some(1_715_000_000),
        source_time_end: Some(1_715_000_030),
        expand_hint: Some("test summary lineage".to_string()),
        metadata_json: None,
    }
}

fn summary_draft_with_times(
    provider: &str,
    session_id: &str,
    depth: i64,
    summary_text: &str,
    source_refs: Vec<LcmSourceRef>,
    source_time_start: i64,
    source_time_end: i64,
) -> LcmSummaryNodeDraft {
    let mut draft = summary_draft(provider, session_id, depth, summary_text, source_refs);
    draft.source_time_start = Some(source_time_start);
    draft.source_time_end = Some(source_time_end);
    draft
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
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
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
async fn ignored_session_pattern_skips_active_ingest_and_compression() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "cron-20260414").await;

    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "cron-20260414".into(),
            messages: vec![json!({
                "id": "cron-message-1",
                "role": "assistant",
                "content": "scheduled report body that must not be indexed"
            })],
            current_tokens: Some(1_000),
            focus_topic: None,
            ignore_session_patterns: vec!["cron-*".into()],
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "should not be used".into(),
            },
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "ignored_session");
    assert_eq!(response.summary_nodes_created, 0);
    assert_eq!(
        response.replay_messages[0]["content"],
        "scheduled report body that must not be indexed"
    );
    assert_eq!(
        db.lcm_status("cursor", Some("cron-20260414"))
            .await
            .unwrap()
            .raw_message_count,
        0
    );
}

#[tokio::test]
async fn stateless_session_pattern_keeps_replay_but_does_not_persist_lcm_rows() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "scratch-shell-a").await;

    let response = db
        .lcm_preflight(LcmPreflightRequest {
            provider: "cursor".into(),
            session_id: "scratch-shell-a".into(),
            messages: vec![json!({
                "id": "scratch-message-1",
                "role": "user",
                "content": "throwaway one-shot prompt"
            })],
            current_tokens: Some(100),
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: vec!["scratch-shell-*".into()],
            ignore_message_patterns: Vec::new(),
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert!(!response.should_compress);
    assert_eq!(response.reason, "stateless_session");
    assert_eq!(
        response.replay_messages[0]["content"],
        "throwaway one-shot prompt"
    );
    assert_eq!(
        db.lcm_status("cursor", Some("scratch-shell-a"))
            .await
            .unwrap()
            .raw_message_count,
        0
    );
}

#[tokio::test]
async fn ignore_message_patterns_and_heartbeat_noise_are_storage_only() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-noise").await;

    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-noise".into(),
            messages: vec![
                json!({"id": "heartbeat-1", "role": "assistant", "content": "Still working..."}),
                json!({"id": "cron-noise-1", "role": "user", "content": "Cronjob Response: noisy heartbeat"}),
                json!({"id": "valuable-1", "role": "user", "content": "real user request"}),
            ],
            current_tokens: Some(100),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: vec!["Cronjob Response:*".into()],
            expected_current_frontier_store_id: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            summarizer: LcmSummarizerMode::Noop,
        })
        .await
        .unwrap();

    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "Still working...",
            "Cronjob Response: noisy heartbeat",
            "real user request"
        ]
    );
    let page = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-noise".into(),
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
    assert_eq!(page.messages[0].content, "real user request");
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
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
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
async fn compression_preserves_leading_system_developer_tool_anchor_outside_summary() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "system policy anchor"),
            ("developer", "developer policy anchor"),
            ("user", "old user request"),
            ("assistant", "old assistant response"),
            ("user", "fresh user request"),
            ("assistant", "fresh assistant response"),
        ],
    )
    .await;

    let response = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "old exchange summary".into(),
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 1);
    let replay = response
        .replay_messages
        .iter()
        .map(|message| {
            (
                message["role"].as_str().unwrap().to_string(),
                message["content"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        replay,
        vec![
            ("system".to_string(), "system policy anchor".to_string()),
            (
                "developer".to_string(),
                "developer policy anchor".to_string()
            ),
            ("system".to_string(), "old exchange summary".to_string()),
            ("user".to_string(), "fresh user request".to_string()),
            (
                "assistant".to_string(),
                "fresh assistant response".to_string()
            ),
        ]
    );
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[3])
    );

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &response.summary_nodes[0].node_id)
        .await
        .unwrap();
    let summarized_contents = expanded
        .sources
        .iter()
        .map(|source| source.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        summarized_contents,
        vec!["old user request", "old assistant response"]
    );
}

#[tokio::test]
async fn compression_summarizes_historical_tool_messages_instead_of_pinning_all() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "system policy anchor"),
            ("tool", "large historical tool result"),
            ("user", "old user follow-up"),
            ("user", "fresh user request"),
            ("assistant", "fresh assistant response"),
        ],
    )
    .await;

    let response = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "tool result summary".into(),
            },
        ))
        .await
        .unwrap();

    let replay = response
        .replay_messages
        .iter()
        .map(|message| message["content"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        replay,
        vec![
            "system policy anchor".to_string(),
            "tool result summary".to_string(),
            "fresh user request".to_string(),
            "fresh assistant response".to_string()
        ]
    );

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &response.summary_nodes[0].node_id)
        .await
        .unwrap();
    assert_eq!(
        expanded
            .sources
            .iter()
            .map(|source| source.content.as_str())
            .collect::<Vec<_>>(),
        vec!["large historical tool result", "old user follow-up"]
    );
}

#[tokio::test]
async fn compression_preserves_interleaved_policy_anchor_outside_summary() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("user", "old user request before policy"),
            ("developer", "interleaved developer policy anchor"),
            ("assistant", "old assistant response after policy"),
            ("user", "old user follow-up after policy"),
            ("user", "fresh user request"),
            ("assistant", "fresh assistant response"),
        ],
    )
    .await;

    let response = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "old exchange summary".into(),
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 1);
    let replay = response
        .replay_messages
        .iter()
        .map(|message| {
            (
                message["role"].as_str().unwrap().to_string(),
                message["content"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        replay,
        vec![
            ("system".to_string(), "old exchange summary".to_string()),
            (
                "developer".to_string(),
                "interleaved developer policy anchor".to_string()
            ),
            ("user".to_string(), "fresh user request".to_string()),
            (
                "assistant".to_string(),
                "fresh assistant response".to_string()
            ),
        ]
    );
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[3])
    );

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &response.summary_nodes[0].node_id)
        .await
        .unwrap();
    let summarized_contents = expanded
        .sources
        .iter()
        .map(|source| source.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        summarized_contents,
        vec![
            "old user request before policy",
            "old assistant response after policy",
            "old user follow-up after policy"
        ]
    );
}

#[tokio::test]
async fn repeated_active_ingest_preserves_existing_message_ordinals() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;
    let messages = vec![
        json!({"id": "active-1", "role": "user", "content": "hello"}),
        json!({"id": "active-2", "role": "assistant", "content": "hi"}),
    ];

    db.lcm_preflight(LcmPreflightRequest {
        provider: "cursor".into(),
        session_id: "session-1".into(),
        messages: messages.clone(),
        current_tokens: Some(10),
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
    })
    .await
    .unwrap();
    let first_ordinals = (
        db.lcm_load_raw_message("cursor", "active-1")
            .await
            .unwrap()
            .ordinal,
        db.lcm_load_raw_message("cursor", "active-2")
            .await
            .unwrap()
            .ordinal,
    );

    db.lcm_compress(LcmCompressionRequest {
        provider: "cursor".into(),
        session_id: "session-1".into(),
        messages,
        current_tokens: Some(10),
        focus_topic: None,
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        summarizer: LcmSummarizerMode::Noop,
    })
    .await
    .unwrap();

    assert_eq!(
        (
            db.lcm_load_raw_message("cursor", "active-1")
                .await
                .unwrap()
                .ordinal,
            db.lcm_load_raw_message("cursor", "active-2")
                .await
                .unwrap()
                .ordinal,
        ),
        first_ordinals
    );
}

#[tokio::test]
async fn compression_noops_when_expected_frontier_is_stale() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids =
        insert_raw_messages(&db, "cursor", "session-1", &["one", "two", "three", "four"]).await;
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "session-1".into(),
        current_session_id: "session-1".into(),
        current_frontier_store_id: Some(store_ids[0]),
        last_finalized_session_id: None,
        last_finalized_frontier_store_id: None,
        maintenance_debt: Vec::new(),
    })
    .await
    .unwrap();

    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            messages: Vec::new(),
            current_tokens: Some(1_000),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: Some(0),
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "stale summary".into(),
            },
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "frontier_changed");
    assert_eq!(response.summary_nodes_created, 0);
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[0])
    );
    let status = db.lcm_status("cursor", Some("session-1")).await.unwrap();
    assert_eq!(status.summary_node_count, 0);
}

#[tokio::test]
async fn hermes_auxiliary_request_mode_returns_summary_contract() {
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
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            messages: Vec::new(),
            current_tokens: Some(1_000),
            focus_topic: Some("billing".into()),
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            summarizer: LcmSummarizerMode::HermesAuxiliary,
        })
        .await
        .unwrap();

    assert_eq!(response.status, "needs_summary");
    assert_eq!(response.summary_nodes_created, 0);
    let summary_request = response
        .summary_request
        .as_ref()
        .expect("HermesAuxiliary should return source contract");
    assert!(summary_request.prompt.contains("session-1"));
    assert!(summary_request.prompt.contains("billing"));
    assert_eq!(summary_request.source_range.from_store_id, store_ids[0]);
    assert_eq!(summary_request.source_range.to_store_id, store_ids[1]);
    assert_eq!(
        summary_request
            .source_messages
            .iter()
            .map(|message| (message.store_id, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![(store_ids[0], "old-1"), (store_ids[1], "old-2")]
    );
    assert_eq!(response.replay_messages[0]["content"], "fresh-1");
    assert_eq!(response.replay_messages[1]["content"], "fresh-2");
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

#[tokio::test]
async fn dynamic_chunking_compacts_bounded_oldest_leaf_chunk_and_records_backlog_debt() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "old-1 token",
            "old-2 token",
            "old-3 token",
            "old-4 token",
            "fresh-1",
            "fresh-2",
        ],
    )
    .await;

    let response = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "first chunk summary".into(),
            },
            Some(4),
            Some(2),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "compressed_backlog");
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[1])
    );
    assert_eq!(
        response.frontier.maintenance_debt,
        vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: store_ids[2],
            to_store_id: store_ids[3],
        }]
    );
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "first chunk summary",
            "old-3 token",
            "old-4 token",
            "fresh-1",
            "fresh-2",
        ]
    );

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &response.summary_nodes[0].node_id)
        .await
        .unwrap();
    assert_eq!(expanded.sources.len(), 2);
    assert_eq!(expanded.sources[0].content, "old-1 token");
    assert_eq!(expanded.sources[1].content, "old-2 token");
    assert!(response.summary_nodes[0]
        .metadata_json
        .as_deref()
        .unwrap()
        .contains(r#""pre_compaction_extraction":"noop_contract""#));
}

#[tokio::test]
async fn condensation_creates_higher_depth_summary_from_existing_leaf_nodes() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["one", "two", "three", "four", "five", "six"],
    )
    .await;
    let mut leaf_ids = Vec::new();
    for (idx, pair) in store_ids.chunks(2).enumerate() {
        let node = db
            .lcm_insert_summary_node(summary_draft(
                "cursor",
                "session-1",
                0,
                &format!("leaf summary {}", idx + 1),
                pair.iter()
                    .copied()
                    .map(|store_id| LcmSourceRef::RawMessage { store_id })
                    .collect(),
            ))
            .await
            .unwrap();
        leaf_ids.push(node.node_id);
    }
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "session-1".into(),
        current_session_id: "session-1".into(),
        current_frontier_store_id: store_ids.last().copied(),
        last_finalized_session_id: None,
        last_finalized_frontier_store_id: None,
        maintenance_debt: Vec::new(),
    })
    .await
    .unwrap();

    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            messages: Vec::new(),
            current_tokens: Some(100),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: Some(3),
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "depth one condensed".into(),
            },
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "condensed_summary_nodes");
    assert_eq!(response.summary_nodes_created, 1);
    let parent = &response.summary_nodes[0];
    assert_eq!(parent.depth, 1);
    assert_eq!(
        parent.source_refs,
        leaf_ids
            .iter()
            .cloned()
            .map(|node_id| LcmSourceRef::SummaryNode { node_id })
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn condensation_waits_for_one_depth_with_enough_unparented_nodes() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["one", "two", "three", "four", "five", "six"],
    )
    .await;
    let low = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "depth zero only child",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .unwrap();
    let high_one = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            1,
            "depth one child a",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[2],
            }],
        ))
        .await
        .unwrap();
    let high_two = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            1,
            "depth one child b",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[4],
            }],
        ))
        .await
        .unwrap();
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "session-1".into(),
        current_session_id: "session-1".into(),
        current_frontier_store_id: store_ids.last().copied(),
        last_finalized_session_id: None,
        last_finalized_frontier_store_id: None,
        maintenance_debt: Vec::new(),
    })
    .await
    .unwrap();

    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            messages: Vec::new(),
            current_tokens: Some(100),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: Some(3),
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "should not mix depths".into(),
            },
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "no_backlog_to_compress");
    assert_eq!(response.summary_nodes_created, 0);
    let status = db.lcm_status("cursor", Some("session-1")).await.unwrap();
    assert_eq!(status.summary_node_count, 3);
    assert_eq!(low.depth, 0);
    assert_eq!(high_one.depth, 1);
    assert_eq!(high_two.depth, 1);
}

#[tokio::test]
async fn condensation_orders_same_depth_candidates_by_source_time() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["one", "two", "three", "four", "five", "six"],
    )
    .await;
    let mut leaves = Vec::new();
    for (idx, pair) in store_ids.chunks(2).enumerate() {
        let leaf = db
            .lcm_insert_summary_node(summary_draft_with_times(
                "cursor",
                "session-1",
                0,
                &format!("leaf {}", idx + 1),
                pair.iter()
                    .copied()
                    .map(|store_id| LcmSourceRef::RawMessage { store_id })
                    .collect(),
                1_715_000_000 + (idx as i64 * 10),
                1_715_000_001 + (idx as i64 * 10),
            ))
            .await
            .unwrap();
        leaves.push(leaf);
    }
    let mut depth_one = Vec::new();
    for idx in [2_usize, 1, 0] {
        let parent = db
            .lcm_insert_summary_node(summary_draft_with_times(
                "cursor",
                "session-1",
                1,
                &format!("depth one {}", idx + 1),
                vec![LcmSourceRef::SummaryNode {
                    node_id: leaves[idx].node_id.clone(),
                }],
                1_715_000_000 + (idx as i64 * 10),
                1_715_000_001 + (idx as i64 * 10),
            ))
            .await
            .unwrap();
        depth_one.push(parent);
    }
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "session-1".into(),
        current_session_id: "session-1".into(),
        current_frontier_store_id: store_ids.last().copied(),
        last_finalized_session_id: None,
        last_finalized_frontier_store_id: None,
        maintenance_debt: Vec::new(),
    })
    .await
    .unwrap();

    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            messages: Vec::new(),
            current_tokens: Some(100),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: Some(3),
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "depth two condensed".into(),
            },
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "condensed_summary_nodes");
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(response.summary_nodes[0].depth, 2);
    assert_eq!(
        response.summary_nodes[0].source_refs,
        depth_one
            .iter()
            .rev()
            .map(|node| LcmSourceRef::SummaryNode {
                node_id: node.node_id.clone()
            })
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn forced_overflow_recovery_compacts_additional_backlog_and_reports_reason() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "system policy anchor"),
            ("user", "old user one"),
            ("assistant", "old assistant one"),
            ("user", "old user two"),
            ("assistant", "old assistant two"),
            ("user", "fresh user"),
            ("assistant", "fresh assistant"),
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "forced overflow summary".into(),
        },
        Some(2),
        Some(1),
        Some(20),
    );
    request.current_tokens = Some(200);
    let response = db.lcm_compress(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "forced_overflow_recovery");
    assert_eq!(response.summary_nodes_created, 4);
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[4])
    );
    assert!(response.frontier.maintenance_debt.is_empty());
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "system policy anchor",
            "forced overflow summary",
            "forced overflow summary",
            "forced overflow summary",
            "forced overflow summary",
            "fresh user",
            "fresh assistant",
        ]
    );
    let mut expanded_sources = 0;
    for node in &response.summary_nodes {
        expanded_sources += db
            .lcm_expand_summary_node("cursor", "session-1", &node.node_id)
            .await
            .unwrap()
            .sources
            .len();
    }
    assert_eq!(expanded_sources, 4);
}

#[tokio::test]
async fn forced_overflow_triggers_at_configured_cap_and_catches_up_in_passes() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "old-1 token",
            "old-2 token",
            "old-3 token",
            "old-4 token",
            "fresh-1",
            "fresh-2",
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "catchup summary".into(),
        },
        Some(4),
        Some(2),
        Some(40),
    );
    request.current_tokens = Some(40);
    let response = db.lcm_compress(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "forced_overflow_recovery");
    assert_eq!(response.summary_nodes_created, 2);
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[3])
    );
    assert!(response.frontier.maintenance_debt.is_empty());
}

#[tokio::test]
async fn forced_overflow_without_backlog_reports_irreducible_best_effort() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "system anchor words"),
            ("user", "fresh tail words that cannot be compacted"),
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "unused summary".into(),
        },
        None,
        None,
        Some(3),
    );
    request.current_tokens = Some(3);
    let response = db.lcm_compress(request).await.unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "best_effort");
    assert_eq!(response.reason, "irreducible_overflow_no_backlog");
    assert_eq!(response.summary_nodes_created, 0);
    assert_eq!(response_json["replay_over_budget"], true);
    assert!(response_json["replay_token_estimate"].as_i64().unwrap() > 3);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "system anchor words".to_string(),
            "fresh tail words that cannot be compacted".to_string()
        ]
    );
}

#[tokio::test]
async fn forced_overflow_reports_best_effort_when_replay_still_exceeds_cap() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "system anchor words words"),
            ("user", "old backlog one"),
            ("assistant", "old backlog two"),
            ("user", "fresh tail words words"),
            ("assistant", "fresh assistant words words"),
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "small summary".into(),
        },
        None,
        None,
        Some(6),
    );
    request.current_tokens = Some(20);
    let response = db.lcm_compress(request).await.unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "best_effort");
    assert_eq!(
        response.reason,
        "forced_overflow_recovery_replay_over_budget"
    );
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(response_json["replay_over_budget"], true);
    assert!(response_json["replay_token_estimate"].as_i64().unwrap() > 6);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "system anchor words words".to_string(),
            "small summary".to_string(),
            "fresh tail words words".to_string(),
            "fresh assistant words words".to_string()
        ]
    );
}

#[tokio::test]
async fn non_compressing_fake_summary_falls_back_deterministically() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "alpha beta gamma delta epsilon zeta eta theta",
            "iota kappa lambda mu nu xi omicron pi",
            "fresh-1",
            "fresh-2",
        ],
    )
    .await;

    let response = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "oversized ".repeat(100),
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "compressed_backlog_with_fallback_summary");
    let summary = &response.summary_nodes[0];
    assert!(summary
        .summary_text
        .starts_with("[deterministic LCM summary:"));
    assert!(summary.summary_token_count < summary.source_token_count);
}

#[tokio::test]
async fn maintenance_debt_clears_when_retry_compacts_remaining_backlog() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "old-1 token",
            "old-2 token",
            "old-3 token",
            "old-4 token",
            "fresh-1",
            "fresh-2",
        ],
    )
    .await;

    let first = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "first retry summary".into(),
            },
            Some(4),
            Some(2),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        first.frontier.maintenance_debt,
        vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: store_ids[2],
            to_store_id: store_ids[3],
        }]
    );

    let second = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "second retry summary".into(),
            },
            Some(4),
            Some(2),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(second.status, "ok");
    assert_eq!(second.reason, "compressed_backlog");
    assert_eq!(
        second.frontier.current_frontier_store_id,
        Some(store_ids[3])
    );
    assert!(second.frontier.maintenance_debt.is_empty());
}

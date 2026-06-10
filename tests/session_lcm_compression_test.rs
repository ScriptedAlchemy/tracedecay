use serde_json::{json, Value};
use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::{
    LcmCompressionRequest, LcmGrepRequest, LcmGrepSort, LcmLifecycleUpdate, LcmLoadSessionRequest,
    LcmMaintenanceDebt, LcmPreflightRequest, LcmScope, LcmSessionBoundaryRequest, LcmSourceRef,
    LcmStorageKind, LcmSummarizerMode, LcmSummaryNodeDraft, MAX_DERIVED_SNIPPET_CHARS,
};
use tokensave::sessions::{SessionMessageRecord, SessionRecord};

mod common;

fn isolated_db_path(tmp: &TempDir) -> std::path::PathBuf {
    common::isolated_lcm_db_path(tmp)
}

async fn open_lcm_db(tmp: &TempDir) -> GlobalDb {
    common::open_lcm_db(tmp).await
}

fn sample_session(provider: &str, session_id: &str) -> SessionRecord {
    common::session_record(
        provider,
        session_id,
        "/tmp/project",
        "LCM compression test",
        None,
        None,
    )
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
    let mut message = common::message_record(
        provider, message_id, session_id, role, ordinal, text, "message", None, None, None, None,
    );
    message.timestamp = Some(1_715_000_000 + ordinal);
    message
}

async fn insert_session(db: &GlobalDb, provider: &str, session_id: &str) {
    assert!(
        db.upsert_session(&sample_session(provider, session_id))
            .await
    );
}

fn externalized_ref_from_placeholder(text: &str) -> String {
    let marker = "ref=";
    let start = text.find(marker).expect("placeholder ref") + marker.len();
    let tail = &text[start..];
    let end = tail.find([']', ',', ';']).unwrap_or(tail.len());
    tail[..end].trim().to_string()
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

fn preflight_request(
    provider: &str,
    session_id: &str,
    messages: Vec<Value>,
    current_tokens: Option<i64>,
) -> LcmPreflightRequest {
    LcmPreflightRequest {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        messages,
        current_tokens,
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
    }
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
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
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
        threshold_tokens: None,
        max_assembly_tokens,
        leaf_chunk_tokens,
        max_source_messages,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
        summarizer,
    }
}

fn boundary_request(
    session_id: &str,
    old_session_id: &str,
    bound_session_id: Option<&str>,
) -> LcmSessionBoundaryRequest {
    LcmSessionBoundaryRequest {
        provider: "cursor".to_string(),
        session_id: session_id.to_string(),
        old_session_id: Some(old_session_id.to_string()),
        boundary_reason: Some("compression".to_string()),
        bound_session_id: bound_session_id.map(str::to_string),
        boundary_skip_at: None,
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs() as i64
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
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
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
            roles: Vec::new(),
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
async fn active_structured_content_survives_preflight_and_noop_compress_replay() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-structured").await;

    let content_array = json!([
        {"type": "text", "text": "first structured block"},
        {"type": "input_json", "value": {"answer": 42, "nested": ["a", "b"]}},
    ]);
    let content_object = json!({
        "type": "structured_payload",
        "parts": [
            {"kind": "text", "content": "object structured block"},
            {"kind": "data", "value": {"ok": true}},
        ],
    });
    let messages = vec![
        json!({"id": "structured-array", "role": "user", "content": content_array.clone()}),
        json!({"id": "structured-object", "role": "assistant", "content": content_object.clone()}),
    ];

    let preflight = db
        .lcm_preflight(LcmPreflightRequest {
            provider: "cursor".into(),
            session_id: "session-structured".into(),
            messages: messages.clone(),
            current_tokens: Some(100),
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(preflight.status, "ok");
    assert_eq!(preflight.replay_messages[0]["content"], content_array);
    assert_eq!(preflight.replay_messages[1]["content"], content_object);

    let compress = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-structured".into(),
            messages,
            current_tokens: Some(100),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::Noop,
        })
        .await
        .unwrap();
    assert_eq!(compress.status, "ok");
    assert_eq!(
        compress.replay_messages[0]["content"],
        preflight.replay_messages[0]["content"]
    );
    assert_eq!(
        compress.replay_messages[1]["content"],
        preflight.replay_messages[1]["content"]
    );

    let raw = db
        .lcm_load_raw_message("cursor", "structured-array")
        .await
        .expect("structured raw message should exist");
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["active_replay"]["content"],
        preflight.replay_messages[0]["content"]
    );
}

#[tokio::test]
async fn active_replay_preserves_top_level_fields_that_collide_with_storage_metadata() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-collision").await;

    let active_message = json!({
        "id": "structured-collision",
        "role": "user",
        "content": [
            {"type": "text", "text": "collision structured block"},
            {"type": "input_json", "value": {"nested": true}},
        ],
        "payload_ref": "user-payload-ref",
        "byte_count": 12345,
        "char_count": 678,
        "sha256": "user-sha256",
        "external_payload": {"kind": "user-field"},
        "ingest_protection": {"kind": "user-metadata"},
    });

    let preflight = db
        .lcm_preflight(LcmPreflightRequest {
            provider: "cursor".into(),
            session_id: "session-collision".into(),
            messages: vec![active_message.clone()],
            current_tokens: Some(100),
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(preflight.replay_messages[0], active_message);

    let raw = db
        .lcm_load_raw_message("cursor", "structured-collision")
        .await
        .expect("structured raw message should exist");
    let replay_from_raw = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-collision".into(),
            messages: Vec::new(),
            current_tokens: Some(100),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "unused".into(),
            },
        })
        .await
        .unwrap();

    let mut expected = active_message;
    expected["store_id"] = Value::from(raw.store_id);
    assert_eq!(replay_from_raw.replay_messages, vec![expected]);
}

#[tokio::test]
async fn raw_replay_preserves_assistant_tool_calls_and_tool_result_linking() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-tools").await;

    let tool_call = json!({
        "id": "call_lookup",
        "type": "function",
        "function": {"name": "lookup", "arguments": "{\"query\":\"parity\"}"},
    });
    let messages = vec![
        json!({
            "id": "assistant-tools",
            "role": "assistant",
            "content": [{"type": "text", "text": "I will look that up."}],
            "tool_calls": [tool_call.clone()],
        }),
        json!({
            "id": "tool-result",
            "role": "tool",
            "tool_call_id": "call_lookup",
            "name": "lookup",
            "content": [{"type": "text", "text": "lookup result"}],
        }),
    ];

    db.lcm_preflight(LcmPreflightRequest {
        provider: "cursor".into(),
        session_id: "session-tools".into(),
        messages,
        current_tokens: Some(100),
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
    })
    .await
    .unwrap();

    let replay_from_raw = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-tools".into(),
            messages: Vec::new(),
            current_tokens: Some(100),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "unused".into(),
            },
        })
        .await
        .unwrap();

    assert_eq!(replay_from_raw.replay_messages.len(), 2);
    assert_eq!(replay_from_raw.replay_messages[0]["role"], "assistant");
    assert_eq!(
        replay_from_raw.replay_messages[0]["tool_calls"],
        json!([tool_call])
    );
    assert_eq!(replay_from_raw.replay_messages[1]["role"], "tool");
    assert_eq!(
        replay_from_raw.replay_messages[1]["tool_call_id"],
        "call_lookup"
    );
    assert_eq!(replay_from_raw.replay_messages[1]["name"], "lookup");
}

#[tokio::test]
async fn active_replay_tool_calls_apply_ingest_protection_and_externalize_media_spans() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-tool-calls-protection").await;

    let media_payload = format!("data:image/png;base64,{}", "A".repeat(9_000));
    let tool_args = serde_json::to_string(&json!({
        "query": "parity",
        "image": media_payload,
        "note": "tool-call-suffix-canary",
    }))
    .expect("tool call arguments should serialize");

    let preflight = db
        .lcm_preflight(preflight_request(
            "cursor",
            "session-tool-calls-protection",
            vec![json!({
                "id": "assistant-tool-calls-protected",
                "role": "assistant",
                "content": "I will look that up.",
                "tool_calls": [{
                    "id": "call_media",
                    "type": "function",
                    "api_key": "sk-tool-calls-1234567890abcdef",
                    "function": {"name": "lookup", "arguments": tool_args},
                }],
                "lcm_ingest": {
                    "sensitive_patterns_enabled": true,
                    "sensitive_patterns": ["api_key"],
                },
            })],
            Some(100),
        ))
        .await
        .unwrap();

    assert_eq!(preflight.status, "ok");
    assert!(preflight.should_compress);
    assert_eq!(preflight.reason, "ingest_protection_changed_replay");
    let protected_args = preflight.replay_messages[0]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("protected tool-call arguments should stay stringified JSON");
    assert!(protected_args.contains("[Externalized LCM ingest payload:"));
    assert!(protected_args.contains("tool-call-suffix-canary"));
    assert!(!protected_args.contains("data:image/png;base64"));
    let protected_tool_call = preflight.replay_messages[0]["tool_calls"][0].to_string();
    assert!(!protected_tool_call.contains("sk-tool-calls-1234567890abcdef"));

    let payload_ref = externalized_ref_from_placeholder(protected_args);
    let expanded = db
        .lcm_store(tmp.path().join(".tokensave"))
        .lcm_expand_payload(
            "cursor",
            "session-tool-calls-protection",
            &payload_ref,
            0,
            media_payload.chars().count(),
        )
        .await
        .expect("tool-calls payload should remain losslessly recoverable");
    assert_eq!(expanded.content, media_payload);

    let replay_from_raw = db
        .lcm_compress(compress_request(
            "cursor",
            "session-tool-calls-protection",
            LcmSummarizerMode::Fake {
                summary_text: "unused".into(),
            },
        ))
        .await
        .unwrap();
    let replay_args = replay_from_raw.replay_messages[0]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("stored replay should preserve protected tool-call arguments");
    assert!(replay_args.contains("[Externalized LCM ingest payload:"));
    assert!(!replay_args.contains("data:image/png;base64"));
}

#[tokio::test]
async fn nested_media_placeholder_remains_inside_structured_active_content() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-media").await;

    let media_payload = format!("data:image/png;base64,{}", "A".repeat(100_000));
    let response = db
        .lcm_preflight(LcmPreflightRequest {
            provider: "cursor".into(),
            session_id: "session-media".into(),
            messages: vec![json!({
                "id": "structured-media",
                "role": "user",
                "content": [
                    {"type": "text", "text": "Please inspect the screenshot."},
                    {"type": "image_url", "image_url": {"url": media_payload}},
                ],
            })],
            current_tokens: Some(100),
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert!(response.should_compress);
    assert_eq!(response.reason, "ingest_protection_changed_replay");
    let replay_content = response.replay_messages[0]["content"]
        .as_array()
        .expect("structured content should stay an array");
    assert_eq!(replay_content[0]["text"], "Please inspect the screenshot.");
    let url = replay_content[1]["image_url"]["url"]
        .as_str()
        .expect("media URL should remain in structured position");
    assert!(url.contains("[Externalized LCM ingest payload:"));
    assert!(!url.contains("data:image/png;base64"));

    let raw = db
        .lcm_load_raw_message("cursor", "structured-media")
        .await
        .expect("structured media raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::Inline);
    assert!(raw.content.contains("[Externalized LCM ingest payload:"));
    assert!(!raw.content.contains("data:image/png;base64"));

    let payload_ref = externalized_ref_from_placeholder(&raw.content);
    let expanded = db
        .lcm_store(tmp.path().join(".tokensave"))
        .lcm_expand_payload(
            "cursor",
            "session-media",
            &payload_ref,
            0,
            media_payload.chars().count(),
        )
        .await
        .expect("nested media payload should expand");
    assert_eq!(expanded.content, media_payload);
}

#[tokio::test]
async fn structured_active_content_replay_preserves_shape_while_grep_snippet_stays_bounded() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-bounded").await;

    let long_text = format!(
        "bounded-structured-canary {} ::structured-tail",
        "x".repeat(MAX_DERIVED_SNIPPET_CHARS * 4)
    );
    let content = json!([
        {"type": "text", "text": long_text},
        {"type": "metadata", "value": {"shape": "kept"}},
    ]);
    let response = db
        .lcm_preflight(LcmPreflightRequest {
            provider: "cursor".into(),
            session_id: "session-bounded".into(),
            messages: vec![json!({
                "id": "structured-bounded",
                "role": "user",
                "content": content.clone(),
            })],
            current_tokens: Some(100),
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(response.replay_messages[0]["content"], content);

    let hits = db
        .lcm_grep(LcmGrepRequest {
            provider: "cursor".into(),
            query: "bounded-structured-canary".into(),
            scope: LcmScope::Session,
            session_id: Some("session-bounded".into()),
            include_summaries: false,
            limit: 10,
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        })
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.chars().count() <= MAX_DERIVED_SNIPPET_CHARS);
    assert!(!hits[0].snippet.contains("::structured-tail"));
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
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
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
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
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
async fn ignore_message_patterns_skip_storage_but_heartbeat_noise_is_stored() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-noise").await;

    let preflight = db
        .lcm_preflight(LcmPreflightRequest {
            provider: "cursor".into(),
            session_id: "session-noise".into(),
            messages: vec![
                json!({"id": "heartbeat-1", "role": "assistant", "content": "Still working..."}),
                json!({"id": "cron-noise-1", "role": "user", "content": "Cronjob Response: noisy heartbeat"}),
                json!({"id": "valuable-1", "role": "user", "content": "real user request"}),
            ],
            current_tokens: Some(100),
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: vec!["Cronjob Response:*".into()],
        })
        .await
        .unwrap();

    assert_eq!(
        preflight
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
    let preflight_page = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-noise".into(),
            after_store_id: None,
            limit: 10,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .unwrap();
    assert_eq!(
        preflight_page
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["Still working...", "real user request"]
    );

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
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
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
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .unwrap();
    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["Still working...", "real user request"]
    );
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
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
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
        .contains("[Externalized LCM ingest payload"));
}

#[tokio::test]
async fn preflight_requests_compression_for_over_threshold_eligible_backlog() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["old-1 token", "old-2 token", "fresh-1", "fresh-2"],
    )
    .await;

    let mut request = preflight_request("cursor", "session-1", Vec::new(), Some(120));
    request.threshold_tokens = Some(100);

    let response = db.lcm_preflight(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert!(response.should_compress);
    assert_eq!(response.reason, "threshold_backlog_ready");
}

#[tokio::test]
async fn preflight_skips_threshold_when_backlog_below_leaf_chunk_threshold() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(&db, "cursor", "session-1", &["tiny", "fresh-1", "fresh-2"]).await;

    let mut request = preflight_request("cursor", "session-1", Vec::new(), Some(120));
    request.threshold_tokens = Some(100);
    request.leaf_chunk_tokens = Some(10);
    request.max_source_messages = Some(2);

    let response = db.lcm_preflight(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert!(!response.should_compress);
    assert_eq!(response.reason, "threshold_no_eligible_backlog");
}

#[tokio::test]
async fn preflight_threshold_eligibility_uses_full_backlog_despite_source_message_cap() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["m1", "m2", "m3", "m4", "m5", "m6", "fresh-1", "fresh-2"],
    )
    .await;

    let mut request = preflight_request("cursor", "session-1", Vec::new(), Some(120));
    request.threshold_tokens = Some(100);
    request.leaf_chunk_tokens = Some(5);
    request.max_source_messages = Some(2);

    let response = db.lcm_preflight(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert!(response.should_compress);
    assert_eq!(response.reason, "threshold_backlog_ready");
}

#[tokio::test]
async fn preflight_requests_compression_for_forced_overflow_without_replay_change() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[("system", "system anchor"), ("user", "fresh user")],
    )
    .await;

    let mut request = preflight_request("cursor", "session-1", Vec::new(), Some(50));
    request.max_assembly_tokens = Some(50);

    let response = db.lcm_preflight(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert!(response.should_compress);
    assert_eq!(response.reason, "forced_overflow_pressure");
}

// Mirrors hermes-lcm `_effective_assembly_token_cap`: with no explicit
// max_assembly_tokens, the assembly cap derives from
// context_length - reserve_tokens_floor when both are positive.
#[tokio::test]
async fn preflight_derives_forced_overflow_cap_from_context_window_reserve_floor() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[("system", "system anchor"), ("user", "fresh user")],
    )
    .await;

    let mut request = preflight_request("cursor", "session-1", Vec::new(), Some(50));
    request.context_length = Some(80);
    request.reserve_tokens_floor = Some(30);

    let response = db.lcm_preflight(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert!(response.should_compress);
    assert_eq!(response.reason, "forced_overflow_pressure");
}

// Mirrors hermes-lcm: a reserve floor that consumes the whole context window
// disables the reserve-based cap instead of clamping it to zero.
#[tokio::test]
async fn preflight_reserve_floor_without_headroom_disables_derived_cap() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[("system", "system anchor"), ("user", "fresh user")],
    )
    .await;

    let mut request = preflight_request("cursor", "session-1", Vec::new(), Some(50));
    request.context_length = Some(30);
    request.reserve_tokens_floor = Some(30);

    let response = db.lcm_preflight(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert!(!response.should_compress);
    assert_eq!(response.reason, "no_compression_needed");
}

// Mirrors hermes-lcm: when both an explicit max_assembly_tokens and a
// reserve-derived cap apply, the effective cap is the minimum of the two.
#[tokio::test]
async fn preflight_effective_cap_uses_minimum_of_explicit_and_reserve_derived() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[("system", "system anchor"), ("user", "fresh user")],
    )
    .await;

    let mut request = preflight_request("cursor", "session-1", Vec::new(), Some(50));
    request.max_assembly_tokens = Some(200);
    request.context_length = Some(80);
    request.reserve_tokens_floor = Some(30);

    let response = db.lcm_preflight(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert!(response.should_compress);
    assert_eq!(response.reason, "forced_overflow_pressure");
}

#[tokio::test]
async fn compress_forces_overflow_recovery_with_reserve_derived_cap() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "forced summary".into(),
        },
        Some(10),
        None,
        None,
    );
    request.context_length = Some(150);
    request.reserve_tokens_floor = Some(50);

    let response = db.lcm_compress(request).await.unwrap();

    assert_eq!(response.reason, "forced_overflow_recovery");
    assert!(response.summary_nodes_created >= 1);
}

#[tokio::test]
async fn preflight_requests_compression_for_maintenance_debt() {
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
                summary_text: "first chunk summary".into(),
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

    let response = db
        .lcm_preflight(preflight_request(
            "cursor",
            "session-1",
            Vec::new(),
            Some(10),
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert!(response.should_compress);
    assert_eq!(response.reason, "maintenance_debt_ready");
}

// Mirrors hermes-lcm `_compression_boundary_cooldown_active`: after a
// compression-boundary session start whose old_session_id does not match the
// bound session (skip-carry-over), preflight must not request compression
// again until the 60-second cooldown elapses — but it must keep ingesting.
#[tokio::test]
async fn boundary_skip_starts_preflight_compression_cooldown() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-b",
        &["old-1 token", "old-2 token", "fresh-1", "fresh-2"],
    )
    .await;

    let boundary = db
        .lcm_session_boundary(boundary_request(
            "session-b",
            "session-c",
            Some("session-a"),
        ))
        .await
        .unwrap();
    assert!(boundary.recorded);
    assert_eq!(boundary.reason, "compression_boundary_skip_recorded");

    let mut request = preflight_request(
        "cursor",
        "session-b",
        vec![json!({"id": "fresh-user", "role": "user", "content": "fresh preflight payload"})],
        Some(120),
    );
    request.threshold_tokens = Some(100);

    let response = db.lcm_preflight(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert!(!response.should_compress);
    assert_eq!(response.reason, "compression_boundary_cooldown");
    // Cooldown is lossless: fresh messages were still ingested and replayed.
    assert_eq!(response.replay_messages.len(), 1);
    assert!(db
        .lcm_load_raw_message("cursor", "fresh-user")
        .await
        .is_some());
}

#[tokio::test]
async fn boundary_cooldown_blocks_replay_diff_compression() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;

    let boundary = db
        .lcm_session_boundary(boundary_request(
            "session-1",
            "session-c",
            Some("session-a"),
        ))
        .await
        .unwrap();
    assert!(boundary.recorded);

    let request = preflight_request(
        "cursor",
        "session-1",
        vec![json!({
            "id": "protected-1",
            "role": "assistant",
            "content": format!("data:image/png;base64,{}", "A".repeat(100_000))
        })],
        Some(100),
    );

    let response = db.lcm_preflight(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert!(!response.should_compress);
    assert_eq!(response.reason, "compression_boundary_cooldown");
}

#[tokio::test]
async fn boundary_cooldown_expires_after_sixty_seconds() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-b",
        &["old-1 token", "old-2 token", "fresh-1", "fresh-2"],
    )
    .await;

    let mut boundary = boundary_request("session-b", "session-c", Some("session-a"));
    boundary.boundary_skip_at = Some(unix_now() - 61);
    let recorded = db.lcm_session_boundary(boundary).await.unwrap();
    assert!(recorded.recorded);

    let mut request = preflight_request("cursor", "session-b", Vec::new(), Some(120));
    request.threshold_tokens = Some(100);

    let response = db.lcm_preflight(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert!(response.should_compress);
    assert_eq!(response.reason, "threshold_backlog_ready");
}

// Mirrors hermes-lcm: when old_session_id matches the bound session, the
// compression boundary continues (Hermes carries LCM data over to the new
// session id) and no cooldown starts.
#[tokio::test]
async fn boundary_continuation_with_matching_bound_session_records_no_cooldown() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-a",
        &["old-1 token", "old-2 token", "fresh-1", "fresh-2"],
    )
    .await;

    let boundary = db
        .lcm_session_boundary(boundary_request(
            "session-b",
            "session-a",
            Some("session-a"),
        ))
        .await
        .unwrap();
    assert!(boundary.recorded);
    assert_eq!(boundary.reason, "compression_boundary_carried_over");

    let mut request = preflight_request("cursor", "session-b", Vec::new(), Some(120));
    request.threshold_tokens = Some(100);

    let response = db.lcm_preflight(request).await.unwrap();

    assert!(response.should_compress);
    assert_eq!(response.reason, "threshold_backlog_ready");
}

#[tokio::test]
async fn non_compression_boundary_records_no_cooldown() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-b").await;

    let mut manual = boundary_request("session-b", "session-c", Some("session-a"));
    manual.boundary_reason = Some("manual".to_string());
    let response = db.lcm_session_boundary(manual).await.unwrap();
    assert!(!response.recorded);
    assert_eq!(response.reason, "not_compression_boundary");

    let mut same_session = boundary_request("session-b", "session-b", Some("session-a"));
    same_session.boundary_reason = Some("compression".to_string());
    let response = db.lcm_session_boundary(same_session).await.unwrap();
    assert!(!response.recorded);
    assert_eq!(response.reason, "not_compression_boundary");
}

#[tokio::test]
async fn compress_noops_for_sub_threshold_backlog_in_threshold_mode() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let response = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "should not be written".into(),
            },
            Some(10),
            None,
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "backlog_below_leaf_chunk_threshold");
    assert_eq!(response.summary_nodes_created, 0);
    assert!(response.summary_nodes.is_empty());
    assert!(response.summary_request.is_none());
    let replay = response
        .replay_messages
        .iter()
        .map(|message| message["content"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(replay, vec!["old-1", "old-2", "fresh-1", "fresh-2"]);
    assert_eq!(response.frontier.current_frontier_store_id, None);
    assert!(response.frontier.maintenance_debt.is_empty());
}

#[tokio::test]
async fn compress_noop_guard_fires_before_auxiliary_summary_request() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let response = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::HermesAuxiliary,
            Some(10),
            None,
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "backlog_below_leaf_chunk_threshold");
    assert_eq!(response.summary_nodes_created, 0);
    assert!(response.summary_request.is_none());
}

#[tokio::test]
async fn compress_proceeds_at_exact_leaf_chunk_threshold() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    // Backlog tokens == leaf_chunk_tokens: hermes-lcm only no-ops on a strict
    // `<` comparison, so the boundary case must still compress.
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["alpha beta", "gamma delta", "fresh-1", "fresh-2"],
    )
    .await;

    let response = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "boundary summary".into(),
            },
            Some(4),
            None,
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 1);
}

#[tokio::test]
async fn forced_overflow_compresses_sub_threshold_backlog() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let response = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "forced summary".into(),
            },
            Some(10),
            None,
            Some(100),
        ))
        .await
        .unwrap();

    assert_eq!(response.reason, "forced_overflow_recovery");
    assert!(response.summary_nodes_created >= 1);
}

#[tokio::test]
async fn maintenance_debt_bypasses_sub_threshold_noop_guard() {
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
                summary_text: "first chunk summary".into(),
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

    // Remaining backlog is 4 tokens, below the 50-token leaf chunk threshold,
    // but outstanding maintenance debt must keep compression flowing.
    let response = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "debt catch-up summary".into(),
            },
            Some(50),
            None,
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 1);
    assert!(response.frontier.maintenance_debt.is_empty());
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
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
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
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
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
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
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
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
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
    let extraction_request = summary_request
        .extraction_request
        .as_ref()
        .expect("auxiliary summary request should include extraction contract");
    assert_eq!(extraction_request.session_id, "session-1");
    assert_eq!(
        extraction_request.source_range,
        summary_request.source_range
    );
    assert!(extraction_request.prompt.contains("NOTHING_TO_EXTRACT"));
    assert!(extraction_request.prompt.contains("[ASSISTANT]: old-1"));
    assert!(extraction_request.prompt.contains("[ASSISTANT]: old-2"));
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
async fn provided_route_envelope_persists_extraction_metadata() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(&db, "cursor", "session-1", &["old-1", "old-2", "fresh-1"]).await;

    let response = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Provided {
                summary_text: "summary with extraction".into(),
                route: Some(
                    json!({
                        "route": "backup",
                        "pre_compaction_extraction": {
                            "status": "ok",
                            "items": [
                                "Decision: keep nightly backups",
                                "Commitment: rotate keys weekly"
                            ],
                            "model": "openai/gpt-5.4-mini",
                            "output_path": "/tmp/extractions"
                        }
                    })
                    .to_string(),
                ),
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 1);
    let metadata: Value = serde_json::from_str(
        response.summary_nodes[0]
            .metadata_json
            .as_deref()
            .expect("summary metadata"),
    )
    .unwrap();
    assert_eq!(
        metadata["summary_route"],
        Value::String("backup".to_string())
    );
    assert_eq!(
        metadata["pre_compaction_extraction"]["status"],
        Value::String("ok".to_string())
    );
    assert_eq!(
        metadata["pre_compaction_extraction"]["items"],
        json!([
            "Decision: keep nightly backups",
            "Commitment: rotate keys weekly"
        ])
    );
    assert_eq!(
        metadata["pre_compaction_extraction"]["model"],
        Value::String("openai/gpt-5.4-mini".to_string())
    );
}

#[tokio::test]
async fn zero_leaf_chunk_tokens_disables_threshold_guard() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-zero-leaf",
        &["old one", "old two", "fresh one"],
    )
    .await;

    let blocked = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-zero-leaf".into(),
            messages: Vec::new(),
            current_tokens: Some(1_000),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: Some(100_000),
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: Some(1),
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "should not be used".into(),
            },
        })
        .await
        .unwrap();
    assert_eq!(blocked.reason, "backlog_below_leaf_chunk_threshold");
    assert_eq!(blocked.summary_nodes_created, 0);

    let allowed = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-zero-leaf".into(),
            messages: Vec::new(),
            current_tokens: Some(1_000),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: Some(0),
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: Some(1),
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "zero leaf summary".into(),
            },
        })
        .await
        .unwrap();
    assert_eq!(allowed.reason, "compressed_backlog");
    assert_eq!(allowed.summary_nodes_created, 1);
}

#[tokio::test]
async fn zero_fresh_tail_count_keeps_no_raw_tail() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-zero-tail",
        &["first", "second", "third"],
    )
    .await;

    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "session-zero-tail".into(),
            messages: Vec::new(),
            current_tokens: Some(1_000),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: Some(0),
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "zero tail summary".into(),
            },
        })
        .await
        .unwrap();

    assert_eq!(response.reason, "compressed_backlog");
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(
        response.summary_nodes[0]
            .source_refs
            .iter()
            .filter_map(|source| match source {
                LcmSourceRef::RawMessage { store_id } => Some(*store_id),
                _ => None,
            })
            .collect::<Vec<_>>(),
        store_ids
    );
    assert_eq!(
        response
            .replay_messages
            .iter()
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>(),
        vec!["zero tail summary"]
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
    let metadata: Value =
        serde_json::from_str(response.summary_nodes[0].metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["pre_compaction_extraction"]["status"],
        Value::String("not_requested".to_string())
    );
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
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: Some(3),
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
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
    // Mirrors hermes-lcm `_assemble_context` after `_maybe_condense`: a
    // condensation-only pass still returns the assembled active context, not
    // an empty replay.
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec!["depth one condensed"]
    );
    assert_eq!(
        response.replay_messages[0]["lcm_summary_node_id"],
        parent.node_id.as_str()
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
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: Some(3),
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
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
    // Insert depth-0 leaves in reverse chronological creation order so that
    // candidate ordering must come from source times, not insertion order.
    let mut leaves = vec![None, None, None];
    for idx in [2_usize, 1, 0] {
        let pair = &store_ids[idx * 2..idx * 2 + 2];
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
        leaves[idx] = Some(leaf);
    }
    let leaves = leaves
        .into_iter()
        .map(|leaf| leaf.unwrap())
        .collect::<Vec<_>>();
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
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: Some(3),
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::Fake {
                summary_text: "depth one condensed".into(),
            },
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "condensed_summary_nodes");
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(response.summary_nodes[0].depth, 1);
    assert_eq!(
        response.summary_nodes[0].source_refs,
        leaves
            .iter()
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

// Mirrors hermes-lcm `_assemble_context` budget enforcement: tail messages
// that do not fit under the assembly cap are dropped (newest kept first) and
// the summary block is budgeted, instead of returning over-cap replay.
#[tokio::test]
async fn forced_overflow_trims_replay_to_assembly_cap() {
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

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "forced_overflow_recovery");
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(response_json["replay_over_budget"], false);
    assert!(response_json["replay_token_estimate"].as_i64().unwrap() <= 6);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "system anchor words words".to_string(),
            "small summary".to_string(),
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
async fn non_compressing_summary_reports_fallback_attempt_state() {
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
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "compressed_backlog_with_fallback_summary");
    assert_eq!(response_json["compression_attempts"], 1);
    assert_eq!(response_json["fallback_used"], true);
    assert_eq!(
        response_json["retry_status"].as_str(),
        Some("fallback_summary")
    );
    assert!(response.frontier.maintenance_debt.is_empty());
    assert_eq!(response_json["replay_over_budget"], false);
}

#[tokio::test]
async fn critical_pressure_catch_up_reports_attempts_debt_and_budget_state() {
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
            "old-5 token",
            "old-6 token",
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
        Some(2),
        Some(1),
        Some(3),
    );
    request.current_tokens = Some(40);
    let response = db.lcm_compress(request).await.unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "forced_overflow_recovery");
    assert_eq!(response.summary_nodes_created, 4);
    assert_eq!(response_json["compression_attempts"], 4);
    assert_eq!(response_json["fallback_used"], false);
    assert_eq!(
        response_json["retry_status"].as_str(),
        Some("critical_pressure_catch_up")
    );
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[3])
    );
    assert_eq!(
        response.frontier.maintenance_debt,
        vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: store_ids[4],
            to_store_id: store_ids[5],
        }]
    );
    // Budget enforcement keeps the freshest tail and drops over-cap summary
    // blocks and deferred backlog from active replay; they stay recoverable
    // through the DAG and maintenance debt.
    assert_eq!(response_json["replay_over_budget"], false);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec!["fresh-1".to_string(), "fresh-2".to_string()]
    );
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

// Mirrors hermes-lcm `_assemble_context` loading all uncondensed DAG nodes:
// a follow-up compress with nothing new to compact must still replay the
// summaries persisted by earlier passes instead of dropping them.
#[tokio::test]
async fn no_backlog_compress_replays_persisted_uncondensed_summaries() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let first = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "old summary".into(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(first.summary_nodes_created, 1);

    let second = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "unused".into(),
            },
        ))
        .await
        .unwrap();

    assert_eq!(second.status, "ok");
    assert_eq!(second.reason, "no_backlog_to_compress");
    assert_eq!(second.summary_nodes_created, 0);
    assert_eq!(
        second
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "old summary".to_string(),
            "fresh-1".to_string(),
            "fresh-2".to_string(),
        ]
    );
    assert_eq!(
        second.replay_messages[0]["lcm_summary_node_id"],
        first.summary_nodes[0].node_id.as_str()
    );
}

// Mirrors hermes-lcm `_maybe_condense` with the default
// `incremental_max_depth = 1`: only depth-0 nodes are eligible for
// condensation, so unparented depth-1 nodes never get condensed to depth 2
// at default settings — they stay in active replay instead.
#[tokio::test]
async fn condensation_respects_default_incremental_max_depth() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["one", "two", "three", "four", "five", "six"],
    )
    .await;
    for (idx, pair) in store_ids.chunks(2).enumerate() {
        db.lcm_insert_summary_node(summary_draft_with_times(
            "cursor",
            "session-1",
            1,
            &format!("depth one {}", idx + 1),
            pair.iter()
                .copied()
                .map(|store_id| LcmSourceRef::RawMessage { store_id })
                .collect(),
            1_715_000_000 + (idx as i64 * 10),
            1_715_000_001 + (idx as i64 * 10),
        ))
        .await
        .unwrap();
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

    let mut request = compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "should not condense above max depth".into(),
        },
    );
    request.summary_fan_in = Some(3);
    let response = db.lcm_compress(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "no_backlog_to_compress");
    assert_eq!(response.summary_nodes_created, 0);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "depth one 1".to_string(),
            "depth one 2".to_string(),
            "depth one 3".to_string(),
        ]
    );
}

#[tokio::test]
async fn condensation_honors_non_default_incremental_max_depth() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["one", "two", "three", "four", "five", "six"],
    )
    .await;
    for (idx, pair) in store_ids.chunks(2).enumerate() {
        db.lcm_insert_summary_node(summary_draft_with_times(
            "cursor",
            "session-1",
            1,
            &format!("depth one {}", idx + 1),
            pair.iter()
                .copied()
                .map(|store_id| LcmSourceRef::RawMessage { store_id })
                .collect(),
            1_715_100_000 + (idx as i64 * 10),
            1_715_100_001 + (idx as i64 * 10),
        ))
        .await
        .unwrap();
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

    let mut request = compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "condensed depth one summaries".into(),
        },
    );
    request.summary_fan_in = Some(3);
    request.incremental_max_depth = Some(2);
    let response = db.lcm_compress(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "condensed_summary_nodes");
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(response.summary_nodes[0].depth, 2);
}

#[tokio::test]
async fn compression_reinjects_latest_user_objective_when_tail_is_tool_heavy() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "policy anchor"),
            ("user", "Ship OAuth login and preserve this objective."),
            ("assistant", "acknowledged"),
            ("tool", "first tool result payload"),
            ("assistant", "working on intermediate steps"),
            ("tool", "latest tool result payload"),
        ],
    )
    .await;

    let response = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "historical summary".into(),
            },
        ))
        .await
        .unwrap();

    let replay_contents = response
        .replay_messages
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>();
    assert!(replay_contents.iter().any(
        |content| content.contains("[Current user objective preserved from compacted history]")
    ));
    assert!(replay_contents
        .iter()
        .any(|content| content.contains("Ship OAuth login and preserve this objective.")));
}

#[tokio::test]
async fn overflow_recovery_keeps_preserved_objective_scaffold_when_evicting_tail() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "policy anchor stays"),
            (
                "assistant",
                "bulky derived assistant turn with many filler words that should be evicted",
            ),
            (
                "assistant",
                "[Current user objective preserved from compacted history]\nShip OAuth login now",
            ),
            ("user", "keep me"),
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
        Some(18),
    );
    request.current_tokens = Some(50);
    let response = db.lcm_compress(request).await.unwrap();
    let replay = response
        .replay_messages
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>();
    assert!(replay.iter().any(
        |content| content.contains("[Current user objective preserved from compacted history]")
    ));
    assert!(replay.contains(&"keep me"));
    assert!(!replay.iter().any(|content| {
        content.contains("bulky derived assistant turn with many filler words")
    }));
}

#[tokio::test]
async fn forced_overflow_replay_budget_accounts_for_prompt_overhead_delta() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "policy anchor words"),
            ("user", "fresh tail words"),
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
        Some(12),
    );
    // Host-observed prompt tokens include local overhead beyond the message
    // token estimate; overflow recovery should tighten the assembly cap.
    request.current_tokens = Some(20);
    request.messages = vec![
        json!({ "role": "system", "content": "policy anchor words" }),
        json!({ "role": "user", "content": "fresh tail words" }),
    ];
    let response = db.lcm_compress(request).await.unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "best_effort");
    assert_eq!(
        response.reason,
        "forced_overflow_recovery_replay_over_budget"
    );
    assert_eq!(response_json["replay_over_budget"], true);
}

// Mirrors hermes-lcm `_assemble_overflow_recovery_context`: with no backlog
// to compact, forced overflow evicts droppable assistant/tool tail turns that
// do not fit under the cap while keeping anchors and budgetable user intent.
#[tokio::test]
async fn overflow_recovery_without_backlog_evicts_droppable_tail() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "policy anchor stays"),
            (
                "assistant",
                "bulky derived assistant turn with many filler words here",
            ),
            ("user", "keep me"),
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
        Some(5),
    );
    request.current_tokens = Some(50);
    let response = db.lcm_compress(request).await.unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "overflow_recovery_no_backlog");
    assert_eq!(response.summary_nodes_created, 0);
    assert_eq!(response_json["replay_over_budget"], false);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec!["policy anchor stays".to_string(), "keep me".to_string()]
    );
}

// Mirrors hermes-lcm `_continue_compression_boundary` happy path
// (engine.py:1902-1923): when the host old_session_id matches the bound
// session, all LCM data is reassigned to the new session id and lifecycle
// state is finalized + rebound instead of orphaning the old session.
#[tokio::test]
async fn compression_boundary_carry_over_reassigns_lcm_data() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-old",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let first = db
        .lcm_compress(compress_request(
            "cursor",
            "session-old",
            LcmSummarizerMode::Fake {
                summary_text: "old summary".into(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(first.summary_nodes_created, 1);
    let node_id = first.summary_nodes[0].node_id.clone();

    let boundary = db
        .lcm_session_boundary(boundary_request(
            "session-new",
            "session-old",
            Some("session-old"),
        ))
        .await
        .unwrap();
    assert!(boundary.recorded);
    assert_eq!(boundary.reason, "compression_boundary_carried_over");

    // Raw messages moved to the new session id.
    let new_page = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-new".into(),
            after_store_id: None,
            limit: 10,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .unwrap();
    assert_eq!(new_page.messages.len(), 4);
    let old_page = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-old".into(),
            after_store_id: None,
            limit: 10,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .unwrap();
    assert!(old_page.messages.is_empty());

    // Summary node moved with its lineage intact.
    let expanded = db
        .lcm_expand_summary_node("cursor", "session-new", &node_id)
        .await
        .unwrap();
    assert_eq!(expanded.sources.len(), 2);

    // Lifecycle finalized for the old session and rebound to the new one.
    let state = db
        .lcm_lifecycle_state("cursor", "session-new")
        .await
        .unwrap();
    assert_eq!(state.current_session_id, "session-new");
    assert_eq!(state.current_frontier_store_id, Some(store_ids[1]));
    assert_eq!(
        state.last_finalized_session_id.as_deref(),
        Some("session-old")
    );
    assert_eq!(state.last_finalized_frontier_store_id, Some(store_ids[1]));
    assert!(db
        .lcm_lifecycle_state("cursor", "session-old")
        .await
        .is_err());

    // Carried summaries keep flowing into the new session's replay.
    let next = db
        .lcm_compress(compress_request(
            "cursor",
            "session-new",
            LcmSummarizerMode::Fake {
                summary_text: "unused".into(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(next.reason, "no_backlog_to_compress");
    assert_eq!(
        next.replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "old summary".to_string(),
            "fresh-1".to_string(),
            "fresh-2".to_string(),
        ]
    );
}

#[tokio::test]
async fn compression_boundary_carry_over_requires_empty_target_session() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(&db, "cursor", "session-old", &["old-1", "old-2"]).await;
    insert_raw_messages(&db, "cursor", "session-new", &["already-there"]).await;

    let err = db
        .lcm_session_boundary(boundary_request(
            "session-new",
            "session-old",
            Some("session-old"),
        ))
        .await
        .expect_err("carry-over must fail when target session already has raw rows");
    assert!(
        matches!(err, tokensave::sessions::lcm::LcmError::Db(message) if message.contains("empty target session"))
    );
}

// The carry-over moves externalized payload ownership and outstanding
// maintenance debt to the new session id, mirroring Hermes
// `reassign_externalized_payloads` and conversation-scoped debt continuity.
#[tokio::test]
async fn compression_boundary_carry_over_moves_payloads_and_maintenance_debt() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-old",
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
    let payload_body = format!("tool output\n{}", "X".repeat(300_000));
    let mut external_message = raw_message_with_role(
        "cursor",
        "session-old-tool-1",
        "session-old",
        "tool",
        7,
        &payload_body,
    );
    external_message.kind = Some("tool_result".to_string());
    let storage_root = tmp.path().join(".tokensave");
    db.lcm_store(&storage_root)
        .ingest_raw_message(&external_message)
        .await
        .unwrap();
    let payload_ref = db
        .lcm_load_raw_message("cursor", "session-old-tool-1")
        .await
        .unwrap()
        .payload_ref
        .expect("payload should externalize");

    let first = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-old",
            LcmSummarizerMode::Fake {
                summary_text: "first chunk summary".into(),
            },
            Some(4),
            Some(2),
            None,
        ))
        .await
        .unwrap();
    assert!(!first.frontier.maintenance_debt.is_empty());

    let boundary = db
        .lcm_session_boundary(boundary_request(
            "session-new",
            "session-old",
            Some("session-old"),
        ))
        .await
        .unwrap();
    assert!(boundary.recorded);
    assert_eq!(boundary.reason, "compression_boundary_carried_over");

    let state = db
        .lcm_lifecycle_state("cursor", "session-new")
        .await
        .unwrap();
    assert_eq!(
        state.maintenance_debt,
        vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: store_ids[2],
            to_store_id: store_ids[4],
        }]
    );

    let expansion = db
        .lcm_expand(tokensave::sessions::lcm::LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-new".into(),
            target: tokensave::sessions::lcm::LcmExpandTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        })
        .await
        .unwrap();
    assert!(expansion.content.starts_with("tool output"));
    assert!(db
        .lcm_expand(tokensave::sessions::lcm::LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-old".into(),
            target: tokensave::sessions::lcm::LcmExpandTarget::ExternalPayload { payload_ref },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        })
        .await
        .is_err());
}

// The carry-over runs in a single transaction; a rejected carry-over must
// leave the source session fully usable (rows, payload ownership, lifecycle
// frontier, and maintenance debt untouched) and write nothing for the target,
// so a later boundary to a genuinely empty session can still succeed.
#[tokio::test]
async fn failed_carry_over_leaves_source_session_state_intact() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-old",
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
    let payload_body = format!("tool output\n{}", "Y".repeat(300_000));
    let mut external_message = raw_message_with_role(
        "cursor",
        "session-old-tool-1",
        "session-old",
        "tool",
        7,
        &payload_body,
    );
    external_message.kind = Some("tool_result".to_string());
    let storage_root = tmp.path().join(".tokensave");
    db.lcm_store(&storage_root)
        .ingest_raw_message(&external_message)
        .await
        .unwrap();
    let payload_ref = db
        .lcm_load_raw_message("cursor", "session-old-tool-1")
        .await
        .unwrap()
        .payload_ref
        .expect("payload should externalize");

    let first = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-old",
            LcmSummarizerMode::Fake {
                summary_text: "first chunk summary".into(),
            },
            Some(4),
            Some(2),
            None,
        ))
        .await
        .unwrap();
    assert!(!first.frontier.maintenance_debt.is_empty());
    let state_before = db
        .lcm_lifecycle_state("cursor", "session-old")
        .await
        .unwrap();

    // The target session already has rows, so the carry-over is rejected.
    insert_raw_messages(&db, "cursor", "session-busy", &["already-there"]).await;
    let err = db
        .lcm_session_boundary(boundary_request(
            "session-busy",
            "session-old",
            Some("session-old"),
        ))
        .await
        .expect_err("carry-over into a non-empty session must fail");
    assert!(
        matches!(err, tokensave::sessions::lcm::LcmError::Db(message) if message.contains("empty target session"))
    );

    // Source rows, payload ownership, and lifecycle state are untouched.
    let old_page = db
        .lcm_load_session(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-old".into(),
            after_store_id: None,
            limit: 10,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .unwrap();
    assert_eq!(old_page.messages.len(), 7);
    assert_eq!(old_page.messages[0].store_id, store_ids[0]);
    let state_after = db
        .lcm_lifecycle_state("cursor", "session-old")
        .await
        .unwrap();
    assert_eq!(state_after.current_session_id, "session-old");
    assert_eq!(
        state_after.current_frontier_store_id,
        state_before.current_frontier_store_id
    );
    assert_eq!(state_after.maintenance_debt, state_before.maintenance_debt);
    let payload_expansion = db
        .lcm_expand(tokensave::sessions::lcm::LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-old".into(),
            target: tokensave::sessions::lcm::LcmExpandTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("payload must remain owned by the source session");
    assert!(payload_expansion.content.starts_with("tool output"));

    // Nothing was written for the rejected target: no lifecycle rebind and
    // no boundary-skip cooldown.
    assert!(db
        .lcm_lifecycle_state("cursor", "session-busy")
        .await
        .is_err());

    // The same source session can still carry over to an empty session.
    insert_session(&db, "cursor", "session-empty").await;
    let boundary = db
        .lcm_session_boundary(boundary_request(
            "session-empty",
            "session-old",
            Some("session-old"),
        ))
        .await
        .unwrap();
    assert!(boundary.recorded);
    assert_eq!(boundary.reason, "compression_boundary_carried_over");
    let rebound = db
        .lcm_lifecycle_state("cursor", "session-empty")
        .await
        .unwrap();
    assert_eq!(rebound.current_session_id, "session-empty");
    assert_eq!(
        rebound.maintenance_debt, state_before.maintenance_debt,
        "outstanding debt must survive the eventual carry-over"
    );
}

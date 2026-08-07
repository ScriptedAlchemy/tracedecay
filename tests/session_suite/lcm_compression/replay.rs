use super::*;

// The daemon LCM authority owns summarizer selection: a compress without a
// persisted summary ingests active messages and, with nothing eligible to
// compact, reports no_backlog_to_compress without creating summary nodes.
#[tokio::test]
async fn ingest_only_compress_stores_messages_without_summary_nodes() {
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
    assert_eq!(response.reason, "no_backlog_to_compress");
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
async fn replay_assembly_terminates_when_existing_summary_sources_contain_cycle() {
    let tmp = TempDir::new().unwrap();
    let db = open_registered_lcm_runtime(&tmp).await;
    let store_ids =
        insert_registered_raw_messages(&db, "cursor", "cycle-session", &["alpha"]).await;
    let leaf = db
        .lcm_insert_summary_node_for_test(
            HostAdmissionScope::Profile,
            summary_draft(
                "cursor",
                "cycle-session",
                0,
                "leaf summary",
                vec![LcmSourceRef::RawMessage {
                    store_id: store_ids[0],
                }],
            ),
        )
        .await
        .expect("leaf summary insert should succeed");
    let middle = db
        .lcm_insert_summary_node_for_test(
            HostAdmissionScope::Profile,
            summary_draft(
                "cursor",
                "cycle-session",
                1,
                "middle summary",
                vec![LcmSourceRef::SummaryNode {
                    node_id: leaf.node_id.clone(),
                }],
            ),
        )
        .await
        .expect("middle summary insert should succeed");
    let _root = db
        .lcm_insert_summary_node_for_test(
            HostAdmissionScope::Profile,
            summary_draft(
                "cursor",
                "cycle-session",
                2,
                "root summary",
                vec![LcmSourceRef::SummaryNode {
                    node_id: middle.node_id.clone(),
                }],
            ),
        )
        .await
        .expect("root summary insert should succeed");

    db.replace_lcm_summary_source_for_test(
        HostAdmissionScope::Profile,
        leaf.node_id.as_str(),
        middle.node_id.as_str(),
    )
    .await
    .unwrap();

    let response = tokio::time::timeout(
        Duration::from_secs(2),
        db.lcm_compress_for_test(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "cycle-session".into(),
            messages: vec![json!({
                "id": "active-after-cycle",
                "role": "user",
                "content": "active after corrupt summary cycle"
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
        }),
    )
    .await
    .expect("replay assembly should terminate despite corrupt summary cycle")
    .expect("compression should succeed");

    assert_eq!(response.status, "ok");
    assert!(!response.replay_messages.is_empty());
}

#[tokio::test]
async fn threshold_pressure_summarizes_short_huge_active_context() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "short-huge").await;

    let messages = vec![
        json!({
            "id": "short-huge-1",
            "role": "user",
            "content": "first long user turn ".repeat(80),
        }),
        json!({
            "id": "short-huge-2",
            "role": "assistant",
            "content": "assistant response ".repeat(80),
        }),
        json!({
            "id": "short-huge-3",
            "role": "user",
            "content": "latest user objective ".repeat(80),
        }),
    ];

    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "cursor".into(),
            session_id: "short-huge".into(),
            messages,
            current_tokens: Some(2_000),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            threshold_tokens: Some(1_000),
            max_assembly_tokens: None,
            leaf_chunk_tokens: Some(100),
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: Some(64),
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::HermesAuxiliary,
        })
        .await
        .unwrap();

    assert_eq!(
        response.status, "needs_summary",
        "response reason: {}",
        response.reason
    );
    // The daemon authority resolves auxiliary summaries itself; without a
    // registered authoritative summarizer the pending summary stays typed
    // unavailable instead of asking the host to fill it.
    assert_eq!(response.reason, "authoritative_summarizer_unavailable");
    let summary_request = response
        .summary_request
        .expect("threshold pressure should select source messages to summarize");
    assert!(
        !summary_request.source_messages.is_empty(),
        "short high-token conversations must not be kept entirely as fresh tail"
    );
}

#[tokio::test]
async fn active_structured_content_survives_compress_ingest_and_preflight_replay() {
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

    let compress =
        ingest_active_messages(&db, "cursor", "session-structured", messages.clone()).await;
    assert_eq!(compress.replay_messages[0]["content"], content_array);
    assert_eq!(compress.replay_messages[1]["content"], content_object);

    // Preflight is read-only and replays the stored transcript.
    let preflight = db
        .lcm_preflight(preflight_request(
            "cursor",
            "session-structured",
            Vec::new(),
            Some(100),
        ))
        .await
        .unwrap();
    assert_eq!(preflight.status, "ok");
    assert_eq!(preflight.replay_messages[0]["content"], content_array);
    assert_eq!(preflight.replay_messages[1]["content"], content_object);

    let raw = db
        .lcm_load_raw_message("cursor", "structured-array")
        .await
        .expect("structured raw message should exist");
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["active_replay"]["content"], content_array);
}

#[tokio::test]
async fn idless_compression_replay_does_not_reingest_existing_raw_messages() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-idless-replay").await;

    let initial = ingest_active_messages(
        &db,
        "cursor",
        "session-idless-replay",
        vec![
            json!({"role": "user", "content": "old user context"}),
            json!({"role": "assistant", "content": "old assistant context"}),
            json!({"role": "user", "content": "fresh user context"}),
            json!({"role": "assistant", "content": "fresh assistant context"}),
        ],
    )
    .await;
    assert_eq!(
        db.lcm_status("cursor", Some("session-idless-replay"))
            .await
            .unwrap()
            .raw_message_count,
        4
    );

    let mut request = compress_request(
        "cursor",
        "session-idless-replay",
        LcmSummarizerMode::Fake {
            summary_text: "condensed old context".into(),
        },
    );
    request.messages = initial.replay_messages;
    let compressed = db.lcm_compress(request).await.unwrap();
    assert_eq!(compressed.summary_nodes_created, 1);
    assert!(compressed.replay_messages[0]["lcm_summary_node_id"].is_string());
    assert!(
        compressed
            .replay_messages
            .iter()
            .skip(1)
            .all(|message| message["store_id"].is_number())
    );

    ingest_active_messages(
        &db,
        "cursor",
        "session-idless-replay",
        compressed.replay_messages,
    )
    .await;
    assert_eq!(
        db.lcm_status("cursor", Some("session-idless-replay"))
            .await
            .unwrap()
            .raw_message_count,
        4,
        "replaying TraceDecay's own summary/tail must not duplicate raw history"
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
        "reasoning": {"kind": "user-authored-field"},
    });

    ingest_active_messages(
        &db,
        "cursor",
        "session-collision",
        vec![active_message.clone()],
    )
    .await;

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
async fn raw_replay_strips_disposable_provider_reasoning_sidecars() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-sidecars").await;

    ingest_active_messages(
        &db,
        "cursor",
        "session-sidecars",
        vec![json!({
            "id": "assistant-sidecars",
            "role": "assistant",
            "content": "assistant visible content",
            "reasoning": "private scratchpad",
            "reasoning_content": "provider scratchpad",
            "reasoning_details": [{"text": "derived reasoning"}],
            "codex_reasoning_items": [{"encrypted_content": "large encrypted blob"}],
            "codex_message_items": [{"type": "reasoning"}],
        })],
    )
    .await;

    let raw = db
        .lcm_load_raw_message("cursor", "assistant-sidecars")
        .await
        .expect("raw sidecar message should exist");
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    let stored_replay = metadata["active_replay"]
        .as_object()
        .expect("stored active replay should be an object");
    for key in [
        "codex_message_items",
        "codex_reasoning_items",
        "reasoning",
        "reasoning_content",
        "reasoning_details",
    ] {
        assert!(
            stored_replay.get(key).is_none(),
            "persisted active replay should drop disposable provider sidecar {key}"
        );
    }

    let replay_from_raw = db
        .lcm_compress(compress_request(
            "cursor",
            "session-sidecars",
            LcmSummarizerMode::Fake {
                summary_text: "unused".into(),
            },
        ))
        .await
        .unwrap();

    let replay = &replay_from_raw.replay_messages[0];
    assert_eq!(replay["role"], "assistant");
    assert_eq!(replay["content"], "assistant visible content");
    for key in [
        "codex_message_items",
        "codex_reasoning_items",
        "reasoning",
        "reasoning_content",
        "reasoning_details",
    ] {
        assert!(
            replay.get(key).is_none(),
            "compressed raw replay should drop disposable provider sidecar {key}"
        );
    }
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

    ingest_active_messages(&db, "cursor", "session-tools", messages).await;

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

    // Ingest protection rewrites the replay at ingest time on the compress
    // path; the read-only preflight no longer reports a replay diff.
    let ingested = ingest_active_messages(
        &db,
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
    )
    .await;

    let protected_args = ingested.replay_messages[0]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("protected tool-call arguments should stay stringified JSON");
    assert!(protected_args.contains("[Externalized LCM ingest payload:"));
    assert!(protected_args.contains("tool-call-suffix-canary"));
    assert!(!protected_args.contains("data:image/png;base64"));
    let protected_tool_call = ingested.replay_messages[0]["tool_calls"][0].to_string();
    assert!(!protected_tool_call.contains("sk-tool-calls-1234567890abcdef"));

    let payload_ref = externalized_ref_from_placeholder(protected_args);
    let expanded = db
        .lcm_expand(tracedecay::sessions::lcm::LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-tool-calls-protection".into(),
            target: tracedecay::sessions::lcm::LcmExpandTarget::ExternalPayload { payload_ref },
            content_slice: Some(tracedecay::sessions::lcm::LcmContentSlice {
                offset: 0,
                limit: media_payload.chars().count(),
            }),
            source_offset: 0,
            source_limit: None,
        })
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
    let response = ingest_active_messages(
        &db,
        "cursor",
        "session-media",
        vec![json!({
            "id": "structured-media",
            "role": "user",
            "content": [
                {"type": "text", "text": "Please inspect the screenshot."},
                {"type": "image_url", "image_url": {"url": media_payload}},
            ],
        })],
    )
    .await;

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
        .lcm_expand(tracedecay::sessions::lcm::LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-media".into(),
            target: tracedecay::sessions::lcm::LcmExpandTarget::ExternalPayload { payload_ref },
            content_slice: Some(tracedecay::sessions::lcm::LcmContentSlice {
                offset: 0,
                limit: media_payload.chars().count(),
            }),
            source_offset: 0,
            source_limit: None,
        })
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
    let response = ingest_active_messages(
        &db,
        "cursor",
        "session-bounded",
        vec![json!({
            "id": "structured-bounded",
            "role": "user",
            "content": content.clone(),
        })],
    )
    .await;
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
            git_filter: Default::default(),
        })
        .await
        .unwrap()
        .hits;
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.chars().count() <= MAX_DERIVED_SNIPPET_CHARS);
    assert!(!hits[0].snippet.contains("::structured-tail"));
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

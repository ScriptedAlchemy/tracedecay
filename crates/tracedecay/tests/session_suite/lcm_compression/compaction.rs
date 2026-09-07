use super::*;

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

use super::*;

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

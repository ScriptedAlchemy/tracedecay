use super::*;

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

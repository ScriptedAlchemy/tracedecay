use super::*;

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

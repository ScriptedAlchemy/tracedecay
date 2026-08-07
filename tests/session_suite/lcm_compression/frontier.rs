use super::*;

#[tokio::test]
async fn compress_frontier_changed_preserves_existing_transaction_state() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-frontier-noop",
        &["one", "two", "three", "four"],
    )
    .await;
    let existing_debt = vec![LcmMaintenanceDebt::RawBacklog {
        from_store_id: store_ids[1],
        to_store_id: store_ids[2],
    }];
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "session-frontier-noop".into(),
        current_session_id: "session-frontier-noop".into(),
        current_frontier_store_id: Some(store_ids[0]),
        last_finalized_session_id: None,
        last_finalized_frontier_store_id: None,
        maintenance_debt: existing_debt.clone(),
    })
    .await
    .unwrap();

    let mut request = compress_request(
        "cursor",
        "session-frontier-noop",
        LcmSummarizerMode::Fake {
            summary_text: "should not be written".into(),
        },
    );
    request.expected_current_frontier_store_id = Some(0);

    let response = db.lcm_compress(request).await.unwrap();
    assert_eq!(response.reason, "frontier_changed");
    assert_eq!(response.summary_nodes_created, 0);
    assert_eq!(
        db.lcm_status("cursor", Some("session-frontier-noop"))
            .await
            .unwrap()
            .summary_node_count,
        0
    );

    let state = db
        .lcm_lifecycle_state("cursor", "session-frontier-noop")
        .await
        .unwrap();
    assert_eq!(state.current_frontier_store_id, Some(store_ids[0]));
    assert_eq!(state.maintenance_debt, existing_debt);
}

#[tokio::test]
async fn compress_persists_summary_frontier_and_remaining_backlog_debt() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-write-success",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let response = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-write-success",
            LcmSummarizerMode::Fake {
                summary_text: "writer summary".into(),
            },
            None,
            Some(1),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "compressed_backlog");
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[0])
    );
    assert_eq!(
        db.lcm_status("cursor", Some("session-write-success"))
            .await
            .unwrap()
            .summary_node_count,
        1
    );

    let state = db
        .lcm_lifecycle_state("cursor", "session-write-success")
        .await
        .unwrap();
    assert_eq!(state.current_frontier_store_id, Some(store_ids[0]));
    assert_eq!(
        state.maintenance_debt,
        vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: store_ids[1],
            to_store_id: store_ids[1],
        }]
    );
}

#[tokio::test]
async fn compress_rolls_back_summary_and_lifecycle_when_debt_write_fails() {
    let tmp = TempDir::new().unwrap();
    let db = open_registered_lcm_runtime(&tmp).await;
    db.set_lcm_compression_debt_insert_failure_for_test(HostAdmissionScope::Profile, true)
        .await
        .expect("install maintenance debt trigger");

    insert_registered_raw_messages(
        &db,
        "cursor",
        "session-write-rollback",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let err = db
        .lcm_compress_for_test(limited_compress_request(
            "cursor",
            "session-write-rollback",
            LcmSummarizerMode::Fake {
                summary_text: "writer summary".into(),
            },
            None,
            Some(1),
            None,
        ))
        .await
        .expect_err("trigger should abort maintenance debt write");

    assert!(
        format!("{err:?}").contains("forced maintenance debt failure"),
        "unexpected error: {err:?}"
    );
    assert_eq!(
        db.lcm_status_for_test("cursor", Some("session-write-rollback"))
            .await
            .unwrap()
            .summary_node_count,
        0
    );
    assert!(
        db.lcm_lifecycle_state_for_test("cursor", "session-write-rollback")
            .await
            .is_err(),
        "failed write should roll back lifecycle state"
    );
}

#[tokio::test]
async fn late_summary_projection_failure_rolls_back_payload_files_and_canonical_rows() {
    let tmp = TempDir::new().unwrap();
    let db = open_registered_lcm_runtime(&tmp).await;
    db.set_lcm_late_summary_projection_failure_for_test(HostAdmissionScope::Profile, true)
        .await
        .expect("install late projection trigger");

    let mut request = limited_compress_request(
        "cursor",
        "session-late-payload-rollback",
        LcmSummarizerMode::Fake {
            summary_text: "must roll back".into(),
        },
        Some(1),
        Some(1),
        None,
    );
    request.threshold_tokens = Some(1);
    request.fresh_tail_count = Some(2);
    request.messages = vec![
        json!({
            "id": "large-tool-result",
            "role": "tool",
            "kind": "tool_result",
            "content": format!("tool output\n{}", "P".repeat(300_000)),
        }),
        json!({"id": "middle-1", "role": "assistant", "content": "middle one"}),
        json!({"id": "fresh-1", "role": "user", "content": "fresh one"}),
        json!({"id": "fresh-2", "role": "assistant", "content": "fresh two"}),
    ];

    let error = db
        .lcm_compress_for_test(request)
        .await
        .expect_err("late compatibility failure must abort the whole publication");
    assert!(
        format!("{error:?}").contains("forced late summary projection failure"),
        "unexpected error: {error:?}"
    );
    assert_eq!(
        db.session_summary_node_count_for_test(
            HostAdmissionScope::Profile,
            "session-late-payload-rollback",
        )
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        db.lcm_raw_message_count_for_test(
            HostAdmissionScope::Profile,
            "session-late-payload-rollback",
        )
        .await
        .unwrap(),
        0
    );
    let payload_dir = tmp.path().join(".tracedecay").join("lcm-payloads");
    let payload_count = std::fs::read_dir(payload_dir)
        .map(|entries| entries.count())
        .unwrap_or_default();
    assert_eq!(
        payload_count, 0,
        "payload rollback must remove created files"
    );
}

#[tokio::test]
async fn lifecycle_frontier_survives_reopen() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;

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

    let reopened = open_lcm_db(&tmp).await;
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
async fn repeated_active_ingest_preserves_existing_message_ordinals() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;
    let messages = vec![
        json!({"id": "active-1", "role": "user", "content": "hello"}),
        json!({"id": "active-2", "role": "assistant", "content": "hi"}),
    ];

    ingest_active_messages(&db, "cursor", "session-1", messages.clone()).await;
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

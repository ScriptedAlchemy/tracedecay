use super::*;

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
    // Cooldown is lossless for stored history: the read-only preflight
    // replays every persisted message. Host-active messages are not ingested
    // by preflight anymore — ingest belongs to the transcript/compress paths.
    assert_eq!(response.replay_messages.len(), 4);
    assert!(
        db.lcm_load_raw_message("cursor", "fresh-user")
            .await
            .is_none()
    );
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

    assert!(response.should_compress, "{response:?}");
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

// Compression boundaries link immutable session authorities. Historical rows
// keep their original owner; only lifecycle continuity is projected forward.
#[tokio::test]
async fn compression_boundary_links_without_reassigning_lcm_data() {
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

    // Raw messages retain their immutable source-session owner.
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
    assert!(new_page.messages.is_empty());
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
    assert_eq!(old_page.messages.len(), 4);

    // Summary authority and compatibility projection retain their owner.
    let expanded = db
        .lcm_expand_summary_node("cursor", "session-old", &node_id)
        .await
        .unwrap();
    assert_eq!(expanded.sources.len(), 2);
    assert!(
        db.lcm_expand_summary_node("cursor", "session-new", &node_id)
            .await
            .is_err()
    );

    // Lifecycle records an immutable boundary link without deleting the old
    // lifecycle row.
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
    let old_state = db
        .lcm_lifecycle_state("cursor", "session-old")
        .await
        .expect("source lifecycle authority remains");
    assert_eq!(old_state.current_session_id, "session-old");

    // The target session starts clean; linked history remains addressable only
    // through its immutable source-session identity.
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
    assert!(next.replay_messages.is_empty());
}

#[tokio::test]
async fn compression_boundary_link_does_not_require_empty_target_session() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(&db, "cursor", "session-old", &["old-1", "old-2"]).await;
    insert_raw_messages(&db, "cursor", "session-new", &["already-there"]).await;

    let boundary = db
        .lcm_session_boundary(boundary_request(
            "session-new",
            "session-old",
            Some("session-old"),
        ))
        .await
        .expect("boundary link does not rewrite target ownership");
    assert!(boundary.recorded);
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
    assert_eq!(old_page.messages.len(), 2);
    assert_eq!(new_page.messages.len(), 1);
}

// The boundary link projects maintenance debt but never rewrites external
// payload ownership.
#[tokio::test]
async fn compression_boundary_link_preserves_payload_owner_and_projects_debt() {
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
    assert!(db.upsert_session_message(&external_message).await);
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
        .lcm_expand(tracedecay_sessions::runtime::lcm::LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-old".into(),
            target: tracedecay_sessions::runtime::lcm::LcmExpandTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        })
        .await
        .unwrap();
    assert!(expansion.content.starts_with("tool output"));
    assert!(
        db.lcm_expand(tracedecay_sessions::runtime::lcm::LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-new".into(),
            target: tracedecay_sessions::runtime::lcm::LcmExpandTarget::ExternalPayload {
                payload_ref
            },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        })
        .await
        .is_err()
    );
}

// Boundary linking never mutates either session's authority, including when
// the target already has its own data.
#[tokio::test]
async fn boundary_link_to_existing_target_leaves_source_session_state_intact() {
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
    assert!(db.upsert_session_message(&external_message).await);
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

    // The target session already has rows; linking is still safe because no
    // ownership is reassigned.
    insert_raw_messages(&db, "cursor", "session-busy", &["already-there"]).await;
    let linked = db
        .lcm_session_boundary(boundary_request(
            "session-busy",
            "session-old",
            Some("session-old"),
        ))
        .await
        .expect("boundary link must not require an empty target");
    assert!(linked.recorded);

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
        .lcm_expand(tracedecay_sessions::runtime::lcm::LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-old".into(),
            target: tracedecay_sessions::runtime::lcm::LcmExpandTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("payload must remain owned by the source session");
    assert!(payload_expansion.content.starts_with("tool output"));

    let target_state = db
        .lcm_lifecycle_state("cursor", "session-busy")
        .await
        .expect("target lifecycle records the immutable source link");
    assert_eq!(
        target_state.last_finalized_session_id.as_deref(),
        Some("session-old")
    );

    // The same immutable source may be linked to another session.
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

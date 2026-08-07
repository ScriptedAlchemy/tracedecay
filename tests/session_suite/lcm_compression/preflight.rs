use super::*;

// Preflight is a read-only decision surface under the daemon-owned compaction
// authority: host-supplied messages are never ingested and the replay comes
// from the stored transcript only (ingest-protection rewriting moved to the
// compress/ingest path, retiring the old ingest_protection_changed_replay
// preflight reason).
#[tokio::test]
async fn preflight_is_read_only_and_never_ingests_active_messages() {
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
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert!(!response.should_compress);
    assert_eq!(response.reason, "no_compression_needed");
    assert!(response.replay_messages.is_empty());
    assert!(
        db.lcm_load_raw_message("cursor", "protected-1")
            .await
            .is_none()
    );
    assert_eq!(
        db.lcm_status("cursor", Some("session-1"))
            .await
            .unwrap()
            .raw_message_count,
        0
    );
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

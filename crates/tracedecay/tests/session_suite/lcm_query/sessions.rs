use super::*;

#[tokio::test]
async fn recent_sessions_orders_by_last_activity_with_provider_filter() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    // raw_message assigns timestamp = base + ordinal, so the session with
    // more messages has the most recent activity.
    insert_raw_messages(
        &db,
        "cursor",
        "session-older",
        &["first turn".to_string(), "second turn".to_string()],
    )
    .await;
    insert_raw_messages(
        &db,
        "codex",
        "session-newer",
        &[
            "first turn".to_string(),
            "second turn".to_string(),
            "third turn".to_string(),
        ],
    )
    .await;

    let sessions = db
        .lcm_recent_sessions_for_test(None, 10)
        .await
        .expect("recent sessions should load");
    assert_eq!(
        sessions
            .iter()
            .map(|session| session.session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["session-newer", "session-older"]
    );
    assert_eq!(sessions[0].provider, "codex");
    assert_eq!(sessions[0].message_count, 3);
    assert_eq!(sessions[0].last_timestamp, Some(1_715_000_003));
    assert_eq!(sessions[1].message_count, 2);

    let cursor_only = db
        .lcm_recent_sessions_for_test(Some("cursor"), 10)
        .await
        .expect("provider-filtered recent sessions should load");
    assert_eq!(cursor_only.len(), 1);
    assert_eq!(cursor_only[0].session_id, "session-older");
}

#[tokio::test]
async fn recent_sessions_uses_store_order_for_null_timestamp_activity() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;

    insert_raw_messages(
        &db,
        "cursor",
        "timestamped-session",
        &["has an epoch timestamp".to_string()],
    )
    .await;

    let session = sample_session("cursor", "null-timestamp-session");
    let mut message = raw_message(
        "cursor",
        "null-timestamp-message-001",
        "null-timestamp-session",
        1,
        "ingested later without source timestamp",
    );
    message.timestamp = None;
    assert!(
        db.upsert_transcript_batch(
            &session,
            &[message],
            "session-lcm-query-cursor-null-timestamp-session.jsonl",
            ParseOffset::default(),
        )
        .await
    );

    let sessions = db
        .lcm_recent_sessions_for_test(None, 1)
        .await
        .expect("recent sessions should load");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "null-timestamp-session");
    assert_eq!(sessions[0].last_timestamp, None);
}

#[tokio::test]
async fn session_providers_finds_explicit_session_beyond_recent_limit() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_raw_messages(
        &db,
        "codex",
        "explicit-session",
        &["older explicit turn".to_string()],
    )
    .await;
    for idx in 0..105 {
        insert_raw_messages(
            &db,
            "cursor",
            &format!("newer-session-{idx:03}"),
            &["newer turn".to_string()],
        )
        .await;
    }

    let recent = db
        .lcm_recent_sessions_for_test(None, 100)
        .await
        .expect("recent sessions should load");
    assert!(
        recent
            .iter()
            .all(|session| session.session_id != "explicit-session"),
        "fixture must place explicit session outside the bounded recent scan"
    );

    let providers = db
        .lcm_session_providers_for_test("explicit-session")
        .await
        .expect("explicit session providers should load without recency limit");
    assert_eq!(providers, vec!["codex"]);
}

#[tokio::test]
async fn session_replay_slice_bounds_head_tail_and_summaries() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let contents = (1..=12)
        .map(|idx| format!("turn-{idx:02} with a deliberately verbose body"))
        .collect::<Vec<_>>();
    let store_ids = insert_raw_messages(&db, "cursor", "session-replay", &contents).await;
    db.lcm_insert_summary_node(summary_draft(
        "cursor",
        "session-replay",
        "summary of the replayed session",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    ))
    .await
    .expect("summary should insert");

    let slice = db
        .lcm_session_replay_slice_for_test(&LcmSessionReplayRequest {
            provider: "cursor".to_string(),
            session_id: "session-replay".to_string(),
            head_limit: 4,
            tail_limit: 4,
            max_snippet_chars: 12,
            summary_limit: 3,
            max_summary_chars: 500,
        })
        .await
        .expect("replay slice should load");

    assert_eq!(slice.total_messages, 12);
    assert_eq!(slice.omitted_messages, 4);
    assert_eq!(
        slice
            .head
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "session-replay-message-001",
            "session-replay-message-002",
            "session-replay-message-003",
            "session-replay-message-004",
        ]
    );
    assert_eq!(
        slice
            .tail
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "session-replay-message-009",
            "session-replay-message-010",
            "session-replay-message-011",
            "session-replay-message-012",
        ]
    );
    for message in slice.head.iter().chain(slice.tail.iter()) {
        assert!(message.truncated);
        assert_eq!(message.snippet.chars().count(), 12);
    }
    assert_eq!(slice.summary_nodes.len(), 1);
    assert_eq!(
        slice.summary_nodes[0].snippet,
        "summary of the replayed session"
    );
    assert!(!slice.summary_nodes[0].truncated);
}

#[tokio::test]
async fn session_replay_slice_rejects_tampered_summary_content() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-replay-integrity",
        &["canonical replay source".into()],
    )
    .await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-replay-integrity",
            "canonical replay summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");
    const TAMPERED_CONTENT: &str = "replay-summary-private-canary";
    replace_summary_content_without_updating_hash(&db, &summary.node_id, TAMPERED_CONTENT).await;

    let error = db
        .lcm_session_replay_slice_for_test(&LcmSessionReplayRequest {
            provider: "cursor".to_string(),
            session_id: "session-replay-integrity".to_string(),
            head_limit: 1,
            tail_limit: 0,
            max_snippet_chars: 500,
            summary_limit: 1,
            max_summary_chars: 500,
        })
        .await
        .expect_err("tampered replay summary content must fail closed");

    assert_eq!(error, LcmError::PayloadIntegrityMismatch);
    assert!(!error.to_string().contains(TAMPERED_CONTENT));
}

#[tokio::test]
async fn session_replay_slice_short_session_has_no_tail_overlap() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-short",
        &[
            "only turn one".to_string(),
            "only turn two".to_string(),
            "only turn three".to_string(),
        ],
    )
    .await;

    let slice = db
        .lcm_session_replay_slice_for_test(&LcmSessionReplayRequest {
            provider: "cursor".to_string(),
            session_id: "session-short".to_string(),
            head_limit: 4,
            tail_limit: 4,
            max_snippet_chars: 500,
            summary_limit: 3,
            max_summary_chars: 500,
        })
        .await
        .expect("replay slice should load");

    assert_eq!(slice.total_messages, 3);
    assert_eq!(slice.omitted_messages, 0);
    assert_eq!(slice.head.len(), 3);
    assert!(slice.tail.is_empty(), "tail must not repeat head turns");
    assert!(!slice.head[0].truncated);
    assert_eq!(slice.head[0].snippet, "only turn one");
    assert!(slice.summary_nodes.is_empty());
}

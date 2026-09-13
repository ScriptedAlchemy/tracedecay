use super::*;

#[tokio::test]
async fn load_session_returns_ordered_raw_pages_with_stable_cursor() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let contents = (1..=105)
        .map(|idx| format!("message-{idx:03}"))
        .collect::<Vec<_>>();
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &contents).await;

    let first = db
        .lcm_load_session_for_test(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            after_store_id: None,
            limit: 500,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .expect("first page should load");
    assert_eq!(first.messages.len(), 100);
    assert_eq!(first.messages[0].content, "message-001");
    assert_eq!(first.messages[99].content, "message-100");
    assert_eq!(
        first.next_cursor.as_deref(),
        Some(store_ids[99].to_string().as_str())
    );

    let next_after_store_id = first
        .next_cursor
        .as_deref()
        .and_then(|cursor| cursor.parse::<i64>().ok());
    let second = db
        .lcm_load_session_for_test(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            after_store_id: next_after_store_id,
            limit: 2,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .expect("second page should load");
    assert_eq!(
        second
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["message-101", "message-102"]
    );
    assert_eq!(
        second.next_cursor.as_deref(),
        Some(store_ids[101].to_string().as_str())
    );

    let min_clamped = db
        .lcm_load_session_for_test(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            after_store_id: None,
            limit: 0,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .expect("minimum-clamped page should load");
    assert_eq!(min_clamped.messages.len(), 1);
    assert_eq!(
        min_clamped.next_cursor.as_deref(),
        Some(store_ids[0].to_string().as_str())
    );
}

#[tokio::test]
async fn load_session_accepts_multiple_roles_and_slices_to_caller_limit() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;

    for message in [
        raw_message_with_role_source_timestamp(
            "cursor",
            "role-user",
            "session-1",
            1,
            "user message content",
            RawMessageContext {
                role: "user",
                source: "cli",
                timestamp: 10,
            },
        ),
        raw_message_with_role_source_timestamp(
            "cursor",
            "role-tool",
            "session-1",
            2,
            "tool message content",
            RawMessageContext {
                role: "tool",
                source: "cli",
                timestamp: 20,
            },
        ),
        raw_message_with_role_source_timestamp(
            "cursor",
            "role-assistant",
            "session-1",
            3,
            "assistant message content",
            RawMessageContext {
                role: "assistant",
                source: "cli",
                timestamp: 30,
            },
        ),
    ] {
        assert!(db.upsert_session_message(&message).await);
    }

    let page = db
        .lcm_load_session_for_test(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            after_store_id: None,
            limit: 10,
            roles: vec!["user".into(), "tool".into()],
            start_time: Some(1),
            end_time: Some(25),
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: 12,
            }),
        })
        .await
        .expect("multi-role page should load");

    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["role-user", "role-tool"]
    );
    assert!(
        page.messages
            .iter()
            .all(|message| message.content_range.returned_chars <= 12)
    );
}

#[tokio::test]
async fn load_session_rejects_tampered_inline_content() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-integrity",
        &["canonical private message".into()],
    )
    .await;
    const TAMPERED_CONTENT: &str = "load-session-private-canary";
    replace_inline_content_without_updating_hash(&db, store_ids[0], TAMPERED_CONTENT).await;

    let error = db
        .lcm_load_session_for_test(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-integrity".into(),
            after_store_id: None,
            limit: 10,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .expect_err("tampered message content must fail closed");

    assert_eq!(error, LcmError::PayloadIntegrityMismatch);
    assert!(!error.to_string().contains(TAMPERED_CONTENT));
}

// Hermes load_session paging only hands back a resume cursor while more rows
// remain: a final page that exactly fills the limit terminates the cursor, and
// resuming past the last row yields an empty page instead of an error.
#[tokio::test]
async fn load_session_exact_final_page_omits_next_cursor() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let contents = (1..=4)
        .map(|idx| format!("edge-message-{idx}"))
        .collect::<Vec<_>>();
    let store_ids = insert_raw_messages(&db, "cursor", "session-edge", &contents).await;
    let request = |after_store_id: Option<i64>, limit: usize| LcmLoadSessionRequest {
        provider: "cursor".into(),
        session_id: "session-edge".into(),
        after_store_id,
        limit,
        roles: Vec::new(),
        start_time: None,
        end_time: None,
        content_slice: None,
    };

    // The whole session in one exactly-sized page: no resume cursor.
    let exact = db
        .lcm_load_session_for_test(request(None, 4))
        .await
        .expect("exact-limit page should load");
    assert_eq!(exact.messages.len(), 4);
    assert_eq!(exact.next_cursor, None);

    // A final page that exactly fills the limit also terminates the cursor.
    let first = db
        .lcm_load_session_for_test(request(None, 2))
        .await
        .expect("first page should load");
    assert_eq!(
        first.next_cursor.as_deref(),
        Some(store_ids[1].to_string().as_str())
    );
    let last = db
        .lcm_load_session_for_test(request(Some(store_ids[1]), 2))
        .await
        .expect("final page should load");
    assert_eq!(last.messages.len(), 2);
    assert_eq!(last.messages[1].content, "edge-message-4");
    assert_eq!(last.next_cursor, None);

    // Resuming from the last row returns an empty terminal page.
    let drained = db
        .lcm_load_session_for_test(request(Some(store_ids[3]), 2))
        .await
        .expect("drained cursor should load");
    assert!(drained.messages.is_empty());
    assert_eq!(drained.next_cursor, None);
}

// A session with no LCM rows is a valid empty state, matching hermes-lcm
// engine behavior on a fresh store: reads return empty results and zeroed
// overviews instead of errors.
#[tokio::test]
async fn empty_session_load_grep_and_describe_return_empty_results() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_session(&db, "cursor", "session-empty").await;

    let page = db
        .lcm_load_session_for_test(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-empty".into(),
            after_store_id: None,
            limit: 10,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: None,
        })
        .await
        .expect("empty session should load");
    assert!(page.messages.is_empty());
    assert_eq!(page.next_cursor, None);

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "anything".into(),
            scope: LcmScope::Session,
            session_id: Some("session-empty".into()),
            include_summaries: true,
            limit: 10,
            sort: LcmGrepSort::Recency,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
            git_filter: Default::default(),
        })
        .await
        .expect("grep on empty session should succeed")
        .hits;
    assert!(hits.is_empty());

    let described = db
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "cursor".into(),
            session_id: "session-empty".into(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .expect("describe on empty session should succeed");
    assert_eq!(described.raw_message_count, 0);
    assert_eq!(described.summary_node_count, 0);
    assert_eq!(described.external_payload_count, 0);
    assert_eq!(described.first_store_id, None);
    assert_eq!(described.last_store_id, None);
    assert!(described.raw_messages.is_empty());
    assert!(described.summary_nodes.is_empty());
}

// Content slices are character offsets, never byte offsets, matching Python
// string slicing in hermes-lcm `lcm_expand`/`lcm_load_session` (text[a:a+n]).
// Multibyte content must slice cleanly with char-based range metadata.
#[tokio::test]
async fn content_slices_use_char_offsets_for_multibyte_content() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    // 9 chars, 17 UTF-8 bytes: byte-based slicing would panic or split chars.
    let content = "αβγδε🦀abc".to_string();
    assert_eq!(content.chars().count(), 9);
    assert_eq!(content.len(), 17);
    let store_ids = insert_raw_messages(&db, "cursor", "session-utf8", &[content]).await;

    let page = db
        .lcm_load_session_for_test(LcmLoadSessionRequest {
            provider: "cursor".into(),
            session_id: "session-utf8".into(),
            after_store_id: None,
            limit: 10,
            roles: Vec::new(),
            start_time: None,
            end_time: None,
            content_slice: Some(LcmContentSlice {
                offset: 4,
                limit: 3,
            }),
        })
        .await
        .expect("multibyte slice should load");
    // Python: "αβγδε🦀abc"[4:7] == "ε🦀a"
    assert_eq!(page.messages[0].content, "ε🦀a");
    let range = &page.messages[0].content_range;
    assert_eq!(range.offset, 4);
    assert_eq!(range.returned_chars, 3);
    assert_eq!(range.total_chars, 9);
    assert!(range.truncated);

    let expanded = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-utf8".into(),
            target: LcmExpandTarget::RawMessage {
                store_id: store_ids[0],
            },
            content_slice: Some(LcmContentSlice {
                offset: 5,
                limit: 2,
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("multibyte expand should succeed");
    // Python: "αβγδε🦀abc"[5:7] == "🦀a"
    assert_eq!(expanded.content, "🦀a");
    assert_eq!(expanded.content_range.offset, 5);
    assert_eq!(expanded.content_range.returned_chars, 2);
    assert_eq!(expanded.content_range.total_chars, 9);
    assert!(expanded.content_range.truncated);

    // An offset past the end clamps to an empty slice like Python s[99:101].
    let beyond = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-utf8".into(),
            target: LcmExpandTarget::RawMessage {
                store_id: store_ids[0],
            },
            content_slice: Some(LcmContentSlice {
                offset: 99,
                limit: 2,
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("out-of-range multibyte slice should clamp");
    assert_eq!(beyond.content, "");
    assert_eq!(beyond.content_range.returned_chars, 0);
    assert_eq!(beyond.content_range.total_chars, 9);
}

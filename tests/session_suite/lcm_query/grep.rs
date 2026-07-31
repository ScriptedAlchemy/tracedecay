use super::*;

#[tokio::test]
async fn grep_searches_raw_snippets_and_summary_nodes() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "raw billing migration details".to_string(),
            "ordinary follow-up".to_string(),
        ],
    )
    .await;

    let external_secret = format!("billing migration secret body {}", "S".repeat(300_000));
    let mut external = raw_message("cursor", "tool-secret", "session-1", 3, &external_secret);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");

    db.lcm_insert_summary_node(summary_draft(
        "cursor",
        "session-1",
        "summary for billing migration decisions",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    ))
    .await
    .expect("summary should insert");

    let memory_db_path = db
        .database_path(HostAdmissionScope::Profile)
        .expect("registered profile database path");
    let memory_conn =
        rusqlite::Connection::open(memory_db_path).expect("open profile memory store");
    memory_conn
        .execute(
            "INSERT INTO memory_facts (content) VALUES (?1)",
            ["billing migration memory-only canary"],
        )
        .expect("seed competing memory fact");
    drop(memory_conn);

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "billing migration".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
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
        .expect("grep should succeed")
        .hits;

    assert!(hits.iter().any(|hit| hit.kind == "raw_message"));
    assert!(hits.iter().any(|hit| hit.kind == "summary_node"));
    assert!(
        hits.iter()
            .all(|hit| matches!(hit.kind.as_str(), "raw_message" | "summary_node")),
        "LCM query results must stay within the raw-message and summary stores"
    );
    assert!(
        hits.iter()
            .all(|hit| !hit.snippet.contains("memory-only canary")),
        "LCM query must not retrieve matching durable memory facts"
    );
    assert!(
        hits.iter()
            .all(|hit| hit.snippet.chars().count() <= MAX_DERIVED_SNIPPET_CHARS)
    );
    assert!(!hits.iter().any(|hit| hit.snippet.contains("secret body")));
}

#[tokio::test]
async fn grep_tokenizes_punctuation_heavy_path_like_queries() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "The regression lives in src/foo.rs and needs a tokenizer-style query.".to_string(),
            "Another message mentions src and foo but not the extension token.".to_string(),
        ],
    )
    .await;

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "src/foo.rs".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
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
        .expect("path-like grep should not miss because punctuation was collapsed")
        .hits;

    assert_eq!(hits.len(), 2);
    let hit_ids = hits
        .iter()
        .filter_map(|hit| hit.store_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        hit_ids,
        std::collections::BTreeSet::from([store_ids[0], store_ids[1]])
    );
    assert!(
        hits.iter()
            .any(|hit| hit.store_id == Some(store_ids[0]) && hit.snippet.contains("src/foo.rs"))
    );
}

#[tokio::test]
async fn grep_like_fallback_recalls_infix_hyphen_query_matches() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "copilot canary rollout checklist".to_string(),
            "baseline note without the compound token".to_string(),
        ],
    )
    .await;
    db.lcm_insert_summary_node(summary_draft(
        "cursor",
        "session-1",
        "summary references copilot migration decisions",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    ))
    .await
    .expect("summary should insert");

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "co-pilot".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
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
        .expect("hyphenated fallback query should keep infix matches")
        .hits;

    assert!(hits.iter().any(|hit| hit.store_id == Some(store_ids[0])));
    assert!(
        hits.iter().any(|hit| hit.kind == "summary_node"
            && hit.snippet.to_ascii_lowercase().contains("copilot"))
    );
}

#[tokio::test]
async fn grep_like_fallback_recalls_infix_slash_query_matches() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["the docs mention srcfoo as a fused path token".to_string()],
    )
    .await;

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "src/foo".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
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
        .expect("slash fallback query should keep infix matches")
        .hits;

    assert!(hits.iter().any(|hit| hit.store_id == Some(store_ids[0])));
    assert!(
        hits.iter()
            .any(|hit| hit.snippet.to_ascii_lowercase().contains("srcfoo"))
    );
}

#[tokio::test]
async fn grep_like_fallback_handles_hash_separator_queries() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["the log references issue#123 inside a Cursor transcript".to_string()],
    )
    .await;

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "issue#123".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
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
        .expect("hash separator grep should not produce an FTS syntax error")
        .hits;

    assert!(hits.iter().any(|hit| hit.store_id == Some(store_ids[0])));
}

#[tokio::test]
async fn grep_quotes_reserved_operator_looking_query_text() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "A literal OR token appears in this transcript.".to_string(),
            "This message deliberately omits the operator word.".to_string(),
        ],
    )
    .await;

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "\"OR\"".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
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
        .expect("reserved FTS operator text should be treated as literal text")
        .hits;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].store_id, Some(store_ids[0]));
    assert!(hits[0].snippet.contains("OR"));
}

#[tokio::test]
async fn grep_preserves_quoted_phrase_semantics() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "alpha beta phrase canary".to_string(),
            "alpha phrase beta (not adjacent)".to_string(),
        ],
    )
    .await;

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "\"alpha beta\"".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
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
        .expect("quoted phrase grep should preserve phrase matching")
        .hits;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].store_id, Some(store_ids[0]));
}

#[tokio::test]
async fn grep_preserves_boolean_or_semantics() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "apple only phrase".to_string(),
            "banana only phrase".to_string(),
            "neither fruit term".to_string(),
        ],
    )
    .await;

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "apple OR banana".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
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
        .expect("OR query should preserve boolean operator semantics")
        .hits;

    let matched = hits
        .iter()
        .filter_map(|hit| hit.store_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        matched,
        [store_ids[0], store_ids[1]]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
}

#[tokio::test]
async fn grep_cjk_query_uses_like_fallback_substring_matching() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "这是一个柠檬测试用例".to_string(),
            "仅包含苹果关键词".to_string(),
        ],
    )
    .await;

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "柠檬".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
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
        .expect("CJK grep should fall back to LIKE substring matching")
        .hits;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].store_id, Some(store_ids[0]));
    assert!(hits[0].snippet.contains("柠檬"));
}

#[tokio::test]
async fn grep_filters_raw_hits_by_role_source_and_time_and_sorts() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;

    for message in [
        raw_message_with_role_source_timestamp(
            "cursor",
            "old-cli-assistant",
            "session-1",
            1,
            "orchard parity old cli assistant",
            RawMessageContext {
                role: "assistant",
                source: "cli",
                timestamp: 10,
            },
        ),
        raw_message_with_role_source_timestamp(
            "cursor",
            "new-cli-user",
            "session-1",
            2,
            "orchard parity new cli user",
            RawMessageContext {
                role: "user",
                source: "cli",
                timestamp: 20,
            },
        ),
        raw_message_with_role_source_timestamp(
            "cursor",
            "new-api-assistant",
            "session-1",
            3,
            "orchard parity new api assistant",
            RawMessageContext {
                role: "assistant",
                source: "api",
                timestamp: 30,
            },
        ),
    ] {
        assert!(db.upsert_session_message(&message).await);
    }

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "orchard parity".into(),
            scope: LcmScope::Session,
            session_id: Some("session-1".into()),
            include_summaries: true,
            limit: 10,
            sort: LcmGrepSort::Recency,
            source: Some("cli".into()),
            role: Some("assistant".into()),
            start_time: Some(5),
            end_time: Some(25),
            git_filter: Default::default(),
        })
        .await
        .expect("filtered grep should succeed")
        .hits;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_id.as_deref(), Some("old-cli-assistant"));
    assert_eq!(hits[0].kind, "raw_message");
}

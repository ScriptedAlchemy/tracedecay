use super::*;

#[tokio::test]
async fn expand_query_returns_no_match_without_synthesis() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["ordinary transcript without the target term".to_string()],
    )
    .await;

    let response = db
        .lcm_expand_query_for_test(LcmExpandQueryRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            prompt: "What did we decide about citron?".into(),
            query: Some("citron".into()),
            node_ids: Vec::new(),
            max_results: 5,
            max_tokens: 2000,
            context_max_tokens: 1024,
        })
        .await
        .expect("expand query should succeed");

    assert!(!response.needs_synthesis);
    assert_eq!(
        response.answer.as_deref(),
        Some("No matching LCM context found in the current session.")
    );
    assert!(response.node_ids.is_empty());
    assert!(response.matches.is_empty());
    assert!(response.context_blocks.is_empty());
    assert!(!response.context_truncated);
}

#[tokio::test]
async fn expand_query_selects_summary_and_raw_context_blocks() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "raw orchard migration source detail".to_string(),
            "unrelated follow-up".to_string(),
        ],
    )
    .await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "summary orchard migration decision",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let response = db
        .lcm_expand_query_for_test(LcmExpandQueryRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            prompt: "What happened with orchard migration?".into(),
            query: Some("orchard migration".into()),
            node_ids: Vec::new(),
            max_results: 5,
            max_tokens: 512,
            context_max_tokens: 4096,
        })
        .await
        .expect("expand query should assemble context");

    assert!(response.needs_synthesis);
    assert_eq!(response.answer, None);
    assert!(
        response
            .node_ids
            .iter()
            .any(|node_id| node_id == &summary.node_id)
    );
    assert!(response.matches.iter().any(
        |item| item.kind == "summary_node" && item.node_id.as_deref() == Some(&summary.node_id)
    ));
    assert!(
        response
            .context_blocks
            .iter()
            .any(|block| block.kind == "summary"
                && block.node_id.as_deref() == Some(&summary.node_id))
    );
    assert!(response.context_blocks.iter().any(
        |block| block.kind == "raw_message" && block.content.contains("raw orchard migration")
    ));
    assert!(
        response
            .synthesis_prompt
            .as_ref()
            .expect("synthesis prompt")
            .user
            .contains("EXPANDED CONTEXT")
    );
}

#[tokio::test]
async fn expand_query_reports_context_budget_truncation() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let long_source = format!("raw lemon details {}", "L".repeat(4000));
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &[long_source]).await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            &format!("summary lemon budget {}", "S".repeat(4000)),
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let response = db
        .lcm_expand_query_for_test(LcmExpandQueryRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            prompt: "Explain lemon budget".into(),
            query: Some("lemon".into()),
            node_ids: vec![summary.node_id.clone()],
            max_results: 1,
            max_tokens: 128,
            context_max_tokens: 64,
        })
        .await
        .expect("expand query should report truncation");

    assert!(response.context_truncated);
    assert!(response.context_budget.used_chars <= 64);
    assert!(
        response
            .context_blocks
            .iter()
            .any(|block| block.content_range.truncated)
    );
    assert!(!response.context_pagination.is_empty());
    assert!(
        response
            .context_pagination
            .iter()
            .any(|page| page.has_more && page.node_id.as_deref() == Some(&summary.node_id))
    );
}

#[tokio::test]
async fn expand_query_filters_base64_noise_blocks_from_context() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    // A realistic high-entropy base64 thinking-signature blob (mixed case and
    // digits, no whitespace), well over the noise-token length threshold.
    let noise = "Ab3Cd9Ef2Gh7Jk1Lm5".repeat(20);
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["raw apricot migration detail".to_string(), noise.clone()],
    )
    .await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "summary apricot migration decision",
            vec![
                LcmSourceRef::RawMessage {
                    store_id: store_ids[0],
                },
                LcmSourceRef::RawMessage {
                    store_id: store_ids[1],
                },
            ],
        ))
        .await
        .expect("summary should insert");

    let response = db
        .lcm_expand_query_for_test(LcmExpandQueryRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            prompt: "What happened with apricot migration?".into(),
            query: None,
            node_ids: vec![summary.node_id.clone()],
            max_results: 5,
            max_tokens: 512,
            context_max_tokens: 8192,
        })
        .await
        .expect("expand query should assemble context");

    assert!(
        response
            .context_blocks
            .iter()
            .any(|block| block.content.contains("raw apricot migration")),
        "the genuine source block should survive"
    );
    assert!(
        !response
            .context_blocks
            .iter()
            .any(|block| block.content.contains(&noise)),
        "the base64 signature blob block should be filtered out of context"
    );
}

use tracedecay_lcm::dag::sanitize_summary_draft;

use super::*;

#[tokio::test]
async fn hermes_auxiliary_request_mode_returns_summary_contract() {
    // Claude has no on-demand summarizer, so the daemon returns the pending
    // contract. A provider that has one (Cursor, Codex) would launch that
    // host's CLI from the ambient PATH with the operator's credentials.
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "claude",
        "session-1",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let response = db
        .lcm_compress(LcmCompressionRequest {
            provider: "claude".into(),
            session_id: "session-1".into(),
            messages: Vec::new(),
            current_tokens: Some(1_000),
            focus_topic: Some("billing".into()),
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            overrides: Default::default(),
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
                extraction_result: None,
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
                extraction_result: None,
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

const WITH_EXTRACTION_METADATA: &str = r#"{"pre_compaction_extraction":{"items":["Decision: keep nightly backups","Commitment: rotate keys weekly"],"model":"openai/gpt-5.4-mini","output_path":"/tmp/extractions","status":"ok"},"summary_route":"backup"}"#;
const ROUTE_ONLY_METADATA: &str = r#"{"pre_compaction_extraction":{"status":"not_requested"},"summary_route":"plain {route} 雪"}"#;
const NO_ROUTE_METADATA: &str = r#"{"pre_compaction_extraction":{"status":"not_requested"}}"#;

async fn persisted_summary_metadata(summarizer: LcmSummarizerMode) -> String {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(&db, "cursor", "session-1", &["old-1", "old-2", "fresh-1"]).await;

    let response = db
        .lcm_compress(compress_request("cursor", "session-1", summarizer))
        .await
        .unwrap();
    assert_eq!(response.status, "ok");
    assert_eq!(response.summary_nodes_created, 1);
    let node = &response.summary_nodes[0];
    assert_eq!(node.source_refs.len(), 1);
    assert_eq!(node.expand_hint.as_deref(), Some("1 raw messages"));
    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &node.node_id)
        .await
        .unwrap();
    let persisted = expanded
        .summary
        .metadata_json
        .expect("persisted summary metadata");
    assert_eq!(node.metadata_json.as_deref(), Some(persisted.as_str()));
    persisted
}

fn provided(
    route: Option<&str>,
    extraction: Option<tracedecay_lcm::types::LcmExtractionResult>,
) -> LcmSummarizerMode {
    LcmSummarizerMode::Provided {
        summary_text: "summary with extraction".into(),
        route: route.map(str::to_string),
        extraction_result: extraction,
    }
}

#[tokio::test]
async fn provided_summary_metadata_rows_are_byte_stable() {
    let extraction = tracedecay_lcm::types::LcmExtractionResult {
        status: "ok".into(),
        items: vec![
            "Decision: keep nightly backups".into(),
            "Commitment: rotate keys weekly".into(),
        ],
        text: None,
        model: Some("openai/gpt-5.4-mini".into()),
        output_path: Some("/tmp/extractions".into()),
        error: None,
    };
    let cases = [
        (
            provided(Some(" backup "), Some(extraction)),
            WITH_EXTRACTION_METADATA,
        ),
        (
            provided(Some("plain {route} 雪"), None),
            ROUTE_ONLY_METADATA,
        ),
        (provided(Some("  "), None), NO_ROUTE_METADATA),
        (provided(None, None), NO_ROUTE_METADATA),
    ];
    for (summarizer, expected) in cases {
        // Pin compression's metadata bytes independently, then pass them
        // through the same privacy authority every persisted summary requires.
        // Compare the entire protected value, including all receipt bindings.
        let mut draft = summary_draft(
            "cursor",
            "session-1",
            0,
            "summary with extraction",
            Vec::new(),
        );
        draft.expand_hint = Some("1 raw messages".to_owned());
        draft.metadata_json = Some(expected.to_owned());
        let expected = sanitize_summary_draft(draft)
            .expect("canonical summary protection")
            .metadata_json
            .expect("protected summary metadata");
        assert_eq!(persisted_summary_metadata(summarizer).await, expected);
    }
}

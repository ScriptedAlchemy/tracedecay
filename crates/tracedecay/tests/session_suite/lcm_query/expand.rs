use super::*;

#[tokio::test]
async fn expand_returns_sliced_raw_summary_and_payload_content_with_ranges() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["0123456789abcdef".to_string()],
    )
    .await;
    let payload = format!("payload-prefix-{}", "Z".repeat(300_000));
    let mut external = raw_message("cursor", "tool-expand", "session-1", 2, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");
    let payload_ref = db
        .lcm_load_raw_message_for_test("cursor", "tool-expand")
        .await
        .unwrap()
        .payload_ref
        .expect("payload ref");
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "summary expansion body",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let raw = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::RawMessage {
                store_id: store_ids[0],
            },
            content_slice: Some(LcmContentSlice {
                offset: 2,
                limit: 4,
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("raw should expand");
    assert_eq!(raw.kind, "raw_message");
    assert_eq!(raw.content, "2345");
    assert_eq!(raw.content_range.offset, 2);
    assert_eq!(raw.content_range.returned_chars, 4);
    assert!(raw.content_range.truncated);
    let raw_metadata = raw.raw_message.as_ref().expect("raw metadata");
    assert_eq!(raw_metadata.content, "2345");
    assert_eq!(raw_metadata.content.chars().count(), 4);
    let rendered_raw = serde_json::to_string(&raw).unwrap();
    assert!(!rendered_raw.contains("0123456789abcdef"));

    let summary_expansion = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::SummaryNode {
                node_id: summary.node_id.clone(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 8,
                limit: 9,
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("summary should expand");
    assert_eq!(summary_expansion.kind, "summary_node");
    assert_eq!(summary_expansion.content, "expansion");
    let summary_metadata = summary_expansion
        .summary_node
        .as_ref()
        .expect("summary metadata");
    assert_eq!(summary_metadata.summary_text, "expansion");
    assert_eq!(summary_metadata.summary_text.chars().count(), 9);
    let rendered_summary = serde_json::to_string(&summary_expansion).unwrap();
    assert!(!rendered_summary.contains("summary expansion body"));
    assert_eq!(summary_expansion.summary_sources.len(), 1);

    let payload_expansion = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: "payload-prefix".chars().count(),
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("payload should expand");
    assert_eq!(payload_expansion.kind, "external_payload");
    assert_eq!(
        payload_expansion.payload_ref.as_deref(),
        Some(payload_ref.as_str())
    );
    assert_eq!(payload_expansion.content, "payload-prefix");
    assert_eq!(payload_expansion.content_range.offset, 0);
    assert!(payload_expansion.content_range.truncated);

    let raw_external = db
        .lcm_load_raw_message_for_test("cursor", "tool-expand")
        .await
        .unwrap();
    assert_eq!(raw_external.storage_kind, LcmStorageKind::External);
    assert!(!payload_expansion.content.contains("ZZZZZZZZZZ"));
}

#[tokio::test]
async fn expand_slices_summary_source_content_and_nested_source_bodies() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    // Varied prose-like filler keeps the oversized body inline (no base64
    // runs, no high-repetition quarantine) so this exercises char slicing.
    let filler = (0..12_000)
        .map(|index| format!("filler{index:05}"))
        .collect::<Vec<_>>()
        .join(" ");
    let huge_source = format!("source-prefix-{filler}");
    let huge_source_chars = huge_source.chars().count() as u64;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &[huge_source]).await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "summary source slicing regression",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let expansion = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::SummaryNode {
                node_id: summary.node_id.clone(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: "source-prefix".chars().count(),
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("summary should expand");

    assert_eq!(expansion.summary_sources.len(), 1);
    let source = &expansion.summary_sources[0];
    assert_eq!(source.content, "source-prefix");
    assert!(source.content.chars().count() <= "source-prefix".chars().count());
    let source_range = source.content_range.as_ref().expect("source range");
    assert_eq!(source_range.offset, 0);
    assert_eq!(source_range.limit, "source-prefix".chars().count() as u64);
    assert_eq!(
        source_range.returned_chars,
        "source-prefix".chars().count() as u64
    );
    assert_eq!(source_range.total_chars, huge_source_chars);
    assert!(source_range.truncated);
    assert!(source.content_truncated);
    let raw_source = source.raw_message.as_ref().expect("raw source metadata");
    assert_eq!(raw_source.store_id, store_ids[0]);
    assert_eq!(raw_source.content, "source-prefix");
    assert_eq!(
        raw_source.content.chars().count(),
        "source-prefix".chars().count()
    );
    assert!(!raw_source.content_hash.is_empty());

    let rendered = serde_json::to_string(&expansion).unwrap();
    assert!(!rendered.contains("filler11999"));
    assert!(rendered.contains("\"content_hash\""));
}

#[tokio::test]
async fn expand_wrapper_denies_cross_session_summary_nodes() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids =
        insert_raw_messages(&db, "cursor", "session-1", &["owned by session one".into()]).await;
    insert_session(&db, "cursor", "session-2").await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "summary belongs to session one",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let err = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-2".into(),
            target: LcmExpandTarget::SummaryNode {
                node_id: summary.node_id,
            },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect_err("wrapper expansion should reject nodes from another session");

    assert_eq!(err, LcmError::SummaryNodeNotFound);
}

#[tokio::test]
async fn expand_paginates_summary_sources_with_offset_and_limit() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let contents: Vec<String> = (1..=5)
        .map(|index| format!("source body {index}"))
        .collect();
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &contents).await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "paginated summary",
            store_ids
                .iter()
                .map(|store_id| LcmSourceRef::RawMessage {
                    store_id: *store_id,
                })
                .collect(),
        ))
        .await
        .expect("summary should insert");
    let expand_request = |source_offset: usize, source_limit: Option<usize>| LcmExpandRequest {
        provider: "cursor".into(),
        session_id: "session-1".into(),
        target: LcmExpandTarget::SummaryNode {
            node_id: summary.node_id.clone(),
        },
        content_slice: None,
        source_offset,
        source_limit,
    };

    let page = db
        .lcm_expand_for_test(expand_request(1, Some(2)))
        .await
        .expect("paginated expand should succeed");
    let returned_store_ids: Vec<i64> = page
        .summary_sources
        .iter()
        .filter_map(|source| source.raw_message.as_ref().map(|raw| raw.store_id))
        .collect();
    assert_eq!(returned_store_ids, vec![store_ids[1], store_ids[2]]);
    let pagination = page.source_pagination.expect("pagination metadata");
    assert_eq!(pagination.source_offset, 1);
    assert_eq!(pagination.source_limit, 2);
    assert_eq!(pagination.returned_sources, 2);
    assert_eq!(pagination.total_sources, 5);
    assert_eq!(pagination.next_source_offset, Some(3));
    assert!(pagination.has_more);
    assert_eq!(pagination.remaining_sources, 2);

    // Resuming from the cursor drains the list; an omitted limit clamps to
    // the remaining sources like hermes-lcm.
    let tail = db
        .lcm_expand_for_test(expand_request(3, None))
        .await
        .expect("cursor resume should succeed");
    assert_eq!(tail.summary_sources.len(), 2);
    let tail_pagination = tail.source_pagination.expect("tail pagination");
    assert_eq!(tail_pagination.source_limit, 2);
    assert_eq!(tail_pagination.next_source_offset, None);
    assert!(!tail_pagination.has_more);
    assert_eq!(tail_pagination.remaining_sources, 0);

    // An offset beyond the end clamps to the source count and returns an
    // empty page instead of erroring.
    let beyond = db
        .lcm_expand_for_test(expand_request(9, Some(2)))
        .await
        .expect("out-of-range offset should clamp");
    assert!(beyond.summary_sources.is_empty());
    let beyond_pagination = beyond.source_pagination.expect("beyond pagination");
    assert_eq!(beyond_pagination.source_offset, 5);
    assert_eq!(beyond_pagination.returned_sources, 0);
    assert!(!beyond_pagination.has_more);

    // The default request still returns every source with full metadata.
    let full = db
        .lcm_expand_for_test(expand_request(0, None))
        .await
        .expect("default expand should succeed");
    assert_eq!(full.summary_sources.len(), 5);
    let full_pagination = full.source_pagination.expect("default pagination");
    assert_eq!(full_pagination.returned_sources, 5);
    assert!(!full_pagination.has_more);
}

#[tokio::test]
async fn paginated_summary_sources_reject_tampered_inline_content() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "first canonical source".into(),
            "second canonical source".into(),
            "third canonical source".into(),
        ],
    )
    .await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "paginated integrity summary",
            store_ids
                .iter()
                .map(|store_id| LcmSourceRef::RawMessage {
                    store_id: *store_id,
                })
                .collect(),
        ))
        .await
        .expect("summary should insert");
    const TAMPERED_CONTENT: &str = "second-page-private-canary";
    replace_inline_content_without_updating_hash(&db, store_ids[1], TAMPERED_CONTENT).await;

    let error = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::SummaryNode {
                node_id: summary.node_id,
            },
            content_slice: None,
            source_offset: 1,
            source_limit: Some(1),
        })
        .await
        .expect_err("tampered source content must fail closed");

    assert_eq!(error, LcmError::PayloadIntegrityMismatch);
    assert!(!error.to_string().contains(TAMPERED_CONTENT));
}

#[tokio::test]
async fn paginated_summary_sources_reject_tampered_child_summary_content() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "first canonical source".into(),
            "child canonical source".into(),
        ],
    )
    .await;
    let child = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "canonical child summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[1],
            }],
        ))
        .await
        .expect("child summary should insert");
    let mut parent_draft = summary_draft(
        "cursor",
        "session-1",
        "parent integrity summary",
        vec![
            LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            },
            LcmSourceRef::SummaryNode {
                node_id: child.node_id.clone(),
            },
        ],
    );
    parent_draft.depth = 1;
    let parent = db
        .lcm_insert_summary_node(parent_draft)
        .await
        .expect("parent summary should insert");
    const TAMPERED_CONTENT: &str = "second-page-summary-private-canary";
    replace_summary_content_without_updating_hash(&db, &child.node_id, TAMPERED_CONTENT).await;

    let error = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmExpandTarget::SummaryNode {
                node_id: parent.node_id,
            },
            content_slice: None,
            source_offset: 1,
            source_limit: Some(1),
        })
        .await
        .expect_err("tampered child summary content must fail closed");

    assert_eq!(error, LcmError::PayloadIntegrityMismatch);
    assert!(!error.to_string().contains(TAMPERED_CONTENT));
}

#[tokio::test]
async fn expand_allows_cross_session_raw_store_id_with_provenance() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["cross session body".to_string()],
    )
    .await;
    insert_session(&db, "cursor", "session-2").await;
    let raw_request = |provider: &str, session_id: &str| LcmExpandRequest {
        provider: provider.into(),
        session_id: session_id.into(),
        target: LcmExpandTarget::RawMessage {
            store_id: store_ids[0],
        },
        content_slice: None,
        source_offset: 0,
        source_limit: None,
    };

    let cross = db
        .lcm_expand_for_test(raw_request("cursor", "session-2"))
        .await
        .expect("cross-session store_id expand should succeed");
    assert_eq!(cross.kind, "raw_message");
    assert_eq!(cross.from_current_session, Some(false));
    assert_eq!(cross.content, "cross session body");
    assert_eq!(
        cross.raw_message.as_ref().expect("raw metadata").session_id,
        "session-1"
    );

    let same = db
        .lcm_expand_for_test(raw_request("cursor", "session-1"))
        .await
        .expect("same-session store_id expand should succeed");
    assert_eq!(same.from_current_session, Some(true));
    assert_eq!(same.externalized_note, None);

    // Cross-provider raw rows stay rejected: providers are a TraceDecay
    // concept with no hermes-lcm equivalent.
    insert_session(&db, "claude", "session-9").await;
    let err = db
        .lcm_expand_for_test(raw_request("claude", "session-9"))
        .await
        .expect_err("cross-provider store_id expand should be rejected");
    assert_eq!(err, LcmError::SummarySourceNotOwnedBySession);
}

#[tokio::test]
async fn expand_cross_session_external_row_can_hydrate_payload_via_two_step_expand() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;
    insert_session(&db, "cursor", "session-2").await;
    let payload = format!("cross-payload-{}", "Z".repeat(300_000));
    let mut external = raw_message("cursor", "cross-external", "session-1", 1, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");
    let raw = db
        .lcm_load_raw_message_for_test("cursor", "cross-external")
        .await
        .expect("external raw message should exist");
    let payload_ref = raw.payload_ref.clone().expect("payload ref");

    let cross = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-2".into(),
            target: LcmExpandTarget::RawMessage {
                store_id: raw.store_id,
            },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("cross-session external row should expand");
    assert_eq!(cross.from_current_session, Some(false));
    assert_eq!(cross.payload_ref.as_deref(), Some(payload_ref.as_str()));
    assert_eq!(cross.externalized_note, None);
    let rendered = serde_json::to_string(&cross).unwrap();
    assert!(
        !rendered.contains("ZZZZZZZZZZ"),
        "cross-session raw-message expand should stay compact until payload expansion"
    );
    let payload_owner_session_id = cross
        .raw_message
        .as_ref()
        .expect("raw metadata should include owner session")
        .session_id
        .clone();
    let expanded_payload = db
        .lcm_expand_for_test(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: payload_owner_session_id,
            target: LcmExpandTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 0,
                limit: 128,
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("cross-session payload should hydrate through explicit payload target");
    assert_eq!(expanded_payload.kind, "external_payload");
    assert!(expanded_payload.content.starts_with("cross-payload-"));
    assert_eq!(
        expanded_payload.payload_ref.as_deref(),
        Some(payload_ref.as_str())
    );
}

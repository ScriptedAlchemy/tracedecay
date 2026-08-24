use super::*;

#[tokio::test]
async fn describe_gives_session_overview_without_full_payload_bodies() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha".to_string()]).await;
    let payload = format!("describe secret body\n{}", "D".repeat(300_000));
    let mut external = raw_message("cursor", "tool-describe", "session-1", 2, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "describe alpha summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should insert");

    let description = db
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .expect("description should load");
    assert_eq!(description.provider, "cursor");
    assert_eq!(description.session_id, "session-1");
    assert_eq!(description.raw_message_count, 2);
    assert_eq!(description.summary_node_count, 1);
    assert!(
        description
            .summary_nodes
            .iter()
            .any(|node| node.node_id == summary.node_id)
    );

    let rendered = serde_json::to_string(&description).unwrap();
    assert!(rendered.contains("tool-describe"));
    assert!(!rendered.contains("describe secret body"));
}

#[tokio::test]
async fn describe_node_and_external_payload_return_metadata_without_body_leaks() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "leaf source alpha body".to_string(),
            "leaf source beta body".to_string(),
        ],
    )
    .await;
    let payload = format!("external describe secret {}", "P".repeat(300_000));
    let mut external = raw_message("cursor", "tool-describe-target", "session-1", 3, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");
    let payload_ref = db
        .lcm_load_raw_message_for_test("cursor", "tool-describe-target")
        .await
        .unwrap()
        .payload_ref
        .expect("payload ref");

    let leaf = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "leaf summary body must not appear in describe",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("leaf summary should insert");
    let parent = db
        .lcm_insert_summary_node(LcmSummaryNodeDraft {
            depth: 1,
            summary_text: "parent summary body must not appear in describe".to_string(),
            source_refs: vec![
                LcmSourceRef::SummaryNode {
                    node_id: leaf.node_id.clone(),
                },
                LcmSourceRef::RawMessage {
                    store_id: store_ids[1],
                },
            ],
            ..summary_draft("cursor", "session-1", "", Vec::new())
        })
        .await
        .expect("parent summary should insert");

    let node_description = db
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmDescribeTarget::SummaryNode {
                node_id: parent.node_id.clone(),
            },
        })
        .await
        .expect("node description should load");
    assert_eq!(node_description.target, "summary_node");
    let node = node_description
        .summary_node
        .as_ref()
        .expect("summary metadata");
    assert_eq!(node.node_id, parent.node_id);
    assert_eq!(node.source_count, 2);
    assert!(
        node.children
            .iter()
            .any(|child| child.node_id.as_deref() == Some(leaf.node_id.as_str()))
    );

    let payload_description = db
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            target: LcmDescribeTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
        })
        .await
        .expect("payload description should load");
    assert_eq!(payload_description.target, "external_payload");
    let payload_meta = payload_description
        .external_payload
        .as_ref()
        .expect("payload metadata");
    assert_eq!(payload_meta.payload_ref, payload_ref);
    assert!(
        payload_meta
            .content_preview
            .contains(&payload_meta.payload_ref)
    );
    assert!(
        !payload_meta
            .content_preview
            .contains("external describe secret")
    );

    let rendered = serde_json::to_string(&(node_description, payload_description)).unwrap();
    assert!(!rendered.contains("parent summary body"));
    assert!(!rendered.contains("leaf summary body"));
    assert!(!rendered.contains("external describe secret"));
}

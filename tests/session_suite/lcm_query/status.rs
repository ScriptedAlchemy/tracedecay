use super::*;

#[tokio::test]
async fn status_reports_schema_frontier_payload_and_debt_counts() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["alpha".to_string(), "beta".to_string()],
    )
    .await;

    let payload = format!("private payload marker\n{}", "P".repeat(300_000));
    let mut external = raw_message("cursor", "tool-payload", "session-1", 3, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");

    db.lcm_insert_summary_node(summary_draft(
        "cursor",
        "session-1",
        "alpha beta summary",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    ))
    .await
    .expect("summary should insert");
    db.lcm_update_lifecycle(LcmLifecycleUpdate {
        provider: "cursor".into(),
        conversation_id: "session-1".into(),
        current_session_id: "session-1".into(),
        current_frontier_store_id: Some(store_ids[1]),
        last_finalized_session_id: Some("session-0".into()),
        last_finalized_frontier_store_id: Some(store_ids[0]),
        maintenance_debt: vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: store_ids[0],
            to_store_id: store_ids[1],
        }],
    })
    .await
    .expect("lifecycle state should update");

    let status = db
        .lcm_status_for_test("cursor", Some("session-1"))
        .await
        .expect("status should load");
    assert_eq!(status.schema_version, LCM_SCHEMA_VERSION);
    assert_eq!(status.raw_message_count, 3);
    assert_eq!(status.summary_node_count, 1);
    assert_eq!(status.external_payload_count, 1);
    assert_eq!(status.missing_payload_count, 0);
    assert_eq!(status.payload.externalized_count, 1);
    assert_eq!(status.payload.referenced_count, 1);
    assert_eq!(status.payload.unreferenced_count, 0);
    assert_eq!(status.payload.orphan_file_count, 0);
    assert_eq!(status.payload.reclaimable_bytes, 0);
    assert_eq!(status.payload_gc.last_gc_at, None);
    assert_eq!(status.payload_gc.last_gc_status, None);
    assert!(status.payload.total_bytes > 0);
    assert_eq!(status.maintenance_debt_count, 1);
    assert_eq!(status.lifecycle.lifecycle_state_count, 1);
    assert_eq!(status.lifecycle.frontier_count, 1);
    assert_eq!(status.lifecycle.maintenance_debt_count, 1);
    assert_eq!(
        status.lifecycle.current_session_id.as_deref(),
        Some("session-1")
    );
    assert_eq!(
        status.lifecycle.current_frontier_store_id,
        Some(store_ids[1])
    );
    assert_eq!(
        status.lifecycle.last_finalized_session_id.as_deref(),
        Some("session-0")
    );
    assert_eq!(
        status.lifecycle.last_finalized_frontier_store_id,
        Some(store_ids[0])
    );

    let rendered = serde_json::to_string(&status).unwrap();
    assert!(!rendered.contains("private payload marker"));
}

#[tokio::test]
async fn status_reports_payload_gc_run_metadata_after_apply() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_session(&db, "cursor", "session-gc").await;

    let payload = format!("gc metadata payload\n{}", "G".repeat(300_000));
    let mut external = raw_message("cursor", "tool-gc", "session-gc", 1, &payload);
    external.role = "tool".to_string();
    external.kind = Some("tool_result".to_string());
    db.lcm_ingest_raw_message(&external)
        .await
        .expect("external payload should ingest");

    let cfg = LcmGcConfig {
        backup_before_reap: false,
        ..LcmGcConfig::default()
    };
    let report = db
        .lcm_run_payload_gc_apply_for_test(
            HostAdmissionScope::Profile,
            "cursor",
            Some("session-gc"),
            &cfg,
            1_715_123_456,
        )
        .await
        .expect("payload gc should run");
    assert_eq!(report.status, "applied");

    let status = db
        .lcm_status_for_test("cursor", Some("session-gc"))
        .await
        .expect("status should load");
    assert_eq!(status.payload_gc.last_gc_at, Some(1_715_123_456));
    assert!(
        status.payload_gc.last_gc_duration_ms.is_some(),
        "status should expose the last GC duration"
    );
    assert_eq!(status.payload_gc.last_gc_status.as_deref(), Some("ok"));
    assert_eq!(status.payload_gc.last_gc_error, None);
    assert_eq!(status.payload_gc.last_reaped_refs, Some(0));
    assert_eq!(status.payload_gc.last_reaped_bytes, Some(0));
}

#[tokio::test]
async fn status_reports_dag_depth_distribution_store_estimate_and_config_defaults() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["alpha beta gamma".to_string(), "delta epsilon".to_string()],
    )
    .await;
    let leaf = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            "leaf summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("leaf summary should insert");
    let mut parent = summary_draft(
        "cursor",
        "session-1",
        "condensed parent summary",
        vec![LcmSourceRef::SummaryNode {
            node_id: leaf.node_id.clone(),
        }],
    );
    parent.depth = 1;
    parent.summary_token_count = 3;
    parent.source_token_count = 5;
    db.lcm_insert_summary_node(parent)
        .await
        .expect("parent summary should insert");

    let status = db
        .lcm_status_for_test("cursor", Some("session-1"))
        .await
        .expect("status should load");

    assert_eq!(status.store.messages, 2);
    assert_eq!(status.store.estimated_tokens, 5);

    assert_eq!(status.dag.total_nodes, 2);
    assert_eq!(status.dag.total_tokens, 8);
    assert_eq!(status.dag.total_source_tokens, 35);
    assert_eq!(status.dag.compression_ratio, "4.4:1");
    let depth_zero = status.dag.depths.get("d0").expect("depth-0 bucket");
    assert_eq!(depth_zero.count, 1);
    assert_eq!(depth_zero.tokens, 5);
    assert_eq!(depth_zero.source_tokens, 30);
    let depth_one = status.dag.depths.get("d1").expect("depth-1 bucket");
    assert_eq!(depth_one.count, 1);
    assert_eq!(depth_one.tokens, 3);
    assert_eq!(depth_one.source_tokens, 5);

    assert_eq!(status.config.fresh_tail_count, 2);
    assert_eq!(status.config.summary_fan_in, 4);
    assert_eq!(status.config.compression_boundary_cooldown_seconds, 60);

    // An empty scope reports an inert DAG rather than dividing by zero.
    insert_session(&db, "cursor", "session-empty").await;
    let empty = db
        .lcm_status_for_test("cursor", Some("session-empty"))
        .await
        .expect("empty status should load");
    assert_eq!(empty.dag.total_nodes, 0);
    assert_eq!(empty.dag.compression_ratio, "0:1");
    assert!(empty.dag.depths.is_empty());
    assert_eq!(empty.store.messages, 0);
    assert_eq!(empty.store.estimated_tokens, 0);
}

#[tokio::test]
async fn status_uses_python_half_even_rounding_for_ratio_ties() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-tie",
        &["alpha".to_string(), "beta".to_string()],
    )
    .await;
    let mut node = summary_draft(
        "cursor",
        "session-tie",
        "ratio tie",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    );
    node.summary_token_count = 4;
    node.source_token_count = 5; // 1.25 -> Python round(..., 1) => 1.2
    db.lcm_insert_summary_node(node).await.unwrap();
    let status = db
        .lcm_status_for_test("cursor", Some("session-tie"))
        .await
        .expect("status should load");
    assert_eq!(status.dag.compression_ratio, "1.2:1");
}

// Pins the SQL pushdown of `count_lossy_ingest_records`: only a JSON boolean
// `true` under `$.ingest_protection.lossy` counts. Provider metadata outside
// the object contract fails closed before persistence.
#[tokio::test]
async fn status_counts_lossy_ingest_records_with_pinned_metadata_semantics() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_session(&db, "cursor", "session-lossy").await;

    let variants: &[(&str, Option<&str>)] = &[
        (
            "lossy-true",
            Some(r#"{"ingest_protection":{"lossy":true}}"#),
        ),
        (
            "lossy-false",
            Some(r#"{"ingest_protection":{"lossy":false}}"#),
        ),
        (
            "lossy-integer",
            Some(r#"{"ingest_protection":{"lossy":1}}"#),
        ),
        ("missing-key", Some(r#"{"ingest_protection":{}}"#)),
        ("missing-section", Some(r#"{"other":true}"#)),
        ("null-metadata", None),
    ];
    for (idx, (message_id, metadata)) in variants.iter().enumerate() {
        let mut message = raw_message(
            "cursor",
            message_id,
            "session-lossy",
            (idx + 1) as i64,
            "body",
        );
        message.metadata_json = metadata.map(str::to_string);
        assert!(db.upsert_session_message(&message).await);
    }
    for (message_id, metadata) in [
        ("invalid-json", "{not json"),
        ("non-object", r#"[{"ingest_protection":{"lossy":true}}]"#),
    ] {
        let mut message = raw_message("cursor", message_id, "session-lossy", 100, "body");
        message.metadata_json = Some(metadata.to_string());
        assert!(
            !db.upsert_session_message(&message).await,
            "malformed or non-object provider metadata must fail closed"
        );
    }

    let status = db
        .lcm_status_for_test("cursor", Some("session-lossy"))
        .await
        .expect("status should load");
    assert_eq!(
        status.redaction.lossy_records, 1,
        "only the JSON boolean true row counts as lossy"
    );
    assert!(status.redaction.enabled);
    assert_eq!(status.redaction.legacy_truncated_count, 0);
}

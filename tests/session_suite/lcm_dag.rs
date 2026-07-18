use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::sessions::lcm::types::{
    LcmImmutableSummaryPublication, LcmSummaryPublicationDisposition,
};
use tracedecay::sessions::lcm::{
    LcmError, LcmSessionBoundaryRequest, LcmSourceRef, LcmStorageKind, LcmSummaryNodeDraft,
};

use crate::common::{
    isolated_lcm_db_path as isolated_db_path, lcm_dag_message as raw_message,
    lcm_dag_session as sample_session, open_lcm_db,
};

async fn summary_table_counts(db_path: &std::path::Path) -> (i64, i64) {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut node_rows = conn
        .query("SELECT COUNT(*) FROM lcm_summary_nodes", ())
        .await
        .unwrap();
    let node_count = node_rows.next().await.unwrap().unwrap().get(0).unwrap();
    let mut source_rows = conn
        .query("SELECT COUNT(*) FROM lcm_summary_sources", ())
        .await
        .unwrap();
    let source_count = source_rows.next().await.unwrap().unwrap().get(0).unwrap();
    (node_count, source_count)
}

async fn summary_fts_count(db_path: &std::path::Path, query: &str) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM lcm_summary_nodes_fts
             WHERE lcm_summary_nodes_fts MATCH ?1",
            libsql::params![query],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn scalar_i64(db_path: &std::path::Path, sql: &str) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn text_rows(db_path: &std::path::Path, sql: &str) -> Vec<String> {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut values = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        values.push(row.get(0).unwrap());
    }
    values
}

async fn lineage_effect_count(db_path: &std::path::Path) -> i64 {
    scalar_i64(
        db_path,
        "SELECT
            (SELECT COUNT(*) FROM session_summary_nodes)
          + (SELECT COUNT(*) FROM session_summary_sources)
          + (SELECT COUNT(*) FROM session_summary_successors)
          + (SELECT COUNT(*) FROM session_external_payload_manifests)
          + (SELECT COUNT(*) FROM session_temporal_generations)
          + (SELECT COUNT(*) FROM session_summary_availability)
          + (SELECT COUNT(*) FROM retrieval_anchors
             WHERE projection_generation = 'lcm_summary_lineage_v1')
          + (SELECT COUNT(*) FROM sanitization_receipts
             WHERE sanitizer_version = 'tracedecay.lcm-summary-publication.v1')
          + (SELECT COUNT(*) FROM lcm_summary_nodes)
          + (SELECT COUNT(*) FROM lcm_summary_sources)",
    )
    .await
}

async fn insert_session(db: &GlobalDb, provider: &str, session_id: &str) {
    assert!(
        db.upsert_session(&sample_session(provider, session_id))
            .await
    );
}

async fn insert_raw_messages(
    db: &GlobalDb,
    provider: &str,
    session_id: &str,
    contents: &[&str],
) -> Vec<i64> {
    insert_session(db, provider, session_id).await;
    let mut store_ids = Vec::new();
    for (idx, content) in contents.iter().enumerate() {
        let message_id = format!("{session_id}-message-{}", idx + 1);
        let message = raw_message(provider, &message_id, session_id, (idx + 1) as i64, content);
        assert!(db.upsert_session_message(&message).await);
        let raw = db
            .lcm_load_raw_message(provider, &message_id)
            .await
            .expect("raw message should exist");
        store_ids.push(raw.store_id);
    }
    store_ids
}

async fn insert_external_raw_message(
    db: &GlobalDb,
    tmp: &TempDir,
    provider: &str,
    session_id: &str,
    message_id: &str,
) -> (i64, String) {
    insert_session(db, provider, session_id).await;
    let payload = format!("tool output\n{}", "X".repeat(300_000));
    let mut message = raw_message(provider, message_id, session_id, 1, &payload);
    message.role = "tool".to_string();
    message.kind = Some("tool_result".to_string());

    let storage_root = tmp.path().join(".tracedecay");
    db.lcm_store(&storage_root)
        .ingest_raw_message(&message)
        .await
        .expect("raw ingest should externalize payload");
    let raw = db
        .lcm_load_raw_message(provider, message_id)
        .await
        .expect("external raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    let payload_ref = raw.payload_ref.clone().expect("payload ref");
    (raw.store_id, payload_ref)
}

fn summary_draft(
    provider: &str,
    session_id: &str,
    depth: i64,
    summary_text: &str,
    source_refs: Vec<LcmSourceRef>,
) -> LcmSummaryNodeDraft {
    LcmSummaryNodeDraft {
        provider: provider.to_string(),
        conversation_id: "conversation-1".to_string(),
        session_id: session_id.to_string(),
        depth,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count: 30,
        summary_token_count: 4,
        source_time_start: Some(1_715_000_000),
        source_time_end: Some(1_715_000_030),
        expand_hint: Some("expand source lineage".to_string()),
        metadata_json: Some(r#"{"topic":"dag"}"#.to_string()),
    }
}

#[tokio::test]
async fn summary_node_preserves_source_lineage_and_expands_sources() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids =
        insert_raw_messages(&db, "cursor", "session-1", &["alpha", "beta", "gamma"]).await;

    let node = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "alpha through gamma",
            store_ids
                .iter()
                .copied()
                .map(|store_id| LcmSourceRef::RawMessage { store_id })
                .collect(),
        ))
        .await
        .expect("summary node insert should succeed");

    assert!(node.node_id.starts_with("sum_"));
    assert_eq!(node.summary_text, "alpha through gamma");
    assert_eq!(node.source_refs.len(), 3);
    assert_eq!(node.summary_token_count, 4);
    assert_eq!(node.source_token_count, 30);
    assert_eq!(node.source_time_start, Some(1_715_000_000));
    assert_eq!(node.source_time_end, Some(1_715_000_030));
    assert_eq!(node.expand_hint.as_deref(), Some("expand source lineage"));
    assert_eq!(node.metadata_json.as_deref(), Some(r#"{"topic":"dag"}"#));

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &node.node_id)
        .await
        .expect("summary node should expand");
    assert_eq!(expanded.summary, node);
    assert_eq!(expanded.sources.len(), 3);
    assert_eq!(
        expanded.sources[0].source_ref,
        LcmSourceRef::RawMessage {
            store_id: store_ids[0]
        }
    );
    assert_eq!(expanded.sources[0].content, "alpha");
    assert_eq!(
        expanded.sources[0].raw_message.as_ref().unwrap().message_id,
        "session-1-message-1"
    );
    assert_eq!(expanded.sources[1].content, "beta");
    assert_eq!(expanded.sources[2].content, "gamma");
}

#[tokio::test]
async fn summary_dag_survives_reopen() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = GlobalDb::open_at(&db_path).await.expect("session db open");
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha", "beta"]).await;
    let node = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "alpha and beta",
            store_ids
                .iter()
                .copied()
                .map(|store_id| LcmSourceRef::RawMessage { store_id })
                .collect(),
        ))
        .await
        .expect("summary node insert should succeed");
    drop(db);

    let reopened = GlobalDb::open_at(&db_path)
        .await
        .expect("session db reopen");
    let expanded = reopened
        .lcm_expand_summary_node("cursor", "session-1", &node.node_id)
        .await
        .expect("summary node should expand after reopen");

    assert_eq!(expanded.summary.node_id, node.node_id);
    assert_eq!(expanded.summary.summary_text, "alpha and beta");
    assert_eq!(expanded.sources.len(), 2);
    assert_eq!(expanded.sources[0].content, "alpha");
    assert_eq!(expanded.sources[1].content, "beta");
    assert_eq!(
        scalar_i64(
            &db_path,
            "SELECT COUNT(*) FROM session_summary_nodes
             WHERE summary_id IN (SELECT node_id FROM lcm_summary_nodes)",
        )
        .await,
        1,
        "authoritative summary and compatibility projection survive restart together"
    );
    assert_eq!(
        scalar_i64(
            &db_path,
            "SELECT COUNT(*) FROM session_temporal_generations WHERE state = 'active'",
        )
        .await,
        1
    );
}

#[tokio::test]
async fn summary_insert_rejects_missing_raw_source_without_persisting_rows() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;

    let result = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "bad missing raw source",
            vec![LcmSourceRef::RawMessage { store_id: 404 }],
        ))
        .await;

    assert!(matches!(
        result,
        Err(LcmError::SummarySourceNotOwnedBySession)
    ));
    assert_eq!(summary_table_counts(&db_path).await, (0, 0));
}

#[tokio::test]
async fn summary_insert_validates_source_session_ownership_without_persisting_rows() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let session_one = insert_raw_messages(&db, "cursor", "session-1", &["owned"]).await;
    let session_two = insert_raw_messages(&db, "cursor", "session-2", &["other"]).await;

    let cross_raw = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "bad raw source",
            vec![LcmSourceRef::RawMessage {
                store_id: session_two[0],
            }],
        ))
        .await
        .expect_err("cross-session raw source should be rejected at insert");
    assert!(matches!(
        cross_raw,
        LcmError::SummarySourceNotOwnedBySession
    ));
    assert_eq!(summary_table_counts(&db_path).await, (0, 0));

    let other_child = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-2",
            0,
            "child summary",
            vec![LcmSourceRef::RawMessage {
                store_id: session_two[0],
            }],
        ))
        .await
        .expect("child summary insert should succeed");
    let before_cross_child = summary_table_counts(&db_path).await;
    let cross_child = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            1,
            "bad child summary source",
            vec![
                LcmSourceRef::RawMessage {
                    store_id: session_one[0],
                },
                LcmSourceRef::SummaryNode {
                    node_id: other_child.node_id,
                },
            ],
        ))
        .await
        .expect_err("cross-session child summary source should be rejected at insert");
    assert!(matches!(
        cross_child,
        LcmError::SummarySourceNotOwnedBySession
    ));
    assert_eq!(summary_table_counts(&db_path).await, before_cross_child);
}

#[tokio::test]
async fn summary_expansion_marks_external_raw_sources_without_silent_empty_content() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let (store_id, payload_ref) =
        insert_external_raw_message(&db, &tmp, "cursor", "session-1", "tool-1").await;

    let node = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "summary over externalized tool payload",
            vec![LcmSourceRef::RawMessage { store_id }],
        ))
        .await
        .expect("summary node insert should succeed");

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &node.node_id)
        .await
        .expect("summary node should expand");
    assert_eq!(expanded.sources.len(), 1);
    let source = &expanded.sources[0];
    assert!(!source.content.is_empty());
    assert!(
        source
            .content
            .contains("[Externalized LCM ingest payload: kind=tool_result;")
    );
    assert!(source.content.contains(&payload_ref));
    let raw = source.raw_message.as_ref().expect("raw message source");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    assert_eq!(raw.payload_ref.as_deref(), Some(payload_ref.as_str()));
    assert_eq!(raw.content, source.content);
}

#[tokio::test]
async fn nested_summary_expansion_is_direct_only() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha"]).await;
    let child = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "child summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("child summary insert should succeed");
    let parent = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            1,
            "parent summary",
            vec![LcmSourceRef::SummaryNode {
                node_id: child.node_id.clone(),
            }],
        ))
        .await
        .expect("parent summary insert should succeed");

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &parent.node_id)
        .await
        .expect("parent summary should expand");
    assert_eq!(expanded.sources.len(), 1);
    assert_eq!(expanded.sources[0].content, child.summary_text);
    assert!(expanded.sources[0].raw_message.is_none());
    let expanded_child = expanded.sources[0]
        .summary_node
        .as_ref()
        .expect("direct child summary source");
    assert_eq!(expanded_child.node_id, child.node_id);
    assert_eq!(
        expanded_child.source_refs,
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0]
        }]
    );
}

#[tokio::test]
async fn summary_insert_rejects_non_decreasing_child_depth_without_persisting_rows() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha"]).await;
    let child = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            1,
            "child summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("child summary insert should succeed");
    let before = summary_table_counts(&db_path).await;

    let result = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            1,
            "parent summary at invalid depth",
            vec![LcmSourceRef::SummaryNode {
                node_id: child.node_id,
            }],
        ))
        .await;

    assert!(matches!(
        result,
        Err(LcmError::SummarySourceNotOwnedBySession)
    ));
    assert_eq!(summary_table_counts(&db_path).await, before);
}

#[tokio::test]
async fn summary_fts_matches_inserted_summary_text() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha"]).await;
    db.lcm_insert_summary_node(summary_draft(
        "cursor",
        "session-1",
        0,
        "unique summary fts phrase",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    ))
    .await
    .expect("summary node insert should succeed");

    assert_eq!(summary_fts_count(&db_path, "\"unique summary\"").await, 1);
}

#[tokio::test]
async fn summary_node_ids_are_stable_for_identical_drafts() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha"]).await;
    let draft = summary_draft(
        "cursor",
        "session-1",
        0,
        "stable summary",
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }],
    );

    let first = db
        .lcm_insert_summary_node(draft.clone())
        .await
        .expect("first summary insert should succeed");
    let second = db
        .lcm_insert_summary_node(draft)
        .await
        .expect("second summary insert should succeed");

    assert_eq!(first.node_id, second.node_id);
}

#[tokio::test]
async fn immutable_publication_replays_exactly_and_rejects_identity_conflicts() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha", "beta"]).await;
    let draft = summary_draft(
        "cursor",
        "session-1",
        0,
        "immutable alpha beta",
        store_ids
            .iter()
            .copied()
            .map(|store_id| LcmSourceRef::RawMessage { store_id })
            .collect(),
    );
    let publication = LcmImmutableSummaryPublication {
        summary_id: "summary.identity-1".to_string(),
        predecessor_summary_id: None,
        draft: draft.clone(),
    };

    let first = db
        .lcm_publish_immutable_summary(publication.clone())
        .await
        .expect("first publication");
    assert_eq!(
        first.disposition,
        LcmSummaryPublicationDisposition::Published
    );
    let replay = db
        .lcm_publish_immutable_summary(publication)
        .await
        .expect("exact replay");
    assert_eq!(
        replay.disposition,
        LcmSummaryPublicationDisposition::ExactReplay
    );
    assert_eq!(replay.summary, first.summary);
    assert_eq!(replay.generation, first.generation);
    assert_eq!(replay.frozen_watermarks_json, first.frozen_watermarks_json);
    assert_eq!(replay.published_at, first.published_at);
    assert_eq!(
        scalar_i64(&db_path, "SELECT COUNT(*) FROM session_summary_nodes").await,
        1
    );
    assert_eq!(
        scalar_i64(
            &db_path,
            "SELECT COUNT(*) FROM session_temporal_generations"
        )
        .await,
        1,
        "exact replay must not publish another generation"
    );
    assert_eq!(
        scalar_i64(&db_path, "SELECT COUNT(*) FROM session_summary_sources").await,
        scalar_i64(&db_path, "SELECT COUNT(*) FROM lcm_summary_sources").await,
        "the legacy source projection must stay in parity with authority"
    );

    let mut changed_content = draft.clone();
    changed_content.summary_text = "changed content".to_string();
    let content_conflict = db
        .lcm_publish_immutable_summary(LcmImmutableSummaryPublication {
            summary_id: "summary.identity-1".to_string(),
            predecessor_summary_id: None,
            draft: changed_content,
        })
        .await
        .expect_err("same identity with changed content must fail");
    assert!(matches!(
        content_conflict,
        LcmError::ImmutableSummaryConflict { ref summary_id }
            if summary_id == "summary.identity-1"
    ));

    let mut changed_order = draft;
    changed_order.source_refs.reverse();
    let order_conflict = db
        .lcm_publish_immutable_summary(LcmImmutableSummaryPublication {
            summary_id: "summary.identity-1".to_string(),
            predecessor_summary_id: None,
            draft: changed_order,
        })
        .await
        .expect_err("same identity with changed source order must fail");
    assert!(matches!(
        order_conflict,
        LcmError::ImmutableSummaryConflict { ref summary_id }
            if summary_id == "summary.identity-1"
    ));
}

#[tokio::test]
async fn immutable_publication_preserves_order_and_stales_transitive_descendants() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids =
        insert_raw_messages(&db, "cursor", "session-1", &["alpha", "beta", "gamma"]).await;

    let leaf = db
        .lcm_publish_immutable_summary(LcmImmutableSummaryPublication {
            summary_id: "summary.leaf-v1".to_string(),
            predecessor_summary_id: None,
            draft: summary_draft(
                "cursor",
                "session-1",
                0,
                "leaf v1",
                store_ids
                    .iter()
                    .copied()
                    .map(|store_id| LcmSourceRef::RawMessage { store_id })
                    .collect(),
            ),
        })
        .await
        .unwrap()
        .summary;
    let parent = db
        .lcm_publish_immutable_summary(LcmImmutableSummaryPublication {
            summary_id: "summary.parent".to_string(),
            predecessor_summary_id: None,
            draft: summary_draft(
                "cursor",
                "session-1",
                1,
                "parent",
                vec![LcmSourceRef::SummaryNode {
                    node_id: leaf.node_id.clone(),
                }],
            ),
        })
        .await
        .unwrap()
        .summary;
    let grandparent = db
        .lcm_publish_immutable_summary(LcmImmutableSummaryPublication {
            summary_id: "summary.grandparent".to_string(),
            predecessor_summary_id: None,
            draft: summary_draft(
                "cursor",
                "session-1",
                2,
                "grandparent",
                vec![LcmSourceRef::SummaryNode {
                    node_id: parent.node_id.clone(),
                }],
            ),
        })
        .await
        .unwrap()
        .summary;

    let successor = db
        .lcm_publish_immutable_summary(LcmImmutableSummaryPublication {
            summary_id: "summary.leaf-v2".to_string(),
            predecessor_summary_id: Some(leaf.node_id.clone()),
            draft: summary_draft(
                "cursor",
                "session-1",
                0,
                "leaf v2",
                store_ids
                    .iter()
                    .copied()
                    .map(|store_id| LcmSourceRef::RawMessage { store_id })
                    .collect(),
            ),
        })
        .await
        .expect("successor publication")
        .summary;

    assert_eq!(
        text_rows(
            &db_path,
            "SELECT authority.source_ordinal || ':' || authority.source_kind || ':'
                    || compatibility.source_id
             FROM session_summary_sources authority
             JOIN lcm_summary_sources compatibility
               ON compatibility.node_id = authority.summary_id
              AND compatibility.ordinal = authority.source_ordinal
             WHERE authority.summary_id = 'summary.leaf-v1'
             ORDER BY authority.source_ordinal",
        )
        .await,
        vec![
            format!("0:anchor:{}", store_ids[0]),
            format!("1:anchor:{}", store_ids[1]),
            format!("2:anchor:{}", store_ids[2]),
        ],
        "the authoritative manifest and compatibility projection preserve source order"
    );
    assert_eq!(
        text_rows(
            &db_path,
            "SELECT predecessor_summary_id || '>' || successor_summary_id
             FROM session_summary_successors ORDER BY created_at",
        )
        .await,
        vec![format!("{}>{}", leaf.node_id, successor.node_id)]
    );
    let active = text_rows(
        &db_path,
        "SELECT summary_id || ':' || availability
         FROM session_summary_availability
         WHERE (session_id, generation) = (
             SELECT session_id, generation FROM session_temporal_generations
             WHERE session_id = 'session-1' AND state = 'active'
         )
         ORDER BY summary_id",
    )
    .await;
    assert!(active.contains(&format!("{}:stale", leaf.node_id)));
    assert!(active.contains(&format!("{}:stale", parent.node_id)));
    assert!(active.contains(&format!("{}:stale", grandparent.node_id)));
    assert!(active.contains(&format!("{}:available", successor.node_id)));
}

#[tokio::test]
async fn immutable_publication_rejects_cycles_and_rolls_back_every_projection() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha"]).await;
    let _leaf = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "leaf",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .unwrap();

    let cycle = db
        .lcm_publish_immutable_summary(LcmImmutableSummaryPublication {
            summary_id: "summary.future".to_string(),
            predecessor_summary_id: None,
            draft: summary_draft(
                "cursor",
                "session-1",
                1,
                "cycle",
                vec![LcmSourceRef::SummaryNode {
                    node_id: "summary.future".to_string(),
                }],
            ),
        })
        .await
        .expect_err("self/source cycle must fail");
    assert!(matches!(cycle, LcmError::SummaryCycle { .. }));

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_compatibility_summary
         BEFORE INSERT ON lcm_summary_nodes
         WHEN NEW.node_id = 'summary.rollback'
         BEGIN SELECT RAISE(ABORT, 'forced compatibility failure'); END;",
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);
    let before = lineage_effect_count(&db_path).await;
    let rollback = db
        .lcm_publish_immutable_summary(LcmImmutableSummaryPublication {
            summary_id: "summary.rollback".to_string(),
            predecessor_summary_id: None,
            draft: summary_draft(
                "cursor",
                "session-1",
                0,
                "rollback",
                vec![LcmSourceRef::RawMessage {
                    store_id: store_ids[0],
                }],
            ),
        })
        .await;
    assert!(rollback.is_err());
    let after = lineage_effect_count(&db_path).await;
    assert_eq!(
        after, before,
        "all authoritative and projection writes roll back"
    );
}

#[tokio::test]
async fn immutable_publication_rejects_redacted_deleted_and_expired_sources() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["sensitive"]).await;
    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();

    for (identity, metadata, reason) in [
        (
            "summary.redacted",
            r#"{"payload_access":"redacted"}"#,
            "redacted",
        ),
        (
            "summary.deleted",
            r#"{"payload_access":"deleted"}"#,
            "deleted",
        ),
        (
            "summary.expired",
            r#"{"retention_expires_at":1}"#,
            "retention_expired",
        ),
    ] {
        conn.execute(
            "UPDATE lcm_raw_messages SET metadata_json = ?2 WHERE store_id = ?1",
            libsql::params![store_ids[0], metadata],
        )
        .await
        .unwrap();
        let error = db
            .lcm_publish_immutable_summary(LcmImmutableSummaryPublication {
                summary_id: identity.to_string(),
                predecessor_summary_id: None,
                draft: summary_draft(
                    "cursor",
                    "session-1",
                    0,
                    "must not publish",
                    vec![LcmSourceRef::RawMessage {
                        store_id: store_ids[0],
                    }],
                ),
            })
            .await
            .expect_err("ineligible source must fail closed");
        assert!(matches!(
            error,
            LcmError::SummarySourceUnavailable {
                reason: ref actual,
                ..
            } if actual == reason
        ));
    }
    assert_eq!(
        scalar_i64(&db_path, "SELECT COUNT(*) FROM session_summary_nodes").await,
        0
    );
    assert_eq!(
        scalar_i64(&db_path, "SELECT COUNT(*) FROM lcm_summary_nodes").await,
        0
    );
}

#[tokio::test]
async fn codex_auxiliary_summary_appends_successor_without_replacement() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "codex", "session-1", &["alpha", "beta"]).await;
    let mut draft = summary_draft(
        "codex",
        "session-1",
        0,
        "encrypted compaction placeholder",
        store_ids
            .iter()
            .copied()
            .map(|store_id| LcmSourceRef::RawMessage { store_id })
            .collect(),
    );
    draft.metadata_json = Some(r#"{"source":"codex_context_compacted"}"#.to_string());
    let original = db.lcm_insert_summary_node(draft).await.unwrap();

    let successor = db
        .publish_codex_compaction_summary_successor(
            &original.node_id,
            "visible Codex auxiliary summary",
            "codex_app_server",
            Some("gpt-test"),
        )
        .await
        .expect("Codex replacement publishes an immutable successor");
    assert_ne!(successor.node_id, original.node_id);
    assert_eq!(successor.summary_text, "visible Codex auxiliary summary");
    assert_eq!(
        scalar_i64(&db_path, "SELECT COUNT(*) FROM session_summary_nodes").await,
        2
    );
    assert_eq!(
        scalar_i64(&db_path, "SELECT COUNT(*) FROM lcm_summary_nodes").await,
        2
    );
    assert_eq!(
        text_rows(
            &db_path,
            "SELECT predecessor_summary_id || '>' || successor_summary_id
             FROM session_summary_successors",
        )
        .await,
        vec![format!("{}>{}", original.node_id, successor.node_id)]
    );
    db.lcm_expand_summary_node("codex", "session-1", &original.node_id)
        .await
        .expect("original summary remains addressable");
}

// A boundary links sessions without changing summary authority or its
// compatibility projection owner.
#[tokio::test]
async fn boundary_link_does_not_reassign_summary_nodes() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["alpha", "beta"]).await;
    let node = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            "session-1",
            0,
            "carried summary",
            store_ids
                .iter()
                .copied()
                .map(|store_id| LcmSourceRef::RawMessage { store_id })
                .collect(),
        ))
        .await
        .expect("summary node insert should succeed");

    let boundary = db
        .lcm_session_boundary(LcmSessionBoundaryRequest {
            provider: "cursor".to_string(),
            session_id: "session-2".to_string(),
            old_session_id: Some("session-1".to_string()),
            boundary_reason: Some("compression".to_string()),
            bound_session_id: Some("session-1".to_string()),
            boundary_skip_at: None,
        })
        .await
        .expect("boundary carry-over should succeed");
    assert!(boundary.recorded);
    assert_eq!(boundary.reason, "compression_boundary_carried_over");

    let expanded = db
        .lcm_expand_summary_node("cursor", "session-1", &node.node_id)
        .await
        .expect("source-owned node remains addressable");
    assert_eq!(expanded.summary.node_id, node.node_id);
    assert_eq!(expanded.summary.session_id, "session-1");
    assert_eq!(expanded.sources.len(), 2);
    assert_eq!(expanded.sources[0].content, "alpha");

    let target = db
        .lcm_expand_summary_node("cursor", "session-2", &node.node_id)
        .await;
    assert!(matches!(target, Err(LcmError::SummaryNodeNotFound)));
}

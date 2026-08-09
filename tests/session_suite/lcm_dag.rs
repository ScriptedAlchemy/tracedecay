use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_sessions::runtime::lcm::types::{
    LcmImmutableSummaryPublication, LcmSummaryPublicationDisposition,
};
use tracedecay_sessions::runtime::lcm::{
    LcmDescribeRequest, LcmDescribeTarget, LcmError, LcmGrepRequest, LcmGrepSort, LcmScope,
    LcmSessionBoundaryRequest, LcmSourceRef, LcmStorageKind, LcmSummaryNodeDraft,
};

use crate::common::{lcm_dag_message as raw_message, lcm_dag_session as sample_session};

async fn registered_lcm_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .expect("registered LCM test runtime")
}

trait ProfileLcmFixture {
    async fn upsert_session(&self, session: &tracedecay_sessions::runtime::SessionRecord) -> bool;

    async fn upsert_session_message(
        &self,
        message: &tracedecay_sessions::runtime::SessionMessageRecord,
    ) -> bool;

    async fn lcm_insert_summary_node(
        &self,
        draft: LcmSummaryNodeDraft,
    ) -> Result<tracedecay_sessions::runtime::lcm::LcmSummaryNode, LcmError>;

    async fn lcm_publish_immutable_summary(
        &self,
        publication: LcmImmutableSummaryPublication,
    ) -> Result<tracedecay_sessions::runtime::lcm::types::LcmSummaryPublicationReceipt, LcmError>;
}

impl ProfileLcmFixture for HostAdmissionTestRuntimeV1 {
    async fn upsert_session(&self, session: &tracedecay_sessions::runtime::SessionRecord) -> bool {
        self.upsert_session_for_test(HostAdmissionScope::Profile, session)
            .await
            .unwrap_or(false)
    }

    async fn upsert_session_message(
        &self,
        message: &tracedecay_sessions::runtime::SessionMessageRecord,
    ) -> bool {
        self.upsert_session_message_for_test(HostAdmissionScope::Profile, message)
            .await
            .unwrap_or(false)
    }

    async fn lcm_insert_summary_node(
        &self,
        draft: LcmSummaryNodeDraft,
    ) -> Result<tracedecay_sessions::runtime::lcm::LcmSummaryNode, LcmError> {
        self.lcm_insert_summary_node_for_test(HostAdmissionScope::Profile, draft)
            .await
    }

    async fn lcm_publish_immutable_summary(
        &self,
        publication: LcmImmutableSummaryPublication,
    ) -> Result<tracedecay_sessions::runtime::lcm::types::LcmSummaryPublicationReceipt, LcmError>
    {
        self.lcm_publish_immutable_summary_for_test(HostAdmissionScope::Profile, publication)
            .await
    }
}

async fn summary_table_counts(db: &HostAdmissionTestRuntimeV1) -> (i64, i64) {
    let sessions = db
        .lcm_recent_sessions_for_test(None, 100)
        .await
        .expect("recent sessions");
    let mut node_ids = std::collections::BTreeSet::new();
    let mut source_count = 0_i64;
    for session in sessions {
        let description = db
            .lcm_describe_for_test(LcmDescribeRequest {
                provider: session.provider,
                session_id: session.session_id,
                target: LcmDescribeTarget::Session,
            })
            .await
            .expect("session description");
        for node in description.summary_nodes {
            if node_ids.insert(node.node_id) {
                source_count += node.source_count as i64;
            }
        }
    }
    (node_ids.len() as i64, source_count)
}

async fn summary_fts_count(db: &HostAdmissionTestRuntimeV1, query: &str) -> i64 {
    db.lcm_grep_for_test(LcmGrepRequest {
        provider: "all".to_string(),
        query: query.to_string(),
        scope: LcmScope::All,
        session_id: None,
        include_summaries: true,
        limit: 100,
        sort: LcmGrepSort::Relevance,
        source: None,
        role: None,
        start_time: None,
        end_time: None,
        git_filter: Default::default(),
    })
    .await
    .expect("summary search")
    .hits
    .into_iter()
    .filter(|hit| hit.kind == "summary_node")
    .count() as i64
}

async fn lineage_effect_count(db: &HostAdmissionTestRuntimeV1) -> i64 {
    let (nodes, sources) = summary_table_counts(db).await;
    let successors = db
        .lcm_summary_successor_edges_for_test()
        .await
        .expect("summary successors")
        .len() as i64;
    nodes + sources + successors
}

async fn insert_session(db: &HostAdmissionTestRuntimeV1, provider: &str, session_id: &str) {
    assert!(
        db.upsert_session(&sample_session(provider, session_id))
            .await
    );
}

async fn insert_raw_messages(
    db: &HostAdmissionTestRuntimeV1,
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
            .lcm_load_raw_message_for_test(provider, &message_id)
            .await
            .expect("raw message should exist");
        store_ids.push(raw.store_id);
    }
    store_ids
}

async fn insert_external_raw_message(
    db: &HostAdmissionTestRuntimeV1,
    _tmp: &TempDir,
    provider: &str,
    session_id: &str,
    message_id: &str,
) -> (i64, String) {
    insert_session(db, provider, session_id).await;
    let payload = format!("tool output\n{}", "X".repeat(300_000));
    let mut message = raw_message(provider, message_id, session_id, 1, &payload);
    message.role = "tool".to_string();
    message.kind = Some("tool_result".to_string());

    db.lcm_ingest_raw_message_for_test(HostAdmissionScope::Profile, &message)
        .await
        .expect("raw ingest should externalize payload");
    let raw = db
        .lcm_load_raw_message_for_test(provider, message_id)
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
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids =
        insert_raw_messages(&db, "cursor", "session-1", &["alpha", "beta", "gamma"]).await;
    let mut first_source = raw_message("cursor", "session-1-message-1", "session-1", 1, "alpha");
    first_source.timestamp = Some(1_715_000_001_000_000);
    assert!(db.upsert_session_message(&first_source).await);

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
        .lcm_expand_summary_node_for_test("cursor", "session-1", &node.node_id)
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
    assert_eq!(
        expanded.sources[0].raw_message.as_ref().unwrap().timestamp,
        Some(1_715_000_001)
    );
    assert_eq!(expanded.sources[1].content, "beta");
    assert_eq!(expanded.sources[2].content, "gamma");
}

#[tokio::test]
async fn summary_dag_survives_reopen() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .expect("registered session runtime");
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

    let reopened = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .expect("registered session runtime reopen");
    let expanded = reopened
        .lcm_expand_summary_node_for_test("cursor", "session-1", &node.node_id)
        .await
        .expect("summary node should expand after reopen");

    assert_eq!(expanded.summary.node_id, node.node_id);
    assert_eq!(expanded.summary.summary_text, "alpha and beta");
    assert_eq!(expanded.sources.len(), 2);
    assert_eq!(expanded.sources[0].content, "alpha");
    assert_eq!(expanded.sources[1].content, "beta");
    assert_eq!(
        summary_table_counts(&reopened).await,
        (1, 2),
        "authoritative summary and compatibility projection survive restart together"
    );
}

#[tokio::test]
async fn summary_insert_rejects_missing_raw_source_without_persisting_rows() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
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
    assert_eq!(summary_table_counts(&db).await, (0, 0));
}

#[tokio::test]
async fn summary_insert_validates_source_session_ownership_without_persisting_rows() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
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
    assert_eq!(summary_table_counts(&db).await, (0, 0));

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
    let before_cross_child = summary_table_counts(&db).await;
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
    assert_eq!(summary_table_counts(&db).await, before_cross_child);
}

#[tokio::test]
async fn summary_expansion_marks_external_raw_sources_without_silent_empty_content() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
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
        .lcm_expand_summary_node_for_test("cursor", "session-1", &node.node_id)
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
    let db = registered_lcm_runtime(&tmp).await;
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
        .lcm_expand_summary_node_for_test("cursor", "session-1", &parent.node_id)
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
    let db = registered_lcm_runtime(&tmp).await;
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
    let before = summary_table_counts(&db).await;

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
    assert_eq!(summary_table_counts(&db).await, before);
}

#[tokio::test]
async fn summary_fts_matches_inserted_summary_text() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
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

    assert_eq!(summary_fts_count(&db, "\"unique summary\"").await, 1);
}

#[tokio::test]
async fn summary_node_ids_are_stable_for_identical_drafts() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
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
    let db = registered_lcm_runtime(&tmp).await;
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
    assert_eq!(summary_table_counts(&db).await, (1, 2));

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
    let db = registered_lcm_runtime(&tmp).await;
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

    let expanded_leaf = db
        .lcm_expand_summary_node_for_test("cursor", "session-1", &leaf.node_id)
        .await
        .expect("leaf expansion");
    assert_eq!(
        expanded_leaf
            .sources
            .iter()
            .enumerate()
            .map(|(ordinal, source)| match source.source_ref {
                LcmSourceRef::RawMessage { store_id } => {
                    format!("{ordinal}:anchor:{store_id}")
                }
                LcmSourceRef::SummaryNode { ref node_id } => {
                    format!("{ordinal}:summary:{node_id}")
                }
            })
            .collect::<Vec<_>>(),
        vec![
            format!("0:anchor:{}", store_ids[0]),
            format!("1:anchor:{}", store_ids[1]),
            format!("2:anchor:{}", store_ids[2]),
        ],
        "the authoritative manifest and compatibility projection preserve source order"
    );
    assert_eq!(
        db.lcm_summary_successor_edges_for_test()
            .await
            .expect("summary successor edges"),
        vec![(leaf.node_id.clone(), successor.node_id.clone())]
    );
    let active = db
        .lcm_active_summary_availability_for_test("session-1")
        .await
        .expect("active summary availability");
    assert!(active.contains(&(leaf.node_id.clone(), "stale".to_string())));
    assert!(active.contains(&(parent.node_id.clone(), "stale".to_string())));
    assert!(active.contains(&(grandparent.node_id.clone(), "stale".to_string())));
    assert!(active.contains(&(successor.node_id.clone(), "available".to_string())));
}

#[tokio::test]
async fn immutable_publication_rejects_cycles_and_rolls_back_every_projection() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
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

    db.install_lcm_summary_insert_abort_trigger_for_test()
        .await
        .expect("install summary insert fault");
    let before = lineage_effect_count(&db).await;
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
    let after = lineage_effect_count(&db).await;
    assert_eq!(
        after, before,
        "all authoritative and projection writes roll back"
    );
    db.remove_lcm_summary_insert_abort_trigger_for_test()
        .await
        .expect("remove summary insert fault");
}

#[tokio::test]
async fn immutable_publication_rejects_redacted_deleted_and_expired_sources() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_raw_messages(&db, "cursor", "session-1", &["sensitive"]).await;

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
        let mut source = raw_message("cursor", "session-1-message-1", "session-1", 1, "sensitive");
        source.metadata_json = Some(metadata.to_string());
        assert!(db.upsert_session_message(&source).await);
        assert_eq!(
            db.lcm_load_raw_message_for_test("cursor", "session-1-message-1")
                .await
                .expect("updated raw message")
                .store_id,
            store_ids[0]
        );
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
    assert_eq!(summary_table_counts(&db).await, (0, 0));
}

// A boundary links sessions without changing summary authority or its
// compatibility projection owner.
#[tokio::test]
async fn boundary_link_does_not_reassign_summary_nodes() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
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
        .lcm_session_boundary_for_test(LcmSessionBoundaryRequest {
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
        .lcm_expand_summary_node_for_test("cursor", "session-1", &node.node_id)
        .await
        .expect("source-owned node remains addressable");
    assert_eq!(expanded.summary.node_id, node.node_id);
    assert_eq!(expanded.summary.session_id, "session-1");
    assert_eq!(expanded.sources.len(), 2);
    assert_eq!(expanded.sources[0].content, "alpha");

    let target = db
        .lcm_expand_summary_node_for_test("cursor", "session-2", &node.node_id)
        .await;
    assert!(matches!(target, Err(LcmError::SummaryNodeNotFound)));
}

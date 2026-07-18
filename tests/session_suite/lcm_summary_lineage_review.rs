use sha2::Digest;
use std::fs;
use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::sessions::lcm::types::{
    LcmImmutableSummaryPublication, LcmSummaryPublicationDisposition,
};
use tracedecay::sessions::lcm::{
    LcmDescribeRequest, LcmDescribeTarget, LcmError, LcmExpandRequest, LcmExpandTarget,
    LcmGrepRequest, LcmGrepSort, LcmScope, LcmSourceRef, LcmSummaryNodeDraft,
};

use crate::common::{isolated_lcm_db_path, lcm_dag_message, lcm_dag_session, open_lcm_db};

const FOREIGN_CANARY: &str = "sk-proj-lineage-foreign-canary-1234567890";

async fn insert_messages(
    db: &GlobalDb,
    provider: &str,
    session_id: &str,
    contents: &[&str],
) -> Vec<i64> {
    assert!(
        db.upsert_session(&lcm_dag_session(provider, session_id))
            .await
    );
    let mut store_ids = Vec::new();
    for (index, content) in contents.iter().enumerate() {
        let message_id = format!("{session_id}-lineage-{}", index + 1);
        assert!(
            db.upsert_session_message(&lcm_dag_message(
                provider,
                &message_id,
                session_id,
                (index + 1) as i64,
                content,
            ))
            .await
        );
        store_ids.push(
            db.lcm_load_raw_message(provider, &message_id)
                .await
                .unwrap()
                .store_id,
        );
    }
    store_ids
}

fn draft(
    provider: &str,
    session_id: &str,
    depth: i64,
    text: &str,
    source_refs: Vec<LcmSourceRef>,
) -> LcmSummaryNodeDraft {
    LcmSummaryNodeDraft {
        provider: provider.to_string(),
        conversation_id: "conversation.lineage-review".to_string(),
        session_id: session_id.to_string(),
        depth,
        summary_text: text.to_string(),
        source_refs,
        source_token_count: 20,
        summary_token_count: 4,
        source_time_start: Some(1_715_000_000),
        source_time_end: Some(1_715_000_010),
        expand_hint: Some("lineage review".to_string()),
        metadata_json: Some(r#"{"route":"lineage-review"}"#.to_string()),
    }
}

fn publication(
    summary_id: &str,
    predecessor_summary_id: Option<String>,
    draft: LcmSummaryNodeDraft,
) -> LcmImmutableSummaryPublication {
    LcmImmutableSummaryPublication {
        summary_id: summary_id.to_string(),
        predecessor_summary_id,
        draft,
    }
}

async fn scalar(db_path: &std::path::Path, sql: &str) -> i64 {
    let database = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = database.connect().unwrap();
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

#[tokio::test]
async fn exact_replay_uses_only_frozen_canonical_authority() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-replay", &["alpha", "beta"]).await;
    let requested = publication(
        "summary.replay.canonical",
        None,
        draft(
            "cursor",
            "session-replay",
            0,
            "frozen canonical summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ),
    );
    let first = db
        .lcm_publish_immutable_summary(requested.clone())
        .await
        .unwrap();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "UPDATE lcm_summary_nodes
         SET summary_text = 'corrupt legacy projection'
         WHERE node_id = 'summary.replay.canonical'",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE lcm_raw_messages SET timestamp = timestamp + 999999
         WHERE store_id = ?1",
        libsql::params![store_ids[0]],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    db.lcm_publish_immutable_summary(publication(
        "summary.replay.generation-evolution",
        None,
        draft(
            "cursor",
            "session-replay",
            0,
            "unrelated generation",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[1],
            }],
        ),
    ))
    .await
    .unwrap();

    let replay = db
        .lcm_publish_immutable_summary(requested)
        .await
        .expect("legacy, anchor, and active-generation evolution must not affect exact replay");
    assert_eq!(
        replay.disposition,
        LcmSummaryPublicationDisposition::ExactReplay
    );
    assert_eq!(replay.summary, first.summary);
    assert_eq!(replay.generation, first.generation);
    assert_eq!(replay.frozen_watermarks_json, first.frozen_watermarks_json);
    assert_eq!(replay.published_at, first.published_at);
}

#[tokio::test]
async fn exact_replay_rejects_missing_generation() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-missing-gen", &["alpha"]).await;
    let requested = publication(
        "summary.replay.missing-generation",
        None,
        draft(
            "cursor",
            "session-missing-gen",
            0,
            "missing generation summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ),
    );
    let first = db
        .lcm_publish_immutable_summary(requested.clone())
        .await
        .unwrap();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    // Generations are durable under product triggers; drop only for this
    // fail-first corruption probe against exact-replay verification.
    conn.execute_batch(
        "DROP TRIGGER session_temporal_generations_delete_guard_v1;
         PRAGMA foreign_keys = OFF;",
    )
    .await
    .unwrap();
    conn.execute(
        "DELETE FROM session_temporal_generations
         WHERE session_id = 'session-missing-gen' AND generation = ?1",
        libsql::params![first.generation],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = db
        .lcm_publish_immutable_summary(requested)
        .await
        .expect_err("missing generation must never replay as success");
    assert!(matches!(
        error,
        LcmError::ImmutableSummaryConflict {
            ref summary_id
        } if summary_id == "summary.replay.missing-generation"
    ));
}

#[tokio::test]
async fn exact_replay_rejects_changed_generation_watermarks() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-changed-wm", &["alpha"]).await;
    let requested = publication(
        "summary.replay.changed-watermarks",
        None,
        draft(
            "cursor",
            "session-changed-wm",
            0,
            "changed watermarks summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ),
    );
    let first = db
        .lcm_publish_immutable_summary(requested.clone())
        .await
        .unwrap();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "DROP TRIGGER session_temporal_generations_state_guard_v1",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET frozen_watermarks_json = ?1
         WHERE session_id = 'session-changed-wm' AND generation = ?2",
        libsql::params![
            r#"{"active_generation":null,"source_frontier":0,"projection_frontier":0,"summary_frontier":"tampered","route":"tampered"}"#,
            first.generation
        ],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = db
        .lcm_publish_immutable_summary(requested)
        .await
        .expect_err("changed generation watermarks must never replay as success");
    assert!(matches!(
        error,
        LcmError::ImmutableSummaryConflict {
            ref summary_id
        } if summary_id == "summary.replay.changed-watermarks"
    ));
}

#[tokio::test]
async fn exact_replay_rejects_missing_availability() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-missing-avail", &["alpha"]).await;
    let requested = publication(
        "summary.replay.missing-availability",
        None,
        draft(
            "cursor",
            "session-missing-avail",
            0,
            "missing availability summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ),
    );
    let first = db
        .lcm_publish_immutable_summary(requested.clone())
        .await
        .unwrap();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "DELETE FROM session_summary_availability
         WHERE session_id = 'session-missing-avail'
           AND generation = ?1
           AND summary_id = 'summary.replay.missing-availability'",
        libsql::params![first.generation],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = db
        .lcm_publish_immutable_summary(requested)
        .await
        .expect_err("missing availability must never replay as success");
    assert!(matches!(
        error,
        LcmError::ImmutableSummaryConflict {
            ref summary_id
        } if summary_id == "summary.replay.missing-availability"
    ));
}

#[tokio::test]
async fn exact_replay_rejects_mismatched_horizon_or_state() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-mismatch-horizon", &["alpha"]).await;
    let requested = publication(
        "summary.replay.mismatch-horizon",
        None,
        draft(
            "cursor",
            "session-mismatch-horizon",
            0,
            "mismatch horizon summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ),
    );
    let first = db
        .lcm_publish_immutable_summary(requested.clone())
        .await
        .unwrap();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "UPDATE session_summary_availability
         SET source_horizon_json = ?1
         WHERE session_id = 'session-mismatch-horizon'
           AND generation = ?2
           AND summary_id = 'summary.replay.mismatch-horizon'",
        libsql::params![
            r#"{"knowledge_through":0,"valid_through":0}"#,
            first.generation
        ],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = db
        .lcm_publish_immutable_summary(requested.clone())
        .await
        .expect_err("mismatched availability horizon must never replay as success");
    assert!(matches!(
        error,
        LcmError::ImmutableSummaryConflict {
            ref summary_id
        } if summary_id == "summary.replay.mismatch-horizon"
    ));

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    // Restore a valid horizon JSON then mark availability non-available.
    conn.execute(
        "UPDATE session_summary_availability
         SET source_horizon_json = (
                SELECT source_horizon_json FROM session_summary_nodes
                WHERE summary_id = 'summary.replay.mismatch-horizon'
             ),
             availability = 'stale',
             reason = 'tampered'
         WHERE session_id = 'session-mismatch-horizon'
           AND generation = ?1
           AND summary_id = 'summary.replay.mismatch-horizon'",
        libsql::params![first.generation],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = db
        .lcm_publish_immutable_summary(requested.clone())
        .await
        .expect_err("non-available availability state must never replay as success");
    assert!(matches!(
        error,
        LcmError::ImmutableSummaryConflict {
            ref summary_id
        } if summary_id == "summary.replay.mismatch-horizon"
    ));

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "UPDATE session_summary_availability
         SET availability = 'available', reason = NULL
         WHERE session_id = 'session-mismatch-horizon'
           AND generation = ?1
           AND summary_id = 'summary.replay.mismatch-horizon'",
        libsql::params![first.generation],
    )
    .await
    .unwrap();
    conn.execute(
        "DROP TRIGGER session_temporal_generations_state_guard_v1",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'failed',
             completed_at = COALESCE(completed_at, activated_at, ready_at, created_at)
         WHERE session_id = 'session-mismatch-horizon' AND generation = ?1",
        libsql::params![first.generation],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = db
        .lcm_publish_immutable_summary(requested)
        .await
        .expect_err("failed generation state must never replay as success");
    assert!(matches!(
        error,
        LcmError::ImmutableSummaryConflict {
            ref summary_id
        } if summary_id == "summary.replay.mismatch-horizon"
    ));
}

#[tokio::test]
async fn exact_replay_allows_valid_superseded_generation() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(
        &db,
        "cursor",
        "session-superseded-replay",
        &["alpha", "beta"],
    )
    .await;
    let requested = publication(
        "summary.replay.superseded",
        None,
        draft(
            "cursor",
            "session-superseded-replay",
            0,
            "superseded but immutable",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ),
    );
    let first = db
        .lcm_publish_immutable_summary(requested.clone())
        .await
        .unwrap();

    db.lcm_publish_immutable_summary(publication(
        "summary.replay.successor-active",
        None,
        draft(
            "cursor",
            "session-superseded-replay",
            0,
            "active successor generation",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[1],
            }],
        ),
    ))
    .await
    .unwrap();

    let replay = db
        .lcm_publish_immutable_summary(requested)
        .await
        .expect("superseded immutable generation may exact-replay");
    assert_eq!(
        replay.disposition,
        LcmSummaryPublicationDisposition::ExactReplay
    );
    assert_eq!(replay.summary, first.summary);
    assert_eq!(replay.generation, first.generation);
    assert_eq!(replay.frozen_watermarks_json, first.frozen_watermarks_json);
    assert_eq!(replay.published_at, first.published_at);
    assert_eq!(first.generation, 1);
}

#[tokio::test]
async fn changed_logical_summary_requires_its_current_predecessor() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-predecessor", &["alpha"]).await;
    let sources = vec![LcmSourceRef::RawMessage {
        store_id: store_ids[0],
    }];
    let first = db
        .lcm_publish_immutable_summary(publication(
            "summary.logical.v1",
            None,
            draft(
                "cursor",
                "session-predecessor",
                0,
                "logical v1",
                sources.clone(),
            ),
        ))
        .await
        .unwrap();

    let missing = db
        .lcm_publish_immutable_summary(publication(
            "summary.logical.v2.missing",
            None,
            draft(
                "cursor",
                "session-predecessor",
                0,
                "logical v2",
                sources.clone(),
            ),
        ))
        .await
        .expect_err("a changed logical summary cannot silently become another root");
    assert!(matches!(
        missing,
        LcmError::SummaryPredecessorRequired {
            ref current_predecessor_id,
            ..
        } if current_predecessor_id == "summary.logical.v1"
    ));

    let successor = db
        .lcm_publish_immutable_summary(publication(
            "summary.logical.v2",
            Some(first.summary.node_id.clone()),
            draft("cursor", "session-predecessor", 0, "logical v2", sources),
        ))
        .await
        .expect("the current predecessor authorizes the logical change");
    assert_eq!(
        successor.disposition,
        LcmSummaryPublicationDisposition::Published
    );
}

#[tokio::test]
async fn successor_rejects_an_incompatible_logical_identity() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-identity", &["alpha", "beta"]).await;
    let first = db
        .lcm_publish_immutable_summary(publication(
            "summary.identity.alpha",
            None,
            draft(
                "cursor",
                "session-identity",
                0,
                "alpha",
                vec![LcmSourceRef::RawMessage {
                    store_id: store_ids[0],
                }],
            ),
        ))
        .await
        .unwrap();

    let error = db
        .lcm_publish_immutable_summary(publication(
            "summary.identity.beta",
            Some(first.summary.node_id),
            draft(
                "cursor",
                "session-identity",
                0,
                "beta",
                vec![LcmSourceRef::RawMessage {
                    store_id: store_ids[1],
                }],
            ),
        ))
        .await
        .expect_err("a predecessor from another logical identity must not authorize publication");
    assert!(matches!(
        error,
        LcmError::InvalidSummarySuccessor {
            ref predecessor_summary_id,
            ..
        } if predecessor_summary_id == "summary.identity.alpha"
    ));
}

#[tokio::test]
async fn summary_source_owner_must_match_its_frozen_publication_owner() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-owner", &["alpha"]).await;
    let leaf = db
        .lcm_publish_immutable_summary(publication(
            "summary.owner.leaf",
            None,
            draft(
                "cursor",
                "session-owner",
                0,
                "leaf",
                vec![LcmSourceRef::RawMessage {
                    store_id: store_ids[0],
                }],
            ),
        ))
        .await
        .unwrap();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS retrieval_anchors_immutable_update;
         DROP TRIGGER IF EXISTS observation_retrieval_anchors_immutable_update;",
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE retrieval_anchors
         SET owner_json = '{\"kind\":\"session\",\"provider\":\"wrong\",\"session_id\":\"wrong\"}',
             anchor_json = json_set(
                 anchor_json,
                 '$.owner',
                 json('{\"kind\":\"session\",\"provider\":\"wrong\",\"session_id\":\"wrong\"}')
             )
         WHERE anchor_id = (
             SELECT summary_anchor_id FROM session_summary_nodes
             WHERE summary_id = 'summary.owner.leaf'
         )",
        (),
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = db
        .lcm_publish_immutable_summary(publication(
            "summary.owner.parent",
            None,
            draft(
                "cursor",
                "session-owner",
                1,
                "parent",
                vec![LcmSourceRef::SummaryNode {
                    node_id: leaf.summary.node_id,
                }],
            ),
        ))
        .await
        .expect_err("uniformly wrong source owners must fail authorization");
    assert!(matches!(error, LcmError::SummarySourceNotOwnedBySession));
}

#[tokio::test]
async fn generation_stale_closure_rejects_corrupt_cycles() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-cycle", &["alpha"]).await;
    let raw_sources = vec![LcmSourceRef::RawMessage {
        store_id: store_ids[0],
    }];
    let leaf = db
        .lcm_publish_immutable_summary(publication(
            "summary.cycle.leaf",
            None,
            draft("cursor", "session-cycle", 0, "leaf", raw_sources.clone()),
        ))
        .await
        .unwrap();
    let parent = db
        .lcm_publish_immutable_summary(publication(
            "summary.cycle.parent",
            None,
            draft(
                "cursor",
                "session-cycle",
                1,
                "parent",
                vec![LcmSourceRef::SummaryNode {
                    node_id: leaf.summary.node_id.clone(),
                }],
            ),
        ))
        .await
        .unwrap();

    let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute_batch("DROP TRIGGER session_summary_sources_immutable_update_v1;")
        .await
        .unwrap();
    conn.execute(
        "UPDATE session_summary_sources
         SET source_kind = 'summary', source_anchor_id = NULL, source_summary_id = ?2
         WHERE summary_id = ?1 AND source_ordinal = 0",
        libsql::params![
            leaf.summary.node_id.as_str(),
            parent.summary.node_id.as_str()
        ],
    )
    .await
    .unwrap();
    drop(conn);
    drop(raw_db);

    let error = db
        .lcm_publish_immutable_summary(publication(
            "summary.cycle.successor",
            Some(leaf.summary.node_id),
            draft("cursor", "session-cycle", 0, "successor", raw_sources),
        ))
        .await
        .expect_err("bounded stale closure must reject a corrupt dependency cycle");
    assert!(matches!(error, LcmError::SummaryCycle { .. }));
}

#[tokio::test]
async fn generation_stale_closure_rejects_excessive_depth() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-depth", &["alpha"]).await;
    let raw_sources = vec![LcmSourceRef::RawMessage {
        store_id: store_ids[0],
    }];
    let root = db
        .lcm_publish_immutable_summary(publication(
            "summary.depth.0",
            None,
            draft("cursor", "session-depth", 0, "depth 0", raw_sources.clone()),
        ))
        .await
        .unwrap();
    let mut previous = root.summary.node_id.clone();
    for depth in 1..=65 {
        previous = db
            .lcm_publish_immutable_summary(publication(
                &format!("summary.depth.{depth}"),
                None,
                draft(
                    "cursor",
                    "session-depth",
                    depth,
                    &format!("depth {depth}"),
                    vec![LcmSourceRef::SummaryNode { node_id: previous }],
                ),
            ))
            .await
            .unwrap()
            .summary
            .node_id;
    }

    let error = db
        .lcm_publish_immutable_summary(publication(
            "summary.depth.successor",
            Some(root.summary.node_id),
            draft("cursor", "session-depth", 0, "root successor", raw_sources),
        ))
        .await
        .expect_err("stale closure must stop at the configured lineage depth bound");
    assert!(matches!(
        error,
        LcmError::SummarySourceUnavailable { ref reason, .. }
            if reason == "lineage_depth_exceeded"
    ));
}

#[tokio::test]
async fn concurrent_publications_leave_one_active_generation() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let setup = open_lcm_db(&tmp).await;
    let store_ids =
        insert_messages(&setup, "cursor", "session-concurrent", &["alpha", "beta"]).await;
    drop(setup);
    let left = GlobalDb::open_at(&db_path).await.unwrap();
    let right = GlobalDb::open_at(&db_path).await.unwrap();
    let left_publication = publication(
        "summary.concurrent.alpha",
        None,
        draft(
            "cursor",
            "session-concurrent",
            0,
            "alpha",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ),
    );
    let right_publication = publication(
        "summary.concurrent.beta",
        None,
        draft(
            "cursor",
            "session-concurrent",
            0,
            "beta",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[1],
            }],
        ),
    );

    let (left_result, right_result) = tokio::join!(
        left.lcm_publish_immutable_summary(left_publication),
        right.lcm_publish_immutable_summary(right_publication),
    );
    left_result.unwrap();
    right_result.unwrap();
    assert_eq!(
        scalar(
            &db_path,
            "SELECT COUNT(*) FROM session_temporal_generations WHERE state = 'active'",
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &db_path,
            "SELECT COUNT(DISTINCT generation) FROM session_temporal_generations",
        )
        .await,
        2
    );
}

#[tokio::test]
async fn immutable_summary_exact_replay_keeps_frozen_lineage_after_close() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-reopen", &["alpha", "beta"]).await;
    let requested = publication(
        "summary.reopen.primary",
        None,
        draft(
            "cursor",
            "session-reopen",
            0,
            "frozen reopen summary",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ),
    );
    let first = db
        .lcm_publish_immutable_summary(requested.clone())
        .await
        .unwrap();
    db.lcm_publish_immutable_summary(publication(
        "summary.reopen.unrelated",
        None,
        draft(
            "cursor",
            "session-reopen",
            0,
            "unrelated generation",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[1],
            }],
        ),
    ))
    .await
    .unwrap();
    let replay = db
        .lcm_publish_immutable_summary(requested)
        .await
        .expect("exact replay must ignore unrelated generation evolution");
    assert_eq!(
        replay.disposition,
        LcmSummaryPublicationDisposition::ExactReplay
    );
    assert_eq!(replay.summary, first.summary);
    assert_eq!(replay.generation, first.generation);
    assert_eq!(replay.frozen_watermarks_json, first.frozen_watermarks_json);
    assert_eq!(replay.published_at, first.published_at);
    drop(db);

    // SQL-layer durability after closing the writer. Production-open authority
    // validation is exercised separately by the ignored blocker regression.
    assert_eq!(
        scalar(
            &db_path,
            "SELECT COUNT(*) FROM session_summary_nodes
             WHERE summary_id = 'summary.reopen.primary'
               AND summary_text = 'frozen reopen summary'",
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &db_path,
            "SELECT COUNT(*) FROM session_temporal_generations WHERE state = 'active'",
        )
        .await,
        1
    );
    assert_eq!(
        scalar(
            &db_path,
            &format!(
                "SELECT COUNT(*) FROM session_temporal_generations
                 WHERE generation = {} AND frozen_watermarks_json = '{}'",
                first.generation,
                first.frozen_watermarks_json.replace('\'', "''")
            ),
        )
        .await,
        1
    );
}

#[tokio::test]
#[ignore = "PR8 blocker: production open rejects immutable-summary sanitization receipt authority"]
async fn immutable_summary_exact_replay_survives_production_open() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let requested = {
        let db = open_lcm_db(&tmp).await;
        let store_ids = insert_messages(&db, "cursor", "session-production-open", &["alpha"]).await;
        let requested = publication(
            "summary.production-open.primary",
            None,
            draft(
                "cursor",
                "session-production-open",
                0,
                "production open summary",
                vec![LcmSourceRef::RawMessage {
                    store_id: store_ids[0],
                }],
            ),
        );
        db.lcm_publish_immutable_summary(requested.clone())
            .await
            .unwrap();
        requested
    };

    let reopened = GlobalDb::open_at(&db_path)
        .await
        .expect("production GlobalDb::open_at must accept immutable-summary receipt authority");
    let replay = reopened
        .lcm_publish_immutable_summary(requested)
        .await
        .expect("exact replay must survive production close/open");
    assert_eq!(
        replay.disposition,
        LcmSummaryPublicationDisposition::ExactReplay
    );
}

#[tokio::test]
async fn immutable_summary_lineage_rejects_foreign_session_canary_sources_without_disclosure() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let victim_ids = insert_messages(&db, "cursor", "session-victim", &["victim safe"]).await;
    let foreign_ids = insert_messages(&db, "cursor", "session-foreign", &[FOREIGN_CANARY]).await;
    let foreign_summary = db
        .lcm_publish_immutable_summary(publication(
            "summary.foreign.leaf",
            None,
            draft(
                "cursor",
                "session-foreign",
                0,
                "foreign leaf",
                vec![LcmSourceRef::RawMessage {
                    store_id: foreign_ids[0],
                }],
            ),
        ))
        .await
        .unwrap();

    let before_victim_summaries = scalar(
        &db_path,
        "SELECT COUNT(*) FROM session_summary_nodes WHERE session_id = 'session-victim'",
    )
    .await;
    let before_sources = scalar(&db_path, "SELECT COUNT(*) FROM session_summary_sources").await;
    let before_successors =
        scalar(&db_path, "SELECT COUNT(*) FROM session_summary_successors").await;

    for (label, source_refs) in [
        (
            "foreign raw",
            vec![LcmSourceRef::RawMessage {
                store_id: foreign_ids[0],
            }],
        ),
        (
            "foreign summary",
            vec![LcmSourceRef::SummaryNode {
                node_id: foreign_summary.summary.node_id.clone(),
            }],
        ),
    ] {
        let error = db
            .lcm_publish_immutable_summary(publication(
                &format!("summary.victim.{label}"),
                None,
                draft(
                    "cursor",
                    "session-victim",
                    0,
                    "must not publish",
                    source_refs,
                ),
            ))
            .await
            .expect_err("foreign session sources must fail closed");
        assert!(
            matches!(error, LcmError::SummarySourceNotOwnedBySession),
            "{label} must reject as SummarySourceNotOwnedBySession: {error:?}"
        );
        let rendered = format!("{error:?}\n{error}");
        assert!(
            !rendered.contains(FOREIGN_CANARY),
            "{label} error disclosed foreign canary: {rendered}"
        );
    }

    let _ = victim_ids;
    assert_eq!(
        scalar(
            &db_path,
            "SELECT COUNT(*) FROM session_summary_nodes WHERE session_id = 'session-victim'",
        )
        .await,
        before_victim_summaries
    );
    assert_eq!(
        scalar(&db_path, "SELECT COUNT(*) FROM session_summary_sources").await,
        before_sources
    );
    assert_eq!(
        scalar(&db_path, "SELECT COUNT(*) FROM session_summary_successors").await,
        before_successors
    );
    assert_eq!(
        scalar(
            &db_path,
            &format!(
                "SELECT COUNT(*) FROM session_summary_nodes
                 WHERE session_id = 'session-victim'
                   AND (
                       instr(summary_text, '{FOREIGN_CANARY}') > 0
                       OR instr(index_text, '{FOREIGN_CANARY}') > 0
                   )"
            ),
        )
        .await,
        0
    );
}

#[tokio::test]
async fn summary_source_pages_remain_gap_free_across_exact_replay_and_unrelated_publication() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let contents = [
        "source body 1",
        "source body 2",
        "source body 3",
        "source body 4",
        "source body 5",
    ];
    let store_ids = insert_messages(&db, "cursor", "session-pages", &contents).await;
    let requested = publication(
        "summary.pages.primary",
        None,
        draft(
            "cursor",
            "session-pages",
            0,
            "paginated primary",
            store_ids
                .iter()
                .map(|store_id| LcmSourceRef::RawMessage {
                    store_id: *store_id,
                })
                .collect(),
        ),
    );
    let first = db
        .lcm_publish_immutable_summary(requested.clone())
        .await
        .unwrap();

    let expand = |source_offset: usize, source_limit: Option<usize>| LcmExpandRequest {
        provider: "cursor".into(),
        session_id: "session-pages".into(),
        target: LcmExpandTarget::SummaryNode {
            node_id: first.summary.node_id.clone(),
        },
        content_slice: None,
        source_offset,
        source_limit,
    };

    let page_one = db.lcm_expand(expand(0, Some(2))).await.unwrap();
    let page_one_ids: Vec<i64> = page_one
        .summary_sources
        .iter()
        .filter_map(|source| source.raw_message.as_ref().map(|raw| raw.store_id))
        .collect();
    assert_eq!(page_one_ids, store_ids[..2]);

    let replay = db
        .lcm_publish_immutable_summary(requested)
        .await
        .expect("exact replay must keep frozen source order");
    assert_eq!(
        replay.disposition,
        LcmSummaryPublicationDisposition::ExactReplay
    );
    db.lcm_publish_immutable_summary(publication(
        "summary.pages.unrelated",
        None,
        draft(
            "cursor",
            "session-pages",
            0,
            "unrelated generation",
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ),
    ))
    .await
    .unwrap();

    let page_two = db.lcm_expand(expand(2, Some(2))).await.unwrap();
    let page_two_ids: Vec<i64> = page_two
        .summary_sources
        .iter()
        .filter_map(|source| source.raw_message.as_ref().map(|raw| raw.store_id))
        .collect();
    assert_eq!(page_two_ids, store_ids[2..4]);
    let page_three = db.lcm_expand(expand(4, Some(2))).await.unwrap();
    let page_three_ids: Vec<i64> = page_three
        .summary_sources
        .iter()
        .filter_map(|source| source.raw_message.as_ref().map(|raw| raw.store_id))
        .collect();
    assert_eq!(page_three_ids, store_ids[4..]);

    let mut concatenated = page_one_ids;
    concatenated.extend(page_two_ids);
    concatenated.extend(page_three_ids);
    assert_eq!(concatenated, store_ids);
    let mut unique = concatenated.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), store_ids.len());
}

#[tokio::test]
async fn lcm_grep_describe_and_expand_preserve_database_generation_and_publication_state() {
    let tmp = TempDir::new().unwrap();
    let db_path = isolated_lcm_db_path(&tmp);
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_messages(
        &db,
        "cursor",
        "session-readonly",
        &["alpha", "beta", "gamma"],
    )
    .await;
    let published = db
        .lcm_publish_immutable_summary(publication(
            "summary.readonly.primary",
            None,
            draft(
                "cursor",
                "session-readonly",
                0,
                "readonly lineage summary",
                store_ids
                    .iter()
                    .map(|store_id| LcmSourceRef::RawMessage {
                        store_id: *store_id,
                    })
                    .collect(),
            ),
        ))
        .await
        .unwrap();

    let before_hash = sha2::Sha256::digest(fs::read(&db_path).unwrap());
    let before_active = scalar(
        &db_path,
        "SELECT COUNT(*) FROM session_temporal_generations WHERE state = 'active'",
    )
    .await;
    let before_summaries = scalar(&db_path, "SELECT COUNT(*) FROM session_summary_nodes").await;
    let before_sources = scalar(&db_path, "SELECT COUNT(*) FROM session_summary_sources").await;
    let before_keys = scalar(&db_path, "SELECT COUNT(*) FROM session_query_cursor_keys").await;

    let grep = db
        .lcm_grep(LcmGrepRequest {
            provider: "cursor".into(),
            query: "readonly lineage".into(),
            scope: LcmScope::Session,
            session_id: Some("session-readonly".into()),
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
        .expect("grep must succeed");
    assert!(!grep.hits.is_empty());

    db.lcm_describe(LcmDescribeRequest {
        provider: "cursor".into(),
        session_id: "session-readonly".into(),
        target: LcmDescribeTarget::SummaryNode {
            node_id: published.summary.node_id.clone(),
        },
    })
    .await
    .expect("describe must succeed");

    let page = db
        .lcm_expand(LcmExpandRequest {
            provider: "cursor".into(),
            session_id: "session-readonly".into(),
            target: LcmExpandTarget::SummaryNode {
                node_id: published.summary.node_id.clone(),
            },
            content_slice: None,
            source_offset: 0,
            source_limit: Some(2),
        })
        .await
        .expect("expand must succeed");
    assert_eq!(page.summary_sources.len(), 2);
    db.lcm_expand(LcmExpandRequest {
        provider: "cursor".into(),
        session_id: "session-readonly".into(),
        target: LcmExpandTarget::SummaryNode {
            node_id: published.summary.node_id,
        },
        content_slice: None,
        source_offset: 2,
        source_limit: Some(2),
    })
    .await
    .expect("expand resume must succeed");

    assert_eq!(
        sha2::Sha256::digest(fs::read(&db_path).unwrap()),
        before_hash
    );
    assert_eq!(
        scalar(
            &db_path,
            "SELECT COUNT(*) FROM session_temporal_generations WHERE state = 'active'",
        )
        .await,
        before_active
    );
    assert_eq!(
        scalar(&db_path, "SELECT COUNT(*) FROM session_summary_nodes").await,
        before_summaries
    );
    assert_eq!(
        scalar(&db_path, "SELECT COUNT(*) FROM session_summary_sources").await,
        before_sources
    );
    assert_eq!(
        scalar(&db_path, "SELECT COUNT(*) FROM session_query_cursor_keys").await,
        before_keys
    );
}

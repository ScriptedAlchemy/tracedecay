use std::sync::Arc;

use tempfile::TempDir;
use tracedecay::host_admission::{HostAdmissionTestRuntimeV1, LcmLineageFaultForTest};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_sessions::runtime::lcm::types::{
    LcmImmutableSummaryPublication, LcmSummaryPublicationDisposition,
};
use tracedecay_sessions::runtime::lcm::{
    LcmDescribeRequest, LcmDescribeTarget, LcmError, LcmExpandRequest, LcmExpandTarget,
    LcmGrepRequest, LcmGrepSort, LcmScope, LcmSourceRef, LcmSummaryNodeDraft,
};
use tracedecay_temporal_query::ports::ExecutionControl;
use tracedecay_usecases::host_admission::HostAdmissionScope;

use crate::common::{lcm_dag_message, lcm_dag_session};

const FOREIGN_CANARY: &str = "lineage-foreign-canary-1234567890";

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

    async fn lcm_publish_immutable_summary(
        &self,
        publication: LcmImmutableSummaryPublication,
    ) -> Result<tracedecay_sessions::runtime::lcm::types::LcmSummaryPublicationReceipt, LcmError>
    {
        self.lcm_publish_immutable_summary_for_test(HostAdmissionScope::Profile, publication)
            .await
    }
}

async fn insert_messages(
    db: &HostAdmissionTestRuntimeV1,
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
            db.lcm_load_raw_message_for_test(provider, &message_id)
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

#[tokio::test]
async fn exact_replay_uses_only_frozen_canonical_authority() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
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

    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::CorruptCompatibilitySummaryText {
        node_id: "summary.replay.canonical".into(),
        text: "corrupt legacy projection".into(),
    })
    .await
    .unwrap();
    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::ShiftRawMessageTimestamp {
        store_id: store_ids[0],
        delta: 999_999,
    })
    .await
    .unwrap();

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
    let db = registered_lcm_runtime(&tmp).await;
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

    let database_path = db
        .database_path(HostAdmissionScope::Profile)
        .expect("profile database path")
        .to_path_buf();
    let fixture = rusqlite::Connection::open(database_path).unwrap();
    fixture.pragma_update(None, "foreign_keys", "OFF").unwrap();
    fixture
        .execute_batch("DROP TRIGGER IF EXISTS session_temporal_generations_delete_guard_v1;")
        .unwrap();
    fixture
        .execute(
            "DELETE FROM session_temporal_generations
             WHERE session_id = ?1 AND generation = ?2",
            rusqlite::params!["session-missing-gen", first.generation],
        )
        .unwrap();
    drop(fixture);

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
    let db = registered_lcm_runtime(&tmp).await;
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

    db.apply_lcm_lineage_fault_for_test(
        LcmLineageFaultForTest::ReplaceGenerationWatermarks {
            session_id: "session-changed-wm".into(),
            generation: first.generation,
            json: r#"{"active_generation":null,"source_frontier":0,"projection_frontier":0,"summary_frontier":"tampered","route":"tampered"}"#.into(),
        },
    )
    .await
    .unwrap();

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
    let db = registered_lcm_runtime(&tmp).await;
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

    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::DeleteAvailability {
        session_id: "session-missing-avail".into(),
        generation: first.generation,
        summary_id: "summary.replay.missing-availability".into(),
    })
    .await
    .unwrap();

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
    let db = registered_lcm_runtime(&tmp).await;
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

    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::ReplaceAvailabilityHorizon {
        session_id: "session-mismatch-horizon".into(),
        generation: first.generation,
        summary_id: "summary.replay.mismatch-horizon".into(),
        source_horizon_json: r#"{"knowledge_through":0,"valid_through":0}"#.into(),
    })
    .await
    .unwrap();

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

    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::SetAvailability {
        session_id: "session-mismatch-horizon".into(),
        generation: first.generation,
        summary_id: "summary.replay.mismatch-horizon".into(),
        availability: "stale".into(),
        reason: Some("tampered".into()),
    })
    .await
    .unwrap();

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

    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::SetAvailability {
        session_id: "session-mismatch-horizon".into(),
        generation: first.generation,
        summary_id: "summary.replay.mismatch-horizon".into(),
        availability: "available".into(),
        reason: None,
    })
    .await
    .unwrap();
    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::SetGenerationFailed {
        session_id: "session-mismatch-horizon".into(),
        generation: first.generation,
    })
    .await
    .unwrap();

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
    let db = registered_lcm_runtime(&tmp).await;
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
    let db = registered_lcm_runtime(&tmp).await;
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
    let db = registered_lcm_runtime(&tmp).await;
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
    let db = registered_lcm_runtime(&tmp).await;
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

    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::CorruptRetrievalAnchorOwner {
        summary_id: "summary.owner.leaf".into(),
    })
    .await
    .unwrap();

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
async fn pending_relation_receipt_requires_explicit_recovery_before_read() {
    let tmp = TempDir::new().unwrap();
    let runtime = registered_lcm_runtime(&tmp).await;
    let store_ids =
        insert_messages(&runtime, "cursor", "session-relation-recovery", &["alpha"]).await;
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let control = ExecutionControl::default();
    let cancel_after_commit = control.clone();

    database
        .lcm_publish_immutable_summary_guarded(
            publication(
                "summary.relation-recovery",
                None,
                draft(
                    "cursor",
                    "session-relation-recovery",
                    0,
                    "pending graph publication",
                    vec![LcmSourceRef::RawMessage {
                        store_id: store_ids[0],
                    }],
                ),
            ),
            &control,
            || {
                cancel_after_commit.cancel();
                Ok(())
            },
        )
        .await
        .expect_err("post-commit cancellation must leave graph recovery pending");

    let session_id =
        tracedecay_domain::SessionId::new("session-relation-recovery").expect("session id");
    let snapshot = database.read_snapshot().await.expect("relation snapshot");
    let mut journal_rows = snapshot
        .query(
            "SELECT projection_json
             FROM session_relation_effect_journal
             WHERE session_id = ?1",
            [session_id.as_str()],
        )
        .await
        .expect("relation effect journal query");
    let projection_json = journal_rows
        .next()
        .await
        .expect("relation effect journal row")
        .expect("pending relation effect");
    let projection_json: String = projection_json.get(0).expect("projection JSON");
    assert!(
        projection_json.contains("summary.relation-recovery"),
        "the durable effect must retain the exact committed topology"
    );
    drop(journal_rows);
    drop(snapshot);
    assert!(
        database
            .active_session_summary_relations(
                &session_id,
                &["summary.relation-recovery".to_owned()],
                10,
                Arc::new(NeverCancelled),
            )
            .await
            .is_err(),
        "a relation read must not repair a pending graph projection"
    );

    assert_eq!(
        database
            .recover_pending_session_relation_projections(10, Arc::new(NeverCancelled))
            .await
            .expect("recover pending graph projection"),
        1
    );
    let snapshot = database.read_snapshot().await.expect("recovered snapshot");
    let mut journal_rows = snapshot
        .query(
            "SELECT COUNT(*)
             FROM session_relation_effect_journal
             WHERE session_id = ?1",
            [session_id.as_str()],
        )
        .await
        .expect("recovered effect journal query");
    assert_eq!(
        journal_rows
            .next()
            .await
            .expect("recovered journal count")
            .expect("recovered journal count row")
            .get::<i64>(0)
            .expect("recovered journal count value"),
        0
    );
    let (_, relations) = database
        .active_session_summary_relations(
            &session_id,
            &["summary.relation-recovery".to_owned()],
            10,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("read recovered graph projection");
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].summary_id, "summary.relation-recovery");
}

#[tokio::test]
async fn generation_stale_closure_rejects_excessive_depth() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
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
    let db = registered_lcm_runtime(&tmp).await;
    let store_ids = insert_messages(&db, "cursor", "session-concurrent", &["alpha", "beta"]).await;
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
        db.lcm_publish_immutable_summary(left_publication),
        db.lcm_publish_immutable_summary(right_publication),
    );
    left_result.expect("the losing publisher must succeed against the refreshed generation");
    right_result.expect("the losing publisher must succeed against the refreshed generation");
    let counts = db
        .lcm_lineage_counts_for_test(Some("session-concurrent"))
        .await
        .unwrap();
    assert_eq!(counts.active_generations, 1);
    assert_eq!(counts.total_generations, 2);

    let database = db
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let session_id = tracedecay_domain::SessionId::new("session-concurrent").expect("session id");
    // A loser that re-applied the refreshed generation may leave its own
    // superseded marker pending; recovery settles exactly that marker.
    database
        .recover_pending_session_relation_projections(10, Arc::new(NeverCancelled))
        .await
        .expect("settle any superseded publication marker");
    let (active_generation, relations) = database
        .active_session_summary_relations(
            &session_id,
            &[
                "summary.concurrent.alpha".to_owned(),
                "summary.concurrent.beta".to_owned(),
            ],
            10,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("read the active native relation projection");
    assert_eq!(active_generation.value(), 2);
    assert_eq!(relations.len(), 2);

    // Exactly one publication wins each generation, every receipt settles as
    // applied with the watermark the native graph acknowledged, and no
    // replayable effect survives that could double-apply.
    let snapshot = database.read_snapshot().await.expect("receipt snapshot");
    let mut receipt_rows = snapshot
        .query(
            "SELECT generation, state, expected_graph_watermark, graph_watermark
             FROM session_relation_receipts
             WHERE session_id = ?1
             ORDER BY generation",
            [session_id.as_str()],
        )
        .await
        .expect("relation receipt query");
    let mut receipt_generations = Vec::new();
    while let Some(row) = receipt_rows.next().await.expect("relation receipt row") {
        receipt_generations.push(row.get::<i64>(0).expect("receipt generation"));
        assert_eq!(row.get::<String>(1).expect("receipt state"), "applied");
        assert_eq!(
            row.get::<Option<String>>(3)
                .expect("acknowledged watermark"),
            Some(row.get::<String>(2).expect("expected watermark")),
        );
    }
    assert_eq!(receipt_generations, vec![1, 2]);
    drop(receipt_rows);
    let mut journal_rows = snapshot
        .query(
            "SELECT COUNT(*) FROM session_relation_effect_journal WHERE session_id = ?1",
            [session_id.as_str()],
        )
        .await
        .expect("effect journal query");
    assert_eq!(
        journal_rows
            .next()
            .await
            .expect("effect journal count")
            .expect("effect journal count row")
            .get::<i64>(0)
            .expect("effect journal count value"),
        0
    );
    drop(journal_rows);
    drop(snapshot);
    assert_eq!(
        database
            .recover_pending_session_relation_projections(10, Arc::new(NeverCancelled))
            .await
            .expect("recovery after settlement is idempotent"),
        0
    );
}

#[tokio::test]
async fn immutable_summary_exact_replay_keeps_frozen_lineage_after_close() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let db = registered_lcm_runtime(&tmp).await;
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
        .lcm_publish_immutable_summary(requested.clone())
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

    let reopened = HostAdmissionTestRuntimeV1::profile(profile_root)
        .await
        .expect("registered runtime should reopen");
    let expansion = reopened
        .lcm_expand_summary_node_for_test("cursor", "session-reopen", "summary.reopen.primary")
        .await
        .expect("frozen summary should survive close and reopen");
    assert_eq!(expansion.summary.summary_text, "frozen reopen summary");
    let replay = reopened
        .lcm_publish_immutable_summary(requested)
        .await
        .expect("frozen publication should exact-replay after close and reopen");
    assert_eq!(
        replay.disposition,
        LcmSummaryPublicationDisposition::ExactReplay
    );
    assert_eq!(replay.generation, first.generation);
    assert_eq!(replay.frozen_watermarks_json, first.frozen_watermarks_json);
    let counts = reopened
        .lcm_lineage_counts_for_test(Some("session-reopen"))
        .await
        .unwrap();
    assert_eq!(counts.active_generations, 1);
    assert_eq!(counts.total_generations, 2);
}

#[tokio::test]
async fn immutable_summary_exact_replay_survives_production_open() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let requested = {
        let db = registered_lcm_runtime(&tmp).await;
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

    let reopened = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .expect("registered runtime must accept immutable-summary receipt authority");
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
    let db = registered_lcm_runtime(&tmp).await;
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

    let before_victim = db
        .lcm_lineage_counts_for_test(Some("session-victim"))
        .await
        .unwrap();
    let before_global = db.lcm_lineage_counts_for_test(None).await.unwrap();

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
    let after_victim = db
        .lcm_lineage_counts_for_test(Some("session-victim"))
        .await
        .unwrap();
    let after_global = db.lcm_lineage_counts_for_test(None).await.unwrap();
    assert_eq!(after_victim.summary_nodes, before_victim.summary_nodes);
    assert_eq!(after_global.summary_sources, before_global.summary_sources);
    assert_eq!(
        after_global.summary_successors,
        before_global.summary_successors
    );
    let canary = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: FOREIGN_CANARY.into(),
            scope: LcmScope::Session,
            session_id: Some("session-victim".into()),
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
        .unwrap();
    assert!(canary.hits.is_empty());
}

#[tokio::test]
async fn summary_source_pages_remain_gap_free_across_exact_replay_and_unrelated_publication() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
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

    let page_one = db.lcm_expand_for_test(expand(0, Some(2))).await.unwrap();
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

    let page_two = db.lcm_expand_for_test(expand(2, Some(2))).await.unwrap();
    let page_two_ids: Vec<i64> = page_two
        .summary_sources
        .iter()
        .filter_map(|source| source.raw_message.as_ref().map(|raw| raw.store_id))
        .collect();
    assert_eq!(page_two_ids, store_ids[2..4]);
    let page_three = db.lcm_expand_for_test(expand(4, Some(2))).await.unwrap();
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
    let db = registered_lcm_runtime(&tmp).await;
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

    let before = db.lcm_lineage_counts_for_test(None).await.unwrap();

    let grep = db
        .lcm_grep_for_test(LcmGrepRequest {
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

    db.lcm_describe_for_test(LcmDescribeRequest {
        provider: "cursor".into(),
        session_id: "session-readonly".into(),
        target: LcmDescribeTarget::SummaryNode {
            node_id: published.summary.node_id.clone(),
        },
    })
    .await
    .expect("describe must succeed");

    let page = db
        .lcm_expand_for_test(LcmExpandRequest {
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
    db.lcm_expand_for_test(LcmExpandRequest {
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

    let after = db.lcm_lineage_counts_for_test(None).await.unwrap();
    assert_eq!(after, before);
}

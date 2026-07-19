//! Memory-shard (facts, relations, feedback) merge consolidation tests.

use super::*;

#[tokio::test]
async fn overlapping_facts_merge_tags_metadata_and_feedback_without_duplication() {
    let fixture = fixture().await;
    add_fact_to_shard(
        &fixture,
        &fixture.source_id,
        "shared fact",
        "source-tag",
        json!({"source_only": true, "winner": "source"}),
        Some(FeedbackAction::Helpful),
    )
    .await;
    add_fact_to_shard(
        &fixture,
        &fixture.target_id,
        "shared fact",
        "target-tag",
        json!({"target_only": true, "winner": "target"}),
        Some(FeedbackAction::Unhelpful),
    )
    .await;

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    assert_eq!(planned.collisions.fact_content_overlaps, 1);
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    let graph_path = applied
        .destination_data_root
        .join(crate::config::DB_FILENAME);
    let (graph, _) = test_open_read_only(&graph_path).await;
    let store = MemoryStore::new(graph.conn());
    let facts = store.list_facts(None, Some(0.0), 100).await.unwrap();
    let shared = facts
        .iter()
        .find(|fact| fact.content == "shared fact")
        .unwrap();
    assert_eq!(facts.len(), 3);
    assert!(shared.tags.contains(&"source-tag".to_string()));
    assert!(shared.tags.contains(&"target-tag".to_string()));
    assert_eq!(shared.metadata["source_only"], true);
    assert_eq!(shared.metadata["target_only"], true);
    assert_eq!(shared.metadata["winner"], "target");
    assert_eq!(shared.helpful_count, 1);
    assert_eq!(shared.unhelpful_count, 1);
    assert_eq!(
        store
            .fact_trust_history(shared.fact_id)
            .await
            .unwrap()
            .len(),
        2
    );
    graph.close();
}

#[tokio::test]
async fn summary_raw_sources_follow_remapped_store_ids() {
    let fixture = fixture().await;
    let source = layout_for_id(&fixture.project, &fixture.profile, &fixture.source_id).unwrap();
    execute_sql(
        &source.sessions_db_path,
        "INSERT INTO lcm_summary_nodes(
             node_id, provider, conversation_id, session_id, depth, summary_text,
             summary_hash, summary_token_count, source_token_count, created_at
         ) VALUES(
             'source-summary', 'codex', 'source-conversation', 'legacy-session', 1,
             'summary', 'summary-hash', 1, 1, 1800000002
         );
         INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
         SELECT 'source-summary', 'raw_message', CAST(store_id AS TEXT), 0
         FROM lcm_raw_messages WHERE message_id='message-legacy-session';",
    )
    .await;

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    let sessions = GlobalDb::open_at_without_structured_backfill(
        &applied
            .destination_data_root
            .join(storage::SESSIONS_DB_FILENAME),
    )
    .await
    .unwrap();
    let mut rows = sessions
        .conn()
        .query(
            "SELECT r.message_id
             FROM lcm_summary_sources s
             JOIN lcm_raw_messages r ON r.store_id=CAST(s.source_id AS INTEGER)
             WHERE s.node_id='source-summary' AND s.source_kind='raw_message'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "message-legacy-session");
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    sessions.close();
}

#[tokio::test]
async fn memory_v2_merge_preserves_deletion_terminality_and_carries_live_facts() {
    // Seeds two graph shards that share profile-owned fact identities and merges
    // the source's memory_v2 authority into the target. A tombstone in either
    // shard must win over a live copy in the other (deletion is terminal), and a
    // fact that only exists in the source must survive the merge.
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-graph.db");
    let source_path = temp.path().join("source-graph.db");

    // Common minimal rows for a profile-owned fact: identity, one lineage event
    // per shard, and a current-fact projection row referencing that event.
    fn seed_fact(fact_id: &str, event_id: &str, payload_access: &str, updated_at: i64) -> String {
        format!(
            "INSERT OR IGNORE INTO memory_v2_facts(
                 fact_id, owner_kind, project_id, owner_json, identity_json, created_at
             ) VALUES ('{fact_id}', 'profile', '', '{{\"kind\":\"profile\"}}',
                       '{{\"id\":\"{fact_id}\"}}', 1);
             INSERT INTO memory_v2_lineage_events(
                 event_id, fact_id, owner_kind, project_id, event_json,
                 occurred_at, recorded_at
             ) VALUES ('{event_id}', '{fact_id}', 'profile', '',
                       '{{\"event\":\"{event_id}\"}}', {updated_at}, {updated_at});
             INSERT INTO memory_v2_current_facts(
                 fact_id, owner_kind, project_id, payload_access, trust_score,
                 active_assertion_id, last_event_id, updated_at, retrieval_count,
                 access_count, helpful_count, unhelpful_count, last_retrieved_at,
                 last_recalled_at, last_feedback_at, projection_state,
                 vector_watermark_json
             ) VALUES ('{fact_id}', 'profile', '', '{payload_access}', 0.5,
                       NULL, '{event_id}', {updated_at}, 0, 0, 0, 0,
                       NULL, NULL, NULL, 'ready', NULL);"
        )
    }

    async fn seed_shard(path: &Path, batch: String) {
        let (db, _) = test_initialize(path).await;
        let transaction = db.begin_write_transaction("seed memory_v2").await.unwrap();
        transaction.execute_batch(&batch).await.unwrap();
        transaction.commit().await.unwrap();
        db.checkpoint().await.unwrap();
        db.close();
    }

    seed_shard(
        &target_path,
        format!(
            "{}{}",
            // fact.shared: live in target, tombstoned (newer) in source.
            seed_fact("fact.shared", "ev.shared.t", "eligible", 100),
            // fact.tombstone: tombstoned in target, live (newer) in source.
            seed_fact("fact.tombstone", "ev.tomb.t", "deleted", 100),
        ),
    )
    .await;
    seed_shard(
        &source_path,
        format!(
            "{}{}{}",
            seed_fact("fact.shared", "ev.shared.s", "deleted", 200),
            seed_fact("fact.tombstone", "ev.tomb.s", "eligible", 200),
            seed_fact("fact.sourceonly", "ev.srconly", "eligible", 50),
        ),
    )
    .await;

    sqlite::merge_memory_v2_for_test(&target_path, &source_path)
        .await
        .unwrap();

    let (target, _) = test_open_read_only(&target_path).await;
    let access = |fact_id: &'static str| {
        let conn = target.conn();
        async move {
            let mut rows = conn
                .query(
                    "SELECT payload_access FROM memory_v2_current_facts
                     WHERE fact_id = ?1",
                    params![fact_id],
                )
                .await
                .unwrap();
            rows.next()
                .await
                .unwrap()
                .map(|row| row.get::<String>(0).unwrap())
        }
    };

    // A tombstone from either shard is terminal, even when the live copy in the
    // other shard is strictly newer.
    assert_eq!(access("fact.shared").await.as_deref(), Some("deleted"));
    assert_eq!(access("fact.tombstone").await.as_deref(), Some("deleted"));
    // A fact only present in the source survives with its live projection.
    assert_eq!(access("fact.sourceonly").await.as_deref(), Some("eligible"));

    // Deletion terminality must not re-materialize a derived projection: no
    // assertion payload or vector row exists for a tombstoned fact.
    for table in [
        "memory_v2_assertion_payloads",
        "memory_v2_assertion_vectors",
    ] {
        let mut rows = target
            .conn()
            .query(
                &format!(
                    "SELECT COUNT(*) FROM {table}
                     WHERE fact_id IN ('fact.shared', 'fact.tombstone')"
                ),
                (),
            )
            .await
            .unwrap();
        let count: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(count, 0, "{table} must not re-materialize for tombstones");
    }
    target.close();
}

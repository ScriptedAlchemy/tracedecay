//! Memory-shard (facts, relations, feedback) merge consolidation tests.

use super::*;

#[tokio::test]
async fn branch_legacy_cutover_accepts_v17_and_preserves_latest_full_fact_state() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("project.db");
    let source_path = temp.path().join("branch-v17.db");
    let (target, _) = test_initialize(&target_path).await;
    let (source, _) = test_initialize(&source_path).await;

    let target_seed = target
        .begin_write_transaction("seed v17 cutover target")
        .await
        .unwrap();
    target_seed
        .execute_batch(
            "INSERT INTO memory_facts(
                 fact_id, content, category, tags, trust_score, retrieval_count,
                 access_count, helpful_count, unhelpful_count, created_at, updated_at,
                 last_recalled_at, source, metadata, hrr_vector, hrr_algebra,
                 hrr_dim, hrr_precision
             ) VALUES(
                 1, 'shared', 'general', '[\"target\"]', 0.4, 2, 3, 1, 0,
                 10, 10, 11, 'target-source', '{\"target\":true,\"winner\":\"target\"}',
                 X'01', 'amari_fhrr', 2048, 'f32'
             );",
        )
        .await
        .unwrap();
    target_seed.commit().await.unwrap();
    let source_seed = source
        .begin_write_transaction("seed v17 cutover source")
        .await
        .unwrap();
    source_seed
        .execute_batch(
            "DROP TABLE memory_fact_relations;
             PRAGMA user_version = 17;
             INSERT INTO memory_facts(
                 fact_id, content, category, tags, trust_score, retrieval_count,
                 access_count, helpful_count, unhelpful_count, created_at, updated_at,
                 last_recalled_at, last_feedback_at, source, metadata, hrr_vector,
                 hrr_algebra, hrr_dim, hrr_precision
             ) VALUES
                 (1, 'shared', 'decision', '[\"source\"]', 0.9, 5, 7, 2, 1,
                  5, 20, 19, 20, 'source-source',
                  '{\"source\":true,\"winner\":\"source\"}',
                  X'0203', 'amari_fhrr', 2048, 'f64'),
                 (2, 'branch exclusive', 'project', '[]', 0.8, 0, 0, 0, 0,
                  20, 20, NULL, NULL, 'branch', '{}', NULL, 'amari_fhrr', 2048, 'f32');",
        )
        .await
        .unwrap();
    source_seed.commit().await.unwrap();
    source.checkpoint().await.unwrap();
    source.close();

    let snapshot =
        tracedecay_runtime_core::sqlite_read_snapshot::open_in(&source_path, temp.path())
            .await
            .unwrap();
    sqlite::merge_branch_legacy_memory_snapshot(&target, &snapshot)
        .await
        .unwrap();

    let mut rows = target
        .conn()
        .query(
            "SELECT category, tags, trust_score, retrieval_count, access_count,
                    helpful_count, unhelpful_count, created_at, updated_at,
                    last_recalled_at, source, metadata, hex(hrr_vector), hrr_precision
             FROM memory_facts WHERE content='shared'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "decision");
    let tags: Vec<String> = serde_json::from_str(&row.get::<String>(1).unwrap()).unwrap();
    assert_eq!(tags, vec!["source", "target"]);
    assert!((row.get::<f64>(2).unwrap() - 0.9).abs() <= f64::EPSILON);
    assert_eq!(row.get::<i64>(3).unwrap(), 5);
    assert_eq!(row.get::<i64>(4).unwrap(), 7);
    assert_eq!(row.get::<i64>(5).unwrap(), 2);
    assert_eq!(row.get::<i64>(6).unwrap(), 1);
    assert_eq!(row.get::<i64>(7).unwrap(), 5);
    assert_eq!(row.get::<i64>(8).unwrap(), 20);
    assert_eq!(row.get::<i64>(9).unwrap(), 19);
    assert_eq!(row.get::<String>(10).unwrap(), "source-source");
    let metadata: serde_json::Value =
        serde_json::from_str(&row.get::<String>(11).unwrap()).unwrap();
    assert_eq!(metadata["target"], true);
    assert_eq!(metadata["source"], true);
    assert_eq!(metadata["winner"], "source");
    assert_eq!(row.get::<String>(12).unwrap(), "0203");
    assert_eq!(row.get::<String>(13).unwrap(), "f64");
    drop(rows);
    let mut rows = target
        .conn()
        .query("SELECT COUNT(*) FROM memory_facts", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        2
    );
    drop(rows);
    target.close();
}

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
        .join(tracedecay_runtime_core::config::DB_FILENAME);
    let (graph, _) = test_open_read_only(&graph_path).await;
    let memory = graph
        .begin_memory_read_transaction("inspect consolidated memory")
        .await
        .unwrap();
    let store = MemoryStore::new_database_transaction(&memory);
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
    drop(memory);
    graph.close();
}

#[tokio::test]
async fn summary_raw_sources_follow_remapped_store_ids() {
    let fixture = fixture().await;
    let source_runtime =
        open_historical_project_runtime(&fixture.profile, &fixture.project, &fixture.source_id)
            .await;
    let source = source_runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let transaction = source.begin_write_transaction().await.unwrap();
    transaction
        .execute_batch(
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
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    source.checkpoint().await;
    drop(source_runtime);

    let options = fixture.options();
    let planned = plan(&options).await.unwrap();
    let applied = apply(&options, &planned.confirmation_token).await.unwrap();
    let runtime = open_historical_project_runtime(
        &fixture.profile,
        &fixture.project,
        &applied.destination_project_id,
    )
    .await;
    assert_eq!(
        runtime.database_path(HostAdmissionScope::Project).unwrap(),
        applied
            .destination_data_root
            .join(storage::SESSIONS_DB_FILENAME)
    );
    let sessions = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let snapshot = sessions.read_snapshot().await.unwrap();
    let mut rows = snapshot
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
            "{}{}{}
             INSERT INTO memory_v2_assertions(
                 assertion_id, fact_id, owner_kind, project_id, owner_json,
                 assertion_header_json, kind_json, payload_reference_json,
                 receipt_json, asserted_at, actor_id
             ) VALUES
                 ('assertion.deleted', 'fact.shared', 'profile', '',
                  '{{\"kind\":\"profile\"}}', '{{}}', '{{}}', '{{}}', '{{}}', 200, NULL),
                 ('assertion.live', 'fact.sourceonly', 'profile', '',
                  '{{\"kind\":\"profile\"}}', '{{}}', '{{}}', '{{}}', '{{}}', 50, NULL);
             INSERT INTO memory_v2_assertion_payloads(
                 assertion_id, fact_id, owner_kind, project_id, payload_json, content
             ) VALUES
                 ('assertion.deleted', 'fact.shared', 'profile', '', '{{}}',
                  'deleted secret'),
                 ('assertion.live', 'fact.sourceonly', 'profile', '', '{{}}',
                  'live searchable fact');
             INSERT INTO memory_v2_assertion_vectors(
                 assertion_id, fact_id, owner_kind, project_id, vector,
                 algebra, dimensions, precision
             ) VALUES
                 ('assertion.deleted', 'fact.shared', 'profile', '', X'00000000',
                  'test', 1, 'f32'),
                 ('assertion.live', 'fact.sourceonly', 'profile', '', X'00000000',
                  'test', 1, 'f32');
             INSERT INTO memory_v2_feedback_history(
                 owner_kind, project_id, fact_id, event_id, action, old_trust,
                 new_trust, occurred_at, source, note, details_availability
             ) VALUES(
                 'profile', '', 'fact.shared', 'ev.shared.s', 'helpful', 0.4,
                 0.5, 200, 'private-source', 'private-note', 'available'
             );
             UPDATE memory_v2_current_facts
             SET active_assertion_id = 'assertion.deleted',
                 vector_watermark_json = '{{\"generation\":1}}'
             WHERE fact_id = 'fact.shared';
             UPDATE memory_v2_current_facts
             SET active_assertion_id = 'assertion.live',
                 vector_watermark_json = '{{\"generation\":1}}'
             WHERE fact_id = 'fact.sourceonly';",
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
    let snapshot = target
        .begin_engine_read_snapshot("read consolidated memory access")
        .await
        .unwrap();
    let access = |fact_id: &'static str| {
        let conn = &snapshot;
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

    let mut rows = target
        .conn()
        .query(
            "SELECT COUNT(*) FROM memory_v2_assertion_payloads_fts
             WHERE memory_v2_assertion_payloads_fts MATCH 'deleted'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0,
        "deleted payload must not survive in FTS history"
    );
    drop(rows);

    let mut rows = target
        .conn()
        .query(
            "SELECT source, note, details_availability
             FROM memory_v2_feedback_history
             WHERE fact_id = 'fact.shared'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert!(row.get::<Option<String>>(0).unwrap().is_none());
    assert!(row.get::<Option<String>>(1).unwrap().is_none());
    assert_eq!(
        row.get::<String>(2).unwrap(),
        "legacy_redacted",
        "deleted fact feedback history must retain only its redacted audit shell"
    );
    drop(rows);

    let mut rows = target
        .conn()
        .query(
            "SELECT current.projection_state, current.vector_watermark_json,
                    payload.content, length(vector.vector)
             FROM memory_v2_current_facts AS current
             JOIN memory_v2_assertion_payloads AS payload
               ON payload.assertion_id = current.active_assertion_id
              AND payload.fact_id = current.fact_id
              AND payload.owner_kind = current.owner_kind
              AND payload.project_id = current.project_id
             JOIN memory_v2_assertion_vectors AS vector
               ON vector.assertion_id = payload.assertion_id
              AND vector.fact_id = payload.fact_id
              AND vector.owner_kind = payload.owner_kind
              AND vector.project_id = payload.project_id
             WHERE current.fact_id = 'fact.sourceonly'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "rebuilding");
    assert!(row.get::<Option<String>>(1).unwrap().is_none());
    assert_eq!(row.get::<String>(2).unwrap(), "live searchable fact");
    assert_eq!(row.get::<i64>(3).unwrap(), 4);
    drop(rows);

    let mut rows = target
        .conn()
        .query(
            "SELECT COUNT(*) FROM memory_v2_compatibility_bank_dirty
             WHERE owner_kind = 'profile' AND project_id = ''
               AND source_store_id = 'legacy-memory-v1'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        7,
        "every owner bank must be rebuilt after its fact set changes"
    );
    target.close();
}

#[tokio::test]
async fn memory_v2_merge_rejects_incompatible_same_fact_identity() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-conflict.db");
    let source_path = temp.path().join("source-conflict.db");
    for (path, identity) in [
        (&target_path, r#"{"content":"target"}"#),
        (&source_path, r#"{"content":"source"}"#),
    ] {
        let (database, _) = test_initialize(path).await;
        let transaction = database
            .begin_write_transaction("seed conflicting memory_v2 identity")
            .await
            .unwrap();
        transaction
            .execute(
                "INSERT INTO memory_v2_facts(
                    fact_id, owner_kind, project_id, owner_json, identity_json, created_at
                 ) VALUES('fact.conflict', 'profile', '', '{\"kind\":\"profile\"}', ?1, 1)",
                params![identity],
            )
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        database.checkpoint().await.unwrap();
        database.close();
    }

    let error = sqlite::merge_memory_v2_for_test(&target_path, &source_path)
        .await
        .expect_err("same FactId with incompatible identity must fail closed");
    assert!(
        error.to_string().contains("memory_v2") && error.to_string().contains("conflict"),
        "unexpected conflict error: {error}"
    );

    let (target, _) = test_open_read_only(&target_path).await;
    let snapshot = target
        .begin_engine_read_snapshot("read rejected memory_v2 conflict")
        .await
        .unwrap();
    let mut rows = snapshot
        .query(
            "SELECT identity_json FROM memory_v2_facts WHERE fact_id='fact.conflict'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        r#"{"content":"target"}"#
    );
    target.close();
}

#[tokio::test]
async fn memory_v2_merge_preserves_anchor_authority_and_evidence_assembly() {
    let temp = TempDir::new().unwrap();
    let target_path = temp.path().join("target-graph.db");
    let source_path = temp.path().join("source-graph.db");
    let (target, _) = test_initialize(&target_path).await;
    target.close();
    let (source, _) = test_initialize(&source_path).await;
    let transaction = source
        .begin_write_transaction("seed memory_v2 evidence assembly")
        .await
        .unwrap();
    transaction
        .execute_batch(
            r#"INSERT INTO retrieval_anchors(
                   anchor_id, anchor_json, owner_json, projection_generation
               ) VALUES('anchor.source', '{}', '{"kind":"profile"}', 'generation.1');
               INSERT INTO retrieval_anchor_aliases(
                   owner_json, alias_kind, locator_digest, anchor_id
               ) VALUES('{"kind":"profile"}', 'source', 'locator.1', 'anchor.source');
               INSERT INTO retrieval_anchor_dispositions(
                   disposition_id, anchor_id, owner_json, state, superseded_by,
                   reason_class, effective_at, record_json
               ) VALUES(
                   'disposition.1', 'anchor.source', '{"kind":"profile"}',
                   'deleted', NULL, 'user_request', 1, '{}'
               );
               INSERT INTO retrieval_anchor_reverse_lineage(
                   source_anchor_id, owner_json, derivative_kind, derivative_id,
                   direct_evidence
               ) VALUES(
                   'anchor.source', '{"kind":"profile"}', 'span', 'span.1', 1
               );
               INSERT INTO retrieval_anchor_derivative_tombstones(
                   source_anchor_id, owner_json, derivative_kind, derivative_id,
                   disposition_id, effective_at
               ) VALUES(
                   'anchor.source', '{"kind":"profile"}', 'span', 'span.1',
                   'disposition.1', 1
               );

               INSERT INTO evidence_source_occurrences(
                   occurrence_id, owner_digest, timeline_digest, source_anchor_id,
                   source_order, record_digest, record_json
               ) VALUES(
                   'occurrence.1', 'owner.1', 'timeline.1', 'anchor.source',
                   0, 'digest.occurrence', '{}'
               );
               INSERT INTO evidence_occurrence_sets(
                   occurrence_set_id, owner_digest, record_digest, record_json
               ) VALUES('set.1', 'owner.1', 'digest.set', '{}');
               INSERT INTO evidence_occurrence_set_members(
                   occurrence_set_id, canonical_ordinal, occurrence_id
               ) VALUES('set.1', 0, 'occurrence.1');
               INSERT INTO evidence_spans(
                   span_id, owner_digest, occurrence_set_id, anchor_id,
                   producer_kind, record_digest, record_json
               ) VALUES(
                   'span.1', 'owner.1', 'set.1', 'anchor.span', 'test',
                   'digest.span', '{}'
               );
               INSERT INTO evidence_span_members(
                   span_id, assembly_ordinal, run_ordinal, run_member_ordinal,
                   occurrence_id
               ) VALUES('span.1', 0, 0, 0, 'occurrence.1');
               INSERT INTO evidence_span_projection_receipts(
                   projection_receipt_id, span_id, record_digest, record_json
               ) VALUES('projection.1', 'span.1', 'digest.projection', '{}');
               INSERT INTO evidence_retriever_contributions(
                   contribution_id, owner_digest, span_id, anchor_id,
                   record_digest, record_json
               ) VALUES(
                   'contribution.1', 'owner.1', 'span.1', 'anchor.contribution',
                   'digest.contribution', '{}'
               );
               INSERT INTO evidence_derived_anchors(
                   anchor_id, owner_digest, target_kind, target_id, anchor_json
               ) VALUES(
                   'anchor.derived', 'owner.1', 'retriever_contribution',
                   'contribution.1', '{}'
               );
               INSERT INTO evidence_assembly_receipts(
                   publication_receipt_id, owner_digest, privacy_domain_id,
                   key_epoch, idempotency_key, assembly_digest, occurrence_set_id,
                   span_id, contribution_id, projection_receipt_id, receipt_json
               ) VALUES(
                   'publication.1', 'owner.1', 'privacy.1', 1, 'idempotency.1',
                   'digest.assembly', 'set.1', 'span.1', 'contribution.1',
                   'projection.1', '{}'
               );
               INSERT INTO memory_v2_facts(
                   fact_id, owner_kind, project_id, owner_json, identity_json, created_at
               ) VALUES(
                   'fact.evidence', 'profile', '', '{"kind":"profile"}', '{}', 1
               );"#,
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    source.checkpoint().await.unwrap();
    source.close();

    sqlite::merge_memory_v2_for_test(&target_path, &source_path)
        .await
        .unwrap();

    let (target, _) = test_open_read_only(&target_path).await;
    for table in [
        "retrieval_anchor_dispositions",
        "retrieval_anchor_reverse_lineage",
        "retrieval_anchor_derivative_tombstones",
        "evidence_source_occurrences",
        "evidence_occurrence_sets",
        "evidence_occurrence_set_members",
        "evidence_spans",
        "evidence_span_members",
        "evidence_span_projection_receipts",
        "evidence_retriever_contributions",
        "evidence_derived_anchors",
        "evidence_assembly_receipts",
    ] {
        let mut rows = target
            .conn()
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1,
            "{table} must survive memory_v2 authority consolidation"
        );
    }
    target.close();
}

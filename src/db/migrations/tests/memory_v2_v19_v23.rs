//! PR7 memory-v2 v19 to v23 migration-chain coverage.

use super::*;

#[tokio::test]
async fn test_create_schema_fresh_v22_proposal_projection_is_terminal() {
    let (conn, _dir) = create_raw_db().await;
    create_schema_connection(&conn)
        .await
        .expect("fresh V22 schema should install");

    conn.execute_batch(
        "INSERT INTO memory_v2_proposals(
            proposal_id, owner_kind, project_id, owner_json, idempotency_key,
            request_digest, request_json, evidence_json, submitted_at
          ) VALUES(
            'proposal.fresh.v22', 'profile', '', '{}', 'fresh-v22', 'fresh-v22',
            '{}', '{}', 1
          );
          INSERT INTO memory_v2_proposal_transitions(
            transition_id, proposal_id, owner_kind, project_id,
            previous_state, current_state, reviewer_json, validation_json,
            origin, promoted_fact_id, promoted_assertion_id, promoted_event_id,
            transition_json, occurred_at
          ) VALUES(
            'transition.fresh.v22', 'proposal.fresh.v22', 'profile', '',
            NULL, 'quarantined', NULL, NULL, 'runtime', NULL, NULL, NULL, '{}', 1
          );
          INSERT INTO memory_v2_proposal_current(
            proposal_id, owner_kind, project_id, state, revision,
            last_transition_id, updated_at
          ) VALUES(
            'proposal.fresh.v22', 'profile', '', 'quarantined', 1,
            'transition.fresh.v22', 1
          );",
    )
    .await
    .expect("fresh V22 projection must admit quarantined terminal state");

    assert!(
        conn.execute(
            "UPDATE memory_v2_proposal_current SET state = 'applying'
             WHERE proposal_id = 'proposal.fresh.v22'",
            (),
        )
        .await
        .is_err(),
        "fresh V22 projection must never durably expose applying"
    );
    assert!(
        conn.execute(
            "UPDATE memory_v2_proposal_current SET revision = 0
             WHERE proposal_id = 'proposal.fresh.v22'",
            (),
        )
        .await
        .is_err(),
        "fresh V22 projection must start revisions at one"
    );
}

#[tokio::test]
async fn test_migrate_v19_pr7_schema_preserves_data_and_enforces_v20_to_v22_contracts() {
    let (conn, _dir) = create_raw_db().await;
    create_v19_memory_schema_for_v20_test(&conn).await;
    conn.execute(
        "UPDATE memory_v2_proposal_current SET state = 'applying'
         WHERE proposal_id = 'proposal.v19'",
        (),
    )
    .await
    .expect("fixture must model a legacy durable applying projection");

    assert!(
        migrate_connection(&conn)
            .await
            .expect("v19 PR7 schema should migrate through v20 and v21")
    );
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert!(column_exists(&conn, "memory_v2_backfill_progress", "cutover_receipt_json").await);
    assert!(column_exists(&conn, "memory_v2_proposals", "idempotency_key").await);
    assert!(column_exists(&conn, "memory_v2_proposals", "request_digest").await);
    assert!(column_exists(&conn, "memory_v2_proposal_transitions", "origin").await);
    assert!(index_exists(&conn, "idx_memory_v2_events_as_of").await);
    assert!(index_exists(&conn, "idx_memory_v2_current_page").await);
    assert!(index_exists(&conn, "idx_memory_v2_evidence_anchor").await);
    assert!(index_exists(&conn, "idx_memory_v2_proposal_list").await);
    assert!(column_exists(&conn, "memory_v2_current_facts", "projection_state").await);
    assert!(index_exists(&conn, "idx_memory_v2_current_compatibility_search").await);
    assert!(table_exists(&conn, "memory_v2_compatibility_operation_receipts").await);
    assert!(table_exists(&conn, "memory_v2_legacy_feedback_event_map").await);
    assert!(table_exists(&conn, "memory_v2_feedback_history").await);
    assert!(table_exists(&conn, "memory_v2_feedback_history_repair_progress").await);

    let mut rows = conn
        .query(
            "SELECT cutover_receipt_json FROM memory_v2_backfill_progress",
            (),
        )
        .await
        .expect("read migrated cutover receipt");
    let receipt: String = rows
        .next()
        .await
        .expect("read migrated cutover receipt row")
        .expect("migrated cutover receipt row")
        .get(0)
        .expect("decode migrated cutover receipt");
    assert!(receipt.contains("legacy_v19_cutover"));

    let mut rows = conn
        .query(
            "SELECT idempotency_key, request_digest FROM memory_v2_proposals
             WHERE proposal_id = 'proposal.v19'",
            (),
        )
        .await
        .expect("read migrated proposal keys");
    let row = rows
        .next()
        .await
        .expect("read migrated proposal key row")
        .expect("migrated proposal key row");
    let idempotency_key: String = row.get(0).expect("decode idempotency key");
    let request_digest: String = row.get(1).expect("decode request digest");
    assert_eq!(idempotency_key, "legacy-v19:proposal.v19");
    assert_eq!(request_digest, "legacy-v19:proposal.v19");

    let mut rows = conn
        .query(
            "SELECT origin FROM memory_v2_proposal_transitions
             WHERE transition_id = 'transition.v19'",
            (),
        )
        .await
        .expect("read migrated transition origin");
    let origin: String = rows
        .next()
        .await
        .expect("read migrated transition origin row")
        .expect("migrated transition origin row")
        .get(0)
        .expect("decode migrated transition origin");
    assert_eq!(origin, "legacy_import");

    let mut rows = conn
        .query(
            "SELECT state, revision, last_transition_id FROM memory_v2_proposal_current
             WHERE proposal_id = 'proposal.v19'",
            (),
        )
        .await
        .expect("read rebuilt proposal current state");
    let row = rows
        .next()
        .await
        .expect("read rebuilt proposal current state row")
        .expect("rebuilt proposal current state row");
    let state: String = row.get(0).expect("decode rebuilt proposal state");
    let revision: i64 = row.get(1).expect("decode rebuilt proposal revision");
    let last_transition_id: String = row.get(2).expect("decode rebuilt transition id");
    assert_eq!(state, "pending");
    assert_eq!(
        revision, 1,
        "V22 rebuild must normalize revision zero to one"
    );
    assert_eq!(last_transition_id, "transition.v19");

    assert!(
        conn.execute(
            "UPDATE memory_v2_proposal_current SET state = 'applying'
             WHERE proposal_id = 'proposal.v19'",
            (),
        )
        .await
        .is_err(),
        "V22 current proposal projection must never durably expose applying"
    );
    conn.execute(
        "UPDATE memory_v2_proposal_current SET state = 'quarantined'
         WHERE proposal_id = 'proposal.v19'",
        (),
    )
    .await
    .expect("V22 current projection must permit quarantined proposals");
    assert!(
        conn.execute(
            "INSERT INTO memory_v2_proposal_transitions(
                transition_id, proposal_id, owner_kind, project_id,
                previous_state, current_state, reviewer_json, validation_json,
                origin, promoted_fact_id, promoted_assertion_id, promoted_event_id,
                transition_json, occurred_at
             ) VALUES(
                'transition.applying.v22', 'proposal.v19', 'profile', '',
                'pending', 'applying', NULL, NULL, 'runtime', NULL, NULL, NULL,
                '{}', 101
             )",
            (),
        )
        .await
        .is_err(),
        "V22 must not append new applying transitions"
    );

    conn.execute_batch(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES ('fact.assertionless', 'profile', '', '{}', '{}', 101);
         INSERT INTO memory_v2_lineage_events(
            event_id, fact_id, owner_kind, project_id, event_json, occurred_at, recorded_at
         ) VALUES ('event.assertionless', 'fact.assertionless', 'profile', '', '{}', 101, 101);",
    )
    .await
    .expect("create assertion-less promotion receipt records");
    conn.execute_batch(
        "INSERT INTO memory_v2_proposal_transitions(
            transition_id, proposal_id, owner_kind, project_id,
            previous_state, current_state, reviewer_json, validation_json,
            origin, promoted_fact_id, promoted_assertion_id, promoted_event_id,
            transition_json, occurred_at
         ) VALUES(
            'transition.assertionless', 'proposal.v19', 'profile', '',
            'pending', 'applied', NULL, NULL,
            'runtime', 'fact.assertionless', NULL, 'event.assertionless',
            '{}', 101
         );",
    )
    .await
    .expect("v20 must permit an applied assertion-less fact batch");
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM pragma_foreign_key_check").await,
        0,
        "the rebuilt proposal projection must retain valid foreign keys"
    );

    let mut rows = conn
        .query(
            "SELECT assertion_header_json FROM memory_v2_assertions
             WHERE assertion_id = 'assertion.v19'",
            (),
        )
        .await
        .expect("read migrated assertion header");
    let header: String = rows
        .next()
        .await
        .expect("read migrated assertion header row")
        .expect("migrated assertion header row")
        .get(0)
        .expect("decode migrated assertion header");
    assert!(!header.contains("v19-header-secret-canary"));
    assert!(
        serde_json::from_str::<serde_json::Value>(&header)
            .expect("migrated assertion header JSON")
            .get("payload")
            .is_none()
    );

    assert!(
        conn.execute(
            "INSERT INTO retrieval_anchor_aliases(
                owner_json, alias_kind, locator_digest, anchor_id
             ) VALUES ('{\"other\":true}', 'fixture', 'other-digest', 'anchor.v19')",
            (),
        )
        .await
        .is_err(),
        "v20 must bind aliases to the exact anchor owner"
    );
    assert!(
        !migrate_connection(&conn)
            .await
            .expect("replaying the v20/v21 migration chain should be a no-op")
    );
}

#[tokio::test]
async fn test_migrate_v20_current_projection_adds_v21_compatibility_state() {
    let (conn, _dir) = create_raw_db().await;
    create_v20_current_projection_for_v21_test(&conn).await;

    assert!(
        migrate_connection(&conn)
            .await
            .expect("v20 current projection should migrate to v21")
    );
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    for column in [
        "retrieval_count",
        "access_count",
        "helpful_count",
        "unhelpful_count",
        "last_retrieved_at",
        "last_recalled_at",
        "last_feedback_at",
        "projection_state",
        "vector_watermark_json",
    ] {
        assert!(
            column_exists(&conn, "memory_v2_current_facts", column).await,
            "v21 current projection must contain {column}"
        );
    }
    assert!(index_exists(&conn, "idx_memory_v2_current_compatibility_search").await);
    assert!(index_exists(&conn, "idx_memory_v2_current_projection_state").await);

    let mut rows = conn
        .query(
            "SELECT retrieval_count, access_count, helpful_count, unhelpful_count,
                    last_retrieved_at, last_recalled_at, last_feedback_at,
                    projection_state, vector_watermark_json
             FROM memory_v2_current_facts WHERE fact_id = 'fact.v20'",
            (),
        )
        .await
        .expect("read migrated V21 current projection");
    let row = rows
        .next()
        .await
        .expect("read V21 current projection row")
        .expect("V21 current projection row");
    for index in 0..4 {
        assert_eq!(
            row.get::<i64>(index).expect("decode V21 telemetry counter"),
            0
        );
    }
    for index in 4..7 {
        assert_eq!(
            row.get::<Option<i64>>(index)
                .expect("decode V21 telemetry timestamp"),
            None
        );
    }
    assert_eq!(
        row.get::<String>(7).expect("decode V21 projection state"),
        "unavailable"
    );
    assert_eq!(
        row.get::<Option<String>>(8)
            .expect("decode V21 vector watermark"),
        None
    );
    assert!(
        conn.execute(
            "UPDATE memory_v2_current_facts
             SET retrieval_count = -1 WHERE fact_id = 'fact.v20'",
            (),
        )
        .await
        .is_err(),
        "V21 telemetry counters must stay non-negative"
    );
    assert!(
        conn.execute(
            "UPDATE memory_v2_current_facts
             SET projection_state = 'invented' WHERE fact_id = 'fact.v20'",
            (),
        )
        .await
        .is_err(),
        "V21 projection state must remain a closed lifecycle"
    );
    assert!(
        conn.execute(
            "UPDATE memory_v2_current_facts
             SET vector_watermark_json = 'not-json' WHERE fact_id = 'fact.v20'",
            (),
        )
        .await
        .is_err(),
        "V21 vector watermark must be JSON when present"
    );
    assert!(
        !migrate_connection(&conn)
            .await
            .expect("replaying V21 migration should be a no-op")
    );
}

#[tokio::test]
async fn test_migrate_v21_adds_owner_bound_compatibility_receipt_ledger() {
    let (conn, _dir) = create_raw_db().await;
    create_v21_current_projection_for_v22_test(&conn).await;
    assert!(!table_exists(&conn, "memory_v2_compatibility_operation_receipts").await);

    assert!(
        migrate_connection(&conn)
            .await
            .expect("v21 current projection should migrate to v22")
    );
    assert_eq!(get_user_version(&conn).await, LATEST_VERSION);
    assert!(table_exists(&conn, "memory_v2_compatibility_operation_receipts").await);
    assert!(table_exists(&conn, "memory_v2_legacy_feedback_event_map").await);
    assert!(table_exists(&conn, "memory_v2_feedback_history").await);
    assert!(table_exists(&conn, "memory_v2_feedback_history_repair_progress").await);
    assert!(table_exists(&conn, "memory_v2_fact_relations").await);
    assert!(table_exists(&conn, "memory_v2_compatibility_banks").await);
    assert!(table_exists(&conn, "memory_v2_compatibility_bank_dirty").await);
    assert!(column_exists(&conn, "memory_v2_fact_relations", "provenance_json").await);
    assert!(index_exists(&conn, "idx_memory_v2_compatibility_receipts_fact").await);
    assert!(index_exists(&conn, "idx_memory_v2_fact_relations_source").await);
    assert!(index_exists(&conn, "idx_memory_v2_fact_relations_target").await);
    assert!(index_exists(&conn, "idx_memory_v2_compatibility_banks_owner").await);
    assert!(index_exists(&conn, "idx_memory_v2_compatibility_bank_dirty_owner").await);

    let mut rows = conn
        .query(
            "SELECT feedback_frontier, feedback_cursor, phase
             FROM memory_v2_feedback_history_repair_progress
             WHERE owner_kind = 'profile' AND project_id = ''
               AND source_store_id = 'legacy-memory-v1'",
            (),
        )
        .await
        .expect("read V22 pending feedback-history repair");
    let row = rows
        .next()
        .await
        .expect("read V22 pending feedback-history repair row")
        .expect("V22 migration must seed a repair row without scanning history");
    assert_eq!(row.get::<i64>(0).expect("decode repair frontier"), 11);
    assert_eq!(row.get::<i64>(1).expect("decode repair cursor"), 0);
    assert_eq!(
        row.get::<String>(2).expect("decode repair phase"),
        "pending"
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_v2_legacy_feedback_event_map"
        )
        .await,
        0,
        "V22 migration must seed repair work, not scan legacy feedback under migration lock"
    );
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_v2_feedback_history").await,
        0,
        "V22 migration must not materialize feedback history under migration lock"
    );

    conn.execute(
        "INSERT INTO memory_v2_compatibility_operation_receipts(
            owner_kind, project_id, operation_id, operation_kind, request_digest,
            fact_id, event_id, receipt_json, recorded_at
         ) VALUES(
            'profile', '', 'operation.v22.feedback', 'feedback', 'digest.v22',
            'fact.v20', 'event.v20',
            '{\"outcome\":\"applied\",\"helpful_count\":1}', 120
         )",
        (),
    )
    .await
    .expect("store a non-payload compatibility receipt");
    conn.execute(
        "INSERT INTO memory_v2_compatibility_operation_receipts(
            owner_kind, project_id, operation_id, operation_kind, request_digest,
            fact_id, event_id, receipt_json, recorded_at
         ) VALUES(
            'profile', '', 'operation.v22.repair', 'repair', 'digest.repair',
            NULL, NULL, '{\"outcome\":\"advanced\",\"processed\":512}', 121
         )",
        (),
    )
    .await
    .expect("a bounded repair command must retain an idempotency receipt");
    for (operation_id, operation_kind) in [
        ("operation.v22.curation", "curation"),
        ("operation.v22.merge", "merge"),
    ] {
        conn.execute(
            "INSERT INTO memory_v2_compatibility_operation_receipts(
                owner_kind, project_id, operation_id, operation_kind, request_digest,
                fact_id, event_id, receipt_json, recorded_at
             ) VALUES(
                'profile', '', ?1, ?2, ?1,
                NULL, NULL, '{\"outcome\":\"applied\"}', 121
             )",
            (operation_id, operation_kind),
        )
        .await
        .expect("a real compatibility operation kind must retain an idempotency receipt");
    }
    assert!(
        conn.execute(
            "INSERT INTO memory_v2_compatibility_operation_receipts(
                owner_kind, project_id, operation_id, operation_kind, request_digest,
                fact_id, event_id, receipt_json, recorded_at
             ) VALUES(
                'profile', '', 'operation.v22.invalid-kind', 'repair_all', 'digest.invalid',
                NULL, NULL, '{\"outcome\":\"rejected\"}', 121
             )",
            (),
        )
        .await
        .is_err(),
        "the receipt ledger must admit only explicit operation kinds"
    );
    assert!(
        conn.execute(
            "INSERT INTO memory_v2_compatibility_operation_receipts(
                owner_kind, project_id, operation_id, operation_kind, request_digest,
                fact_id, event_id, receipt_json, recorded_at
             ) VALUES(
                'profile', '', 'operation.v22.feedback', 'update', 'different-digest',
                'fact.v20', 'event.v20', '{\"outcome\":\"applied\"}', 121
             )",
            (),
        )
        .await
        .is_err(),
        "same owner-bound operation id must not admit a conflicting retry"
    );
    assert!(
        conn.execute(
            "INSERT INTO memory_v2_compatibility_operation_receipts(
                owner_kind, project_id, operation_id, operation_kind, request_digest,
                fact_id, event_id, receipt_json, recorded_at
             ) VALUES(
                'profile', '', 'operation.v22.payload', 'feedback', 'digest.payload',
                'fact.v20', 'event.v20', '{\"metadata\":{\"content\":\"secret\"}}', 121
             )",
            (),
        )
        .await
        .is_err(),
        "compatibility receipts must not retain payload-bearing fields"
    );
    assert!(
        conn.execute(
            "INSERT INTO memory_v2_compatibility_operation_receipts(
                owner_kind, project_id, operation_id, operation_kind, request_digest,
                fact_id, event_id, receipt_json, recorded_at
             ) VALUES(
                'profile', '', 'operation.v22.case-payload', 'feedback', 'digest.case',
                'fact.v20', 'event.v20', '{\"Content\":\"secret\"}', 121
             )",
            (),
        )
        .await
        .is_err(),
        "receipt payload-key filtering must be case-insensitive"
    );
    assert!(
        conn.execute(
            "UPDATE memory_v2_compatibility_operation_receipts
             SET receipt_json = '{\"outcome\":\"changed\"}'
             WHERE operation_id = 'operation.v22.feedback'",
            (),
        )
        .await
        .is_err(),
        "compatibility receipts must remain immutable"
    );
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM pragma_foreign_key_check").await,
        0,
        "a receipt must retain owner-bound canonical references"
    );
    assert!(
        !migrate_connection(&conn)
            .await
            .expect("replaying V22 migration should be a no-op")
    );
}

#[tokio::test]
async fn test_migrate_v21_adds_owner_bound_typed_fact_relations() {
    let (conn, _dir) = create_raw_db().await;
    create_v21_current_projection_for_v22_test(&conn).await;
    migrate_connection(&conn)
        .await
        .expect("v21 current projection should migrate to V22 relations");

    assert!(table_exists(&conn, "memory_v2_fact_relations").await);
    assert!(column_exists(&conn, "memory_v2_fact_relations", "provenance_json").await);
    assert!(index_exists(&conn, "idx_memory_v2_fact_relations_source").await);
    assert!(index_exists(&conn, "idx_memory_v2_fact_relations_target").await);
    conn.execute_batch(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES
            ('fact.relation.source', 'profile', '', '{}', '{}', 200),
            ('fact.relation.target', 'profile', '', '{}', '{}', 200),
            ('fact.relation.evidence', 'profile', '', '{}', '{}', 200),
            ('fact.relation.other-owner', 'project', 'project.other',
             '{\"project_id\":\"project.other\"}', '{}', 200);",
    )
    .await
    .expect("create canonical fact-relation fixture");
    conn.execute(
        "INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, provenance_json, evidence_fact_ids_json, occurred_at, updated_at
         ) VALUES(
            'profile', '', 'fact.relation.source', 'fact.relation.target', 'supports',
            0.8, 'curator', '{\"provenance\":\"fixture\"}',
            '[\"fact.relation.evidence\"]', 200, 200
         )",
        (),
    )
    .await
    .expect("insert a typed supports relation");
    conn.execute(
        "INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, provenance_json, evidence_fact_ids_json, occurred_at, updated_at
         ) VALUES(
            'profile', '', 'fact.relation.source', 'fact.relation.target', 'supports',
            0.9, 'curator', '{\"provenance\":\"updated\"}',
            '[\"fact.relation.evidence\"]', 200, 201
         ) ON CONFLICT(owner_kind, project_id, source_fact_id, target_fact_id, relation)
         DO UPDATE SET confidence = excluded.confidence,
                       source_label = excluded.source_label,
                       provenance_json = excluded.provenance_json,
                       evidence_fact_ids_json = excluded.evidence_fact_ids_json,
                       updated_at = excluded.updated_at",
        (),
    )
    .await
    .expect("typed relation rows must support canonical upsert");
    let mut rows = conn
        .query(
            "SELECT confidence, provenance_json, occurred_at, updated_at
             FROM memory_v2_fact_relations
             WHERE owner_kind = 'profile' AND project_id = ''
               AND source_fact_id = 'fact.relation.source'
               AND target_fact_id = 'fact.relation.target' AND relation = 'supports'",
            (),
        )
        .await
        .expect("read upserted typed relation");
    let row = rows
        .next()
        .await
        .expect("read upserted typed relation row")
        .expect("typed relation row");
    assert!((row.get::<f64>(0).expect("decode relation confidence") - 0.9).abs() <= f64::EPSILON);
    assert_eq!(
        row.get::<String>(1).expect("decode relation provenance"),
        "{\"provenance\":\"updated\"}"
    );
    assert_eq!(row.get::<i64>(2).expect("decode relation occurrence"), 200);
    assert_eq!(row.get::<i64>(3).expect("decode relation update"), 201);

    for relation in ["contradicts", "supersedes", "derived_from"] {
        conn.execute(
            "INSERT INTO memory_v2_fact_relations(
                owner_kind, project_id, source_fact_id, target_fact_id, relation,
                confidence, source_label, provenance_json, evidence_fact_ids_json,
                occurred_at, updated_at
             ) VALUES(
                'profile', '', 'fact.relation.source', 'fact.relation.target', ?1,
                0.8, 'curator', '{\"provenance\":\"fixture\"}',
                '[\"fact.relation.evidence\"]', 202, 202
             )",
            (relation,),
        )
        .await
        .expect("V22 must preserve every legacy relation kind canonically");
    }
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory_v2_fact_relations").await,
        4,
        "all four legacy relation kinds must have canonical rows"
    );

    for statement in [
        "INSERT INTO memory_v2_fact_relations(
             owner_kind, project_id, source_fact_id, target_fact_id, relation,
             confidence, source_label, provenance_json, evidence_fact_ids_json, occurred_at, updated_at
          ) VALUES(
             'profile', '', 'fact.relation.source', 'fact.relation.target', 'unknown',
             0.8, 'curator', '{\"provenance\":\"fixture\"}',
             '[\"fact.relation.evidence\"]', 203, 203
          )",
        "INSERT INTO memory_v2_fact_relations(
             owner_kind, project_id, source_fact_id, target_fact_id, relation,
             confidence, source_label, provenance_json, evidence_fact_ids_json, occurred_at, updated_at
          ) VALUES(
             'profile', '', 'fact.relation.source', 'fact.relation.other-owner', 'derived_from',
             0.8, 'curator', '{\"provenance\":\"fixture\"}',
             '[\"fact.relation.evidence\"]', 203, 203
          )",
        "INSERT INTO memory_v2_fact_relations(
             owner_kind, project_id, source_fact_id, target_fact_id, relation,
             confidence, source_label, provenance_json, evidence_fact_ids_json, occurred_at, updated_at
          ) VALUES(
             'profile', '', 'fact.relation.source', 'fact.relation.target', 'derived_from',
             0.8, 'curator', '{\"provenance\":\"fixture\"}',
             '[\"fact.relation.other-owner\"]', 203, 203
          )",
        "INSERT INTO memory_v2_fact_relations(
             owner_kind, project_id, source_fact_id, target_fact_id, relation,
             confidence, source_label, provenance_json, evidence_fact_ids_json, occurred_at, updated_at
          ) VALUES(
             'profile', '', 'fact.relation.source', 'fact.relation.target', 'derived_from',
             0.8, 'curator', 'not-json', '[\"fact.relation.evidence\"]', 203, 203
          )",
        "INSERT INTO memory_v2_fact_relations(
             owner_kind, project_id, source_fact_id, target_fact_id, relation,
             confidence, source_label, provenance_json, evidence_fact_ids_json, occurred_at, updated_at
          ) VALUES(
             'profile', '', 'fact.relation.source', 'fact.relation.target', 'derived_from',
             0.8, 'curator', '{\"provenance\":\"fixture\"}',
             '[\"fact.relation.evidence\", \"fact.relation.evidence\"]', 203, 203
          )",
    ] {
        assert!(
            conn.execute(statement, ()).await.is_err(),
            "V22 relation rows must reject invalid kinds, provenance, and owner evidence"
        );
    }
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM pragma_foreign_key_check").await,
        0,
        "typed relation rows must retain exact owner-bound canonical references"
    );
}

#[tokio::test]
async fn test_migrate_v21_to_v23_scopes_compatibility_banks_and_dirty_state_by_owner() {
    let (conn, _dir) = create_raw_db().await;
    create_v21_current_projection_for_v22_test(&conn).await;
    migrate_connection(&conn)
        .await
        .expect("v21 current projection should migrate to V23 compatibility banks");

    let profile_vector = valid_v23_compatibility_bank_vector();
    let project_vector = valid_v23_compatibility_bank_vector();
    conn.execute(
        "INSERT INTO memory_v2_compatibility_banks(
            owner_kind, project_id, source_store_id, owner_json, bank_name,
            vector, hrr_algebra, hrr_dim, fact_count, updated_at
         ) VALUES(
            'profile', '', 'legacy-memory-v1', '{\"kind\":\"profile\"}', 'all',
            ?1, 'amari_fhrr', 2048, 1, 100
         )",
        (profile_vector,),
    )
    .await
    .expect("insert profile-owned compatibility bank");
    conn.execute(
        "INSERT INTO memory_v2_compatibility_banks(
            owner_kind, project_id, source_store_id, owner_json, bank_name,
            vector, hrr_algebra, hrr_dim, fact_count, updated_at
         ) VALUES(
            'project', 'project.other', 'legacy-memory-v1',
            '{\"kind\":\"project\",\"project_id\":\"project.other\"}', 'all',
            ?1, 'amari_fhrr', 2048, 1, 100
         )",
        (project_vector,),
    )
    .await
    .expect("insert project-owned compatibility bank");

    let rebuilt_profile_vector = valid_v23_compatibility_bank_vector();
    conn.execute(
        "INSERT INTO memory_v2_compatibility_banks(
            owner_kind, project_id, source_store_id, owner_json, bank_name,
            vector, hrr_algebra, hrr_dim, fact_count, updated_at
         ) VALUES(
            'profile', '', 'legacy-memory-v1', '{\"kind\":\"profile\"}', 'all',
            ?1, 'amari_fhrr', 2048, 2, 101
         ) ON CONFLICT(owner_kind, project_id, source_store_id, bank_name)
         DO UPDATE SET vector = excluded.vector,
                       hrr_algebra = excluded.hrr_algebra,
                       hrr_dim = excluded.hrr_dim,
                       fact_count = excluded.fact_count,
                       updated_at = excluded.updated_at",
        (rebuilt_profile_vector,),
    )
    .await
    .expect("rebuild must replace only the owning bank projection");
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT fact_count FROM memory_v2_compatibility_banks
             WHERE owner_kind = 'profile' AND project_id = ''
               AND source_store_id = 'legacy-memory-v1' AND bank_name = 'all'",
        )
        .await,
        2
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT fact_count FROM memory_v2_compatibility_banks
             WHERE owner_kind = 'project' AND project_id = 'project.other'
               AND source_store_id = 'legacy-memory-v1' AND bank_name = 'all'",
        )
        .await,
        1,
        "a profile rebuild must not mutate another owner's bank"
    );

    conn.execute_batch(
        "INSERT INTO memory_v2_compatibility_bank_dirty(
            owner_kind, project_id, source_store_id, owner_json, bank_name, updated_at
         ) VALUES
            ('profile', '', 'legacy-memory-v1', '{\"kind\":\"profile\"}', 'all', 100),
            ('project', 'project.other', 'legacy-memory-v1',
             '{\"kind\":\"project\",\"project_id\":\"project.other\"}', 'all', 100);",
    )
    .await
    .expect("insert owner-scoped dirty bank projections");
    assert_eq!(
        conn.execute(
            "DELETE FROM memory_v2_compatibility_bank_dirty
             WHERE owner_kind = 'profile' AND project_id = ''
               AND source_store_id = 'legacy-memory-v1' AND bank_name = 'all'
               AND updated_at = 99",
            (),
        )
        .await
        .expect("attempt stale dirty clear"),
        0,
        "a rebuild may clear only the exact dirty generation it read"
    );
    assert_eq!(
        conn.execute(
            "DELETE FROM memory_v2_compatibility_bank_dirty
             WHERE owner_kind = 'profile' AND project_id = ''
               AND source_store_id = 'legacy-memory-v1' AND bank_name = 'all'
               AND updated_at = 100",
            (),
        )
        .await
        .expect("clear rebuilt profile dirty generation"),
        1
    );
    assert_eq!(
        scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM memory_v2_compatibility_bank_dirty
             WHERE owner_kind = 'project' AND project_id = 'project.other'",
        )
        .await,
        1,
        "clearing one owner's rebuilt bank must retain another owner's dirty state"
    );

    let malformed_header = vec![0_u8; 8200];
    assert!(
        conn.execute(
            "INSERT INTO memory_v2_compatibility_banks(
                owner_kind, project_id, source_store_id, owner_json, bank_name,
                vector, hrr_algebra, hrr_dim, fact_count, updated_at
             ) VALUES(
                'profile', '', 'legacy-memory-v1', '{\"kind\":\"profile\"}', 'general',
                ?1, 'amari_fhrr', 2048, 1, 102
             )",
            (malformed_header,),
        )
        .await
        .is_err(),
        "a bank vector must carry the canonical f32-2048 serialization header"
    );
    let mut malformed_length = valid_v23_compatibility_bank_vector();
    malformed_length.pop();
    assert!(
        conn.execute(
            "INSERT INTO memory_v2_compatibility_banks(
                owner_kind, project_id, source_store_id, owner_json, bank_name,
                vector, hrr_algebra, hrr_dim, fact_count, updated_at
             ) VALUES(
                'profile', '', 'legacy-memory-v1', '{\"kind\":\"profile\"}', 'general',
                ?1, 'amari_fhrr', 2048, 1, 102
             )",
            (malformed_length,),
        )
        .await
        .is_err(),
        "a bank vector must have the exact canonical f32-2048 serialization length"
    );
    assert!(
        conn.execute(
            "INSERT INTO memory_v2_compatibility_bank_dirty(
                owner_kind, project_id, source_store_id, owner_json, bank_name, updated_at
             ) VALUES(
                'profile', '', 'legacy-memory-v1',
                '{\"kind\":\"project\",\"project_id\":\"project.other\"}', 'all', 102
             )",
            (),
        )
        .await
        .is_err(),
        "owner JSON must agree with the canonical owner-keyed dirty identity"
    );
}

#[tokio::test]
async fn test_migrate_v20_proposal_rebuild_rolls_back_atomically() {
    let (conn, _dir) = create_raw_db().await;
    create_v19_memory_schema_for_v20_test(&conn).await;
    conn.execute_batch("CREATE TABLE memory_v2_proposal_current_v19 (sentinel TEXT NOT NULL);")
        .await
        .expect("create a deterministic rebuild collision");

    migrate_connection(&conn)
        .await
        .expect_err("a rebuild collision must fail the v20 migration");

    assert_eq!(get_user_version(&conn).await, 19);
    assert!(table_exists(&conn, "memory_v2_proposal_current").await);
    assert!(table_exists(&conn, "memory_v2_proposal_transitions").await);
    assert!(!column_exists(&conn, "memory_v2_proposal_transitions", "origin").await);
    assert!(!column_exists(&conn, "memory_v2_proposals", "idempotency_key").await);
    assert!(!column_exists(&conn, "memory_v2_proposals", "request_digest").await);
}

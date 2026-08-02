use tempfile::TempDir;
use tracedecay_domain::{
    FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1,
    LegacyFactMappingV1, LegacyHistoryCoverageV1, ProvenanceId, UtcMicros,
};

use crate::db::engine::{Connection, TestConnection, params};

use super::schema::{proposal_schema_is_v22, table_exists, table_has_column};
use super::writers::{ensure_current, insert_event, insert_fact_identity, insert_mapping};
use super::*;

async fn database() -> (TestConnection, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory-v2.db");
    let conn = TestConnection::open(&path);
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")
        .await
        .unwrap();
    crate::db::migrations::create_schema_connection(&conn)
        .await
        .unwrap();
    (conn, dir)
}

async fn pre_v22_database() -> (TestConnection, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory-v2-pre-v22.db");
    let conn = TestConnection::open(&path);
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")
        .await
        .unwrap();
    create_schema(&*conn, "memory_v2_pre_v22_test")
        .await
        .unwrap();
    upgrade_v20_schema(&*conn, "memory_v2_pre_v22_test")
        .await
        .unwrap();
    upgrade_v21_schema(&*conn, "memory_v2_pre_v22_test")
        .await
        .unwrap();
    (conn, dir)
}

fn owner() -> FactOwnerV1 {
    FactOwnerV1::Project {
        project_id: tracedecay_domain::ProjectId::new("project.memory-v2-test").unwrap(),
    }
}

fn source_store_id() -> SourceStoreId {
    SourceStoreId::new(V1_COMPATIBILITY_SOURCE_STORE).unwrap()
}

async fn scalar(conn: &Connection, sql: &str) -> i64 {
    scalar_i64(conn, sql).await.unwrap()
}

#[tokio::test]
async fn schema_install_does_not_start_unowned_backfill() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM memory_v2_backfill_progress").await,
        0
    );
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM retrieval_anchors").await,
        0
    );
    assert!(
        !row_exists(
            &conn,
            "SELECT 1 FROM sqlite_master WHERE name = 'memory_v2_retrieval_anchors'",
            (),
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn v20_and_v21_installers_do_not_leak_v22_or_v23_schema() {
    let (runtime, _dir) = pre_v22_database().await;
    let conn = (*runtime).clone();
    assert!(
        !table_exists(&conn, "memory_v2_compatibility_operation_receipts")
            .await
            .unwrap()
    );
    assert!(
        !table_exists(&conn, "memory_v2_feedback_history_repair_progress")
            .await
            .unwrap()
    );
    assert!(
        !table_exists(&conn, "memory_v2_fact_relations")
            .await
            .unwrap()
    );
    assert!(
        !table_exists(&conn, "memory_v2_compatibility_banks")
            .await
            .unwrap()
    );
    assert!(
        !table_exists(&conn, "memory_v2_compatibility_bank_dirty")
            .await
            .unwrap()
    );
    assert!(!proposal_schema_is_v22(&conn).await.unwrap());

    install_v22_fresh_schema(&conn, "memory_v2_v22_fresh_test")
        .await
        .unwrap();
    assert!(
        table_exists(&conn, "memory_v2_compatibility_operation_receipts")
            .await
            .unwrap()
    );
    assert!(
        table_exists(&conn, "memory_v2_feedback_history_repair_progress")
            .await
            .unwrap()
    );
    assert!(
        table_exists(&conn, "memory_v2_fact_relations")
            .await
            .unwrap()
    );
    assert!(
        !table_exists(&conn, "memory_v2_compatibility_banks")
            .await
            .unwrap()
    );
    assert!(
        !table_exists(&conn, "memory_v2_compatibility_bank_dirty")
            .await
            .unwrap()
    );
    assert!(
        !table_has_column(
            &conn,
            "memory_v2_fact_relations",
            "provenance_json",
            "memory_v2_v22_fresh_test",
        )
        .await
        .unwrap()
    );
    assert!(proposal_schema_is_v22(&conn).await.unwrap());

    install_v23_fresh_schema(&conn, "memory_v2_v23_fresh_test")
        .await
        .unwrap();
    assert!(
        table_exists(&conn, "memory_v2_compatibility_banks")
            .await
            .unwrap()
    );
    assert!(
        table_exists(&conn, "memory_v2_compatibility_bank_dirty")
            .await
            .unwrap()
    );
    assert!(
        table_has_column(
            &conn,
            "memory_v2_fact_relations",
            "provenance_json",
            "memory_v2_v23_fresh_test",
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn v20_scrubs_more_assertion_headers_than_the_engine_row_limit() {
    const ASSERTION_COUNT: i64 = 10_001;
    const SEED_BATCH_SIZE: i64 = 1_000;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory-v2-v20-large-scrub.db");
    let runtime = TestConnection::open(&path);
    let conn = (*runtime).clone();
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")
        .await
        .unwrap();
    create_schema(&conn, "memory_v2_v20_large_scrub_test")
        .await
        .unwrap();
    let owner = owner_key(&owner()).unwrap();
    let mut first = 1_i64;
    while first <= ASSERTION_COUNT {
        let last = (first + SEED_BATCH_SIZE - 1).min(ASSERTION_COUNT);
        conn.execute(
            "WITH RECURSIVE sequence(value) AS (
                SELECT ?1
                UNION ALL
                SELECT value + 1 FROM sequence WHERE value < ?2
             )
             INSERT INTO memory_v2_facts(
                fact_id, owner_kind, project_id, owner_json, identity_json, created_at
             )
             SELECT printf('large-scrub.fact.%05d', value), ?3, ?4, ?5, '{}', value
             FROM sequence",
            params![
                first,
                last,
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str()
            ],
        )
        .await
        .unwrap();
        conn.execute(
            "WITH RECURSIVE sequence(value) AS (
                SELECT ?1
                UNION ALL
                SELECT value + 1 FROM sequence WHERE value < ?2
             )
             INSERT INTO memory_v2_assertions(
                assertion_id, fact_id, owner_kind, project_id, owner_json,
                assertion_header_json, kind_json, payload_reference_json,
                receipt_json, asserted_at, actor_id
             )
             SELECT printf('large-scrub.assertion.%05d', value),
                    printf('large-scrub.fact.%05d', value),
                    ?3, ?4, ?5,
                    json_object(
                        'assertion_id', printf('large-scrub.assertion.%05d', value),
                        'payload', 'must-be-removed'
                    ),
                    '{}', '{}', '{}', value, NULL
             FROM sequence",
            params![
                first,
                last,
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str()
            ],
        )
        .await
        .unwrap();
        first = last + 1;
    }

    assert_eq!(
        scalar(
            &conn,
            "SELECT COUNT(*) FROM memory_v2_assertions
             WHERE json_type(assertion_header_json, '$.payload') IS NOT NULL",
        )
        .await,
        ASSERTION_COUNT
    );
    upgrade_v20_schema(&conn, "memory_v2_v20_large_scrub_test")
        .await
        .unwrap();
    assert_eq!(
        scalar(
            &conn,
            "SELECT COUNT(*) FROM memory_v2_assertions
             WHERE json_type(assertion_header_json, '$.payload') IS NOT NULL
                OR json_type(assertion_header_json, '$.content') IS NOT NULL",
        )
        .await,
        0
    );
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertions").await,
        ASSERTION_COUNT
    );
}

#[tokio::test]
async fn v23_rebuilds_v22_fact_relations_without_losing_rows() {
    let (runtime, _dir) = pre_v22_database().await;
    let conn = (*runtime).clone();
    install_v22_fresh_schema(&conn, "memory_v2_v23_relation_upgrade_test")
        .await
        .unwrap();
    let owner = owner_key(&owner()).unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES
            ('v23.relation.source', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1),
            ('v23.relation.target', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1),
            ('v23.relation.evidence', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1);
         INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, evidence_fact_ids_json, occurred_at, updated_at
         ) VALUES(
            '{kind}', '{project_id}', 'v23.relation.source', 'v23.relation.target',
            'supports', 0.8, 'fixture', '[\"v23.relation.evidence\"]', 1, 1
         );",
        kind = owner.kind,
        project_id = owner.project_id,
        owner_json = owner.json,
    ))
    .await
    .unwrap();

    conn.execute("PRAGMA user_version = 22", ()).await.unwrap();
    assert!(
        super::super::migrations::migrate_connection(&conn)
            .await
            .expect("V22 relation fixture must migrate to V23")
    );
    assert_eq!(
        optional_i64(&conn, "PRAGMA user_version", ())
            .await
            .unwrap(),
        Some(i64::from(super::super::migrations::LATEST_VERSION))
    );
    assert!(
        table_exists(&conn, "memory_v2_compatibility_banks")
            .await
            .unwrap()
    );
    assert!(
        table_exists(&conn, "memory_v2_compatibility_bank_dirty")
            .await
            .unwrap()
    );
    assert!(
        table_has_column(
            &conn,
            "memory_v2_fact_relations",
            "provenance_json",
            "memory_v2_v23_relation_upgrade_test",
        )
        .await
        .unwrap()
    );
    assert_eq!(
        optional_string(
            &conn,
            "SELECT provenance_json FROM memory_v2_fact_relations
             WHERE source_fact_id = 'v23.relation.source'
               AND target_fact_id = 'v23.relation.target' AND relation = 'supports'",
            (),
        )
        .await
        .unwrap(),
        Some("{}".to_owned())
    );
    conn.execute(
        "INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, provenance_json, evidence_fact_ids_json,
            occurred_at, updated_at
         ) VALUES(?1, ?2, 'v23.relation.source', 'v23.relation.target',
                   'contradicts', 0.8, 'fixture', '{}',
                   '[\"v23.relation.evidence\"]', 2, 2)",
        params![owner.kind, owner.project_id.as_str()],
    )
    .await
    .unwrap();
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM memory_v2_fact_relations").await,
        2
    );
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM pragma_foreign_key_check").await,
        0
    );
}

#[tokio::test]
async fn purge_clears_runtime_fact_payload_without_a_legacy_mapping() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    let owner = owner();
    let owner_key = owner_key(&owner).unwrap();
    let material = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: ProvenanceId::new("memory-v2.runtime-purge").unwrap(),
        },
    )
    .unwrap();
    let fact_id = FactId::derive(&material).unwrap();
    let identity_json = json_text(&material).unwrap();
    insert_fact_identity(&conn, &owner_key, &fact_id, &identity_json, 10)
        .await
        .unwrap();
    let initial = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Unavailable,
            current: PayloadAccessState::Eligible,
        },
        UtcMicros(10),
        None,
    )
    .unwrap();
    insert_event(&conn, &owner_key, &initial, 10).await.unwrap();
    ensure_current(&conn, &owner_key, &fact_id, initial.event_id(), 10)
        .await
        .unwrap();
    conn.execute(
        "INSERT INTO memory_v2_assertions(
            assertion_id, fact_id, owner_kind, project_id, owner_json,
            assertion_header_json, kind_json, payload_reference_json,
            receipt_json, asserted_at, actor_id
         ) VALUES(
            'assertion.runtime-purge', ?1, ?2, ?3, ?4,
            '{\"assertion_id\":\"assertion.runtime-purge\"}', '{}', '{}', '{}', 10, NULL
         )",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str(),
            owner_key.json.as_str()
        ],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO memory_v2_assertion_payloads(
            assertion_id, fact_id, owner_kind, project_id, payload_json, content
         ) VALUES(
            'assertion.runtime-purge', ?1, ?2, ?3,
            '{\"content\":\"runtime-purge-canary\"}', 'runtime-purge-canary'
         )",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO memory_v2_assertion_vectors(
            assertion_id, fact_id, owner_kind, project_id, vector, algebra, dimensions, precision
         ) VALUES(
            'assertion.runtime-purge', ?1, ?2, ?3, x'0102', 'fixture', 2, 'f32'
         )",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await
    .unwrap();

    let source = source_store_id();
    assert!(
        purge_memory_v2_fact(
            &conn,
            &owner,
            &source,
            &fact_id,
            initial.event_id(),
            UtcMicros(20),
        )
        .await
        .unwrap()
        .payload_purged()
    );
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertion_payloads").await,
        0
    );
    assert_eq!(
        scalar(&conn, "SELECT COUNT(*) FROM memory_v2_assertion_vectors").await,
        0
    );
    assert_eq!(
        scalar(
            &conn,
            "SELECT COUNT(*) FROM memory_v2_assertion_payloads_fts
             WHERE memory_v2_assertion_payloads_fts MATCH '\"runtime-purge-canary\"'"
        )
        .await,
        0
    );
    assert_eq!(
        current_fact_state(&conn, &owner_key, &fact_id)
            .await
            .unwrap()
            .access,
        PayloadAccessState::Deleted
    );
}

#[tokio::test]
async fn owner_archive_exports_and_imports_production_writer_closure_idempotently() {
    let (source_runtime, _source_dir) = database().await;
    let source_conn = (*source_runtime).clone();
    let (target_runtime, _target_dir) = database().await;
    let target_conn = (*target_runtime).clone();
    let owner = owner();
    let owner_key = owner_key(&owner).unwrap();
    let source_store = source_store_id();
    let material = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Legacy {
            source_store_id: source_store.clone(),
            legacy_fact_id: 91,
        },
    )
    .unwrap();
    let fact_id = FactId::derive(&material).unwrap();
    insert_fact_identity(
        &source_conn,
        &owner_key,
        &fact_id,
        &json_text(&material).unwrap(),
        100,
    )
    .await
    .unwrap();
    let mapping = LegacyFactMappingV1::new(
        owner.clone(),
        source_store,
        91,
        fact_id.clone(),
        LegacyHistoryCoverageV1::Complete,
        UtcMicros(100),
    )
    .unwrap();
    insert_mapping(&source_conn, &owner_key, &mapping)
        .await
        .unwrap();
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::LegacyImported { mapping },
        UtcMicros(100),
        None,
    )
    .unwrap();
    insert_event(&source_conn, &owner_key, &event, 100)
        .await
        .unwrap();
    ensure_current(&source_conn, &owner_key, &fact_id, event.event_id(), 100)
        .await
        .unwrap();

    let archive =
        export_memory_v2_owner_archive(&source_conn, MemoryV2ArchiveDatabase::Main, &owner)
            .await
            .unwrap();
    assert_eq!(archive.owner(), &owner);
    for family in [
        tracedecay_store::MemoryV2ArchiveFamilyV1::Fact,
        tracedecay_store::MemoryV2ArchiveFamilyV1::LineageEvent,
        tracedecay_store::MemoryV2ArchiveFamilyV1::CurrentFact,
        tracedecay_store::MemoryV2ArchiveFamilyV1::LegacyFactMap,
    ] {
        assert!(
            archive
                .records()
                .iter()
                .any(|record| record.family() == family),
            "archive omitted {family:?}"
        );
    }

    let transaction = target_conn.transaction().await.unwrap();
    let plan = plan_memory_v2_owner_archive_import(&transaction, &archive)
        .await
        .unwrap();
    assert!(plan.can_apply());
    import_memory_v2_owner_archive(&transaction, &archive, &plan)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let imported =
        export_memory_v2_owner_archive(&target_conn, MemoryV2ArchiveDatabase::Main, &owner)
            .await
            .unwrap();
    assert_eq!(imported, archive);

    let retry = target_conn.transaction().await.unwrap();
    let retry_plan = plan_memory_v2_owner_archive_import(&retry, &archive)
        .await
        .unwrap();
    assert!(retry_plan.can_apply());
    assert!(retry_plan.inserts().is_empty());
    import_memory_v2_owner_archive(&retry, &archive, &retry_plan)
        .await
        .unwrap();
    retry.commit().await.unwrap();
}

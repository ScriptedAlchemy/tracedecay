use tempfile::TempDir;
use tracedecay_domain::{
    FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1,
    ProvenanceId, UtcMicros,
};
use tracedecay_store::{MemoryV2ArchiveFamilyV1, MemoryV2ArchiveScalarV1};

use crate::db::engine::{Connection, TestConnection, params};

use super::schema::{table_exists, table_has_column};
use super::*;

async fn seed_fact_identity(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    fact_id: &FactId,
    identity_json: &str,
    created_at: i64,
) {
    conn.execute(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            identity_json,
            created_at
        ],
    )
    .await
    .unwrap();
}

async fn seed_current_fact(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    fact_id: &FactId,
    event_id: &tracedecay_domain::FactEventId,
    updated_at: i64,
) {
    conn.execute(
        "INSERT INTO memory_v2_current_facts(
            fact_id, owner_kind, project_id, payload_access, trust_score,
            active_assertion_id, last_event_id, updated_at
         ) VALUES(?1, ?2, ?3, 'unavailable', NULL, NULL, ?4, ?5)",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            event_id.as_str(),
            updated_at
        ],
    )
    .await
    .unwrap();
}

async fn database() -> (TestConnection, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory-v2.db");
    let conn = TestConnection::open(&path);
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")
        .await
        .unwrap();
    crate::db::schema::create_schema_connection(&conn)
        .await
        .unwrap();
    (conn, dir)
}

fn owner() -> FactOwnerV1 {
    FactOwnerV1::Project {
        project_id: tracedecay_domain::ProjectId::new("project.memory-v2-test").unwrap(),
    }
}

async fn scalar(conn: &Connection, sql: &str) -> i64 {
    scalar_i64(conn, sql).await.unwrap()
}

#[tokio::test]
async fn fresh_fact_relations_carry_provenance_and_referential_integrity() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    let owner = owner_key(&owner()).unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES
            ('relation.source', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1),
            ('relation.target', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1),
            ('relation.evidence', '{kind}', '{project_id}', '{owner_json}', '{{}}', 1);
         INSERT INTO memory_v2_fact_relations(
            owner_kind, project_id, source_fact_id, target_fact_id, relation,
            confidence, source_label, provenance_json, evidence_fact_ids_json,
            occurred_at, updated_at
         ) VALUES(
            '{kind}', '{project_id}', 'relation.source', 'relation.target',
            'supports', 0.8, 'fixture', '{{}}', '[\"relation.evidence\"]', 1, 1
         );",
        kind = owner.kind,
        project_id = owner.project_id,
        owner_json = owner.json,
    ))
    .await
    .unwrap();

    assert_eq!(
        optional_i64(&conn, "PRAGMA user_version", ())
            .await
            .unwrap(),
        Some(i64::from(super::super::schema::SCHEMA_VERSION))
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
            "memory_v2_relation_integrity_test",
        )
        .await
        .unwrap()
    );
    assert_eq!(
        optional_string(
            &conn,
            "SELECT provenance_json FROM memory_v2_fact_relations
             WHERE source_fact_id = 'relation.source'
               AND target_fact_id = 'relation.target' AND relation = 'supports'",
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
         ) VALUES(?1, ?2, 'relation.source', 'relation.target',
                   'contradicts', 0.8, 'fixture', '{}',
                   '[\"relation.evidence\"]', 2, 2)",
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
async fn owner_archive_exports_and_imports_production_writer_closure_idempotently() {
    let (source_runtime, _source_dir) = database().await;
    let source_conn = (*source_runtime).clone();
    let (target_runtime, _target_dir) = database().await;
    let target_conn = (*target_runtime).clone();
    let owner = owner();
    let owner_key = owner_key(&owner).unwrap();
    let material = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: ProvenanceId::new("memory-v2.archive-test").unwrap(),
        },
    )
    .unwrap();
    let fact_id = FactId::derive(&material).unwrap();
    seed_fact_identity(
        &source_conn,
        &owner_key,
        &fact_id,
        &json_text(&material).unwrap(),
        100,
    )
    .await;
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Unavailable,
            current: PayloadAccessState::Eligible,
        },
        UtcMicros(100),
        None,
    )
    .unwrap();
    source_conn
        .execute(
            "INSERT INTO memory_v2_lineage_events(
                event_id, fact_id, owner_kind, project_id, event_json, occurred_at, recorded_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.event_id().as_str(),
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str(),
                json_text(&event).unwrap(),
                event.occurred_at().0,
                100,
            ],
        )
        .await
        .unwrap();
    seed_current_fact(&source_conn, &owner_key, &fact_id, event.event_id(), 100).await;

    let archive =
        export_memory_v2_owner_archive(&source_conn, MemoryV2ArchiveDatabase::Main, &owner)
            .await
            .unwrap();
    assert_eq!(archive.owner(), &owner);
    for family in [
        tracedecay_store::MemoryV2ArchiveFamilyV1::Fact,
        tracedecay_store::MemoryV2ArchiveFamilyV1::LineageEvent,
        tracedecay_store::MemoryV2ArchiveFamilyV1::CurrentFact,
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

#[tokio::test]
async fn owner_archive_requires_affirmative_eligibility_for_private_details() {
    let (runtime, _dir) = database().await;
    let conn = (*runtime).clone();
    let owner = owner();
    let owner_key = owner_key(&owner).unwrap();

    for (ordinal, payload_access) in [
        Some("quarantined"),
        Some("retention_expired"),
        None,
        Some("eligible"),
    ]
    .into_iter()
    .enumerate()
    {
        let fact_id = format!("archive.denied.fact.{ordinal}");
        let assertion_id = format!("archive.denied.assertion.{ordinal}");
        let event_id = format!("archive.denied.event.{ordinal}");
        conn.execute(
            "INSERT INTO memory_v2_facts(
                fact_id, owner_kind, project_id, owner_json, identity_json, created_at
             ) VALUES(?1, ?2, ?3, ?4, '{}', 100)",
            params![
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str(),
                owner_key.json.as_str(),
            ],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_v2_assertions(
                assertion_id, fact_id, owner_kind, project_id, owner_json,
                assertion_header_json, kind_json, payload_reference_json, receipt_json,
                asserted_at, actor_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, '{}', '{}', '{}', '{}', 100, NULL)",
            params![
                assertion_id.as_str(),
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str(),
                owner_key.json.as_str(),
            ],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_v2_assertion_payloads(
                assertion_id, fact_id, owner_kind, project_id, payload_json, content
             ) VALUES(?1, ?2, ?3, ?4, '{}', 'private payload')",
            params![
                assertion_id.as_str(),
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str(),
            ],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_v2_assertion_vectors(
                assertion_id, fact_id, owner_kind, project_id,
                vector, algebra, dimensions, precision
             ) VALUES(?1, ?2, ?3, ?4, X'00000000', 'cosine', 1, 'f32')",
            params![
                assertion_id.as_str(),
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str(),
            ],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO memory_v2_lineage_events(
                event_id, fact_id, owner_kind, project_id, event_json, occurred_at, recorded_at
             ) VALUES(?1, ?2, ?3, ?4, '{}', 100, 100)",
            params![
                event_id.as_str(),
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str(),
            ],
        )
        .await
        .unwrap();
        if let Some(payload_access) = payload_access {
            conn.execute(
                "INSERT INTO memory_v2_current_facts(
                    fact_id, owner_kind, project_id, payload_access, trust_score,
                    active_assertion_id, last_event_id, updated_at
                 ) VALUES(?1, ?2, ?3, ?4, 0.5, ?5, ?6, 100)",
                params![
                    fact_id.as_str(),
                    owner_key.kind,
                    owner_key.project_id.as_str(),
                    payload_access,
                    assertion_id.as_str(),
                    event_id.as_str(),
                ],
            )
            .await
            .unwrap();
        }
        conn.execute(
            "INSERT INTO memory_v2_feedback_history(
                owner_kind, project_id, fact_id, event_id, action, old_trust, new_trust,
                occurred_at, source, note, details_availability
             ) VALUES(?1, ?2, ?3, ?4, 'helpful', 0.4, 0.5, 100,
                      'private source', 'private note', 'available')",
            params![
                owner_key.kind,
                owner_key.project_id.as_str(),
                fact_id.as_str(),
                event_id.as_str(),
            ],
        )
        .await
        .unwrap();
    }

    let archive = export_memory_v2_owner_archive(&conn, MemoryV2ArchiveDatabase::Main, &owner)
        .await
        .unwrap();
    assert_eq!(
        archive
            .records()
            .iter()
            .filter(|record| record.family() == MemoryV2ArchiveFamilyV1::AssertionPayload)
            .count(),
        1,
        "only an affirmatively eligible payload may leave the owner store"
    );
    assert_eq!(
        archive
            .records()
            .iter()
            .filter(|record| record.family() == MemoryV2ArchiveFamilyV1::AssertionVector)
            .count(),
        1,
        "only an affirmatively eligible vector may leave the owner store"
    );
    let feedback_records = archive
        .records()
        .iter()
        .filter(|record| record.family() == MemoryV2ArchiveFamilyV1::FeedbackHistory)
        .collect::<Vec<_>>();
    assert_eq!(feedback_records.len(), 4);
    assert_eq!(
        feedback_records
            .iter()
            .filter(|record| {
                record.fields().get("source")
                    == Some(&MemoryV2ArchiveScalarV1::Text("private source".to_owned()))
                    && record.fields().get("note")
                        == Some(&MemoryV2ArchiveScalarV1::Text("private note".to_owned()))
                    && record.fields().get("details_availability")
                        == Some(&MemoryV2ArchiveScalarV1::Text("available".to_owned()))
            })
            .count(),
        1,
        "only affirmatively eligible feedback may retain private details"
    );
    for record in feedback_records.into_iter().filter(|record| {
        record.fields().get("details_availability")
            != Some(&MemoryV2ArchiveScalarV1::Text("available".to_owned()))
    }) {
        assert_eq!(
            record.fields().get("source"),
            Some(&MemoryV2ArchiveScalarV1::Null)
        );
        assert_eq!(
            record.fields().get("note"),
            Some(&MemoryV2ArchiveScalarV1::Null)
        );
        assert_eq!(
            record.fields().get("details_availability"),
            Some(&MemoryV2ArchiveScalarV1::Text("legacy_redacted".to_owned()))
        );
    }
}

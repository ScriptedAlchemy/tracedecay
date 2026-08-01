use super::*;
use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::store::memory::DatabaseFactStore;
use crate::store::memory::primitives::{OwnerKey, storage_message};
use tempfile::tempdir;
use tracedecay_domain::{Confidence, FactId, FactOwnerV1, ProjectId, UtcMicros};
use tracedecay_store::{FactCompatibilityResult, FactCompatibilityStoreError, FactStoreError};

async fn seed_fact(db: &crate::db::Database, owner: &FactOwnerV1, fact_id: &FactId) {
    let key = OwnerKey::new(owner).unwrap();
    db.writer_connection("seed curated correction provenance fact")
        .await
        .unwrap()
        .execute_engine(
            "INSERT INTO memory_v2_facts(
                fact_id, owner_kind, project_id, owner_json, identity_json, created_at
             ) VALUES(?1, ?2, ?3, ?4, '{}', 1)",
            crate::db::engine::params![
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn curated_correction_provenance_is_exact_owner_scoped_and_replay_safe() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("curated-correction-provenance.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "curated correction provenance test").unwrap();
    let (db, _) = crate::db::Database::publish_test_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    let owner = FactOwnerV1::Project {
        project_id: ProjectId::new("pr7.curated-correction".to_owned()).unwrap(),
    };
    let source = FactId::new("fact.curated.source").unwrap();
    let evidence_a = FactId::new("fact.curated.evidence-a").unwrap();
    let evidence_b = FactId::new("fact.curated.evidence-b").unwrap();
    for fact_id in [&source, &evidence_a, &evidence_b] {
        seed_fact(&db, &owner, fact_id).await;
    }
    let evidence = vec![evidence_b.clone(), evidence_a.clone()];
    let store = DatabaseFactStore::new(&db);
    for _ in 0..2 {
        let owner = owner.clone();
        let source = source.clone();
        let evidence = evidence.clone();
        store
            .compatibility_write(move |transaction| {
                Box::pin(async move {
                    compatibility_record_curated_correction_provenance_tx(
                        transaction,
                        &owner,
                        &source,
                        &evidence,
                        Confidence::new(0.91).unwrap(),
                        "normalize_tags",
                        None,
                        UtcMicros(77),
                    )
                    .await
                    .map_err(Into::into)
                })
            })
            .await
            .unwrap();
    }

    let key = OwnerKey::new(&owner).unwrap();
    let expected_evidence =
        serde_json::to_string(&[evidence_b.as_str(), evidence_a.as_str()]).unwrap();
    let writer = db
        .writer_connection("read curated correction provenance")
        .await
        .unwrap();
    let mut rows = writer
        .query_engine(
            "SELECT target_fact_id, relation, confidence, source_label,
                    provenance_json, evidence_fact_ids_json, occurred_at, updated_at
             FROM memory_v2_fact_relations
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_fact_id = ?3
             ORDER BY target_fact_id",
            crate::db::engine::params![key.kind, key.project_id.as_str(), source.as_str(),],
        )
        .await
        .unwrap();
    let mut targets = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        targets.push(row.get::<String>(0).unwrap());
        assert_eq!(row.get::<String>(1).unwrap(), "derived_from");
        assert!((row.get::<f64>(2).unwrap() - 0.91).abs() <= f64::EPSILON);
        assert_eq!(
            row.get::<String>(3).unwrap(),
            "compatibility_curation_normalize_tags"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&row.get::<String>(4).unwrap()).unwrap(),
            serde_json::json!({"actor_id": null, "operation": "normalize_tags"})
        );
        assert_eq!(row.get::<String>(5).unwrap(), expected_evidence);
        assert_eq!(row.get::<i64>(6).unwrap(), 77);
        assert_eq!(row.get::<i64>(7).unwrap(), 77);
    }
    assert_eq!(
        targets,
        vec![
            evidence_a.as_str().to_owned(),
            evidence_b.as_str().to_owned()
        ]
    );
}

#[tokio::test]
async fn curated_correction_provenance_rolls_back_with_its_transaction() {
    let temp = tempdir().unwrap();
    let path = temp
        .path()
        .join("curated-correction-provenance-rollback.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "curated correction provenance rollback").unwrap();
    let (db, _) = crate::db::Database::publish_test_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    let owner = FactOwnerV1::Profile;
    let source = FactId::new("fact.curated.rollback-source").unwrap();
    let evidence = FactId::new("fact.curated.rollback-evidence").unwrap();
    seed_fact(&db, &owner, &source).await;
    seed_fact(&db, &owner, &evidence).await;

    let store = DatabaseFactStore::new(&db);
    let failed: FactCompatibilityResult<()> = store
        .compatibility_write(move |transaction| {
            Box::pin(async move {
                compatibility_record_curated_correction_provenance_tx(
                    transaction,
                    &owner,
                    &source,
                    &[evidence],
                    Confidence::new(0.8).unwrap(),
                    "merge_entities",
                    None,
                    UtcMicros(88),
                )
                .await?;
                Err(storage_message("curated correction provenance test", "force rollback").into())
            })
        })
        .await;
    assert!(failed.is_err());

    let writer = db
        .writer_connection("verify curated correction provenance rollback")
        .await
        .unwrap();
    let mut rows = writer
        .query_engine("SELECT COUNT(*) FROM memory_v2_fact_relations", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}

#[tokio::test]
async fn curated_correction_provenance_rejects_cross_owner_evidence() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("curated-correction-owner-isolation.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "curated correction owner isolation").unwrap();
    let (db, _) = crate::db::Database::publish_test_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    let source_owner = FactOwnerV1::Profile;
    let evidence_owner = FactOwnerV1::Project {
        project_id: ProjectId::new("pr7.foreign-evidence".to_owned()).unwrap(),
    };
    let source = FactId::new("fact.curated.owner-source").unwrap();
    let evidence = FactId::new("fact.curated.foreign-evidence").unwrap();
    seed_fact(&db, &source_owner, &source).await;
    seed_fact(&db, &evidence_owner, &evidence).await;

    let store = DatabaseFactStore::new(&db);
    let failed: FactCompatibilityResult<()> = store
        .compatibility_write(move |transaction| {
            Box::pin(async move {
                compatibility_record_curated_correction_provenance_tx(
                    transaction,
                    &source_owner,
                    &source,
                    &[evidence],
                    Confidence::new(0.8).unwrap(),
                    "normalize_tags",
                    None,
                    UtcMicros(89),
                )
                .await
                .map_err(Into::into)
            })
        })
        .await;
    assert!(failed.is_err());

    let writer = db
        .writer_connection("verify curated correction owner isolation")
        .await
        .unwrap();
    let mut rows = writer
        .query_engine("SELECT COUNT(*) FROM memory_v2_fact_relations", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}

#[tokio::test]
async fn curated_correction_rejects_self_only_evidence_without_writing_provenance() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("curated-correction-self-evidence.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "curated correction self evidence").unwrap();
    let (db, _) = crate::db::Database::publish_test_runtime(
        &path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    let owner = FactOwnerV1::Profile;
    let source = FactId::new("fact.curated.self-evidence").unwrap();
    seed_fact(&db, &owner, &source).await;

    let store = DatabaseFactStore::new(&db);
    let failed: FactCompatibilityResult<()> = store
        .compatibility_write(move |transaction| {
            Box::pin(async move {
                compatibility_record_curated_correction_provenance_tx(
                    transaction,
                    &owner,
                    &source,
                    std::slice::from_ref(&source),
                    Confidence::new(0.8).unwrap(),
                    "normalize_tags",
                    None,
                    UtcMicros(99),
                )
                .await
                .map_err(Into::into)
            })
        })
        .await;
    let FactCompatibilityStoreError::Store(FactStoreError::Storage { source, .. }) =
        failed.unwrap_err()
    else {
        panic!("self-only correction must fail as a storage contract violation");
    };
    assert!(
        source
            .to_string()
            .contains("evidence cannot be the corrected fact")
    );

    let writer = db
        .writer_connection("verify self evidence rejection")
        .await
        .unwrap();
    let mut rows = writer
        .query_engine("SELECT COUNT(*) FROM memory_v2_fact_relations", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}

use super::*;

use crate::db::DatabaseAuthority;
use tempfile::tempdir;

#[tokio::test]
async fn daemon_cutover_binds_raw_v1_rows_without_query_fallback() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("compatibility-cutover.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "compatibility cutover authority test").unwrap();
    let (db, _) = Database::initialize(&path, &authority).await.unwrap();
    let owner = FactOwnerV1::Profile;
    let source_store_id = compatibility_source_store_id().unwrap();
    {
        let writer = db
            .writer_connection("seed raw v1 compatibility cutover fixture")
            .await
            .unwrap();
        for fact_id in 1..=202 {
            writer
                .execute(
                    "INSERT INTO memory_facts(
                        fact_id, content, category, tags, trust_score, source,
                        metadata, hrr_vector, created_at, updated_at
                     ) VALUES(?1, ?2, 'project', '[]', 0.5, 'manual', '{}', NULL, 10, 10)",
                    params![fact_id, format!("cutover fixture fact {fact_id}")],
                )
                .await
                .unwrap();
        }
    }

    let store = DatabaseFactStore::new(&db);
    let target = CompatibilityFactTargetV1::Legacy(
        LegacyFactQuery::new(owner.clone(), source_store_id.clone(), 1).unwrap(),
    );
    assert!(
        store
            .get_compatibility_fact(target.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        compatibility_mapping_count(&db, &owner, &source_store_id).await,
        0
    );

    let request = CompatibilityLegacyMemoryCutoverCommandV1::new(
        owner.clone(),
        ProvenanceId::new("compatibility-legacy-cutover-test".to_owned()).unwrap(),
    )
    .unwrap();
    let mut progress = Vec::new();
    for _ in 0..8 {
        let step = store
            .advance_compatibility_legacy_memory_cutover(request.clone())
            .await
            .unwrap();
        progress.push(step);
        if step.is_complete() {
            break;
        }
    }
    assert_eq!(
        progress.last(),
        Some(&CompatibilityLegacyMemoryCutoverProgressV1::Complete)
    );
    assert!(progress.iter().any(|step| {
        matches!(
            step,
            CompatibilityLegacyMemoryCutoverProgressV1::Incomplete { processed: 202 }
        )
    }));
    assert_eq!(
        compatibility_mapping_count(&db, &owner, &source_store_id).await,
        202
    );

    let projection = store.get_compatibility_fact(target).await.unwrap().unwrap();
    let mapping = projection
        .mapping()
        .and_then(CompatibilityFactMappingV1::legacy_mapping)
        .unwrap();
    assert_eq!(mapping.owner(), &owner);
    assert_eq!(mapping.source_store_id(), &source_store_id);
    assert_eq!(mapping.legacy_fact_id(), 1);
    assert_eq!(
        store
            .advance_compatibility_legacy_memory_cutover(request)
            .await
            .unwrap(),
        CompatibilityLegacyMemoryCutoverProgressV1::Complete
    );
}

async fn compatibility_mapping_count(
    db: &Database,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
) -> i64 {
    let key = OwnerKey::new(owner).unwrap();
    let writer = db
        .writer_connection("count raw v1 compatibility cutover mappings")
        .await
        .unwrap();
    let mut rows = writer
        .query(
            "SELECT COUNT(*) FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2 AND owner_json = ?3
               AND source_store_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .unwrap();
    let count = rows.next().await.unwrap().unwrap().get(0).unwrap();
    drop(rows);
    count
}

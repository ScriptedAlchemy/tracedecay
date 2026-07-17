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

#[tokio::test]
async fn cutover_preserves_legacy_usage_telemetry_and_search_ranking() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("compatibility-cutover-telemetry.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "compatibility cutover telemetry test").unwrap();
    let (db, _) = Database::initialize(&path, &authority).await.unwrap();
    let owner = FactOwnerV1::Project {
        project_id: tracedecay_domain::ProjectId::new("pr7.project.cutover-telemetry".to_owned())
            .unwrap(),
    };
    {
        let raw_db = libsql::Builder::new_local(&path).build().await.unwrap();
        let writer = raw_db.connect().unwrap();
        let legacy = crate::memory::store::MemoryStore::new(&writer);
        for (content, source) in [
            (
                "Database backups run via pg_dump every night",
                "on-topic-backup-plural",
            ),
            (
                "Acme primary database runs on Postgres",
                "off-topic-backup-exact",
            ),
        ] {
            legacy
                .add_fact(
                    crate::memory::types::AddFactRequest {
                        content: content.to_owned(),
                        category: crate::memory::types::MemoryCategory::Project,
                        source: Some(source.to_owned()),
                        tags: Vec::new(),
                        entities: Vec::new(),
                        trust: Some(0.5),
                        metadata: serde_json::json!({}),
                    },
                    crate::memory::trust::DEFAULT_TRUST,
                )
                .await
                .unwrap();
        }
        // Usage counters have no legacy event log to replay; they exist only
        // as columns on `memory_facts`, so the cutover must carry them or a
        // migrated store silently loses its ranking usage signal. The recency
        // timestamps stay behind by contract: canonical created_at is the
        // migration time and pre-creation recency never validates.
        writer
            .execute(
                "UPDATE memory_facts SET trust_score = 0.5, retrieval_count = 5000,
                     access_count = 5100, helpful_count = 7, unhelpful_count = 2,
                     last_retrieved_at = 1700000000, last_recalled_at = 1700000100,
                     last_feedback_at = 1700000200
                 WHERE source = 'on-topic-backup-plural'",
                (),
            )
            .await
            .unwrap();
    }
    let store = DatabaseFactStore::new(&db);
    let request = CompatibilityLegacyMemoryCutoverCommandV1::new(
        owner.clone(),
        ProvenanceId::new("compatibility-legacy-cutover-telemetry-test".to_owned()).unwrap(),
    )
    .unwrap();
    for _ in 0..8 {
        if store
            .advance_compatibility_legacy_memory_cutover(request.clone())
            .await
            .unwrap()
            .is_complete()
        {
            break;
        }
    }

    let application =
        crate::application::memory::MemoryApplication::new(owner.clone(), store).unwrap();
    let context = crate::application::memory::MemoryOperationContext::from_trusted_request_id(
        &owner,
        "search",
        "cutover-telemetry-search-1",
        None,
    )
    .unwrap();
    let results = application
        .search_facts_v1(
            crate::memory::types::SearchFactsRequest {
                query: "database backup".to_owned(),
                category: None,
                limit: Some(5),
                min_trust: None,
                include_why: true,
            },
            context,
        )
        .await
        .unwrap();
    let hit = results
        .iter()
        .find(|row| row.fact.source.as_deref() == Some("on-topic-backup-plural"))
        .expect("backfilled fact must be searchable through the canonical projection");
    // The tracked search itself bumps retrieval/access by one; the carried
    // legacy baseline must still dominate the count.
    assert!(
        hit.fact.retrieval_count >= 5000,
        "legacy retrieval_count must survive the cutover, got {}",
        hit.fact.retrieval_count
    );
    assert!(hit.fact.access_count >= 5100);
    assert_eq!(hit.fact.helpful_count, 7);
    assert_eq!(hit.fact.unhelpful_count, 2);
    assert_eq!(
        hit.fact.last_feedback_at, None,
        "pre-migration recency timestamps cannot precede canonical created_at and must be dropped"
    );
    assert_eq!(
        results[0].fact.source.as_deref(),
        Some("on-topic-backup-plural"),
        "usage boost from carried retrieval telemetry must rank the reinforced fact first"
    );
}

#[tokio::test]
async fn dashboard_vector_points_report_v1_entity_link_connections() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("vector-points.db");
    let authority = DatabaseAuthority::acquire_test(&path, "vector points test").unwrap();
    let (db, _) = Database::initialize(&path, &authority).await.unwrap();
    let owner = FactOwnerV1::Profile;
    let store = DatabaseFactStore::new(&db);
    let application =
        crate::application::memory::MemoryApplication::new(owner.clone(), store).unwrap();
    for (content, category, entities) in [
        (
            "Cache invalidation policy must be explicit",
            crate::memory::types::MemoryCategory::Project,
            vec!["CachePolicy".to_owned()],
        ),
        (
            "LCM dashboard empty states must stay friendly",
            crate::memory::types::MemoryCategory::Tool,
            vec!["LCMTab".to_owned(), "SimilarityView".to_owned()],
        ),
    ] {
        application
            .add_fact_v1(
                crate::memory::types::AddFactRequest {
                    content: content.to_owned(),
                    category,
                    source: Some("scratch".to_owned()),
                    tags: vec!["cache".to_owned()],
                    entities,
                    trust: Some(0.8),
                    metadata: serde_json::json!({}),
                },
                crate::application::memory::MemoryOperationContext::generated(
                    &owner,
                    "scratch-add",
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap();
    }
    let points = application
        .dashboard_vector_points_v1(None, 50)
        .await
        .unwrap();
    assert_eq!(points.len(), 2);
    // V1 dashboard parity: a fact's graph connection count is its
    // entity-link count, not a shared-fact tally.
    let mut counts = points
        .iter()
        .map(|point| (point.entity_count, point.connection_count))
        .collect::<Vec<_>>();
    counts.sort_unstable();
    assert_eq!(counts, vec![(1, 1), (2, 2)]);
}

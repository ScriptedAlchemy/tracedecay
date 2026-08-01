//! PR7 compatibility fact authority suite (moved verbatim from `memory_test`).

use super::*;

fn compatibility_project_owner(project_id: &str) -> FactOwnerV1 {
    FactOwnerV1::Project {
        project_id: ProjectId::new(project_id.to_owned()).unwrap(),
    }
}

async fn add_compatibility_fixture_fact(
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    owner: &FactOwnerV1,
    content: &str,
    category: MemoryCategory,
) -> FactRecord {
    memory
        .add_fact_v1(
            fact_request(content, category, 0.8),
            MemoryOperationContext::generated(owner, "memory-test-add", None).unwrap(),
        )
        .await
        .unwrap()
        .fact
        .unwrap()
}

fn compatibility_owner_scope(owner: &FactOwnerV1) -> (&'static str, String, String) {
    let (kind, project_id) = match owner {
        FactOwnerV1::Profile => ("profile", String::new()),
        FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_string()),
    };
    (kind, project_id, serde_json::to_string(owner).unwrap())
}

async fn compatibility_bank_rows(
    db: &Database,
    owner: &FactOwnerV1,
) -> Vec<(String, Vec<u8>, i64, i64)> {
    let (kind, project_id, owner_json) = compatibility_owner_scope(owner);
    let conn = rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = conn
        .prepare(
            "SELECT bank_name, vector, fact_count, updated_at
             FROM memory_v2_compatibility_banks
             WHERE owner_kind = ?1 AND project_id = ?2 AND owner_json = ?3
               AND source_store_id = 'legacy-memory-v1'
             ORDER BY bank_name",
        )
        .unwrap();
    statement
        .query_map(rusqlite::params![kind, project_id, owner_json], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

async fn compatibility_dirty_bank_rows(db: &Database, owner: &FactOwnerV1) -> Vec<(String, i64)> {
    let (kind, project_id, owner_json) = compatibility_owner_scope(owner);
    let conn = rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = conn
        .prepare(
            "SELECT bank_name, updated_at
             FROM memory_v2_compatibility_bank_dirty
             WHERE owner_kind = ?1 AND project_id = ?2 AND owner_json = ?3
               AND source_store_id = 'legacy-memory-v1'
             ORDER BY bank_name",
        )
        .unwrap();
    statement
        .query_map(rusqlite::params![kind, project_id, owner_json], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[tokio::test]
async fn compatibility_repair_rebuilds_only_requested_owner_banks() {
    let (db, _tmp) = make_memory_store().await;
    let owner_a = compatibility_project_owner("repair-owner-a");
    let owner_b = compatibility_project_owner("repair-owner-b");
    let memory_a = MemoryApplication::new(owner_a.clone(), DatabaseFactStore::new(&db)).unwrap();
    let memory_b = MemoryApplication::new(owner_b.clone(), DatabaseFactStore::new(&db)).unwrap();
    let fact_a = add_compatibility_fixture_fact(
        &memory_a,
        &owner_a,
        "Owner A repair must not rebuild owner B banks",
        MemoryCategory::Project,
    )
    .await;
    let fact_b = add_compatibility_fixture_fact(
        &memory_b,
        &owner_b,
        "Owner B dirty vector must remain pending during owner A repair",
        MemoryCategory::Tool,
    )
    .await;
    execute_sql(
        &db,
        "UPDATE memory_facts
         SET hrr_vector = NULL, hrr_algebra = 'broken', hrr_dim = 0, hrr_precision = 'broken'
         WHERE fact_id IN (?1, ?2)",
        rusqlite::params![fact_a.fact_id, fact_b.fact_id],
    );

    assert_eq!(
        memory_b
            .dashboard_memory_status_v1()
            .await
            .unwrap()
            .missing_vector_count(),
        1
    );
    let repair_a = memory_a
        .dashboard_repair_v1(
            MemoryOperationContext::generated(&owner_a, "owner-a-explicit-repair", None).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(repair_a.missing_vectors_repaired(), 1);
    assert_eq!(repair_a.banks_rebuilt(), 2);

    let status_a = memory_a.dashboard_memory_status_v1().await.unwrap();
    let status_b = memory_b.dashboard_memory_status_v1().await.unwrap();
    assert_eq!(status_a.missing_vector_count(), 0);
    assert_eq!(status_a.bank_count(), 2);
    assert_eq!(status_b.missing_vector_count(), 1);
    assert_eq!(status_b.bank_count(), 0);
    assert!(
        fact_has_no_hrr_vector(&db, fact_b.fact_id).await,
        "owner B vector must remain pending after owner A repair"
    );
    assert_eq!(
        memory_a
            .dashboard_overview_v1(10, 10)
            .await
            .unwrap()
            .memory_banks
            .len(),
        2
    );
    assert!(
        memory_b
            .dashboard_overview_v1(10, 10)
            .await
            .unwrap()
            .memory_banks
            .is_empty(),
        "owner B dashboard must not expose owner A bank state"
    );
}

#[tokio::test]
async fn compatibility_rebuild_keeps_ready_peer_owner_banks_unchanged() {
    let (db, _tmp) = make_memory_store().await;
    let owner_a = compatibility_project_owner("bank-rebuild-owner-a");
    let owner_b = compatibility_project_owner("bank-rebuild-owner-b");
    let memory_a = MemoryApplication::new(owner_a.clone(), DatabaseFactStore::new(&db)).unwrap();
    let memory_b = MemoryApplication::new(owner_b.clone(), DatabaseFactStore::new(&db)).unwrap();
    let fact_a = add_compatibility_fixture_fact(
        &memory_a,
        &owner_a,
        "Owner A deletion rebuilds only owner A compatibility banks",
        MemoryCategory::Project,
    )
    .await;
    add_compatibility_fixture_fact(
        &memory_b,
        &owner_b,
        "Owner B ready compatibility banks must remain unchanged",
        MemoryCategory::Project,
    )
    .await;

    assert_eq!(
        memory_a
            .dashboard_repair_v1(
                MemoryOperationContext::generated(&owner_a, "prepare-owner-a-banks", None).unwrap(),
            )
            .await
            .unwrap()
            .banks_rebuilt(),
        2
    );
    assert_eq!(
        memory_b
            .dashboard_repair_v1(
                MemoryOperationContext::generated(&owner_b, "prepare-owner-b-banks", None).unwrap(),
            )
            .await
            .unwrap()
            .banks_rebuilt(),
        2
    );

    let overview_b_before = memory_b.dashboard_overview_v1(10, 10).await.unwrap();
    let banks_b_before = compatibility_bank_rows(&db, &owner_b).await;
    let dirty_b_before = compatibility_dirty_bank_rows(&db, &owner_b).await;
    assert_eq!(banks_b_before.len(), 2);
    assert!(dirty_b_before.is_empty());
    assert_eq!(overview_b_before.bank_count, 2);
    assert_eq!(overview_b_before.memory_banks.len(), 2);
    assert_eq!(overview_b_before.hrr_coverage.len(), 1);
    assert_eq!(
        overview_b_before.hrr_coverage[0].state,
        tracedecay_store::CompatibilityDashboardHrrStateV1::Ready
    );

    assert!(
        memory_a
            .remove_fact_v1(
                fact_a.fact_id,
                MemoryOperationContext::generated(&owner_a, "delete-owner-a-fact", None).unwrap(),
            )
            .await
            .unwrap()
    );
    assert_eq!(
        memory_a
            .dashboard_repair_v1(
                MemoryOperationContext::generated(&owner_a, "rebuild-owner-a-banks", None).unwrap(),
            )
            .await
            .unwrap()
            .banks_rebuilt(),
        2
    );
    assert!(
        memory_a
            .dashboard_overview_v1(10, 10)
            .await
            .unwrap()
            .memory_banks
            .is_empty()
    );

    let overview_b_after = memory_b.dashboard_overview_v1(10, 10).await.unwrap();
    assert_eq!(compatibility_bank_rows(&db, &owner_b).await, banks_b_before);
    assert_eq!(
        compatibility_dirty_bank_rows(&db, &owner_b).await,
        dirty_b_before
    );
    assert_eq!(
        overview_b_after.memory_banks,
        overview_b_before.memory_banks
    );
    assert_eq!(
        overview_b_after.hrr_coverage,
        overview_b_before.hrr_coverage
    );
    assert_eq!(overview_b_after.bank_count, 2);
    assert_eq!(
        overview_b_after.hrr_coverage[0].state,
        tracedecay_store::CompatibilityDashboardHrrStateV1::Ready
    );
}

#[tokio::test]
async fn compatibility_v1_remove_defaults_to_the_current_event() {
    let (db, _tmp) = make_memory_store().await;
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
    let fact = add_compatibility_fixture_fact(
        &memory,
        &owner,
        "A V1 remove without a caller CAS must use the current fact event",
        MemoryCategory::Project,
    )
    .await;

    assert!(
        memory
            .remove_fact_v1(
                fact.fact_id,
                MemoryOperationContext::generated(&owner, "remove-default-cas", None).unwrap(),
            )
            .await
            .unwrap(),
        "the V1 surface has no caller CAS and must remove an existing fact"
    );
}

#[tokio::test]
async fn compatibility_v1_remove_redacts_feedback_history_free_text() {
    let (db, _tmp) = make_memory_store().await;
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
    let fact = add_compatibility_fixture_fact(
        &memory,
        &owner,
        "Deleting a fact must erase its feedback free text",
        MemoryCategory::Project,
    )
    .await;
    memory
        .record_fact_feedback_v1(
            FeedbackRequest {
                fact_id: fact.fact_id,
                action: FeedbackAction::Helpful,
                source: Some("reviewer".to_string()),
                note: Some("private context that must not outlive the fact".to_string()),
            },
            MemoryOperationContext::generated(&owner, "redaction-feedback", None).unwrap(),
        )
        .await
        .unwrap();

    let history_state = |label: &'static str| {
        let db = &db;
        async move {
            rusqlite::Connection::open_with_flags(
                db.database_path(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap()
            .query_row(
                "SELECT COUNT(*),
                            COUNT(source), COUNT(note),
                            SUM(CASE WHEN details_availability = 'available' THEN 1 ELSE 0 END)
                     FROM memory_v2_feedback_history
                     WHERE fact_id = (
                         SELECT fact_id FROM memory_v2_legacy_map WHERE legacy_fact_id = ?1
                     )",
                rusqlite::params![fact.fact_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .unwrap_or_else(|_| panic!("{label}: feedback history row must exist"))
        }
    };
    let (rows, sources, notes, available) = history_state("before remove").await;
    assert_eq!(rows, 1);
    assert_eq!((sources, notes, available), (1, 1, 1));

    assert!(
        memory
            .remove_fact_v1(
                fact.fact_id,
                MemoryOperationContext::generated(&owner, "redaction-remove", None).unwrap(),
            )
            .await
            .unwrap()
    );

    // A live deletion must erase the same feedback free-text surface as the
    // canonical purge path: rows stay for lineage, but source/note are gone
    // and availability is downgraded.
    let (rows, sources, notes, available) = history_state("after remove").await;
    assert_eq!(rows, 1, "feedback lineage rows must survive deletion");
    assert_eq!(
        (sources, notes, available),
        (0, 0, 0),
        "deleted facts must not retain feedback source/note free text"
    );
}

#[tokio::test]
async fn compatibility_repair_skips_malformed_unavailable_vectors() {
    let (db, _tmp) = make_memory_store().await;
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
    let fact = add_compatibility_fixture_fact(
        &memory,
        &owner,
        "Malformed unavailable vectors must not abort repair",
        MemoryCategory::Project,
    )
    .await;
    execute_sql(
        &db,
        "UPDATE memory_facts
         SET hrr_vector = X'00', hrr_algebra = 'amari_fhrr', hrr_dim = 2048,
             hrr_precision = 'f32'
         WHERE fact_id = ?1",
        rusqlite::params![fact.fact_id],
    );
    execute_sql(
        &db,
        "UPDATE memory_v2_current_facts
         SET payload_access = 'quarantined'
         WHERE fact_id = (
             SELECT fact_id FROM memory_v2_legacy_map WHERE legacy_fact_id = ?1
         )",
        rusqlite::params![fact.fact_id],
    );

    let repair = memory
        .dashboard_repair_v1(
            MemoryOperationContext::generated(&owner, "malformed-vector-repair", None).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(repair.missing_vectors_repaired(), 0);
    assert_eq!(
        memory
            .dashboard_memory_status_v1()
            .await
            .unwrap()
            .missing_vector_count(),
        0
    );
    assert_eq!(fact_hrr_blob(&db, fact.fact_id).await, vec![0]);
    assert!(
        memory
            .dashboard_overview_v1(10, 10)
            .await
            .unwrap()
            .memory_banks
            .is_empty(),
        "a malformed unavailable vector must not become a dashboard bank"
    );
}

#[tokio::test]
async fn compatibility_repair_scans_past_a_full_batch_of_unavailable_vectors() {
    const UNAVAILABLE_CANDIDATES: usize = 512;

    let (db, _tmp) = make_memory_store().await;
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
    let eligible = add_compatibility_fixture_fact(
        &memory,
        &owner,
        "Eligible vector after unavailable repair batch",
        MemoryCategory::Project,
    )
    .await;
    for index in 0..UNAVAILABLE_CANDIDATES {
        add_compatibility_fixture_fact(
            &memory,
            &owner,
            &format!("Unavailable vector repair candidate {index}"),
            MemoryCategory::Project,
        )
        .await;
    }
    execute_sql(
        &db,
        "UPDATE memory_v2_current_facts
         SET payload_access = 'unavailable'
         WHERE fact_id <> (
             SELECT fact_id FROM memory_v2_legacy_map WHERE legacy_fact_id = ?1
         )",
        rusqlite::params![eligible.fact_id],
    );
    execute_sql(
        &db,
        "UPDATE memory_facts
         SET hrr_vector = NULL, hrr_algebra = 'broken', hrr_dim = 0, hrr_precision = 'broken',
             updated_at = CASE WHEN fact_id = ?1 THEN 1 ELSE 2 END",
        rusqlite::params![eligible.fact_id],
    );

    assert_eq!(
        memory
            .dashboard_memory_status_v1()
            .await
            .unwrap()
            .missing_vector_count(),
        1,
        "only the eligible candidate is repairable"
    );
    let repair = memory
        .dashboard_repair_v1(
            MemoryOperationContext::generated(&owner, "full-unavailable-batch-repair", None)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(repair.missing_vectors_repaired(), 1);
    assert_eq!(
        fact_hrr_blob(&db, eligible.fact_id).await.len(),
        HolographicEncoder::SERIALIZED_F32_BYTES
    );
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*)
             FROM memory_facts AS legacy_facts
             JOIN memory_v2_legacy_map AS mappings
               ON mappings.legacy_fact_id = legacy_facts.fact_id
             JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = mappings.fact_id
              AND current_facts.owner_kind = mappings.owner_kind
              AND current_facts.project_id = mappings.project_id
             WHERE current_facts.payload_access = 'unavailable'
               AND legacy_facts.hrr_vector IS NULL",
        )
        .await,
        UNAVAILABLE_CANDIDATES as i64,
        "unavailable mirrors are neither repaired nor counted as repair work"
    );
    assert_eq!(
        memory
            .dashboard_memory_status_v1()
            .await
            .unwrap()
            .missing_vector_count(),
        0
    );
}

#[tokio::test]
async fn compatibility_v1_feedback_on_nonexistent_fact_id_fails_fast() {
    // Regression for the live-verified defect where `fact_feedback` on a
    // nonexistent fact hung to the client deadline instead of failing fast
    // like `fact_store --action get`. The compatibility write transaction
    // resolves the legacy target and rejects a miss inside its own
    // transaction — it must never block on anything beyond that lookup.
    let (db, _tmp) = make_memory_store().await;
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        memory.record_fact_feedback_v1(
            FeedbackRequest {
                fact_id: 999_999_999,
                action: FeedbackAction::Helpful,
                source: None,
                note: None,
            },
            MemoryOperationContext::generated(&owner, "nonexistent-feedback", None).unwrap(),
        ),
    )
    .await
    .expect("feedback on a nonexistent fact must not hang until a timeout");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "feedback on a nonexistent fact must fail fast: {:?}",
        started.elapsed()
    );
    assert!(result.is_err(), "feedback on a nonexistent fact must fail");
}

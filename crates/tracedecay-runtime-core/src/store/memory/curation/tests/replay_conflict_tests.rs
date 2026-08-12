use super::*;

#[tokio::test]
async fn changed_actor_under_the_same_normalization_id_conflicts_without_mutation() {
    let fixture = Fixture::new("normalize-actor-conflict").await;
    let normalized = fixture.seed("actor-conflict-subject", 10).await;
    let operation_id = provenance_id("fixture.normalize.actor-conflict");
    let first_request = normalize_request(
        &fixture.owner,
        operation_id.as_str(),
        Some(ActorId::new("actor.normalize.first").expect("first actor")),
        &normalized,
        vec!["Cache Policy".to_owned(), "canonical-tag".to_owned()],
        vec![normalized.clone()],
        0.92,
    );
    let changed_actor_request = normalize_request(
        &fixture.owner,
        operation_id.as_str(),
        Some(ActorId::new("actor.normalize.second").expect("second actor")),
        &normalized,
        vec!["Cache Policy".to_owned(), "canonical-tag".to_owned()],
        vec![normalized.clone()],
        0.92,
    );
    let store = DatabaseFactStore::new(&fixture.db);
    store
        .apply_project_memory_fact_curation(first_request.clone(), &fixture.control)
        .await
        .expect("commit first actor normalization");
    let before_events = lineage_events_for_fact(&fixture.db, &fixture.owner, &normalized).await;
    let before_receipts = operation_receipts(&fixture.db, &fixture.owner, &operation_id).await;

    assert!(matches!(
        store
            .apply_project_memory_fact_curation(changed_actor_request, &fixture.control)
            .await,
        Err(FactStoreError::OperationConflict)
    ));
    assert_eq!(
        lineage_events_for_fact(&fixture.db, &fixture.owner, &normalized).await,
        before_events
    );
    assert_eq!(
        operation_receipts(&fixture.db, &fixture.owner, &operation_id).await,
        before_receipts
    );
    assert!(
        store
            .apply_project_memory_fact_curation(first_request, &fixture.control)
            .await
            .expect("original actor request remains replayable")
            .replayed()
    );
}

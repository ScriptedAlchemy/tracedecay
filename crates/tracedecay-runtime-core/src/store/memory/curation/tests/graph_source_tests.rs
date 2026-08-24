use tracedecay_store::FactStoreError;

use crate::db::engine::params;
use crate::store::memory::graph::{
    hydrate_roots_from_canonical_source_for_test, relation_kinds_from_canonical_source_for_test,
};
use crate::store::memory::primitives::PROJECT_MEMORY_WRITE_OPERATION;

use super::{Fixture, accepting_read_control};

#[tokio::test]
async fn graph_source_rejects_an_eligible_fact_without_its_active_payload() {
    let fixture = Fixture::new("link-graph-missing-payload").await;
    let receipt = fixture.seed_commit("missing-payload", 10).await;
    let assertion_id = receipt
        .active_assertion_id()
        .expect("eligible seed has an active assertion");
    let transaction = fixture
        .db
        .begin_memory_write_transaction(PROJECT_MEMORY_WRITE_OPERATION)
        .await
        .expect("begin payload corruption transaction");
    assert_eq!(
        transaction
            .execute(
                "DELETE FROM memory_v2_assertion_payloads
                 WHERE assertion_id = ?1 AND fact_id = ?2",
                params![assertion_id.as_str(), receipt.fact_id().as_str()],
            )
            .await
            .expect("remove active payload in isolated corruption fixture"),
        1
    );
    transaction
        .commit()
        .await
        .expect("commit payload corruption");

    assert!(matches!(
        relation_kinds_from_canonical_source_for_test(
            &fixture.db,
            &fixture.owner,
            &accepting_read_control(),
        )
        .await,
        Err(FactStoreError::PayloadAccessMismatch)
    ));
}

#[tokio::test]
async fn graph_source_rejects_an_eligible_fact_without_an_active_assertion() {
    let fixture = Fixture::new("link-graph-missing-assertion").await;
    let fact = fixture.seed("missing-assertion", 10).await;
    let transaction = fixture
        .db
        .begin_memory_write_transaction(PROJECT_MEMORY_WRITE_OPERATION)
        .await
        .expect("begin assertion corruption transaction");
    assert_eq!(
        transaction
            .execute(
                "UPDATE memory_v2_current_facts SET active_assertion_id = NULL
                 WHERE fact_id = ?1",
                params![fact.as_str()],
            )
            .await
            .expect("remove active assertion in isolated corruption fixture"),
        1
    );
    transaction
        .commit()
        .await
        .expect("commit assertion corruption");

    assert!(matches!(
        relation_kinds_from_canonical_source_for_test(
            &fixture.db,
            &fixture.owner,
            &accepting_read_control(),
        )
        .await,
        Err(FactStoreError::PayloadAccessMismatch)
    ));
}

#[tokio::test]
async fn graph_hydration_rejects_a_fact_that_became_unavailable() {
    let fixture = Fixture::new("link-graph-hydration-unavailable").await;
    let fact = fixture.seed("hydration-unavailable", 10).await;
    let transaction = fixture
        .db
        .begin_memory_write_transaction(PROJECT_MEMORY_WRITE_OPERATION)
        .await
        .expect("begin hydration state transition");
    assert_eq!(
        transaction
            .execute(
                "UPDATE memory_v2_current_facts SET payload_access = 'unavailable'
                 WHERE fact_id = ?1",
                params![fact.as_str()],
            )
            .await
            .expect("make graph source fact unavailable"),
        1
    );
    transaction
        .commit()
        .await
        .expect("commit hydration state transition");

    assert!(matches!(
        hydrate_roots_from_canonical_source_for_test(
            &fixture.db,
            fixture.owner.clone(),
            std::slice::from_ref(&fact),
            &accepting_read_control(),
        )
        .await,
        Err(FactStoreError::PayloadAccessMismatch)
    ));
}

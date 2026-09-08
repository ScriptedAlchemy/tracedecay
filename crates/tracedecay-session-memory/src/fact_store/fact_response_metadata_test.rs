//! The retained-runtime branch of `DatabaseFactStore` must report the same
//! measured coverage and contradiction the direct-SQL branch reports.
//!
//! Both branches now answer through `fact_response_metadata_tx`. These tests
//! pin that shared measurement against a seeded database and, critically, pin
//! that the measured values differ from the constants the runtime branch used
//! to fabricate — `FactQueryCoverageV1::new(0, 0, observed, 0)` and
//! `FactContradictionStateV1::Unknown` — so a regression back to constants
//! cannot pass.
//!
//! The unit-test database fixture publishes a `Profile`-scoped shard, which
//! `runtime::retained_fact_runtime` reports as not fact-capable, so the branch
//! itself cannot be entered here. What is pinned is the exact function and
//! arguments the branch delegates to; end-to-end coverage of the direct branch
//! lives in `tests/storage_suite/fact_merge_hydration_test.rs`.

use super::*;

use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};
use tempfile::tempdir;
use tracedecay_domain::{
    FactCurationActionV1, FactId, FactIdentityMaterialV1, FactIdentitySourceV1,
    FactLineageEventKindV1, FactLineageEventV1, PayloadAccessState, UtcMicros,
};
use tracedecay_store::{FactContradictionStateV1, FactCurrentQuery, FactQueryCoverageV1};

fn profile_fact_id(operation: &str) -> FactId {
    FactId::derive(
        &FactIdentityMaterialV1::new(
            FactOwnerV1::Profile,
            FactIdentitySourceV1::Application {
                operation_id: ProvenanceId::new(operation).unwrap(),
            },
        )
        .unwrap(),
    )
    .unwrap()
}

/// Seeds a purged fact whose only lineage is a purge and a contradiction, so
/// the measured coverage is `hidden` and the measured contradiction is
/// `Present` — neither of which a constant could have produced.
async fn seed_purged_and_contradicted_fact(db: &Database, fact_id: &FactId, other: &FactId) {
    let owner = FactOwnerV1::Profile;
    let owner_key = OwnerKey::new(&owner).unwrap();
    let purge = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Deleted,
        },
        UtcMicros(2),
        None,
    )
    .unwrap();
    let contradiction = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::ContradictedBy {
                fact_id: other.clone(),
            },
            evidence_ids: vec![],
        },
        UtcMicros(3),
        None,
    )
    .unwrap();

    let writer = db
        .writer_connection("seed fact response metadata fixture")
        .await
        .unwrap();
    writer
        .execute_engine(
            "INSERT INTO memory_v2_facts(
                fact_id, owner_kind, project_id, owner_json, identity_json, created_at
             ) VALUES(?1, ?2, ?3, ?4, '{}', 1)",
            params![
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str(),
                owner_key.json.as_str(),
            ],
        )
        .await
        .unwrap();
    for event in [&purge, &contradiction] {
        writer
            .execute_engine(
                "INSERT INTO memory_v2_lineage_events(
                    event_id, fact_id, owner_kind, project_id,
                    event_json, occurred_at, recorded_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.event_id().as_str(),
                    fact_id.as_str(),
                    owner_key.kind,
                    owner_key.project_id.as_str(),
                    serde_json::to_string(event).unwrap(),
                    event.occurred_at().0,
                    event.occurred_at().0,
                ],
            )
            .await
            .unwrap();
    }
    writer
        .execute_engine(
            "INSERT INTO memory_v2_current_facts(
                fact_id, owner_kind, project_id, payload_access, trust_score,
                active_assertion_id, last_event_id, updated_at
             ) VALUES(?1, ?2, ?3, 'deleted', 0.5, NULL, ?4, 3)",
            params![
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str(),
                contradiction.event_id().as_str(),
            ],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn runtime_branch_metadata_is_the_direct_branch_measurement() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("fact-response-metadata.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "fact response metadata authority test").unwrap();
    let (db, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .unwrap();
    let owner = FactOwnerV1::Profile;
    let fact_id = profile_fact_id("operation.response-metadata");
    let other = profile_fact_id("operation.response-metadata.other");
    seed_purged_and_contradicted_fact(&db, &fact_id, &other).await;

    let store = DatabaseFactStore::new(&db);
    let response = store
        .query_fact_current_response(FactCurrentQuery::new(owner.clone(), fact_id.clone()).unwrap())
        .await
        .unwrap();

    let snapshot = db
        .begin_memory_read_transaction(QUERY_OPERATION)
        .await
        .unwrap();
    let measured = fact_response_metadata_tx(&snapshot, &owner, &fact_id, None).await;
    let (coverage, contradiction) = finish_read_snapshot(snapshot, measured).await.unwrap();

    // The branch delegates to exactly this measurement with exactly these
    // arguments; a wrapper that drifted would show up here.
    assert_eq!(response.coverage(), &coverage);
    assert_eq!(response.contradiction(), &contradiction);

    // A purged fact reads as absent, so the old runtime-branch constants were
    // `FactQueryCoverageV1::new(0, 0, 0, 0)` and `Unknown`. The measurement is
    // neither.
    assert_eq!(response.fact(), None);
    assert_eq!(coverage, FactQueryCoverageV1::new(0, 1, 0, 0));
    assert_ne!(coverage, FactQueryCoverageV1::new(0, 0, 0, 0));
    assert_eq!(
        contradiction,
        FactContradictionStateV1::Present {
            contradicted_by: vec![other],
        }
    );
    assert_ne!(contradiction, FactContradictionStateV1::Unknown);
}

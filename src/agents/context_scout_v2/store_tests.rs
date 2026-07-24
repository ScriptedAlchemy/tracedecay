use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

async fn database() -> (TempDir, Database) {
    let temporary = tempfile::tempdir().expect("temporary project database");
    let path = temporary.path().join("graph.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "context scout durable store test").unwrap();
    let database =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .unwrap()
            .0;
    (temporary, database)
}

fn address(project_id: [u8; 16]) -> ContextScoutAddressV1 {
    ContextScoutAddressV1 {
        profile_id: [1; 16],
        provider_id: [2; 16],
        protected_session_id: [3; 32],
        thread_id: [4; 16],
        turn_id: [5; 16],
        agent_id: [6; 16],
        logical_message_id: [7; 16],
        project_id,
    }
}

fn entry(project_id: [u8; 16], generation: u64) -> ContextScoutDurableQueueEntryV1 {
    let address = address(project_id);
    ContextScoutDurableQueueEntryV1 {
        work: ContextScoutWorkV1 {
            address,
            generation,
            input_watermark: [14; 32],
        },
        route: ContextScoutRouteV1::Deterministic,
        model_outcome: ContextScoutModelRunOutcomeV1::NotRequested,
        model_receipt: None,
        envelope: ContextScoutSuggestionEnvelopeV1 {
            envelope_id: [17; 16],
            address,
            input_watermark: [14; 32],
            configuration_revision: [16; 32],
            delivery_window: ContextScoutDeliveryWindowV1::Immediate,
            candidate: ContextScoutCandidateV1 {
                dedupe_key: [18; 32],
                category: ContextScoutCategoryV1::Retrieval,
                relevance_score: 10,
                suggestion_text: "Use the saved diagnostic anchor.".to_owned(),
                evidence: vec![ContextScoutEvidenceBindingV1 {
                    anchor_id: [19; 16],
                    content_identity: [20; 32],
                    generation: ContextScoutEvidenceGenerationV1::SavedContent,
                }],
                expires_at: UtcMicros(1_000),
            },
        },
    }
}

fn lease(id: u8, expires_at: i64) -> ContextScoutLeaseV1 {
    ContextScoutLeaseV1 {
        lease_id: [id; 16],
        expires_at: UtcMicros(expires_at),
    }
}

#[tokio::test]
async fn restart_requeues_expired_claim_and_keeps_receipt_feedback_idempotent() {
    let (_temporary, database) = database().await;
    let project_id = [8; 16];
    let store =
        ProjectContextScoutDurableStoreV1::from_project_database(database.clone(), project_id)
            .expect("owned project store");
    let pending = entry(project_id, 1);

    assert_eq!(
        store.enqueue(pending.clone()).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    let claimed = match store
        .claim(pending.work.address, UtcMicros(10), lease(21, 20))
        .await
    {
        ContextScoutDurableClaimOutcomeV1::Claimed(claimed) => claimed,
        other => panic!("expected claimed entry, got {other:?}"),
    };
    assert_eq!(claimed.entry, pending);

    drop(store);
    let (restarted, startup) = ProjectContextScoutDurableStoreV1::startup_from_project_database(
        database,
        project_id,
        UtcMicros(21),
        8,
    )
    .await
    .expect("restarted project store");
    let ContextScoutDurableStartupOutcomeV1::Ready { entries, truncated } = startup else {
        panic!("startup should recover the expired claim");
    };
    assert_eq!(entries, vec![pending.clone()]);
    assert!(!truncated);

    let reclaimed = match restarted
        .claim(pending.work.address, UtcMicros(22), lease(22, 40))
        .await
    {
        ContextScoutDurableClaimOutcomeV1::Claimed(claimed) => claimed,
        other => panic!("expected reclaimed entry, got {other:?}"),
    };
    assert_eq!(
        restarted.requeue(reclaimed.clone()).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    assert_eq!(
        restarted.requeue(reclaimed).await,
        ContextScoutDurableStoreOutcomeV1::Duplicate
    );

    let receipt = ContextScoutDeliveryReceiptV1 {
        receipt_id: [23; 16],
        envelope_id: pending.envelope.envelope_id,
        delivered_at: UtcMicros(30),
        outcome: ContextScoutOutcomeV1::Displayed,
    };
    assert_eq!(
        restarted.record_delivery(&pending, &receipt).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    assert_eq!(
        restarted.record_delivery(&pending, &receipt).await,
        ContextScoutDurableStoreOutcomeV1::Duplicate
    );
    let feedback = ContextScoutFeedbackV1 {
        receipt_id: receipt.receipt_id,
        kind: ContextScoutFeedbackKindV1::ExplicitlyAccepted,
    };
    assert_eq!(
        restarted.record_feedback(&receipt, feedback).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    assert_eq!(
        restarted.record_feedback(&receipt, feedback).await,
        ContextScoutDurableStoreOutcomeV1::Duplicate
    );
}

#[tokio::test]
async fn exact_project_scope_and_durable_generation_are_enforced() {
    let (_temporary, database) = database().await;
    let project_id = [8; 16];
    let store =
        ProjectContextScoutDurableStoreV1::from_project_database(database, project_id).unwrap();

    assert_eq!(
        store.enqueue(entry([9; 16], 1)).await,
        ContextScoutDurableStoreOutcomeV1::Unavailable
    );

    let mut dirty = entry(project_id, 1);
    dirty.envelope.candidate.evidence[0].generation =
        ContextScoutEvidenceGenerationV1::DirtyOverlay;
    assert_eq!(
        store.enqueue(dirty).await,
        ContextScoutDurableStoreOutcomeV1::Unavailable
    );
    assert!(matches!(
        store.startup(UtcMicros(1), 8).await,
        ContextScoutDurableStartupOutcomeV1::Ready {
            entries,
            truncated: false
        } if entries.is_empty()
    ));
    assert_eq!(
        store
            .startup(UtcMicros(1), MAX_SCOUT_ACTIVE_ADDRESSES + 1)
            .await,
        ContextScoutDurableStartupOutcomeV1::Unavailable
    );
}

#[tokio::test]
async fn cancellation_tombstone_blocks_stale_generation_but_allows_newer_work() {
    let (_temporary, database) = database().await;
    let project_id = [8; 16];
    let store =
        ProjectContextScoutDurableStoreV1::from_project_database(database, project_id).unwrap();
    let stale = entry(project_id, 1);

    assert_eq!(
        store.enqueue(stale.clone()).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    assert_eq!(
        store.cancel_work(stale.work).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    assert_eq!(
        store.enqueue(stale).await,
        ContextScoutDurableStoreOutcomeV1::Superseded
    );

    let mut newer = entry(project_id, 2);
    newer.envelope.envelope_id = [24; 16];
    assert_eq!(
        store.enqueue(newer.clone()).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    let started = store.startup(UtcMicros(1), 1).await;
    assert_eq!(
        started,
        ContextScoutDurableStartupOutcomeV1::Ready {
            entries: vec![newer],
            truncated: false,
        }
    );
    assert!(Arc::strong_count(&store) >= 1);
}

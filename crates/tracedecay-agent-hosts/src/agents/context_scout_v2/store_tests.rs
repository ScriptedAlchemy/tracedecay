use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

async fn database() -> (TempDir, Database) {
    crate::register_test_schema_installer();
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
                evidence: super::evidence::fixture_context_scout_evidence(),
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
    let delivery_claim = match restarted
        .claim(pending.work.address, UtcMicros(23), lease(24, 40))
        .await
    {
        ContextScoutDurableClaimOutcomeV1::Claimed(claimed) => claimed,
        other => panic!("expected delivery claim, got {other:?}"),
    };

    let receipt = ContextScoutDeliveryReceiptV1 {
        receipt_id: [23; 16],
        envelope_id: pending.envelope.envelope_id,
        delivered_at: UtcMicros(30),
        outcome: ContextScoutOutcomeV1::Displayed,
    };
    assert_eq!(
        restarted.record_delivery(&delivery_claim, &receipt).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    assert_eq!(
        restarted.record_delivery(&delivery_claim, &receipt).await,
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
    assert_eq!(
        restarted
            .recent(
                pending.work.address,
                pending.envelope.configuration_revision,
                UtcMicros(31),
                8,
            )
            .await,
        ContextScoutRecentReadOutcomeV1::Ready(ContextScoutRecentStateV1 {
            configuration_revision: pending.envelope.configuration_revision,
            observed_at: UtcMicros(31),
            pending: Vec::new(),
            deliveries: vec![ContextScoutRecentDeliveryV1 {
                entry: pending.clone(),
                receipt,
                feedback: Some(feedback),
            }],
            omitted: 0,
        })
    );
    assert!(matches!(
        restarted
            .recent_for_protected_session(
                pending.work.address.protected_session_id,
                pending.envelope.configuration_revision,
                UtcMicros(31),
                8,
            )
            .await,
        ContextScoutRecentReadOutcomeV1::Ready(ContextScoutRecentStateV1 {
            deliveries,
            ..
        }) if deliveries.len() == 1
    ));
}

#[tokio::test]
async fn delivery_requires_the_current_claim_after_lease_takeover() {
    let (_temporary, database) = database().await;
    let project_id = [8; 16];
    let store =
        ProjectContextScoutDurableStoreV1::from_project_database(database, project_id).unwrap();
    let pending = entry(project_id, 1);
    assert_eq!(
        store.enqueue(pending.clone()).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    let stale = match store
        .claim(pending.work.address, UtcMicros(10), lease(41, 20))
        .await
    {
        ContextScoutDurableClaimOutcomeV1::Claimed(claimed) => claimed,
        other => panic!("expected initial claim, got {other:?}"),
    };
    assert!(matches!(
        store.startup(UtcMicros(21), 8).await,
        ContextScoutDurableStartupOutcomeV1::Ready { .. }
    ));
    let _current = match store
        .claim(pending.work.address, UtcMicros(22), lease(42, 40))
        .await
    {
        ContextScoutDurableClaimOutcomeV1::Claimed(claimed) => claimed,
        other => panic!("expected replacement claim, got {other:?}"),
    };
    let receipt = ContextScoutDeliveryReceiptV1 {
        receipt_id: [43; 16],
        envelope_id: pending.envelope.envelope_id,
        delivered_at: UtcMicros(15),
        outcome: ContextScoutOutcomeV1::Displayed,
    };

    assert_eq!(
        store.record_delivery(&stale, &receipt).await,
        ContextScoutDurableStoreOutcomeV1::Superseded
    );
}

#[tokio::test]
async fn restart_generation_snapshot_includes_a_live_claim_without_requeueing_it() {
    let (_temporary, database) = database().await;
    let project_id = [8; 16];
    let store =
        ProjectContextScoutDurableStoreV1::from_project_database(database, project_id).unwrap();
    let pending = entry(project_id, 4);
    assert_eq!(
        store.enqueue(pending.clone()).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    assert!(matches!(
        store
            .claim(pending.work.address, UtcMicros(10), lease(46, 50))
            .await,
        ContextScoutDurableClaimOutcomeV1::Claimed(_)
    ));
    assert_eq!(
        store.startup(UtcMicros(11), 8).await,
        ContextScoutDurableStartupOutcomeV1::Ready {
            entries: Vec::new(),
            truncated: false,
        }
    );
    assert_eq!(
        store.work_snapshot(UtcMicros(11), 8).await,
        ContextScoutDurableStartupOutcomeV1::Ready {
            entries: vec![pending],
            truncated: false,
        }
    );
}

#[tokio::test]
async fn older_work_generation_cannot_replace_newer_durable_entry() {
    let (_temporary, database) = database().await;
    let project_id = [8; 16];
    let store =
        ProjectContextScoutDurableStoreV1::from_project_database(database, project_id).unwrap();
    let newer = entry(project_id, 3);
    assert_eq!(
        store.enqueue(newer.clone()).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );

    let mut older = entry(project_id, 2);
    older.envelope.envelope_id = [44; 16];
    older.envelope.candidate.dedupe_key = [45; 32];
    assert_eq!(
        store.enqueue(older).await,
        ContextScoutDurableStoreOutcomeV1::Superseded
    );
    assert_eq!(
        store.startup(UtcMicros(1), 8).await,
        ContextScoutDurableStartupOutcomeV1::Ready {
            entries: vec![newer],
            truncated: false,
        }
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
    dirty.envelope.candidate.evidence.content =
        tracedecay_domain::feedback::FeedbackContentIdentityV1::EphemeralOverlay {
            session_id: tracedecay_domain::SessionId::new("session.overlay").unwrap(),
            owner_client_id: tracedecay_domain::HostInstanceId::new("client.overlay").unwrap(),
            agent_id: None,
            document_version: 1,
            overlay_digest: tracedecay_domain::ManifestDigest::new(format!(
                "sha256:{}",
                "f".repeat(64)
            ))
            .unwrap(),
        };
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
async fn recent_is_current_and_protected_session_scoped() {
    let (_temporary, database) = database().await;
    let project_id = [8; 16];
    let store =
        ProjectContextScoutDurableStoreV1::from_project_database(database, project_id).unwrap();
    let current = entry(project_id, 1);
    assert_eq!(
        store.enqueue(current.clone()).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );

    let mut sibling = entry(project_id, 1);
    sibling.work.address.protected_session_id = [31; 32];
    sibling.work.address.logical_message_id = [32; 16];
    sibling.envelope.address = sibling.work.address;
    sibling.envelope.envelope_id = [33; 16];
    sibling.envelope.candidate.dedupe_key = [34; 32];
    assert_eq!(
        store.enqueue(sibling).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );

    assert!(matches!(
        store
            .recent_for_protected_session(
                current.work.address.protected_session_id,
                current.envelope.configuration_revision,
                UtcMicros(999),
                8,
            )
            .await,
        ContextScoutRecentReadOutcomeV1::Ready(ContextScoutRecentStateV1 {
            pending,
            ..
        }) if pending == vec![current.clone()]
    ));
    assert!(matches!(
        store
            .recent(
                current.work.address,
                [99; 32],
                UtcMicros(999),
                8,
            )
            .await,
        ContextScoutRecentReadOutcomeV1::Ready(ContextScoutRecentStateV1 {
            pending,
            ..
        }) if pending.is_empty()
    ));
    assert!(matches!(
        store
            .recent(
                current.work.address,
                current.envelope.configuration_revision,
                current.envelope.candidate.expires_at,
                8,
            )
            .await,
        ContextScoutRecentReadOutcomeV1::Ready(ContextScoutRecentStateV1 {
            pending,
            ..
        }) if pending.is_empty()
    ));
}

#[tokio::test]
async fn legacy_delivery_provenance_fails_closed_until_exact_replay_migrates_it() {
    let (_temporary, database) = database().await;
    let project_id = [8; 16];
    let delivered = entry(project_id, 1);
    let receipt = ContextScoutDeliveryReceiptV1 {
        receipt_id: [23; 16],
        envelope_id: delivered.envelope.envelope_id,
        delivered_at: UtcMicros(30),
        outcome: ContextScoutOutcomeV1::Displayed,
    };
    let feedback = ContextScoutFeedbackV1 {
        receipt_id: receipt.receipt_id,
        kind: ContextScoutFeedbackKindV1::ExplicitlyAccepted,
    };
    let legacy = serde_json::json!({
        "project_id": project_id,
        "entries": [],
        "tombstones": [delivered.work],
        "receipts": [receipt],
        "feedback": [feedback],
    });
    database
        .set_metadata("agents.context-scout.durable.v1", &legacy.to_string())
        .await
        .expect("legacy Context Scout state");
    let store =
        ProjectContextScoutDurableStoreV1::from_project_database(database.clone(), project_id)
            .expect("owned project store");

    assert_eq!(
        store
            .recent(
                delivered.work.address,
                delivered.envelope.configuration_revision,
                UtcMicros(31),
                8,
            )
            .await,
        ContextScoutRecentReadOutcomeV1::Unavailable
    );
    assert_eq!(
        store
            .record_delivery(
                &ContextScoutDurableClaimV1 {
                    entry: delivered.clone(),
                    lease: lease(24, 40),
                },
                &receipt,
            )
            .await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    drop(store);

    let restarted = ProjectContextScoutDurableStoreV1::from_project_database(database, project_id)
        .expect("restarted project store");
    assert_eq!(
        restarted
            .recent(
                delivered.work.address,
                delivered.envelope.configuration_revision,
                UtcMicros(31),
                8,
            )
            .await,
        ContextScoutRecentReadOutcomeV1::Ready(ContextScoutRecentStateV1 {
            configuration_revision: delivered.envelope.configuration_revision,
            observed_at: UtcMicros(31),
            pending: Vec::new(),
            deliveries: vec![ContextScoutRecentDeliveryV1 {
                entry: delivered,
                receipt,
                feedback: Some(feedback),
            }],
            omitted: 0,
        })
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

use super::*;

use crate::store::memory::curation::apply::load_commit_events_tx;

/// Enough synthetic lineage events to force `load_commit_events_tx` past a single
/// `COMMIT_EVENT_BATCH` chunk and past the historical 999-variable `SQLite`
/// statement ceiling, so an unchunked `IN (?, ?, ...)` build would be visible here.
const BATCHED_EVENT_COUNT: usize = 1_100;

/// The batched read must hand every event back in the receipt's own order, not in
/// the `event_sequence` order `SQLite` returns rows in. Callers destructure the
/// result positionally and reject a non-increasing sequence run, so storage order
/// would silently launder a corrupt receipt.
#[tokio::test]
async fn commit_event_load_preserves_receipt_order_across_chunks() {
    let fixture = Fixture::new("commit-event-batch-order").await;
    let fact = fixture.seed("commit-event-batch-subject", 10).await;
    let key = OwnerKey::new(&fixture.owner).expect("owner key");
    let seed_event = lineage_events_for_fact(&fixture.db, &fixture.owner, &fact)
        .await
        .into_iter()
        .next()
        .expect("seeded lineage event");

    let transaction = fixture
        .db
        .begin_memory_write_transaction(PROJECT_MEMORY_WRITE_OPERATION)
        .await
        .expect("begin commit-event batch transaction");

    // `event_sequence` is AUTOINCREMENT, so insertion order is ascending storage
    // order. Only `occurred_at` varies, which is enough to derive a distinct
    // `event_id` per row.
    let mut storage_order = Vec::with_capacity(BATCHED_EVENT_COUNT);
    for offset in 0..BATCHED_EVENT_COUNT {
        let event = FactLineageEventV1::new(
            fact.clone(),
            fixture.owner.clone(),
            seed_event.kind().clone(),
            UtcMicros(1_000_000 + i64::try_from(offset).expect("offset fits i64")),
            seed_event.actor_id().cloned(),
        )
        .expect("synthetic lineage event");
        transaction
            .execute(
                "INSERT INTO memory_v2_lineage_events(
                    event_id, fact_id, owner_kind, project_id,
                    event_json, occurred_at, recorded_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.event_id().as_str(),
                    event.fact_id().as_str(),
                    key.kind,
                    key.project_id.as_str(),
                    serde_json::to_string(&event).expect("serialize synthetic event"),
                    event.occurred_at().0,
                    event.occurred_at().0,
                ],
            )
            .await
            .expect("insert synthetic lineage event");
        storage_order.push(event.event_id().clone());
    }

    let requested = storage_order
        .iter()
        .rev()
        .cloned()
        .collect::<Vec<FactEventId>>();
    let receipt = FactCommitReceipt::new(
        fact.clone(),
        fixture.owner.clone(),
        requested.clone(),
        requested.last().expect("requested tail").clone(),
        None,
    )
    .expect("commit receipt over the synthetic events");

    let loaded = load_commit_events_tx(&transaction, &key, &receipt)
        .await
        .expect("load batched commit events");
    transaction
        .rollback()
        .await
        .expect("roll back commit-event batch transaction");

    // Every requested event came back: no chunk was dropped and no oversized
    // statement was refused.
    assert_eq!(loaded.len(), BATCHED_EVENT_COUNT);
    assert_eq!(
        loaded
            .iter()
            .map(|(_, event)| event.event_id().clone())
            .collect::<Vec<_>>(),
        requested,
    );
    // Storage order is the failure mode to rule out: the sequences must descend
    // because the receipt asked for them reversed.
    let sequences = loaded
        .iter()
        .map(|(sequence, _)| *sequence)
        .collect::<Vec<_>>();
    assert!(
        sequences.windows(2).all(|pair| pair[0] > pair[1]),
        "batched read returned storage order instead of receipt order: {sequences:?}",
    );
}

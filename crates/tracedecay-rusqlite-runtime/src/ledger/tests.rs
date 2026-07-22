use rusqlite::Connection;

use super::*;
use crate::test_support::{binding, metadata, outbox, scope};

fn commit(
    transaction: &rusqlite::Transaction<'_>,
    metadata: &tracedecay_store::StoreOperationMetadataV1,
) -> tracedecay_store::StoreCommitReceiptV1 {
    match record_commit(transaction, metadata, &scope(metadata), None).unwrap() {
        LedgerDisposition::Committed(receipt) => receipt,
        disposition => panic!("expected commit, got {disposition:?}"),
    }
}

#[test]
fn ledger_records_share_the_callers_transaction_boundary() {
    let mut connection = Connection::open_in_memory().unwrap();
    let metadata = metadata("operation.rollback", "key.rollback", 'a');
    let binding = binding(&metadata);
    let entry = outbox(&metadata);
    let effect_id = entry.identity.effect_id.clone();
    let transaction = connection.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    transaction
        .execute_batch("CREATE TABLE domain_marker (value INTEGER NOT NULL)")
        .unwrap();
    transaction
        .execute("INSERT INTO domain_marker(value) VALUES (1)", [])
        .unwrap();
    assert!(matches!(
        record_commit(&transaction, &metadata, &scope(&metadata), Some(&entry)).unwrap(),
        LedgerDisposition::Committed(_)
    ));
    transaction.rollback().unwrap();

    let transaction = connection.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    let marker_exists: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'domain_marker'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(marker_exists, 0);
    assert!(current_watermark(&transaction, &binding).unwrap().is_none());
    assert!(
        outbox_entry(&transaction, &binding, &effect_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn commit_uses_one_replay_and_conflict_disposition() {
    let mut connection = Connection::open_in_memory().unwrap();
    let original = metadata("operation.original", "key.replay", 'a');
    let transaction = connection.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    let receipt = commit(&transaction, &original);
    transaction.commit().unwrap();

    let replay = metadata("operation.replay", "key.replay", 'a');
    let conflict = metadata("operation.conflict", "key.replay", 'b');
    let transaction = connection.transaction().unwrap();
    assert!(matches!(
        record_commit(&transaction, &replay, &scope(&replay), None).unwrap(),
        LedgerDisposition::Replay(found) if found == receipt
    ));
    assert!(matches!(
        record_commit(&transaction, &conflict, &scope(&conflict), None).unwrap(),
        LedgerDisposition::Conflict(found) if found == receipt
    ));
}

#[test]
fn malformed_canonical_json_fails_closed() {
    let mut connection = Connection::open_in_memory().unwrap();
    let metadata = metadata("operation.corrupt", "key.corrupt", 'a');
    let binding = binding(&metadata);
    let transaction = connection.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    commit(&transaction, &metadata);
    transaction.commit().unwrap();
    connection
        .execute(
            "UPDATE td_runtime_writer_idempotency_v1 SET original_receipt_json = '{}'",
            [],
        )
        .unwrap();

    let transaction = connection.transaction().unwrap();
    assert!(matches!(
        lookup_receipt(&transaction, &binding, &metadata.idempotency),
        Err(LedgerError::Corrupt { .. })
    ));
}

#[test]
fn runtime_effect_payloads_persist_inbox_and_ack_bookkeeping() {
    let mut source = Connection::open_in_memory().unwrap();
    let source_metadata = metadata("operation.enqueue", "key.enqueue", 'a');
    let source_binding = binding(&source_metadata);
    let entry = outbox(&source_metadata);
    let effect_id = entry.identity.effect_id.clone();
    let transaction = source.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    assert!(matches!(
        record_runtime_commit(
            &transaction,
            &source_metadata,
            &scope(&source_metadata),
            &tracedecay_store::RepositoryWritePayloadV1::EnqueueOutbox(Box::new(entry.clone())),
        )
        .unwrap(),
        LedgerDisposition::Committed(_)
    ));
    transaction.commit().unwrap();

    let mut dispatch_metadata = metadata("operation.dispatch", "key.dispatch", 'd');
    dispatch_metadata.admitted_at = serde_json::from_value(serde_json::json!(2)).unwrap();
    let mut dispatched = entry.clone();
    dispatched
        .transition(
            tracedecay_store::OutboxEffectStateV1::Dispatched,
            dispatch_metadata.admitted_at,
        )
        .unwrap();
    let transaction = source.transaction().unwrap();
    assert!(matches!(
        record_runtime_commit(
            &transaction,
            &dispatch_metadata,
            &scope(&dispatch_metadata),
            &tracedecay_store::RepositoryWritePayloadV1::EnqueueOutbox(Box::new(
                dispatched.clone(),
            )),
        )
        .unwrap(),
        LedgerDisposition::Committed(_)
    ));
    transaction.commit().unwrap();

    let mut target_metadata = metadata("operation.apply", "key.apply", 'b');
    target_metadata.shard_id = dispatched.identity.target_watermark.shard_id.clone();
    target_metadata.incarnation = dispatched.identity.target_watermark.incarnation;
    target_metadata.authority_epoch = dispatched.identity.target_watermark.authority_epoch;
    target_metadata.admitted_at = serde_json::from_value(serde_json::json!(3)).unwrap();
    let target_binding = binding(&target_metadata);
    let mut target = Connection::open_in_memory().unwrap();
    let transaction = target.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    assert!(matches!(
        record_runtime_commit(
            &transaction,
            &target_metadata,
            &scope(&target_metadata),
            &tracedecay_store::RepositoryWritePayloadV1::ApplyInbox(Box::new(dispatched.clone(),)),
        )
        .unwrap(),
        LedgerDisposition::Committed(_)
    ));
    let inbox = lookup_inbox(&transaction, &target_binding, &dispatched)
        .unwrap()
        .unwrap();
    transaction.commit().unwrap();

    let mut ack_metadata = metadata("operation.ack", "key.ack", 'c');
    ack_metadata.admitted_at = serde_json::from_value(serde_json::json!(4)).unwrap();
    let transaction = source.transaction().unwrap();
    assert!(matches!(
        record_runtime_commit(
            &transaction,
            &ack_metadata,
            &scope(&ack_metadata),
            &tracedecay_store::RepositoryWritePayloadV1::AcknowledgeOutbox(Box::new(inbox,)),
        )
        .unwrap(),
        LedgerDisposition::Committed(_)
    ));
    let acknowledged = outbox_entry(&transaction, &source_binding, &effect_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        acknowledged.state,
        tracedecay_store::OutboxEffectStateV1::Acknowledged
    );
    assert!(acknowledged.acknowledgement.is_some());
}

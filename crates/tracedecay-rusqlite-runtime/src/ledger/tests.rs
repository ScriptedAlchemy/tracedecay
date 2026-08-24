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

fn at_authority(
    operation_id: &str,
    key: &str,
    digest_byte: char,
    incarnation: u64,
    authority_epoch: u64,
) -> tracedecay_store::StoreOperationMetadataV1 {
    let mut metadata = metadata(operation_id, key, digest_byte);
    metadata.incarnation = serde_json::from_value(serde_json::json!(incarnation)).unwrap();
    metadata.authority_epoch = serde_json::from_value(serde_json::json!(authority_epoch)).unwrap();
    metadata
}

fn idempotency_rows(transaction: &rusqlite::Transaction<'_>) -> Vec<(i64, i64)> {
    transaction
        .prepare(
            "SELECT incarnation, authority_epoch FROM td_runtime_writer_idempotency_v1
             ORDER BY incarnation, authority_epoch",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Advancing the authority epoch retires the records that no admissible
/// submission can match again, and leaves every still-reachable record alone.
#[test]
fn advancing_authority_prunes_only_the_superseded_records() {
    let mut connection = Connection::open_in_memory().unwrap();
    let transaction = connection.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    for (index, key) in ["key.one", "key.two", "key.three"].iter().enumerate() {
        let metadata = at_authority(&format!("operation.epoch7.{index}"), key, 'a', 1, 7);
        commit(&transaction, &metadata);
    }
    // A second incarnation still sitting at epoch 7 must survive incarnation 1
    // advancing: supersession is per incarnation, not per shard.
    let other = at_authority("operation.other", "key.other", 'a', 2, 7);
    commit(&transaction, &other);
    assert_eq!(
        idempotency_rows(&transaction),
        vec![(1, 7), (1, 7), (1, 7), (2, 7)],
        "four records are reachable before any authority advance"
    );

    let advanced = at_authority("operation.epoch8", "key.four", 'a', 1, 8);
    commit(&transaction, &advanced);

    assert_eq!(
        idempotency_rows(&transaction),
        vec![(1, 8), (2, 7)],
        "the three superseded epoch-7 records for incarnation 1 are gone, the \
         new epoch-8 record and the untouched incarnation-2 record remain"
    );
}

/// Authority rotation may discover an arbitrarily large legacy ledger. The
/// foreground commit that discovers it must make bounded progress instead of
/// turning the whole backlog into one writer transaction.
#[test]
fn one_authority_advance_prunes_at_most_256_superseded_records() {
    let mut connection = Connection::open_in_memory().unwrap();
    let transaction = connection.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    for index in 0..258 {
        let metadata = at_authority(
            &format!("operation.bounded.{index}"),
            &format!("key.bounded.{index}"),
            'a',
            1,
            7,
        );
        commit(&transaction, &metadata);
    }

    let advanced = at_authority(
        "operation.bounded.advance",
        "key.bounded.advance",
        'b',
        1,
        8,
    );
    commit(&transaction, &advanced);

    let rows = idempotency_rows(&transaction);
    assert_eq!(
        rows.iter().filter(|(_, epoch)| *epoch == 7).count(),
        2,
        "one foreground commit may prune at most 256 legacy records"
    );
    assert_eq!(
        rows.iter().filter(|(_, epoch)| *epoch == 8).count(),
        1,
        "the authority-advancing commit remains recorded"
    );
}

/// Cleanup cannot be tied only to the transition commit: that bounded pass can
/// leave a backlog, and an upgraded database can already have its current
/// checkpoint. Later commits at the standing epoch must keep draining it.
#[test]
fn same_epoch_commits_converge_a_bounded_superseded_backlog() {
    let mut connection = Connection::open_in_memory().unwrap();
    let transaction = connection.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    for index in 0..257 {
        let metadata = at_authority(
            &format!("operation.converge.{index}"),
            &format!("key.converge.{index}"),
            'a',
            1,
            7,
        );
        commit(&transaction, &metadata);
    }

    let advanced = at_authority(
        "operation.converge.advance",
        "key.converge.advance",
        'b',
        1,
        8,
    );
    commit(&transaction, &advanced);
    assert_eq!(
        idempotency_rows(&transaction)
            .iter()
            .filter(|(_, epoch)| *epoch == 7)
            .count(),
        1,
        "the bounded transition pass leaves one legacy record"
    );

    let follow_up = at_authority(
        "operation.converge.follow-up",
        "key.converge.follow-up",
        'c',
        1,
        8,
    );
    commit(&transaction, &follow_up);

    let rows = idempotency_rows(&transaction);
    assert_eq!(
        rows.iter().filter(|(_, epoch)| *epoch == 7).count(),
        0,
        "a same-epoch commit continues draining the superseded backlog"
    );
    assert_eq!(
        rows.iter().filter(|(_, epoch)| *epoch == 8).count(),
        2,
        "both current-epoch commits remain replayable"
    );
}

/// The safety property that bounds the whole retention rule: a submission whose
/// idempotency record was pruned must still not be able to commit a second
/// time. It fails closed on stale authority instead of being admitted as new.
#[test]
fn a_pruned_record_fails_closed_rather_than_admitting_a_duplicate() {
    let mut connection = Connection::open_in_memory().unwrap();
    let transaction = connection.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    let original = at_authority("operation.original", "key.duplicate", 'a', 1, 7);
    let receipt = commit(&transaction, &original);

    let advanced = at_authority("operation.advance", "key.advance", 'a', 1, 8);
    commit(&transaction, &advanced);
    assert_eq!(
        idempotency_rows(&transaction),
        vec![(1, 8)],
        "the epoch-7 record backing the original receipt has been pruned"
    );

    // The exact same submission arrives again under its now-revoked authority.
    let duplicate = record_commit(&transaction, &original, &scope(&original), None);
    assert!(
        matches!(
            duplicate,
            Err(LedgerError::StaleAuthority {
                persisted,
                requested,
            }) if persisted == advanced.authority_epoch
                && requested == original.authority_epoch
        ),
        "a resubmission under superseded authority must be refused, got {duplicate:?}"
    );
    assert_eq!(
        idempotency_rows(&transaction),
        vec![(1, 8)],
        "the refused duplicate wrote no ledger record"
    );
    assert_eq!(
        current_watermark(&transaction, &binding(&advanced))
            .unwrap()
            .unwrap()
            .commit_sequence
            .0,
        receipt.commit_sequence.0 + 1,
        "the refused duplicate did not advance the commit sequence"
    );
}

/// Without an authority advance the ledger must retain everything: a replay has
/// to keep returning the original receipt.
#[test]
fn records_are_retained_while_their_authority_still_stands() {
    let mut connection = Connection::open_in_memory().unwrap();
    let transaction = connection.transaction().unwrap();
    initialize_schema(&transaction).unwrap();
    let original = at_authority("operation.stable", "key.stable", 'a', 1, 7);
    let receipt = commit(&transaction, &original);
    for index in 0..3 {
        let next = at_authority(&format!("operation.more.{index}"), "key.more", 'a', 1, 7);
        let _ = record_commit(&transaction, &next, &scope(&next), None).unwrap();
    }
    assert_eq!(
        idempotency_rows(&transaction).len(),
        2,
        "the distinct keys are retained at the standing epoch"
    );
    assert!(
        matches!(
            record_commit(&transaction, &original, &scope(&original), None).unwrap(),
            LedgerDisposition::Replay(found) if found == receipt
        ),
        "a replay under standing authority still returns the original receipt"
    );
}

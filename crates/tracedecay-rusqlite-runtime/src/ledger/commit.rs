use tracedecay_store::{
    InboxEffectDispositionV1, OutboxAcknowledgementReceiptV1, RepositoryWritePayloadV1,
    RuntimeTransactionScopeV1, StoreCommitReceiptV1, StoreOperationMetadataV1,
    TransactionalInboxReceiptV1, TransactionalOutboxEntryV1,
};

use super::{
    LedgerDisposition, LedgerError, checkpoint, idempotency, inbox, outbox,
    sqlite::{LedgerTransaction, Submission, encode_json},
};

enum RuntimeBookkeeping<'a> {
    None,
    Outbox(&'a TransactionalOutboxEntryV1),
    Inbox(&'a TransactionalOutboxEntryV1),
    Acknowledgement(&'a TransactionalInboxReceiptV1),
}

#[cfg(test)]
pub(crate) fn record_commit(
    transaction: &impl LedgerTransaction,
    metadata: &StoreOperationMetadataV1,
    transaction_scope: &RuntimeTransactionScopeV1,
    outbox_entry: Option<&TransactionalOutboxEntryV1>,
) -> Result<LedgerDisposition, LedgerError> {
    record_with_bookkeeping(
        transaction,
        metadata,
        transaction_scope,
        outbox_entry
            .map(RuntimeBookkeeping::Outbox)
            .unwrap_or(RuntimeBookkeeping::None),
    )
}

pub(crate) fn record_runtime_commit(
    transaction: &impl LedgerTransaction,
    metadata: &StoreOperationMetadataV1,
    transaction_scope: &RuntimeTransactionScopeV1,
    payload: &RepositoryWritePayloadV1,
) -> Result<LedgerDisposition, LedgerError> {
    let bookkeeping = match payload {
        RepositoryWritePayloadV1::EnqueueOutbox(entry) => RuntimeBookkeeping::Outbox(entry),
        RepositoryWritePayloadV1::ApplyInbox(entry) => RuntimeBookkeeping::Inbox(entry),
        RepositoryWritePayloadV1::AcknowledgeOutbox(inbox) => {
            RuntimeBookkeeping::Acknowledgement(inbox)
        }
        _ => RuntimeBookkeeping::None,
    };
    record_with_bookkeeping(transaction, metadata, transaction_scope, bookkeeping)
}

fn record_with_bookkeeping(
    transaction: &impl LedgerTransaction,
    metadata: &StoreOperationMetadataV1,
    transaction_scope: &RuntimeTransactionScopeV1,
    bookkeeping: RuntimeBookkeeping<'_>,
) -> Result<LedgerDisposition, LedgerError> {
    let submission = Submission::new(metadata, transaction_scope)?;
    match idempotency::disposition(transaction, &submission)? {
        LedgerDisposition::New => {}
        existing => return Ok(existing),
    }

    let checkpoint = checkpoint::next(transaction, &submission)?;
    let receipt = StoreCommitReceiptV1 {
        operation_id: metadata.operation_id.clone(),
        idempotency: metadata.idempotency.clone(),
        shard_id: metadata.shard_id.clone(),
        incarnation: metadata.incarnation,
        authority_epoch: metadata.authority_epoch,
        commit_sequence: checkpoint.watermark.commit_sequence,
        committed_at: metadata.admitted_at,
    };
    let receipt_json = encode_json(&receipt, "original_receipt_json")?;
    checkpoint::persist(transaction, &submission, &checkpoint, &receipt)?;
    idempotency::insert(transaction, &submission, &receipt, &receipt_json)?;
    match bookkeeping {
        RuntimeBookkeeping::None => {}
        RuntimeBookkeeping::Outbox(entry) => {
            outbox::record(transaction, &submission, &receipt, entry)?;
        }
        RuntimeBookkeeping::Inbox(entry) => {
            let inbox_receipt = inbox_receipt(&receipt, entry)?;
            inbox::insert(transaction, &submission.binding(), &inbox_receipt)?;
        }
        RuntimeBookkeeping::Acknowledgement(inbox) => {
            let acknowledgement = acknowledgement(&receipt, inbox)?;
            let entry = outbox::outbox_entry(
                transaction,
                &submission.binding(),
                &acknowledgement.identity.effect_id,
            )?
            .ok_or(LedgerError::OutboxEffectConflict)?;
            outbox::acknowledge(transaction, &submission.binding(), &entry, acknowledgement)?;
        }
    }
    Ok(LedgerDisposition::Committed(receipt))
}

fn inbox_receipt(
    commit: &StoreCommitReceiptV1,
    entry: &TransactionalOutboxEntryV1,
) -> Result<TransactionalInboxReceiptV1, LedgerError> {
    if entry.state != tracedecay_store::OutboxEffectStateV1::Dispatched
        || entry.acknowledgement.is_some()
        || entry.identity.target_watermark.shard_id != commit.shard_id
        || entry.identity.target_watermark.incarnation != commit.incarnation
        || entry.identity.target_watermark.authority_epoch != commit.authority_epoch
    {
        return Err(LedgerError::OutboxEffectConflict);
    }
    serde_json::from_value(serde_json::json!({
        "identity": entry.identity,
        "disposition": InboxEffectDispositionV1::Applied,
        "target_commit_watermark": {
            "shard_id": commit.shard_id,
            "incarnation": commit.incarnation,
            "authority_epoch": commit.authority_epoch,
            "commit_sequence": commit.commit_sequence,
        },
        "committed_at": commit.committed_at,
    }))
    .map_err(|_| LedgerError::Encoding {
        value: "inbox receipt",
    })
}

fn acknowledgement(
    commit: &StoreCommitReceiptV1,
    inbox: &TransactionalInboxReceiptV1,
) -> Result<OutboxAcknowledgementReceiptV1, LedgerError> {
    if inbox.identity.source_watermark.shard_id != commit.shard_id
        || inbox.identity.source_watermark.incarnation != commit.incarnation
        || inbox.identity.source_watermark.authority_epoch != commit.authority_epoch
    {
        return Err(LedgerError::OutboxEffectConflict);
    }
    serde_json::from_value(serde_json::json!({
        "identity": inbox.identity,
        "inbox_receipt": inbox,
        "source_commit_watermark": {
            "shard_id": commit.shard_id,
            "incarnation": commit.incarnation,
            "authority_epoch": commit.authority_epoch,
            "commit_sequence": commit.commit_sequence,
        },
        "acknowledged_at": commit.committed_at,
    }))
    .map_err(|_| LedgerError::Encoding {
        value: "outbox acknowledgement",
    })
}

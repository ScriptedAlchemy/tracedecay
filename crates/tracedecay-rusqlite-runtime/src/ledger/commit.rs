use tracedecay_store::{
    RepositoryWritePayloadV1, RuntimeTransactionScopeV1, StoreCommitReceiptV1,
    StoreOperationMetadataV1,
};

use super::{
    LedgerDisposition, LedgerError, checkpoint, idempotency, prune,
    sqlite::{LedgerTransaction, Submission},
};

#[cfg(test)]
pub(crate) fn record_commit(
    transaction: &impl LedgerTransaction,
    metadata: &StoreOperationMetadataV1,
    transaction_scope: &RuntimeTransactionScopeV1,
) -> Result<LedgerDisposition, LedgerError> {
    record(
        transaction,
        metadata,
        transaction_scope,
        ReplayAuthority::Ledger,
    )
}

/// Where a resubmission of an already-committed operation is answered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplayAuthority {
    /// The ledger retains the original receipt and replays it.
    Ledger,
    /// The repository executor settles a resubmission from its own durable
    /// state, so a ledger copy of every receipt would only duplicate it:
    /// external-source receipts are keyed by the same idempotency identity,
    /// and an observation already stored is classified as an exact
    /// duplicate. Cursor advances stay on the ledger: its replay is what tells
    /// a losing concurrent owner that the frontier was not its commit.
    Repository,
}

impl ReplayAuthority {
    fn for_payload(payload: &RepositoryWritePayloadV1) -> Self {
        match payload {
            RepositoryWritePayloadV1::ExternalSource(_)
            | RepositoryWritePayloadV1::ExternalSourceBatch(_)
            | RepositoryWritePayloadV1::ExternalSourceProjection(_)
            | RepositoryWritePayloadV1::Observation(_)
            | RepositoryWritePayloadV1::ObservationBatch(_) => Self::Repository,
            _ => Self::Ledger,
        }
    }
}

pub(crate) fn record_runtime_commit(
    transaction: &impl LedgerTransaction,
    metadata: &StoreOperationMetadataV1,
    transaction_scope: &RuntimeTransactionScopeV1,
    payload: &RepositoryWritePayloadV1,
) -> Result<LedgerDisposition, LedgerError> {
    record(
        transaction,
        metadata,
        transaction_scope,
        ReplayAuthority::for_payload(payload),
    )
}

#[tracing::instrument(name = "rusqlite.ledger.record_commit", level = "trace", skip_all)]
fn record(
    transaction: &impl LedgerTransaction,
    metadata: &StoreOperationMetadataV1,
    transaction_scope: &RuntimeTransactionScopeV1,
    replay: ReplayAuthority,
) -> Result<LedgerDisposition, LedgerError> {
    let submission = Submission::new(metadata, transaction_scope)?;
    if replay == ReplayAuthority::Ledger {
        match idempotency::disposition(transaction, &submission)? {
            LedgerDisposition::New => {}
            existing => return Ok(existing),
        }
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
    checkpoint::persist(transaction, &submission, &checkpoint, &receipt)?;
    // The persisted checkpoint is the validated authority for which of this
    // incarnation's records are now unreachable, so the prune runs after
    // persist and carries that checkpoint. Every commit makes one bounded pass
    // so a backlog converges; the record inserted below sits at the persisted
    // epoch and is never eligible.
    prune::prune_unreachable(transaction, &submission, &checkpoint)?;
    if replay == ReplayAuthority::Ledger {
        idempotency::insert(transaction, &submission, &receipt)?;
    }
    Ok(LedgerDisposition::Committed(receipt))
}

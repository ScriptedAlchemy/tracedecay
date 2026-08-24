use rusqlite::{Row, params};
use tracedecay_store::{
    CommandDigestV1, DurabilityClassV1, IdempotencyIdentityV1, RuntimeTransactionScopeV1,
    StoreCommitReceiptV1, StoreIdempotencyKeyV1, StoreOperationIdV1, StoreRuntimeBindingV1,
};

use super::{
    LedgerError,
    sqlite::{BindingKey, LedgerTransaction, Submission, decode_json, sqlite_u64},
};

const IDEMPOTENCY_TABLE: &str = "td_runtime_writer_idempotency_v1";
const SELECT_IDEMPOTENCY: &str = r#"
SELECT request_digest, original_receipt_json, transaction_scope_json,
       operation_id, durability_json, committed_at_micros
FROM td_runtime_writer_idempotency_v1
WHERE shard_json = ?1 AND incarnation = ?2 AND authority_epoch = ?3
  AND idempotency_key = ?4
"#;
const INSERT_IDEMPOTENCY: &str = r#"
INSERT OR IGNORE INTO td_runtime_writer_idempotency_v1 (
    shard_json, incarnation, authority_epoch, idempotency_key, request_digest,
    original_receipt_json, transaction_scope_json, operation_id, durability_json,
    committed_at_micros
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
"#;

#[derive(Debug)]
pub(crate) enum LedgerDisposition {
    New,
    Committed(StoreCommitReceiptV1),
    Replay(StoreCommitReceiptV1),
    Conflict(StoreCommitReceiptV1),
}

struct IdempotencyRecord {
    request_digest: CommandDigestV1,
    receipt: StoreCommitReceiptV1,
    transaction_scope: RuntimeTransactionScopeV1,
    durability: DurabilityClassV1,
}

pub(super) fn disposition(
    transaction: &impl LedgerTransaction,
    submission: &Submission<'_>,
) -> Result<LedgerDisposition, LedgerError> {
    let binding = submission.binding();
    let Some(record) = load(transaction, &binding, &submission.metadata.idempotency.key)? else {
        return Ok(LedgerDisposition::New);
    };
    if record.request_digest != submission.metadata.idempotency.command_digest {
        return Ok(LedgerDisposition::Conflict(record.receipt));
    }
    if record.durability != submission.metadata.durability {
        return Err(LedgerError::ReplayBindingMismatch {
            field: "durability",
        });
    }
    if record.transaction_scope.compatibility != submission.transaction_scope.compatibility {
        return Err(LedgerError::ReplayBindingMismatch {
            field: "transaction compatibility",
        });
    }
    record
        .receipt
        .validate_replay_for(submission.metadata)
        .map_err(|_| LedgerError::Corrupt {
            table: IDEMPOTENCY_TABLE,
            field: "original receipt replay binding",
        })?;
    Ok(LedgerDisposition::Replay(record.receipt))
}

pub(crate) fn lookup_receipt(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    idempotency: &IdempotencyIdentityV1,
) -> Result<Option<StoreCommitReceiptV1>, LedgerError> {
    Ok(load(transaction, binding, &idempotency.key)?.map(|record| record.receipt))
}

pub(super) fn insert(
    transaction: &impl LedgerTransaction,
    submission: &Submission<'_>,
    receipt: &StoreCommitReceiptV1,
    receipt_json: &str,
) -> Result<(), LedgerError> {
    let changed = transaction.execute(
        INSERT_IDEMPOTENCY,
        params![
            &submission.binding_key.shard_json,
            submission.binding_key.incarnation_sql,
            submission.authority_epoch_sql,
            submission.metadata.idempotency.key.as_str(),
            submission.metadata.idempotency.command_digest.as_str(),
            receipt_json,
            &submission.transaction_scope_json,
            submission.metadata.operation_id.as_str(),
            &submission.durability_json,
            receipt.committed_at.0,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::ConcurrentIdempotencyWrite);
    }
    Ok(())
}

fn load(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    key: &StoreIdempotencyKeyV1,
) -> Result<Option<IdempotencyRecord>, LedgerError> {
    let binding_key = BindingKey::from_binding(binding)?;
    let authority_epoch = sqlite_u64(binding.authority_epoch.get(), "authority epoch")?;
    let mut statement = transaction.prepare(SELECT_IDEMPOTENCY)?;
    let mut rows = statement.query(params![
        &binding_key.shard_json,
        binding_key.incarnation_sql,
        authority_epoch,
        key.as_str(),
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let record = decode_row(row, binding, key)?;
    if rows.next()?.is_some() {
        return Err(LedgerError::Corrupt {
            table: IDEMPOTENCY_TABLE,
            field: "duplicate idempotency identity",
        });
    }
    Ok(Some(record))
}

fn decode_row(
    row: &Row<'_>,
    binding: &StoreRuntimeBindingV1,
    key: &StoreIdempotencyKeyV1,
) -> Result<IdempotencyRecord, LedgerError> {
    let request_digest =
        CommandDigestV1::new(row.get::<_, String>(0)?).map_err(|_| LedgerError::Corrupt {
            table: IDEMPOTENCY_TABLE,
            field: "request_digest",
        })?;
    let receipt: StoreCommitReceiptV1 = decode_json(
        &row.get::<_, String>(1)?,
        IDEMPOTENCY_TABLE,
        "original_receipt_json",
    )?;
    let transaction_scope: RuntimeTransactionScopeV1 = decode_json(
        &row.get::<_, String>(2)?,
        IDEMPOTENCY_TABLE,
        "transaction_scope_json",
    )?;
    let operation_id =
        StoreOperationIdV1::new(row.get::<_, String>(3)?).map_err(|_| LedgerError::Corrupt {
            table: IDEMPOTENCY_TABLE,
            field: "operation_id",
        })?;
    let durability: DurabilityClassV1 = decode_json(
        &row.get::<_, String>(4)?,
        IDEMPOTENCY_TABLE,
        "durability_json",
    )?;
    let committed_at_micros: i64 = row.get(5)?;
    let receipt_binding = StoreRuntimeBindingV1::new(
        receipt.shard_id.clone(),
        receipt.incarnation,
        receipt.authority_epoch,
    );
    if receipt.validate().is_err()
        || receipt_binding != *binding
        || receipt.idempotency.key != *key
        || receipt.idempotency.command_digest != request_digest
        || receipt.operation_id != operation_id
        || receipt.committed_at.0 != committed_at_micros
        || transaction_scope.compatibility.binding != receipt_binding
        || transaction_scope.compatibility.durability != durability
    {
        return Err(LedgerError::Corrupt {
            table: IDEMPOTENCY_TABLE,
            field: "original receipt binding",
        });
    }
    Ok(IdempotencyRecord {
        request_digest,
        receipt,
        transaction_scope,
        durability,
    })
}

use rusqlite::{Row, params};
use tracedecay_store::{
    CommitSequenceV1, DurabilityClassV1, RuntimeTransactionScopeV1, ShardWatermarkV1,
    StoreAuthorityEpochV1, StoreCommitReceiptV1, StoreOperationIdV1, StoreRuntimeBindingV1,
};

use super::{
    LedgerError,
    sqlite::{BindingKey, LedgerTransaction, Submission, decode_json, encode_json, sqlite_u64},
};

const CHECKPOINT_TABLE: &str = "td_runtime_writer_checkpoint_v1";
const SELECT_CHECKPOINT: &str = r#"
SELECT authority_epoch, commit_sequence, watermark_json, transaction_scope_json,
       original_receipt_json, operation_id, durability_json, committed_at_micros
FROM td_runtime_writer_checkpoint_v1
WHERE shard_json = ?1 AND incarnation = ?2
"#;
const INSERT_CHECKPOINT: &str = r#"
INSERT OR IGNORE INTO td_runtime_writer_checkpoint_v1 (
    shard_json, incarnation, authority_epoch, commit_sequence, watermark_json,
    transaction_scope_json, original_receipt_json, operation_id, durability_json,
    committed_at_micros
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
"#;
const UPDATE_CHECKPOINT: &str = r#"
UPDATE td_runtime_writer_checkpoint_v1
SET authority_epoch = ?3, commit_sequence = ?4, watermark_json = ?5,
    transaction_scope_json = ?6, original_receipt_json = ?7, operation_id = ?8,
    durability_json = ?9, committed_at_micros = ?10
WHERE shard_json = ?1 AND incarnation = ?2
  AND authority_epoch = ?11 AND commit_sequence = ?12
"#;

pub(super) struct Checkpoint {
    pub(super) watermark: ShardWatermarkV1,
}

pub(super) struct NextCheckpoint {
    previous: Option<Checkpoint>,
    pub(super) watermark: ShardWatermarkV1,
}

pub(super) fn next(
    transaction: &impl LedgerTransaction,
    submission: &Submission<'_>,
) -> Result<NextCheckpoint, LedgerError> {
    let previous = load(transaction, &submission.binding_key)?;
    let sequence = match previous.as_ref() {
        None => CommitSequenceV1(1),
        Some(checkpoint) => {
            if checkpoint.watermark.authority_epoch > submission.metadata.authority_epoch {
                return Err(LedgerError::StaleAuthority {
                    persisted: checkpoint.watermark.authority_epoch,
                    requested: submission.metadata.authority_epoch,
                });
            }
            CommitSequenceV1(
                checkpoint
                    .watermark
                    .commit_sequence
                    .0
                    .checked_add(1)
                    .ok_or(LedgerError::SequenceExhausted)?,
            )
        }
    };
    Ok(NextCheckpoint {
        previous,
        watermark: ShardWatermarkV1 {
            shard_id: submission.metadata.shard_id.clone(),
            incarnation: submission.metadata.incarnation,
            authority_epoch: submission.metadata.authority_epoch,
            commit_sequence: sequence,
        },
    })
}

pub(super) fn persist(
    transaction: &impl LedgerTransaction,
    submission: &Submission<'_>,
    checkpoint: &NextCheckpoint,
    receipt: &StoreCommitReceiptV1,
) -> Result<(), LedgerError> {
    let watermark_json = encode_json(&checkpoint.watermark, "watermark_json")?;
    let receipt_json = encode_json(receipt, "original_receipt_json")?;
    let sequence = sqlite_u64(checkpoint.watermark.commit_sequence.0, "commit sequence")?;
    let changed = match checkpoint.previous.as_ref() {
        None => transaction.execute(
            INSERT_CHECKPOINT,
            params![
                &submission.binding_key.shard_json,
                submission.binding_key.incarnation_sql,
                submission.authority_epoch_sql,
                sequence,
                watermark_json,
                &submission.transaction_scope_json,
                receipt_json,
                submission.metadata.operation_id.as_str(),
                &submission.durability_json,
                receipt.committed_at.0,
            ],
        )?,
        Some(previous) => transaction.execute(
            UPDATE_CHECKPOINT,
            params![
                &submission.binding_key.shard_json,
                submission.binding_key.incarnation_sql,
                submission.authority_epoch_sql,
                sequence,
                watermark_json,
                &submission.transaction_scope_json,
                receipt_json,
                submission.metadata.operation_id.as_str(),
                &submission.durability_json,
                receipt.committed_at.0,
                sqlite_u64(previous.watermark.authority_epoch.get(), "authority epoch")?,
                sqlite_u64(previous.watermark.commit_sequence.0, "commit sequence")?,
            ],
        )?,
    };
    if changed != 1 {
        return Err(LedgerError::ConcurrentCheckpointUpdate);
    }
    Ok(())
}

fn load(
    transaction: &impl LedgerTransaction,
    binding_key: &BindingKey,
) -> Result<Option<Checkpoint>, LedgerError> {
    let mut statement = transaction.prepare(SELECT_CHECKPOINT)?;
    let mut rows = statement.query(params![
        &binding_key.shard_json,
        binding_key.incarnation_sql,
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let checkpoint = decode_row(row, binding_key)?;
    if rows.next()?.is_some() {
        return Err(LedgerError::Corrupt {
            table: CHECKPOINT_TABLE,
            field: "duplicate shard incarnation",
        });
    }
    Ok(Some(checkpoint))
}

fn decode_row(row: &Row<'_>, binding_key: &BindingKey) -> Result<Checkpoint, LedgerError> {
    let authority_epoch = decode_authority_epoch(row.get(0)?, "authority_epoch")?;
    let sequence = decode_sequence(row.get(1)?, "commit_sequence")?;
    let watermark_json: String = row.get(2)?;
    let scope_json: String = row.get(3)?;
    let receipt_json: String = row.get(4)?;
    let operation_id: String = row.get(5)?;
    let durability_json: String = row.get(6)?;
    let committed_at_micros: i64 = row.get(7)?;

    let watermark: ShardWatermarkV1 =
        decode_json(&watermark_json, CHECKPOINT_TABLE, "watermark_json")?;
    let scope: RuntimeTransactionScopeV1 =
        decode_json(&scope_json, CHECKPOINT_TABLE, "transaction_scope_json")?;
    let receipt: StoreCommitReceiptV1 =
        decode_json(&receipt_json, CHECKPOINT_TABLE, "original_receipt_json")?;
    let durability: DurabilityClassV1 =
        decode_json(&durability_json, CHECKPOINT_TABLE, "durability_json")?;
    let operation_id = StoreOperationIdV1::new(operation_id).map_err(|_| LedgerError::Corrupt {
        table: CHECKPOINT_TABLE,
        field: "operation_id",
    })?;
    let expected_binding = StoreRuntimeBindingV1::new(
        watermark.shard_id.clone(),
        watermark.incarnation,
        watermark.authority_epoch,
    );
    if encode_json(&watermark.shard_id, "shard_json")? != binding_key.shard_json
        || watermark.incarnation != binding_key.incarnation
        || watermark.authority_epoch != authority_epoch
        || watermark.commit_sequence != sequence
        || receipt.validate().is_err()
        || receipt.shard_id != watermark.shard_id
        || receipt.incarnation != watermark.incarnation
        || receipt.authority_epoch != watermark.authority_epoch
        || receipt.commit_sequence != watermark.commit_sequence
        || receipt.operation_id != operation_id
        || receipt.committed_at.0 != committed_at_micros
        || scope.compatibility.binding != expected_binding
        || scope.compatibility.durability != durability
    {
        return Err(LedgerError::Corrupt {
            table: CHECKPOINT_TABLE,
            field: "checkpoint binding",
        });
    }
    Ok(Checkpoint { watermark })
}

fn decode_authority_epoch(
    raw: i64,
    field: &'static str,
) -> Result<StoreAuthorityEpochV1, LedgerError> {
    let raw = u64::try_from(raw).map_err(|_| LedgerError::Corrupt {
        table: CHECKPOINT_TABLE,
        field,
    })?;
    StoreAuthorityEpochV1::new(raw).map_err(|_| LedgerError::Corrupt {
        table: CHECKPOINT_TABLE,
        field,
    })
}

fn decode_sequence(raw: i64, field: &'static str) -> Result<CommitSequenceV1, LedgerError> {
    let raw = u64::try_from(raw).map_err(|_| LedgerError::Corrupt {
        table: CHECKPOINT_TABLE,
        field,
    })?;
    if raw == 0 {
        return Err(LedgerError::Corrupt {
            table: CHECKPOINT_TABLE,
            field,
        });
    }
    Ok(CommitSequenceV1(raw))
}

#[cfg(test)]
pub(crate) fn current_watermark(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
) -> Result<Option<ShardWatermarkV1>, LedgerError> {
    let binding_key = BindingKey::from_binding(binding)?;
    let Some(checkpoint) = load(transaction, &binding_key)? else {
        return Ok(None);
    };
    if checkpoint.watermark.authority_epoch > binding.authority_epoch {
        return Err(LedgerError::StaleAuthority {
            persisted: checkpoint.watermark.authority_epoch,
            requested: binding.authority_epoch,
        });
    }
    Ok(
        (checkpoint.watermark.authority_epoch == binding.authority_epoch)
            .then_some(checkpoint.watermark),
    )
}

use rusqlite::{Row, params};
use tracedecay_store::{
    DurabilityClassV1, OutboxAcknowledgementReceiptV1, OutboxEffectStateV1,
    RuntimeTransactionScopeV1, StoreCommitReceiptV1, StoreEffectIdV1, StoreOperationIdV1,
    StoreRuntimeBindingV1, TransactionalOutboxEntryV1,
};

use super::sqlite::{BindingKey, decode_json, sqlite_u64};
use super::{
    LedgerError,
    sqlite::{LedgerTransaction, Submission, encode_json},
};

const OUTBOX_TABLE: &str = "td_runtime_writer_outbox_v1";
const INSERT_OUTBOX: &str = r#"
INSERT OR IGNORE INTO td_runtime_writer_outbox_v1 (
    source_shard_json, source_incarnation, source_authority_epoch, effect_id,
    ordering_key, source_sequence, state, entry_json, source_receipt_json,
    transaction_scope_json, operation_id, durability_json, updated_at_micros
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
"#;
const SELECT_OUTBOX: &str = r#"
SELECT source_incarnation, source_authority_epoch, ordering_key, source_sequence,
       state, entry_json, source_receipt_json, transaction_scope_json, operation_id,
       durability_json, updated_at_micros
FROM td_runtime_writer_outbox_v1
WHERE source_shard_json = ?1 AND effect_id = ?2
"#;
const SELECT_ORDERING_HEAD: &str = r#"
SELECT effect_id
FROM td_runtime_writer_outbox_v1
WHERE source_shard_json = ?1 AND source_incarnation = ?2
  AND source_authority_epoch = ?3 AND ordering_key = ?4
  AND state != 'acknowledged'
ORDER BY source_sequence, effect_id
LIMIT 1
"#;
const UPDATE_OUTBOX: &str = r#"
UPDATE td_runtime_writer_outbox_v1
SET state = ?5, entry_json = ?6, updated_at_micros = ?7
WHERE source_shard_json = ?1 AND source_incarnation = ?2
  AND source_authority_epoch = ?3 AND effect_id = ?4
  AND state = ?8 AND entry_json = ?9
"#;

pub(super) fn insert(
    transaction: &impl LedgerTransaction,
    submission: &Submission<'_>,
    receipt: &StoreCommitReceiptV1,
    entry: &TransactionalOutboxEntryV1,
) -> Result<(), LedgerError> {
    entry.validate().map_err(LedgerError::InvalidRequest)?;
    if entry.state != OutboxEffectStateV1::Pending || entry.acknowledgement.is_some() {
        return Err(LedgerError::OutboxEffectConflict);
    }
    if submission.metadata.durability != DurabilityClassV1::Full {
        return Err(LedgerError::OutboxRequiresFullDurability);
    }
    validate_source(entry, receipt)?;
    let changed = transaction.execute(
        INSERT_OUTBOX,
        params![
            &submission.binding_key.shard_json,
            submission.binding_key.incarnation_sql,
            submission.authority_epoch_sql,
            entry.identity.effect_id.as_str(),
            entry.identity.ordering_key.as_str(),
            sqlite_u64(
                entry.identity.source_watermark.commit_sequence.0,
                "outbox source sequence",
            )?,
            state_name(entry.state),
            encode_json(entry, "entry_json")?,
            encode_json(receipt, "original_receipt_json")?,
            &submission.transaction_scope_json,
            submission.metadata.operation_id.as_str(),
            &submission.durability_json,
            entry.updated_at.0,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::OutboxEffectConflict);
    }
    Ok(())
}

pub(super) fn record(
    transaction: &impl LedgerTransaction,
    submission: &Submission<'_>,
    receipt: &StoreCommitReceiptV1,
    desired: &TransactionalOutboxEntryV1,
) -> Result<(), LedgerError> {
    if desired.state == OutboxEffectStateV1::Pending {
        return insert(transaction, submission, receipt, desired);
    }
    if desired.state == OutboxEffectStateV1::Acknowledged {
        return Err(LedgerError::OutboxEffectConflict);
    }
    let binding = submission.binding();
    let current = outbox_entry(transaction, &binding, &desired.identity.effect_id)?
        .ok_or(LedgerError::OutboxEffectConflict)?;
    if current.identity != desired.identity
        || current.effect != desired.effect
        || current.enqueued_at != desired.enqueued_at
    {
        return Err(LedgerError::OutboxEffectConflict);
    }
    let persisted = match desired.state {
        OutboxEffectStateV1::Dispatched
            if matches!(
                current.state,
                OutboxEffectStateV1::Pending | OutboxEffectStateV1::EffectUnknown
            ) =>
        {
            prepare_dispatch(transaction, &binding, &current, desired.updated_at.0)?
        }
        OutboxEffectStateV1::EffectUnknown if current.state == OutboxEffectStateV1::Dispatched => {
            mark_effect_unknown(transaction, &binding, &current, desired.updated_at.0)?
        }
        _ => return Err(LedgerError::OutboxEffectConflict),
    };
    if persisted != *desired {
        return Err(LedgerError::OutboxEffectConflict);
    }
    Ok(())
}

fn validate_source(
    entry: &TransactionalOutboxEntryV1,
    receipt: &StoreCommitReceiptV1,
) -> Result<(), LedgerError> {
    let source = &entry.identity.source_watermark;
    let target = &entry.identity.target_watermark;
    if source.shard_id.brain_id != target.shard_id.brain_id
        || source.shard_id.profile_id != target.shard_id.profile_id
    {
        return Err(LedgerError::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::ShardMismatch {
                field: "effect authority root",
            },
        ));
    }
    if source.shard_id != receipt.shard_id
        || source.incarnation != receipt.incarnation
        || source.authority_epoch != receipt.authority_epoch
        || source.commit_sequence >= receipt.commit_sequence
    {
        return Err(LedgerError::OutboxSourceWatermarkMismatch);
    }
    Ok(())
}

pub(crate) fn outbox_entry(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    effect_id: &StoreEffectIdV1,
) -> Result<Option<TransactionalOutboxEntryV1>, LedgerError> {
    let binding_key = BindingKey::from_binding(binding)?;
    let mut statement = transaction.prepare(SELECT_OUTBOX)?;
    let mut rows = statement.query(params![&binding_key.shard_json, effect_id.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let entry = decode_row(row, binding, effect_id)?;
    if rows.next()?.is_some() {
        return Err(LedgerError::Corrupt {
            table: OUTBOX_TABLE,
            field: "duplicate effect identity",
        });
    }
    Ok(Some(entry))
}

pub(crate) fn prepare_dispatch(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    entry: &TransactionalOutboxEntryV1,
    updated_at_micros: i64,
) -> Result<TransactionalOutboxEntryV1, LedgerError> {
    ensure_ordering_head(transaction, binding, entry)?;
    transition(
        transaction,
        binding,
        entry,
        OutboxEffectStateV1::Dispatched,
        updated_at_micros,
    )
}

pub(crate) fn mark_effect_unknown(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    entry: &TransactionalOutboxEntryV1,
    updated_at_micros: i64,
) -> Result<TransactionalOutboxEntryV1, LedgerError> {
    transition(
        transaction,
        binding,
        entry,
        OutboxEffectStateV1::EffectUnknown,
        updated_at_micros,
    )
}

pub(crate) fn acknowledge(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    expected: &TransactionalOutboxEntryV1,
    acknowledgement: OutboxAcknowledgementReceiptV1,
) -> Result<TransactionalOutboxEntryV1, LedgerError> {
    let mut acknowledged = expected.clone();
    acknowledged
        .acknowledge(acknowledgement)
        .map_err(LedgerError::InvalidRequest)?;
    persist_transition(transaction, binding, expected, &acknowledged)?;
    Ok(acknowledged)
}

fn ensure_ordering_head(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    entry: &TransactionalOutboxEntryV1,
) -> Result<(), LedgerError> {
    let binding_key = BindingKey::from_binding(binding)?;
    let authority_epoch = sqlite_u64(binding.authority_epoch.get(), "authority epoch")?;
    let mut statement = transaction.prepare(SELECT_ORDERING_HEAD)?;
    let head = statement.query_row(
        params![
            &binding_key.shard_json,
            binding_key.incarnation_sql,
            authority_epoch,
            entry.identity.ordering_key.as_str(),
        ],
        |row| row.get::<_, String>(0),
    )?;
    if head != entry.identity.effect_id.as_str() {
        return Err(LedgerError::ReplayBindingMismatch {
            field: "outbox ordering key busy",
        });
    }
    Ok(())
}

fn transition(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    expected: &TransactionalOutboxEntryV1,
    next: OutboxEffectStateV1,
    updated_at_micros: i64,
) -> Result<TransactionalOutboxEntryV1, LedgerError> {
    let mut updated = expected.clone();
    let updated_at =
        serde_json::from_value(serde_json::json!(updated_at_micros)).map_err(|_| {
            LedgerError::Encoding {
                value: "outbox updated_at",
            }
        })?;
    updated
        .transition(next, updated_at)
        .map_err(LedgerError::InvalidRequest)?;
    persist_transition(transaction, binding, expected, &updated)?;
    Ok(updated)
}

fn persist_transition(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    expected: &TransactionalOutboxEntryV1,
    updated: &TransactionalOutboxEntryV1,
) -> Result<(), LedgerError> {
    let binding_key = BindingKey::from_binding(binding)?;
    let authority_epoch = sqlite_u64(binding.authority_epoch.get(), "authority epoch")?;
    let changed = transaction.execute(
        UPDATE_OUTBOX,
        params![
            &binding_key.shard_json,
            binding_key.incarnation_sql,
            authority_epoch,
            expected.identity.effect_id.as_str(),
            state_name(updated.state),
            encode_json(updated, "entry_json")?,
            updated.updated_at.0,
            state_name(expected.state),
            encode_json(expected, "entry_json")?,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::OutboxEffectConflict);
    }
    Ok(())
}

fn decode_row(
    row: &Row<'_>,
    binding: &StoreRuntimeBindingV1,
    effect_id: &StoreEffectIdV1,
) -> Result<TransactionalOutboxEntryV1, LedgerError> {
    let incarnation: i64 = row.get(0)?;
    let authority_epoch: i64 = row.get(1)?;
    let ordering_key: String = row.get(2)?;
    let source_sequence: i64 = row.get(3)?;
    let state: String = row.get(4)?;
    let entry: TransactionalOutboxEntryV1 =
        decode_json(&row.get::<_, String>(5)?, OUTBOX_TABLE, "entry_json")?;
    let receipt: StoreCommitReceiptV1 = decode_json(
        &row.get::<_, String>(6)?,
        OUTBOX_TABLE,
        "original_receipt_json",
    )?;
    let scope: RuntimeTransactionScopeV1 = decode_json(
        &row.get::<_, String>(7)?,
        OUTBOX_TABLE,
        "transaction_scope_json",
    )?;
    let operation_id =
        StoreOperationIdV1::new(row.get::<_, String>(8)?).map_err(|_| LedgerError::Corrupt {
            table: OUTBOX_TABLE,
            field: "operation_id",
        })?;
    let durability: DurabilityClassV1 =
        decode_json(&row.get::<_, String>(9)?, OUTBOX_TABLE, "durability_json")?;
    let updated_at_micros: i64 = row.get(10)?;
    if entry.validate().is_err()
        || receipt.validate().is_err()
        || entry.identity.effect_id != *effect_id
        || incarnation
            != sqlite_u64(
                entry.identity.source_watermark.incarnation.get(),
                "incarnation",
            )?
        || authority_epoch
            != sqlite_u64(
                entry.identity.source_watermark.authority_epoch.get(),
                "authority epoch",
            )?
        || ordering_key != entry.identity.ordering_key.as_str()
        || source_sequence
            != sqlite_u64(
                entry.identity.source_watermark.commit_sequence.0,
                "outbox source sequence",
            )?
        || state != state_name(entry.state)
        || updated_at_micros != entry.updated_at.0
        || receipt.shard_id != entry.identity.source_watermark.shard_id
        || receipt.incarnation != entry.identity.source_watermark.incarnation
        || receipt.authority_epoch != entry.identity.source_watermark.authority_epoch
        || receipt.operation_id != operation_id
        || scope.compatibility.binding.shard_id != receipt.shard_id
        || scope.compatibility.binding.incarnation != receipt.incarnation
        || scope.compatibility.binding.authority_epoch != receipt.authority_epoch
        || scope.compatibility.durability != durability
        || validate_source(&entry, &receipt).is_err()
    {
        return Err(LedgerError::Corrupt {
            table: OUTBOX_TABLE,
            field: "outbox binding",
        });
    }
    if entry.identity.source_watermark.shard_id != binding.shard_id {
        return Err(LedgerError::ReplayBindingMismatch {
            field: "outbox source shard",
        });
    }
    if entry.identity.source_watermark.incarnation != binding.incarnation {
        return Err(LedgerError::ReplayBindingMismatch {
            field: "outbox source incarnation",
        });
    }
    if entry.identity.source_watermark.authority_epoch != binding.authority_epoch {
        return Err(LedgerError::ReplayBindingMismatch {
            field: "outbox source authority epoch",
        });
    }
    Ok(entry)
}

fn state_name(state: OutboxEffectStateV1) -> &'static str {
    match state {
        OutboxEffectStateV1::Pending => "pending",
        OutboxEffectStateV1::Dispatched => "dispatched",
        OutboxEffectStateV1::EffectUnknown => "effect_unknown",
        OutboxEffectStateV1::Acknowledged => "acknowledged",
    }
}

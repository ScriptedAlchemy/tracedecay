#[cfg(test)]
use rusqlite::Row;
use rusqlite::params;
use serde::Serialize;
#[cfg(test)]
use serde::de::DeserializeOwned;
#[cfg(test)]
use tracedecay_store::TransactionalOutboxEntryV1;
use tracedecay_store::{
    EffectIdentityV1, InboxEffectDispositionV1, StoreRuntimeBindingV1, TransactionalInboxReceiptV1,
};

use super::{
    LedgerError,
    sqlite::{BindingKey, LedgerTransaction, sqlite_u64},
};

#[cfg(test)]
const INBOX_TABLE: &str = "td_runtime_writer_inbox_v1";
#[cfg(test)]
const SELECT_INBOX: &str = r#"
SELECT target_incarnation, target_authority_epoch, ordering_key, source_sequence,
       target_sequence, identity_json, receipt_json, committed_at_micros
FROM td_runtime_writer_inbox_v1
WHERE target_shard_json = ?1 AND effect_id = ?2
"#;
const INSERT_INBOX: &str = r#"
INSERT OR IGNORE INTO td_runtime_writer_inbox_v1 (
    target_shard_json, target_incarnation, target_authority_epoch, effect_id,
    ordering_key, source_sequence, target_sequence, identity_json, receipt_json,
    committed_at_micros
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
"#;

#[cfg(test)]
pub(crate) fn lookup(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    entry: &TransactionalOutboxEntryV1,
) -> Result<Option<TransactionalInboxReceiptV1>, LedgerError> {
    let binding_key = BindingKey::from_binding(binding)?;
    let mut statement = transaction.prepare(SELECT_INBOX)?;
    let mut rows = statement.query(params![
        &binding_key.shard_json,
        entry.identity.effect_id.as_str(),
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let receipt = decode_row(row, binding, entry)?;
    if rows.next()?.is_some() {
        return Err(LedgerError::Corrupt {
            table: INBOX_TABLE,
            field: "duplicate effect identity",
        });
    }
    Ok(Some(receipt))
}

pub(crate) fn insert(
    transaction: &impl LedgerTransaction,
    binding: &StoreRuntimeBindingV1,
    receipt: &TransactionalInboxReceiptV1,
) -> Result<(), LedgerError> {
    receipt.validate().map_err(LedgerError::InvalidRequest)?;
    validate_target(binding, &receipt.identity, receipt)?;
    if receipt.disposition != InboxEffectDispositionV1::Applied {
        return Err(LedgerError::OutboxEffectConflict);
    }
    let binding_key = BindingKey::from_binding(binding)?;
    let authority_epoch = sqlite_u64(binding.authority_epoch.get(), "authority epoch")?;
    let changed = transaction.execute(
        INSERT_INBOX,
        params![
            &binding_key.shard_json,
            binding_key.incarnation_sql,
            authority_epoch,
            receipt.identity.effect_id.as_str(),
            receipt.identity.ordering_key.as_str(),
            sqlite_u64(
                receipt.identity.source_watermark.commit_sequence.0,
                "inbox source sequence",
            )?,
            sqlite_u64(
                receipt.target_commit_watermark.commit_sequence.0,
                "inbox target sequence",
            )?,
            encode_canonical(&receipt.identity, "identity_json")?,
            encode_canonical(receipt, "receipt_json")?,
            receipt.committed_at.0,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::OutboxEffectConflict);
    }
    Ok(())
}

#[cfg(test)]
fn decode_row(
    row: &Row<'_>,
    binding: &StoreRuntimeBindingV1,
    expected: &TransactionalOutboxEntryV1,
) -> Result<TransactionalInboxReceiptV1, LedgerError> {
    let incarnation: i64 = row.get(0)?;
    let authority_epoch: i64 = row.get(1)?;
    let ordering_key: String = row.get(2)?;
    let source_sequence: i64 = row.get(3)?;
    let target_sequence: i64 = row.get(4)?;
    let identity: EffectIdentityV1 =
        decode_canonical(&row.get::<_, String>(5)?, INBOX_TABLE, "identity_json")?;
    let receipt: TransactionalInboxReceiptV1 =
        decode_canonical(&row.get::<_, String>(6)?, INBOX_TABLE, "receipt_json")?;
    let committed_at_micros: i64 = row.get(7)?;
    if identity.validate().is_err()
        || receipt.validate_for(&identity).is_err()
        || incarnation != sqlite_u64(identity.target_watermark.incarnation.get(), "incarnation")?
        || authority_epoch
            != sqlite_u64(
                identity.target_watermark.authority_epoch.get(),
                "authority epoch",
            )?
        || ordering_key != identity.ordering_key.as_str()
        || source_sequence
            != sqlite_u64(
                identity.source_watermark.commit_sequence.0,
                "inbox source sequence",
            )?
        || target_sequence
            != sqlite_u64(
                receipt.target_commit_watermark.commit_sequence.0,
                "inbox target sequence",
            )?
        || committed_at_micros != receipt.committed_at.0
    {
        return Err(LedgerError::Corrupt {
            table: INBOX_TABLE,
            field: "inbox binding",
        });
    }
    if identity != expected.identity {
        return Err(LedgerError::OutboxEffectConflict);
    }
    validate_target(binding, &identity, &receipt)?;
    Ok(receipt)
}

fn encode_canonical<T: Serialize>(value: &T, field: &'static str) -> Result<String, LedgerError> {
    serde_json::to_string(value).map_err(|_| LedgerError::Encoding { value: field })
}

#[cfg(test)]
fn decode_canonical<T: DeserializeOwned + Serialize>(
    raw: &str,
    table: &'static str,
    field: &'static str,
) -> Result<T, LedgerError> {
    let value = serde_json::from_str(raw).map_err(|_| LedgerError::Corrupt { table, field })?;
    if encode_canonical(&value, field)? != raw {
        return Err(LedgerError::Corrupt { table, field });
    }
    Ok(value)
}

fn validate_target(
    binding: &StoreRuntimeBindingV1,
    identity: &EffectIdentityV1,
    receipt: &TransactionalInboxReceiptV1,
) -> Result<(), LedgerError> {
    if identity.target_watermark.shard_id != binding.shard_id {
        return Err(LedgerError::ReplayBindingMismatch {
            field: "inbox target shard",
        });
    }
    if identity.target_watermark.incarnation != binding.incarnation {
        return Err(LedgerError::ReplayBindingMismatch {
            field: "inbox target incarnation",
        });
    }
    if identity.target_watermark.authority_epoch != binding.authority_epoch {
        return Err(LedgerError::ReplayBindingMismatch {
            field: "inbox target authority epoch",
        });
    }
    if receipt.target_commit_watermark.shard_id != binding.shard_id
        || receipt.target_commit_watermark.incarnation != binding.incarnation
        || receipt.target_commit_watermark.authority_epoch != binding.authority_epoch
    {
        return Err(LedgerError::ReplayBindingMismatch {
            field: "inbox receipt target binding",
        });
    }
    Ok(())
}

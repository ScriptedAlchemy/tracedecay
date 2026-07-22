use rusqlite::{Connection, OptionalExtension, params};
use tracedecay_store::{
    StoreRuntimeBindingV1, TransactionalInboxReceiptV1, TransactionalOutboxEntryV1,
};

use crate::ledger::LedgerError;

const INBOX_TABLE: &str = "td_runtime_writer_inbox_v1";
const SELECT_RECEIPT: &str = r#"
SELECT receipt_json
FROM td_runtime_writer_inbox_v1
WHERE target_shard_json = ?1
  AND target_incarnation = ?2
  AND target_authority_epoch = ?3
  AND effect_id = ?4
"#;

pub(crate) fn receipt(
    reader: &mut Connection,
    binding: &StoreRuntimeBindingV1,
    entry: &TransactionalOutboxEntryV1,
) -> Result<Option<TransactionalInboxReceiptV1>, LedgerError> {
    let table_exists = reader.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
         )",
        [INBOX_TABLE],
        |row| row.get::<_, bool>(0),
    )?;
    if !table_exists {
        return Ok(None);
    }
    let shard_json =
        serde_json::to_string(&binding.shard_id).map_err(|_| LedgerError::Encoding {
            value: "inbox receipt shard",
        })?;
    let incarnation =
        i64::try_from(binding.incarnation.get()).map_err(|_| LedgerError::UnsupportedInteger {
            field: "inbox receipt incarnation",
        })?;
    let authority_epoch = i64::try_from(binding.authority_epoch.get()).map_err(|_| {
        LedgerError::UnsupportedInteger {
            field: "inbox receipt authority epoch",
        }
    })?;
    let transaction = reader.transaction()?;
    let raw = transaction
        .query_row(
            SELECT_RECEIPT,
            params![
                shard_json,
                incarnation,
                authority_epoch,
                entry.identity.effect_id.as_str()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let receipt = raw
        .map(|raw| {
            let receipt: TransactionalInboxReceiptV1 =
                serde_json::from_str(&raw).map_err(|_| LedgerError::Corrupt {
                    table: "td_runtime_writer_inbox_v1",
                    field: "receipt_json",
                })?;
            if serde_json::to_string(&receipt).ok().as_deref() != Some(raw.as_str())
                || receipt.validate().is_err()
                || receipt.target_commit_watermark.shard_id != binding.shard_id
                || receipt.target_commit_watermark.incarnation != binding.incarnation
                || receipt.target_commit_watermark.authority_epoch != binding.authority_epoch
            {
                return Err(LedgerError::Corrupt {
                    table: "td_runtime_writer_inbox_v1",
                    field: "receipt binding",
                });
            }
            if receipt.identity != entry.identity {
                return Err(LedgerError::ReplayBindingMismatch {
                    field: "inbox effect identity",
                });
            }
            Ok(receipt)
        })
        .transpose()?;
    transaction.commit()?;
    Ok(receipt)
}

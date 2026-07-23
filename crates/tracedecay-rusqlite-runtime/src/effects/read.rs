//! Typed read executor over the writer ledger's transactional outbox/inbox.
//!
//! The writer ledger owns `td_runtime_writer_outbox_v1` and
//! `td_runtime_writer_inbox_v1` (see [`crate::ledger`]); the repository
//! attachment does not. This executor answers the effects family of
//! [`EffectsReadOperationV1`] by projecting those two ledger tables into the
//! store-owned DTOs, mirroring how [`crate::repository::session`]'s
//! `execute_read` projects the session tables. Every decoded row is re-validated
//! and re-bound to the requested shard before it leaves the executor.

use rusqlite::{OptionalExtension, Transaction, params};
use tracedecay_store::{
    EffectsInboxCursorV1, EffectsInboxPageQueryV1, EffectsInboxPageV1, EffectsOutboxCursorV1,
    EffectsOutboxPageQueryV1, EffectsOutboxPageV1, EffectsReadOperationV1, EffectsReadResultV1,
    StoreEffectIdV1, StoreRuntimeBindingV1, TransactionalInboxReceiptV1,
    TransactionalOutboxEntryV1,
};

const OUTBOX_TABLE: &str = "td_runtime_writer_outbox_v1";
const INBOX_TABLE: &str = "td_runtime_writer_inbox_v1";

/// Reads durable outbox/inbox effect state from the writer ledger tables.
#[derive(Clone, Copy, Debug, Default)]
pub struct EffectsLedgerReadExecutor;

impl EffectsLedgerReadExecutor {
    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &EffectsReadOperationV1,
    ) -> rusqlite::Result<EffectsReadResultV1> {
        match operation {
            EffectsReadOperationV1::OutboxEntry { binding, effect_id } => {
                read_outbox_entry(snapshot, binding, effect_id)
                    .map(|entry| EffectsReadResultV1::OutboxEntry(entry.map(Box::new)))
            }
            EffectsReadOperationV1::OutboxPage(query) => {
                read_outbox_page(snapshot, query).map(EffectsReadResultV1::OutboxPage)
            }
            EffectsReadOperationV1::InboxReceipt { binding, effect_id } => {
                read_inbox_receipt(snapshot, binding, effect_id)
                    .map(|receipt| EffectsReadResultV1::InboxReceipt(receipt.map(Box::new)))
            }
            EffectsReadOperationV1::InboxPage(query) => {
                read_inbox_page(snapshot, query).map(EffectsReadResultV1::InboxPage)
            }
        }
    }
}

fn read_outbox_entry(
    snapshot: &Transaction<'_>,
    binding: &StoreRuntimeBindingV1,
    effect_id: &StoreEffectIdV1,
) -> rusqlite::Result<Option<TransactionalOutboxEntryV1>> {
    if !table_exists(snapshot, OUTBOX_TABLE)? {
        return Ok(None);
    }
    let shard_json = shard_json(binding)?;
    let raw = snapshot
        .query_row(
            "SELECT entry_json FROM td_runtime_writer_outbox_v1
             WHERE source_shard_json = ?1 AND effect_id = ?2",
            params![shard_json, effect_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    raw.map(|raw| decode_outbox(&raw, binding, Some(effect_id)))
        .transpose()
}

fn read_outbox_page(
    snapshot: &Transaction<'_>,
    query: &EffectsOutboxPageQueryV1,
) -> rusqlite::Result<EffectsOutboxPageV1> {
    if query.limit == 0 || !table_exists(snapshot, OUTBOX_TABLE)? {
        return Ok(EffectsOutboxPageV1 {
            entries: Vec::new(),
            next: None,
        });
    }
    let shard_json = shard_json(&query.binding)?;
    let incarnation = incarnation_sql(&query.binding)?;
    let authority_epoch = authority_epoch_sql(&query.binding)?;
    let (has_after, after_sequence, after_effect_id) = match &query.after {
        Some(cursor) => (
            1_i64,
            u64_to_i64(cursor.source_sequence)?,
            cursor.effect_id.as_str().to_owned(),
        ),
        None => (0_i64, 0_i64, String::new()),
    };
    let fetch = fetch_limit(query.limit);
    let mut statement = snapshot.prepare(
        "SELECT source_sequence, effect_id, entry_json
         FROM td_runtime_writer_outbox_v1
         WHERE source_shard_json = ?1 AND source_incarnation = ?2
           AND source_authority_epoch = ?3
           AND (?4 = 0
                OR source_sequence > ?5
                OR (source_sequence = ?5 AND effect_id > ?6))
         ORDER BY source_sequence, effect_id
         LIMIT ?7",
    )?;
    let mut rows = statement.query(params![
        shard_json,
        incarnation,
        authority_epoch,
        has_after,
        after_sequence,
        after_effect_id,
        fetch,
    ])?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next()? {
        let raw = row.get::<_, String>(2)?;
        entries.push(decode_outbox(&raw, &query.binding, None)?);
    }
    let next = page_tail(&mut entries, query.limit).map(|entry| EffectsOutboxCursorV1 {
        source_sequence: entry.identity.source_watermark.commit_sequence.0,
        effect_id: entry.identity.effect_id.clone(),
    });
    Ok(EffectsOutboxPageV1 { entries, next })
}

fn read_inbox_receipt(
    snapshot: &Transaction<'_>,
    binding: &StoreRuntimeBindingV1,
    effect_id: &StoreEffectIdV1,
) -> rusqlite::Result<Option<TransactionalInboxReceiptV1>> {
    if !table_exists(snapshot, INBOX_TABLE)? {
        return Ok(None);
    }
    let shard_json = shard_json(binding)?;
    let raw = snapshot
        .query_row(
            "SELECT receipt_json FROM td_runtime_writer_inbox_v1
             WHERE target_shard_json = ?1 AND effect_id = ?2",
            params![shard_json, effect_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    raw.map(|raw| decode_inbox(&raw, binding, Some(effect_id)))
        .transpose()
}

fn read_inbox_page(
    snapshot: &Transaction<'_>,
    query: &EffectsInboxPageQueryV1,
) -> rusqlite::Result<EffectsInboxPageV1> {
    if query.limit == 0 || !table_exists(snapshot, INBOX_TABLE)? {
        return Ok(EffectsInboxPageV1 {
            receipts: Vec::new(),
            next: None,
        });
    }
    let shard_json = shard_json(&query.binding)?;
    let incarnation = incarnation_sql(&query.binding)?;
    let authority_epoch = authority_epoch_sql(&query.binding)?;
    let (has_after, after_sequence, after_effect_id) = match &query.after {
        Some(cursor) => (
            1_i64,
            u64_to_i64(cursor.target_sequence)?,
            cursor.effect_id.as_str().to_owned(),
        ),
        None => (0_i64, 0_i64, String::new()),
    };
    let fetch = fetch_limit(query.limit);
    let mut statement = snapshot.prepare(
        "SELECT target_sequence, effect_id, receipt_json
         FROM td_runtime_writer_inbox_v1
         WHERE target_shard_json = ?1 AND target_incarnation = ?2
           AND target_authority_epoch = ?3
           AND (?4 = 0
                OR target_sequence > ?5
                OR (target_sequence = ?5 AND effect_id > ?6))
         ORDER BY target_sequence, effect_id
         LIMIT ?7",
    )?;
    let mut rows = statement.query(params![
        shard_json,
        incarnation,
        authority_epoch,
        has_after,
        after_sequence,
        after_effect_id,
        fetch,
    ])?;
    let mut receipts = Vec::new();
    while let Some(row) = rows.next()? {
        let raw = row.get::<_, String>(2)?;
        receipts.push(decode_inbox(&raw, &query.binding, None)?);
    }
    let next = page_tail(&mut receipts, query.limit).map(|receipt| EffectsInboxCursorV1 {
        target_sequence: receipt.target_commit_watermark.commit_sequence.0,
        effect_id: receipt.identity.effect_id.clone(),
    });
    Ok(EffectsInboxPageV1 { receipts, next })
}

/// Decodes an outbox row, re-validating it and binding it to the requested
/// shard. `expected` pins the effect id for point lookups; page walks pass
/// `None` because the ordering already constrains the row.
fn decode_outbox(
    raw: &str,
    binding: &StoreRuntimeBindingV1,
    expected: Option<&StoreEffectIdV1>,
) -> rusqlite::Result<TransactionalOutboxEntryV1> {
    let entry: TransactionalOutboxEntryV1 = serde_json::from_str(raw).map_err(corrupt)?;
    entry.validate().map_err(corrupt)?;
    let source = &entry.identity.source_watermark;
    if source.shard_id != binding.shard_id
        || source.incarnation != binding.incarnation
        || source.authority_epoch != binding.authority_epoch
    {
        return Err(corrupt("outbox entry is bound to a different shard"));
    }
    if let Some(expected) = expected
        && entry.identity.effect_id != *expected
    {
        return Err(corrupt("outbox entry identity does not match the request"));
    }
    Ok(entry)
}

fn decode_inbox(
    raw: &str,
    binding: &StoreRuntimeBindingV1,
    expected: Option<&StoreEffectIdV1>,
) -> rusqlite::Result<TransactionalInboxReceiptV1> {
    let receipt: TransactionalInboxReceiptV1 = serde_json::from_str(raw).map_err(corrupt)?;
    receipt.validate().map_err(corrupt)?;
    let target = &receipt.target_commit_watermark;
    if target.shard_id != binding.shard_id
        || target.incarnation != binding.incarnation
        || target.authority_epoch != binding.authority_epoch
    {
        return Err(corrupt("inbox receipt is bound to a different shard"));
    }
    if let Some(expected) = expected
        && receipt.identity.effect_id != *expected
    {
        return Err(corrupt("inbox receipt identity does not match the request"));
    }
    Ok(receipt)
}

/// Truncates an over-fetched page back to `limit` and returns the last retained
/// element as the keyset cursor when more rows remain.
fn page_tail<T: Clone>(items: &mut Vec<T>, limit: u32) -> Option<T> {
    let limit = limit as usize;
    if items.len() > limit {
        items.truncate(limit);
        items.last().cloned()
    } else {
        None
    }
}

/// One extra row is fetched to distinguish "exactly a full page" from "more to
/// come" without a spurious trailing empty page.
fn fetch_limit(limit: u32) -> i64 {
    i64::from(limit).saturating_add(1)
}

fn table_exists(snapshot: &Transaction<'_>, table: &str) -> rusqlite::Result<bool> {
    snapshot.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
         )",
        params![table],
        |row| row.get::<_, bool>(0),
    )
}

fn shard_json(binding: &StoreRuntimeBindingV1) -> rusqlite::Result<String> {
    serde_json::to_string(&binding.shard_id).map_err(corrupt)
}

fn incarnation_sql(binding: &StoreRuntimeBindingV1) -> rusqlite::Result<i64> {
    u64_to_i64(binding.incarnation.get())
}

fn authority_epoch_sql(binding: &StoreRuntimeBindingV1) -> rusqlite::Result<i64> {
    u64_to_i64(binding.authority_epoch.get())
}

fn u64_to_i64(value: u64) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|_| corrupt("effect integer exceeds SQLite INTEGER"))
}

fn corrupt(error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(error.to_string())
}

//! Retention for the writer idempotency ledger.
//!
//! An idempotency record only has to outlive the window in which a duplicate
//! submission could still match it. A lookup matches on the exact
//! `(shard, incarnation, authority_epoch, idempotency_key)` tuple, so a record
//! stops being reachable as soon as no admissible submission can carry its
//! `(incarnation, authority_epoch)` pair again.
//!
//! `checkpoint::next` rejects any submission whose authority epoch is below the
//! epoch persisted for its incarnation, so once the checkpoint for an
//! incarnation stands at epoch `E`, every idempotency record for that
//! incarnation below `E` is permanently unreachable: no submission carrying it
//! can reach the ledger insert, and the writer rolls its savepoint back on the
//! resulting `StaleAuthority` error. Deleting those rows therefore cannot admit
//! a duplicate write - the only observable change is that a submission under
//! revoked authority fails closed instead of replaying its original receipt.
//!
//! Records are deliberately *not* pruned by age. Idempotency keys are derived
//! from request content, so a resubmission of the same content reproduces the
//! same key with no bound on how much later it can arrive. Only supersession by
//! authority is a provable end to the duplicate window.

use rusqlite::params;

use super::{
    LedgerError,
    checkpoint::NextCheckpoint,
    sqlite::{LedgerTransaction, Submission, sqlite_u64},
};

/// Maximum superseded records one foreground commit may remove.
///
/// The cleanup shares the user mutation's savepoint and the process's sole
/// SQLite writer transaction, so it must never scale with the size of the
/// backlog it discovers. A legacy ledger can hold hundreds of thousands of
/// superseded rows; deleting them in one statement would monopolise admission,
/// and an interruption or `SQLITE_FULL` would roll back the checkpoint advance
/// with it, so every retry would re-attempt the same delete and the new epoch
/// would never commit. One bounded batch keeps the epoch advance committable no
/// matter how much retention work remains.
pub(super) const MAX_PRUNED_ROWS_PER_COMMIT: i64 = 256;

/// Deletes at most one bounded batch of rows whose `(incarnation,
/// authority_epoch)` can no longer be carried by any admissible submission.
///
/// The candidate set is restricted to the single incarnation whose checkpoint
/// this commit just decoded, validated, and persisted, at that checkpoint's
/// validated epoch. No other incarnation's checkpoint row is consulted: reading
/// a neighbour's raw scalar `authority_epoch` would trust a value that nothing
/// has validated, so one inconsistent row - a scalar corrupted to `999` while
/// its watermark and receipt still encode `7` - would silently retire that
/// incarnation's live receipts and re-admit the duplicate writes they exist to
/// stop. A neighbour's superseded records are retired by that incarnation's own
/// commits, under its own validated checkpoint.
///
/// The candidate scan matches the table's primary-key prefix and is therefore
/// already in key order, which makes each bounded pass deterministic and lets
/// repeated commits converge on the remaining backlog.
const DELETE_SUPERSEDED: &str = r#"
DELETE FROM td_runtime_writer_idempotency_v1
WHERE (shard_json, incarnation, authority_epoch, idempotency_key) IN (
    SELECT candidate.shard_json, candidate.incarnation,
           candidate.authority_epoch, candidate.idempotency_key
    FROM td_runtime_writer_idempotency_v1 AS candidate
    WHERE candidate.shard_json = ?1
      AND candidate.incarnation = ?2
      AND candidate.authority_epoch < ?3
    ORDER BY candidate.authority_epoch, candidate.idempotency_key
    LIMIT ?4
)
"#;

/// Removes idempotency records that authority supersession has made
/// unreachable, returning how many rows were deleted.
///
/// Runs in the caller's transaction, like every other ledger operation, so the
/// deletion shares the commit boundary of the mutation that advances cleanup.
/// Every commit makes one bounded pass, so a backlog left by an earlier pass -
/// or already present in an upgraded database - converges over subsequent
/// commits instead of being tied to the single transition commit that first
/// discovered it.
#[hotpath::measure(label = "rusqlite.ledger.prune_superseded")]
pub(super) fn prune_superseded(
    transaction: &impl LedgerTransaction,
    submission: &Submission<'_>,
    checkpoint: &NextCheckpoint,
) -> Result<usize, LedgerError> {
    let persisted_epoch = sqlite_u64(
        checkpoint.watermark.authority_epoch.get(),
        "authority epoch",
    )?;
    let pruned = transaction.execute(
        DELETE_SUPERSEDED,
        params![
            &submission.binding_key.shard_json,
            submission.binding_key.incarnation_sql,
            persisted_epoch,
            MAX_PRUNED_ROWS_PER_COMMIT,
        ],
    )?;
    crate::hotpath_observe::record_ledger_pruned_rows(u64::try_from(pruned).unwrap_or(u64::MAX));
    Ok(pruned)
}

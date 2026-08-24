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
//! incarnation advances to epoch `E`, every idempotency record for that
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
use tracedecay_store::StoreShardIdV1;

use super::{
    LedgerError,
    sqlite::{LedgerTransaction, encode_json},
};

/// Maximum superseded records removed by one foreground commit.
const MAX_PRUNED_ROWS_PER_COMMIT: i64 = 256;

/// Deletes a bounded set of rows whose `(incarnation, authority_epoch)` can no
/// longer be carried by any admissible submission.
///
/// The join restricts candidates to incarnations that already have a
/// checkpoint, so an unknown authority position never authorises a delete.
/// Ordering by the table's composite primary key makes each bounded pass
/// deterministic and guarantees that repeated commits converge on the
/// remaining backlog.
const DELETE_SUPERSEDED: &str = r#"
DELETE FROM td_runtime_writer_idempotency_v1
WHERE (shard_json, incarnation, authority_epoch, idempotency_key) IN (
    SELECT candidate.shard_json, candidate.incarnation,
           candidate.authority_epoch, candidate.idempotency_key
    FROM td_runtime_writer_idempotency_v1 AS candidate
    JOIN td_runtime_writer_checkpoint_v1 AS checkpoint
      ON checkpoint.shard_json = candidate.shard_json
     AND checkpoint.incarnation = candidate.incarnation
    WHERE candidate.shard_json = ?1
      AND candidate.authority_epoch < checkpoint.authority_epoch
    ORDER BY candidate.shard_json, candidate.incarnation,
             candidate.authority_epoch, candidate.idempotency_key
    LIMIT ?2
)
"#;

/// Removes idempotency records that authority supersession has made
/// unreachable, returning how many rows were deleted.
///
/// Runs in the caller's transaction, like every other ledger operation, so the
/// deletion shares the commit boundary of the mutation that advances cleanup.
/// Every new commit performs one bounded pass so legacy or partially drained
/// backlogs converge without monopolising the writer transaction.
pub(super) fn prune_superseded(
    transaction: &impl LedgerTransaction,
    shard_id: &StoreShardIdV1,
) -> Result<usize, LedgerError> {
    let shard_json = encode_json(shard_id, "shard_json")?;
    Ok(transaction.execute(
        DELETE_SUPERSEDED,
        params![&shard_json, MAX_PRUNED_ROWS_PER_COMMIT],
    )?)
}

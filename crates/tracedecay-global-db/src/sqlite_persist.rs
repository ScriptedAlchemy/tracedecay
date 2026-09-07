//! Shared SQLite persist / compare-and-swap / receipt machine.
//!
//! Git-index, native-integration, and configuration each own their tables and
//! conflict kinds. The commit, rollback, receipt-replay, CAS row-count, and
//! insert-or-equal decisions used to be copied beside those tables and then
//! dropped the engine cause — or, on rollback failure, the original outcome.
//! One helper keeps both errors.

use std::fmt;
use std::future::Future;

use tracedecay_runtime_core::db::engine::{self, Transaction};

use crate::RegisteredGlobalDbWriteTransaction;

/// Write transaction that can commit or roll back through the engine.
pub(crate) trait PersistWriteTransaction: Sized {
    fn commit(self) -> impl Future<Output = engine::Result<()>> + Send;
    fn rollback(self) -> impl Future<Output = engine::Result<()>> + Send;
}

impl PersistWriteTransaction for Transaction {
    fn commit(self) -> impl Future<Output = engine::Result<()>> + Send {
        Transaction::commit(self)
    }

    fn rollback(self) -> impl Future<Output = engine::Result<()>> + Send {
        Transaction::rollback(self)
    }
}

impl PersistWriteTransaction for RegisteredGlobalDbWriteTransaction<'_> {
    fn commit(self) -> impl Future<Output = engine::Result<()>> + Send {
        RegisteredGlobalDbWriteTransaction::commit(self)
    }

    fn rollback(self) -> impl Future<Output = engine::Result<()>> + Send {
        RegisteredGlobalDbWriteTransaction::rollback(self)
    }
}

/// Commit a successful persist outcome, or roll back a failed one.
///
/// Rollback success returns the original error unchanged. Rollback failure
/// returns `unavailable` with both the original error and the rollback cause.
pub(crate) async fn commit_outcome<T, E, Tx>(
    transaction: Tx,
    outcome: Result<T, E>,
    unavailable: impl Fn(String) -> E,
) -> Result<T, E>
where
    Tx: PersistWriteTransaction,
    E: fmt::Display,
{
    match outcome {
        Ok(value) => match transaction.commit().await {
            Ok(()) => Ok(value),
            Err(error) => Err(unavailable(error.to_string())),
        },
        Err(source) => match transaction.rollback().await {
            Ok(()) => Err(source),
            Err(rollback) => Err(unavailable(format!(
                "{source}; rollback failed: {rollback}"
            ))),
        },
    }
}

/// Replay an already-published value when it is byte-equal, otherwise conflict.
pub(crate) fn replay_if_equal<T, E>(existing: T, incoming: &T, conflict: E) -> Result<T, E>
where
    T: PartialEq,
{
    if existing == *incoming {
        Ok(existing)
    } else {
        Err(conflict)
    }
}

/// Whether a looked-up row is missing (insert) or equal (replay).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayPresence {
    Absent,
    Equal,
}

/// Insert-if-absent / replay-if-equal decision used by preview commitments.
pub(crate) fn require_absent_or_equal<T, E>(
    existing: Option<T>,
    incoming: &T,
    conflict: E,
) -> Result<ReplayPresence, E>
where
    T: PartialEq,
{
    match existing {
        None => Ok(ReplayPresence::Absent),
        Some(stored) if stored == *incoming => Ok(ReplayPresence::Equal),
        Some(_) => Err(conflict),
    }
}

/// Compare-and-swap writes must change exactly one durable row.
pub(crate) fn require_single_cas_row<E>(updated: u64, conflict: E) -> Result<(), E> {
    if updated == 1 { Ok(()) } else { Err(conflict) }
}

/// `INSERT … ON CONFLICT DO NOTHING` must either create the fence or find it
/// still active. A retained proven-clear row is a conflict, not a replay.
pub(crate) fn require_inserted_or_active<E>(
    inserted: u64,
    already_active: bool,
    conflict: E,
) -> Result<(), E> {
    if inserted == 1 || already_active {
        Ok(())
    } else {
        Err(conflict)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ReplayPresence, replay_if_equal, require_absent_or_equal, require_inserted_or_active,
        require_single_cas_row,
    };

    #[test]
    fn receipt_replay_accepts_equal_and_rejects_conflict() {
        assert_eq!(replay_if_equal("same", &"same", "conflict"), Ok("same"));
        assert_eq!(
            replay_if_equal("left", &"right", "conflict"),
            Err("conflict")
        );
    }

    #[test]
    fn preview_insert_replays_equal_and_rejects_divergence() {
        assert_eq!(
            require_absent_or_equal(None, &"preview", "conflict"),
            Ok(ReplayPresence::Absent)
        );
        assert_eq!(
            require_absent_or_equal(Some("preview"), &"preview", "conflict"),
            Ok(ReplayPresence::Equal)
        );
        assert_eq!(
            require_absent_or_equal(Some("other"), &"preview", "conflict"),
            Err("conflict")
        );
    }

    #[test]
    fn cas_requires_exactly_one_updated_row() {
        assert_eq!(require_single_cas_row(1, "conflict"), Ok(()));
        assert_eq!(require_single_cas_row(0, "conflict"), Err("conflict"));
        assert_eq!(require_single_cas_row(2, "conflict"), Err("conflict"));
    }

    #[test]
    fn quarantine_accepts_insert_or_active_and_rejects_cleared_fence() {
        assert_eq!(require_inserted_or_active(1, false, "conflict"), Ok(()));
        assert_eq!(require_inserted_or_active(0, true, "conflict"), Ok(()));
        assert_eq!(
            require_inserted_or_active(0, false, "conflict"),
            Err("conflict")
        );
    }
}

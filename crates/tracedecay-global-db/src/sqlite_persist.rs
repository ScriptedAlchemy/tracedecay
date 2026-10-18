//! Shared SQLite persist / compare-and-swap / receipt decisions.
//!
//! Git-index, native-integration, and configuration each own their tables and
//! conflict kinds. The receipt-replay, CAS row-count, and insert-or-equal
//! decisions used to be copied beside those tables. The write itself is a
//! `RegisteredGlobalDb::transact` job that commits on `Ok` and rolls back on
//! `Err`, so none of these decisions commit or roll back.

pub(crate) use tracedecay_runtime_core::db::{
    decode_stored_json as decode_json, encode_stored_json as encode_json,
    optional_text_column as row_optional_text, text_column as row_text,
};

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
    use super::{require_inserted_or_active, require_single_cas_row};

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

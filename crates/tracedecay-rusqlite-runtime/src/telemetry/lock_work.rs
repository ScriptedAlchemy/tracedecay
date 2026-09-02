//! Bytes encoded or decoded while a writer holds the SQLite lock.

use std::cell::Cell;

use super::WriterLockWorkSnapshot;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ENCODED: Cell<u64> = const { Cell::new(0) };
    static DECODED: Cell<u64> = const { Cell::new(0) };
}

pub(crate) struct LockWorkScope;

impl LockWorkScope {
    pub(crate) fn enter() -> Self {
        ACTIVE.with(|cell| cell.set(true));
        ENCODED.with(|cell| cell.set(0));
        DECODED.with(|cell| cell.set(0));
        Self
    }

    pub(crate) fn take(&self) -> WriterLockWorkSnapshot {
        take_lock_work()
    }
}

impl Drop for LockWorkScope {
    fn drop(&mut self) {
        ACTIVE.with(|cell| cell.set(false));
        ENCODED.with(|cell| cell.set(0));
        DECODED.with(|cell| cell.set(0));
    }
}

pub(crate) fn record_encoded_bytes(bytes: u64) {
    if ACTIVE.with(Cell::get) {
        ENCODED.with(|cell| cell.set(cell.get().saturating_add(bytes)));
    }
}

pub(crate) fn record_decoded_bytes(bytes: u64) {
    if ACTIVE.with(Cell::get) {
        DECODED.with(|cell| cell.set(cell.get().saturating_add(bytes)));
    }
}

pub(crate) fn take_lock_work() -> WriterLockWorkSnapshot {
    WriterLockWorkSnapshot {
        bytes_encoded: ENCODED.with(|cell| cell.replace(0)),
        bytes_decoded: DECODED.with(|cell| cell.replace(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_work_is_ignored_outside_a_held_lock_scope() {
        record_encoded_bytes(8);
        record_decoded_bytes(16);
        assert_eq!(take_lock_work(), WriterLockWorkSnapshot::default());
    }

    #[test]
    fn lock_work_counts_only_bytes_observed_while_the_scope_is_active() {
        let scope = LockWorkScope::enter();
        record_encoded_bytes(4);
        record_decoded_bytes(12);
        assert_eq!(
            scope.take(),
            WriterLockWorkSnapshot {
                bytes_encoded: 4,
                bytes_decoded: 12,
            }
        );
        drop(scope);
        record_encoded_bytes(99);
        assert_eq!(take_lock_work(), WriterLockWorkSnapshot::default());
    }
}

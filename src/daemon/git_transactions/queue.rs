//! Daemon-local per-repository mutation serialization.
//!
//! This guard only serializes `TraceDecay` callers. External Git processes are
//! still detected through snapshot compare-and-swap and native index locks.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::Duration;

use thiserror::Error;
use tracedecay_domain::{RepositoryId, UtcMicros};

#[derive(Debug, Error)]
pub(crate) enum RepositoryMutationQueueError {
    #[error("repository mutation queue is unavailable")]
    Unavailable,
    #[error("repository mutation queue is saturated")]
    Saturated,
}

pub(crate) struct RepositoryMutationQueue {
    gates: Mutex<BTreeMap<RepositoryId, Arc<Mutex<()>>>>,
    pending: AtomicUsize,
    capacity: usize,
}

const MAX_PENDING_REPOSITORY_MUTATIONS: usize = 64;

impl Default for RepositoryMutationQueue {
    fn default() -> Self {
        Self {
            gates: Mutex::new(BTreeMap::new()),
            pending: AtomicUsize::new(0),
            capacity: MAX_PENDING_REPOSITORY_MUTATIONS,
        }
    }
}

struct MutationAdmission<'a> {
    pending: &'a AtomicUsize,
}

impl Drop for MutationAdmission<'_> {
    fn drop(&mut self) {
        self.pending.fetch_sub(1, Ordering::Release);
    }
}

impl RepositoryMutationQueue {
    #[cfg(test)]
    pub(crate) fn with_capacity_for_test(capacity: usize) -> Self {
        Self {
            gates: Mutex::new(BTreeMap::new()),
            pending: AtomicUsize::new(0),
            capacity,
        }
    }

    pub(crate) fn with_repository<T>(
        &self,
        repository_id: &RepositoryId,
        operation: impl FnOnce() -> T,
    ) -> Result<T, RepositoryMutationQueueError> {
        self.with_repository_cancellable(repository_id, || None, |_| operation())
    }

    /// Waits for the exact repository permit while polling cancellation.
    ///
    /// A `Some(cancelled_at)` callback runs without the repository permit and
    /// therefore must only publish a proven-no-change outcome. This lets
    /// callers durably record cancellation without waiting behind unrelated
    /// native work.
    pub(crate) fn with_repository_cancellable<T>(
        &self,
        repository_id: &RepositoryId,
        cancellation_requested: impl Fn() -> Option<UtcMicros>,
        operation: impl FnOnce(Option<UtcMicros>) -> T,
    ) -> Result<T, RepositoryMutationQueueError> {
        self.pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                (pending < self.capacity).then_some(pending + 1)
            })
            .map_err(|_| RepositoryMutationQueueError::Saturated)?;
        let _admission = MutationAdmission {
            pending: &self.pending,
        };
        let gate = {
            let mut gates = self
                .gates
                .lock()
                .map_err(|_| RepositoryMutationQueueError::Unavailable)?;
            Arc::clone(
                gates
                    .entry(repository_id.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        if let Some(cancelled_at) = cancellation_requested() {
            self.release_idle_gate(repository_id, &gate)?;
            return Ok(operation(Some(cancelled_at)));
        }
        let _guard = loop {
            match gate.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(_)) => {
                    return Err(RepositoryMutationQueueError::Unavailable);
                }
                Err(TryLockError::WouldBlock) => {
                    if let Some(cancelled_at) = cancellation_requested() {
                        self.release_idle_gate(repository_id, &gate)?;
                        return Ok(operation(Some(cancelled_at)));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        };
        if let Some(cancelled_at) = cancellation_requested() {
            drop(_guard);
            self.release_idle_gate(repository_id, &gate)?;
            return Ok(operation(Some(cancelled_at)));
        }
        let result = operation(None);
        drop(_guard);
        self.release_idle_gate(repository_id, &gate)?;
        Ok(result)
    }

    fn release_idle_gate(
        &self,
        repository_id: &RepositoryId,
        gate: &Arc<Mutex<()>>,
    ) -> Result<(), RepositoryMutationQueueError> {
        let mut gates = self
            .gates
            .lock()
            .map_err(|_| RepositoryMutationQueueError::Unavailable)?;
        if gates
            .get(repository_id)
            .is_some_and(|current| Arc::ptr_eq(current, gate) && Arc::strong_count(current) == 2)
        {
            gates.remove(repository_id);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn retained_gate_count_for_test(
        &self,
    ) -> Result<usize, RepositoryMutationQueueError> {
        self.gates
            .lock()
            .map(|gates| gates.len())
            .map_err(|_| RepositoryMutationQueueError::Unavailable)
    }
}

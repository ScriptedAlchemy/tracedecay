//! Daemon-local per-repository mutation serialization.
//!
//! This guard only serializes `TraceDecay` callers. External Git processes are
//! still detected through snapshot compare-and-swap and native index locks.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::Semaphore;
use tracedecay_domain::{RepositoryId, UtcMicros};
use tracedecay_runtime_core::native_runtime::{self, TimeoutError};

/// How often a queued mutation re-checks cancellation while it waits for the
/// repository permit.
const CANCELLATION_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Error)]
pub enum RepositoryMutationQueueError {
    #[error("repository mutation queue is unavailable")]
    Unavailable,
    #[error("repository mutation queue is saturated")]
    Saturated,
}

pub struct RepositoryMutationQueue {
    gates: Mutex<BTreeMap<RepositoryId, Arc<Semaphore>>>,
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

/// Holds one pending-capacity slot and one repository gate reference, and
/// returns both when the mutation ends, however it ends.
struct MutationAdmission<'a> {
    queue: &'a RepositoryMutationQueue,
    repository_id: &'a RepositoryId,
    gate: Arc<Semaphore>,
}

impl Drop for MutationAdmission<'_> {
    fn drop(&mut self) {
        if let Ok(mut gates) = self.queue.gates.lock()
            && gates.get(self.repository_id).is_some_and(|current| {
                Arc::ptr_eq(current, &self.gate) && Arc::strong_count(current) == 2
            })
        {
            gates.remove(self.repository_id);
        }
        self.queue.pending.fetch_sub(1, Ordering::Release);
    }
}

impl RepositoryMutationQueue {
    #[cfg(test)]
    pub fn with_capacity_for_test(capacity: usize) -> Self {
        Self {
            gates: Mutex::new(BTreeMap::new()),
            pending: AtomicUsize::new(0),
            capacity,
        }
    }

    pub async fn with_repository<T, F>(
        &self,
        repository_id: &RepositoryId,
        operation: impl FnOnce() -> F,
    ) -> Result<T, RepositoryMutationQueueError>
    where
        F: Future<Output = T>,
    {
        self.with_repository_cancellable(repository_id, || None, |_| operation())
            .await
    }

    /// Waits for the exact repository permit while polling cancellation.
    ///
    /// A `Some(cancelled_at)` callback runs without the repository permit and
    /// therefore must only publish a proven-no-change outcome. This lets
    /// callers durably record cancellation without waiting behind unrelated
    /// native work.
    #[tracing::instrument(name = "daemon.git.tx.queue", level = "trace", skip_all)]
    pub async fn with_repository_cancellable<T, F>(
        &self,
        repository_id: &RepositoryId,
        cancellation_requested: impl Fn() -> Option<UtcMicros>,
        operation: impl FnOnce(Option<UtcMicros>) -> F,
    ) -> Result<T, RepositoryMutationQueueError>
    where
        F: Future<Output = T>,
    {
        self.pending
            .try_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                (pending < self.capacity).then_some(pending + 1)
            })
            .map_err(|_| RepositoryMutationQueueError::Saturated)?;
        let gate = match self.gates.lock() {
            Ok(mut gates) => Arc::clone(
                gates
                    .entry(repository_id.clone())
                    .or_insert_with(|| Arc::new(Semaphore::new(1))),
            ),
            Err(_) => {
                self.pending.fetch_sub(1, Ordering::Release);
                return Err(RepositoryMutationQueueError::Unavailable);
            }
        };
        let admission = MutationAdmission {
            queue: self,
            repository_id,
            gate,
        };
        let permit = loop {
            if let Some(cancelled_at) = cancellation_requested() {
                return Ok(operation(Some(cancelled_at)).await);
            }
            match native_runtime::timeout(CANCELLATION_POLL, admission.gate.acquire()).await {
                Ok(Ok(permit)) => break permit,
                Err(TimeoutError::Elapsed) => {}
                Ok(Err(_)) | Err(TimeoutError::TimerUnavailable(_)) => {
                    return Err(RepositoryMutationQueueError::Unavailable);
                }
            }
        };
        if let Some(cancelled_at) = cancellation_requested() {
            drop(permit);
            return Ok(operation(Some(cancelled_at)).await);
        }
        let result = operation(None).await;
        drop(permit);
        Ok(result)
    }

    #[cfg(test)]
    pub fn retained_gate_count_for_test(&self) -> Result<usize, RepositoryMutationQueueError> {
        self.gates
            .lock()
            .map(|gates| gates.len())
            .map_err(|_| RepositoryMutationQueueError::Unavailable)
    }
}

//! Daemon-local per-repository mutation serialization.
//!
//! This guard only serializes `TraceDecay` callers. External Git processes are
//! still detected through snapshot compare-and-swap and native index locks.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tracedecay_domain::RepositoryId;

#[derive(Debug, Error)]
pub(crate) enum RepositoryMutationQueueError {
    #[error("repository mutation queue is unavailable")]
    Unavailable,
}

#[derive(Default)]
pub(crate) struct RepositoryMutationQueue {
    gates: Mutex<BTreeMap<RepositoryId, Arc<Mutex<()>>>>,
}

impl RepositoryMutationQueue {
    pub(crate) fn with_repository<T>(
        &self,
        repository_id: &RepositoryId,
        operation: impl FnOnce() -> T,
    ) -> Result<T, RepositoryMutationQueueError> {
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
        let _guard = gate
            .lock()
            .map_err(|_| RepositoryMutationQueueError::Unavailable)?;
        Ok(operation())
    }
}

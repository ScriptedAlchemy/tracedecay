use std::{
    error::Error,
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tracedecay_store::{
    ConsistencyModeV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeRequestProbeV1,
    SnapshotLeaseIdV1, SnapshotLeaseV1, StorageRuntimeContractErrorV1, UnavailableReasonV1,
};

use crate::read_consistency::{RetainedSnapshotRegistry, RetainedSnapshotState};

use super::{ReaderAcquireError, ReaderLease, ReaderPool, ReaderQueryExecutor};

struct RetainedEntry<E: ReaderQueryExecutor> {
    lease: SnapshotLeaseV1,
    reader: Mutex<ReaderLease<E>>,
}

struct RegistryInner<E: ReaderQueryExecutor> {
    pool: ReaderPool<E>,
    entries: Mutex<Vec<Arc<RetainedEntry<E>>>>,
}

/// SQLite-backed retained-snapshot authority.
///
/// Every entry owns a pinned transaction on a worker leased from the existing
/// reader pool. The registry never opens the database itself, so snapshot
/// retention cannot create a second, unaccounted connection authority.
pub struct SqliteRetainedSnapshotRegistry<E: ReaderQueryExecutor> {
    inner: Arc<RegistryInner<E>>,
}

impl<E: ReaderQueryExecutor> Clone for SqliteRetainedSnapshotRegistry<E> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<E: ReaderQueryExecutor> SqliteRetainedSnapshotRegistry<E> {
    pub fn new(pool: ReaderPool<E>) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                pool,
                entries: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn retain(
        &self,
        lease: SnapshotLeaseV1,
        request: &RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
        max_wait: Duration,
    ) -> Result<(), RetainedSnapshotError> {
        lease
            .validate()
            .map_err(RetainedSnapshotError::InvalidLease)?;
        request
            .validate()
            .map_err(RetainedSnapshotError::InvalidLease)?;
        if request.binding() != self.inner.pool.binding()
            || lease.watermark.shard_id != request.binding().shard_id
            || lease.watermark.incarnation != request.binding().incarnation
            || lease.watermark.authority_epoch != request.binding().authority_epoch
        {
            return Err(RetainedSnapshotError::BindingMismatch);
        }
        self.reclaim_expired();
        if self.entry(&lease.lease_id).is_some() {
            return Err(RetainedSnapshotError::LeaseConflict);
        }

        let mut reader = self
            .inner
            .pool
            .acquire(request, probe, max_wait)
            .map_err(RetainedSnapshotError::Acquire)?;
        reader
            .begin_pinned_snapshot(probe)
            .map_err(RetainedSnapshotError::Acquire)?;
        let expired = {
            let mut entries = self
                .inner
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let expired = take_expired_entries(&mut entries);
            if entries
                .iter()
                .any(|entry| entry.lease.lease_id == lease.lease_id)
            {
                drop(entries);
                drop(expired);
                return Err(RetainedSnapshotError::LeaseConflict);
            }
            entries.push(Arc::new(RetainedEntry {
                lease,
                reader: Mutex::new(reader),
            }));
            expired
        };
        drop(expired);
        Ok(())
    }

    pub fn release(&self, lease: &SnapshotLeaseV1) -> bool {
        let removed = {
            let mut entries = self
                .inner
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            entries
                .iter()
                .position(|entry| &entry.lease == lease)
                .map(|index| entries.remove(index))
        };
        removed.is_some()
    }

    pub(crate) fn execute_exact(
        &self,
        lease_id: &SnapshotLeaseIdV1,
        request: RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<RetainedExecution, ReaderAcquireError> {
        let Some(entry) = self.entry(lease_id) else {
            return Ok(RetainedExecution::Unavailable(
                UnavailableReasonV1::SnapshotNotRetained,
            ));
        };
        if is_expired(&entry.lease) {
            self.remove_entry(&entry);
            return Ok(RetainedExecution::Unavailable(
                UnavailableReasonV1::SnapshotExpired,
            ));
        }
        let requested = match request.consistency() {
            ConsistencyModeV1::ExactSnapshot { lease } => lease,
            _ => {
                return Ok(RetainedExecution::Unavailable(
                    UnavailableReasonV1::SnapshotNotRetained,
                ));
            }
        };
        if requested.as_ref() != &entry.lease {
            return Ok(RetainedExecution::Unavailable(
                UnavailableReasonV1::SnapshotNotRetained,
            ));
        }
        let mut reader = entry
            .reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if is_expired(&entry.lease) {
            drop(reader);
            self.remove_entry(&entry);
            return Ok(RetainedExecution::Unavailable(
                UnavailableReasonV1::SnapshotExpired,
            ));
        }
        let outcome = reader.execute_active_raw(request, probe)?;
        Ok(RetainedExecution::Outcome(Box::new(outcome)))
    }

    fn entry(&self, lease_id: &SnapshotLeaseIdV1) -> Option<Arc<RetainedEntry<E>>> {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|entry| &entry.lease.lease_id == lease_id)
            .cloned()
    }

    fn reclaim_expired(&self) {
        let expired = {
            let mut entries = self
                .inner
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            take_expired_entries(&mut entries)
        };
        drop(expired);
    }

    fn remove_entry(&self, target: &Arc<RetainedEntry<E>>) -> bool {
        let removed = {
            let mut entries = self
                .inner
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            entries
                .iter()
                .position(|entry| Arc::ptr_eq(entry, target))
                .map(|index| entries.remove(index))
        };
        removed.is_some()
    }
}

impl<E: ReaderQueryExecutor> RetainedSnapshotRegistry for SqliteRetainedSnapshotRegistry<E> {
    fn lookup(&self, lease_id: &SnapshotLeaseIdV1) -> RetainedSnapshotState {
        let Some(entry) = self.entry(lease_id) else {
            return RetainedSnapshotState::NotRetained;
        };
        if is_expired(&entry.lease) {
            self.remove_entry(&entry);
            RetainedSnapshotState::Expired
        } else {
            RetainedSnapshotState::Retained(Box::new(entry.lease.clone()))
        }
    }
}

pub(crate) enum RetainedExecution {
    Outcome(Box<RuntimeReadOutcomeV1>),
    Unavailable(UnavailableReasonV1),
}

#[derive(Debug)]
pub enum RetainedSnapshotError {
    InvalidLease(StorageRuntimeContractErrorV1),
    BindingMismatch,
    LeaseConflict,
    Acquire(ReaderAcquireError),
}

impl fmt::Display for RetainedSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLease(error) => write!(formatter, "invalid retained snapshot: {error}"),
            Self::BindingMismatch => {
                formatter.write_str("retained snapshot does not bind to this reader pool")
            }
            Self::LeaseConflict => formatter.write_str("snapshot lease id is already retained"),
            Self::Acquire(error) => {
                write!(formatter, "retained snapshot acquisition failed: {error}")
            }
        }
    }
}

impl Error for RetainedSnapshotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidLease(error) => Some(error),
            Self::Acquire(error) => Some(error),
            Self::BindingMismatch | Self::LeaseConflict => None,
        }
    }
}

fn is_expired(lease: &SnapshotLeaseV1) -> bool {
    utc_now_micros() >= lease.expires_at.0
}

fn take_expired_entries<E: ReaderQueryExecutor>(
    entries: &mut Vec<Arc<RetainedEntry<E>>>,
) -> Vec<Arc<RetainedEntry<E>>> {
    let now = utc_now_micros();
    let mut expired = Vec::new();
    let mut index = 0;
    while index < entries.len() {
        if now >= entries[index].lease.expires_at.0 {
            expired.push(entries.swap_remove(index));
        } else {
            index += 1;
        }
    }
    expired
}

fn utc_now_micros() -> i64 {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros())
        .unwrap_or(0);
    i64::try_from(micros).unwrap_or(i64::MAX)
}

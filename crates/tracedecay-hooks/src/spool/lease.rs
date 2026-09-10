use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::lock_admission::{LockAdmissionError, lock_until};

use tracedecay_domain::UtcMicros;

use super::types::{HookSpoolWriterLeaseV1, LeaseFileV1};
use super::{
    DIRECTORY_POLICY, HookSpoolError, HookSpoolV1, MAX_LEASE_BYTES, SPOOL_FORMAT_VERSION,
    lease_path, next_token, shared_sync_directory, validate_regular_or_missing,
};

impl HookSpoolV1 {
    /// Reject a mutation once the acquired lease deadline has passed.
    ///
    /// Writer leases are deliberately single-shot and non-renewable: a writer
    /// acquires one in [`HookSpoolV1::open`], performs bounded work against the
    /// same caller-supplied `now`, and drops. There is no renewal API, because
    /// the recovery for an elapsed lease is to drop the spool and reopen it,
    /// which acquires a fresh lease and rescans the durable records. Nothing is
    /// lost by that: records, acknowledgements, and the replay cursor are all
    /// on disk before a mutation returns.
    ///
    /// The consequence callers must respect is that a single spool handle must
    /// not be held across a clock advance larger than
    /// `HookSpoolConfigV1::writer_lease_micros`. Every mutating entry point
    /// takes `now` from the caller, so a writer that reuses the timestamp it
    /// opened with can never observe expiry mid-session; one that reads a fresh
    /// clock per mutation must reopen instead of retrying, or it will spin on
    /// [`HookSpoolError::WriterLeaseLost`] forever.
    pub(super) fn ensure_live_lease(&self, now: UtcMicros) -> Result<(), HookSpoolError> {
        if self.lease.expires_at.0 <= now.0 {
            hotpath::gauge!("hooks.spool.lease.lost").inc(1);
            return Err(HookSpoolError::WriterLeaseLost);
        }
        Ok(())
    }
}

#[hotpath::measure(label = "hooks.spool.write_lease")]
pub(super) fn write_lease_file(
    file: &mut File,
    lease: HookSpoolWriterLeaseV1,
) -> Result<(), HookSpoolError> {
    let bytes = serde_json::to_vec(&LeaseFileV1 {
        version: SPOOL_FORMAT_VERSION,
        token: lease.token,
        expires_at: lease.expires_at,
    })
    .map_err(|_| HookSpoolError::InvalidLease)?;
    if bytes.is_empty() || bytes.len() > MAX_LEASE_BYTES {
        return Err(HookSpoolError::InvalidLease);
    }
    file.set_len(0).map_err(|_| HookSpoolError::Io)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| HookSpoolError::Io)?;
    file.write_all(&bytes).map_err(|_| HookSpoolError::Io)?;
    hotpath::measure_block!("hooks.spool.fsync.lease", {
        file.sync_all().map_err(|_| HookSpoolError::Io)
    })
}

/// Acquires the single-writer lease without waiting. Native callbacks use the
/// bounded admission path so capture and delivery each wait one budget.
#[hotpath::measure(label = "hooks.spool.acquire_lease")]
pub(super) fn acquire_lease(
    root: &Path,
    lease_duration_micros: i64,
    now: UtcMicros,
) -> Result<(HookSpoolWriterLeaseV1, File), HookSpoolError> {
    acquire_lease_bounded(root, lease_duration_micros, now, None)
}

/// `wait_budget` bounds only the lock wait and is measured from the lock
/// attempt itself. Creating the spool root and the lease file fsync the
/// directory first; measuring the budget from before that work let a
/// loaded disk spend it on an uncontended first-ever open, which then
/// reported `AdmissionTimedOut` without ever contending for anything.
pub(super) fn acquire_lease_bounded(
    root: &Path,
    lease_duration_micros: i64,
    now: UtcMicros,
    wait_budget: Option<Duration>,
) -> Result<(HookSpoolWriterLeaseV1, File), HookSpoolError> {
    let expires_at = UtcMicros(
        now.0
            .checked_add(lease_duration_micros)
            .ok_or(HookSpoolError::InvalidLease)?,
    );
    let candidate = HookSpoolWriterLeaseV1 {
        token: next_token(),
        expires_at,
    };
    let path = lease_path(root);
    let lease_file_existed = validate_regular_or_missing(&path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|_| HookSpoolError::Io)?;
    if !validate_regular_or_missing(&path)? {
        return Err(HookSpoolError::UnsafePath);
    }
    match wait_budget {
        Some(wait_budget) => {
            lock_until(&file, Instant::now() + wait_budget).map_err(|error| match error {
                LockAdmissionError::TimedOut => HookSpoolError::AdmissionTimedOut,
                LockAdmissionError::Io => HookSpoolError::Io,
            })?;
        }
        None => file.try_lock().map_err(map_try_lock_error)?,
    }
    write_lease_file(&mut file, candidate)?;
    // Only a newly created lease file needs its directory entry made durable;
    // re-syncing an entry that already survived a crash buys nothing and is
    // paid inside the exclusive section every sibling hook is queued behind.
    // The cost is not uniform: `File::sync_all` is `fcntl(F_FULLFSYNC)` on
    // macOS, a device-level barrier rather than the page-cache flush the same
    // call makes on Linux. A creator that has not yet reached this line still
    // holds the lock, so it performs the sync before any waiter proceeds.
    if !lease_file_existed {
        hotpath::measure_block!("hooks.spool.fsync.directory", {
            shared_sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)
        })?;
    }
    Ok((candidate, file))
}

pub(super) fn map_try_lock_error(error: std::fs::TryLockError) -> HookSpoolError {
    match error {
        std::fs::TryLockError::WouldBlock => {
            hotpath::gauge!("hooks.spool.lease.contended").inc(1);
            HookSpoolError::WriterLeaseHeld
        }
        std::fs::TryLockError::Error(_) => HookSpoolError::Io,
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn standard_try_lock_errors_keep_contention_distinct_from_io() {
        assert_eq!(
            map_try_lock_error(std::fs::TryLockError::WouldBlock),
            HookSpoolError::WriterLeaseHeld
        );
        assert_eq!(
            map_try_lock_error(std::fs::TryLockError::Error(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "denied",
            ))),
            HookSpoolError::Io
        );
    }
}

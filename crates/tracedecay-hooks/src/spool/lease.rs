use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::lock_admission::{LockAdmissionError, lock_until};

use tracedecay_domain::UtcMicros;

use super::types::HookSpoolWriterLeaseV1;
use super::{HookSpoolError, HookSpoolV1, lease_path, next_token, validate_regular_or_missing};

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
/// attempt itself. Root validation and opening the advisory lock file happen
/// before that budget because they are setup rather than lock contention.
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
    validate_regular_or_missing(&path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|_| HookSpoolError::Io)?;
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
    // The open file description and its OS lock are the sole cross-process
    // ownership authority. The token and deadline remain in memory only to
    // reject a stale live handle; no process reads lease-file bytes. Therefore
    // persisting advisory ownership would add a durability barrier without
    // strengthening exclusion, while records, metadata and replay cursors keep
    // their independent fsync-before-return contracts.
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
    fn advisory_lease_acquisition_does_not_write_ownership_state() {
        let root =
            std::env::temp_dir().join(format!("tracedecay-hook-lease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        tracedecay_private_fs::create_private_directory(&root).expect("lease fixture root");

        let (_lease, file) =
            acquire_lease(&root, 1_000, UtcMicros(1)).expect("acquire advisory lease");

        assert_eq!(
            file.metadata().expect("lease metadata").len(),
            0,
            "the OS lock is the ownership authority; idle acquisition must not write or fsync"
        );
        drop(file);
        std::fs::remove_dir_all(root).expect("remove lease fixture");
    }

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

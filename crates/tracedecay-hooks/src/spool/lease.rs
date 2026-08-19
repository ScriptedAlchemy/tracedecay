use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

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
            return Err(HookSpoolError::WriterLeaseLost);
        }
        Ok(())
    }
}

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
    file.sync_all().map_err(|_| HookSpoolError::Io)
}

pub(super) fn acquire_lease(
    root: &Path,
    lease_duration_micros: i64,
    now: UtcMicros,
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
    let mut file = options.open(&path).map_err(|_| HookSpoolError::Io)?;
    if !validate_regular_or_missing(&path)? {
        return Err(HookSpoolError::UnsafePath);
    }
    file.try_lock().map_err(map_try_lock_error)?;
    write_lease_file(&mut file, candidate)?;
    shared_sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)?;
    Ok((candidate, file))
}

pub(super) fn map_try_lock_error(error: std::fs::TryLockError) -> HookSpoolError {
    match error {
        std::fs::TryLockError::WouldBlock => HookSpoolError::WriterLeaseHeld,
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

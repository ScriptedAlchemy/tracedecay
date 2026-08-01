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
    /// Refresh a still-owned writer lease. Expired or replaced leases fail
    /// closed; no caller may continue appending after that point.
    ///
    /// DECISION NEEDED: this method has zero callers today, but it is the
    /// only lease-renewal path in this crate. A long-lived spool writer
    /// (e.g. a daemon replay loop that keeps a `HookSpoolV1` open across
    /// many `append` calls) never renews its lease, so `writer_lease_micros`
    /// after acquisition, `ensure_live_lease` (see below) starts rejecting
    /// every subsequent append with `HookSpoolError::WriterLeaseExpired`
    /// (or similar), even though nothing else holds the lease. Either wire
    /// this into the daemon replay loop so long-lived writers renew before
    /// expiry, or explicitly document spool writer leases as single-shot
    /// (acquire, do bounded work, drop) and size `writer_lease_micros`
    /// accordingly. Left in place pending that decision; do not delete.
    pub fn renew_writer_lease(&mut self, now: UtcMicros) -> Result<(), HookSpoolError> {
        self.ensure_live_lease(now)?;
        let renewed = HookSpoolWriterLeaseV1 {
            token: self.lease.token,
            expires_at: UtcMicros(
                now.0
                    .checked_add(self.config.writer_lease_micros)
                    .ok_or(HookSpoolError::InvalidLease)?,
            ),
        };
        write_lease_file(&mut self.lease_file, renewed)?;
        self.lease = renewed;
        Ok(())
    }

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

//! Deadline-aware admission to an exclusive or shared advisory file lock.
use std::fs::File;
use std::io;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum LockAdmissionError {
    TimedOut,
    Io(io::Error),
}

// Avoid spinning while the admitted writer completes durable filesystem work.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Exclusive-locks `file`, waiting until `deadline`.
/// A lock taken after the deadline is released and the call times out.
#[tracing::instrument(name = "private_fs.lock.admission", level = "trace", skip_all)]
pub fn lock_until(file: &File, deadline: Instant) -> Result<(), LockAdmissionError> {
    admit_until(file, deadline, File::try_lock)
}

/// Shared-locks `file`, waiting until `deadline` for any exclusive holder.
/// A lock taken after the deadline is released and the call times out.
#[tracing::instrument(name = "private_fs.lock.shared_admission", level = "trace", skip_all)]
pub fn lock_shared_until(file: &File, deadline: Instant) -> Result<(), LockAdmissionError> {
    admit_until(file, deadline, File::try_lock_shared)
}

fn admit_until(
    file: &File,
    deadline: Instant,
    try_lock: impl Fn(&File) -> Result<(), std::fs::TryLockError>,
) -> Result<(), LockAdmissionError> {
    loop {
        if Instant::now() >= deadline {
            return Err(LockAdmissionError::TimedOut);
        }
        match try_lock(file) {
            Ok(()) => {
                if Instant::now() >= deadline {
                    file.unlock().map_err(LockAdmissionError::Io)?;
                    return Err(LockAdmissionError::TimedOut);
                }
                return Ok(());
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(LockAdmissionError::TimedOut);
                }
                std::thread::sleep(remaining.min(LOCK_POLL_INTERVAL));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(LockAdmissionError::Io(error)),
        }
    }
}

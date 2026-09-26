//! Deadline-aware admission to an exclusive advisory file lock.
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

/// Takes `file`'s exclusive lock, waiting for a contended holder until
/// `deadline`. An acquisition that lands after the deadline is released and
/// reported as timed out, so an expired attempt never crosses admission.
#[hotpath::measure(label = "private_fs.lock.admission")]
pub fn lock_until(file: &File, deadline: Instant) -> Result<(), LockAdmissionError> {
    loop {
        if Instant::now() >= deadline {
            return Err(LockAdmissionError::TimedOut);
        }
        match file.try_lock() {
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

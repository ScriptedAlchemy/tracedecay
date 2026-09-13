//! Deadline-aware admission to existing hook spool writer locks.
use std::fs::File;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LockAdmissionError {
    TimedOut,
    Io,
}

// Avoid spinning while the admitted writer completes durable filesystem work.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(1);

#[hotpath::measure(label = "hooks.lock.admission")]
pub(crate) fn lock_until(file: &File, deadline: Instant) -> Result<(), LockAdmissionError> {
    loop {
        if Instant::now() >= deadline {
            return Err(LockAdmissionError::TimedOut);
        }
        match file.try_lock() {
            Ok(()) => {
                // An expired attempt must not cross the admission boundary.
                if Instant::now() >= deadline {
                    file.unlock().map_err(|_| LockAdmissionError::Io)?;
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
            Err(std::fs::TryLockError::Error(_)) => return Err(LockAdmissionError::Io),
        }
    }
}

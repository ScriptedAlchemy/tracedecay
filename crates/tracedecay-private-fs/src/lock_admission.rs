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

/// Exclusive-locks `file`, waiting until `deadline` for any other holder.
#[tracing::instrument(name = "private_fs.lock.admission", level = "trace", skip_all)]
pub fn lock_until(file: &File, deadline: Instant) -> Result<(), LockAdmissionError> {
    admit_until(file, deadline, File::try_lock)
}

/// Shared-locks `file`, waiting until `deadline` for any exclusive holder.
#[tracing::instrument(name = "private_fs.lock.shared_admission", level = "trace", skip_all)]
pub fn lock_shared_until(file: &File, deadline: Instant) -> Result<(), LockAdmissionError> {
    admit_until(file, deadline, File::try_lock_shared)
}

/// The deadline bounds waiting on contention, not the caller's own latency:
/// the lock is always attempted once, so a caller descheduled past its
/// deadline still takes a free lock. Once contended, every retry happens
/// before the deadline, so a holder that outlasts the deadline times out.
fn admit_until(
    file: &File,
    deadline: Instant,
    try_lock: impl Fn(&File) -> Result<(), std::fs::TryLockError>,
) -> Result<(), LockAdmissionError> {
    loop {
        match try_lock(file) {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(LockAdmissionError::TimedOut);
                }
                std::thread::sleep(remaining.min(LOCK_POLL_INTERVAL));
                if Instant::now() >= deadline {
                    return Err(LockAdmissionError::TimedOut);
                }
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(LockAdmissionError::Io(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::path::Path;

    use super::*;

    fn open(path: &Path) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap()
    }

    #[test]
    fn a_free_lock_is_admitted_after_the_deadline_has_passed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admission.lock");
        let elapsed = Instant::now();

        let exclusive = open(&path);
        assert!(lock_until(&exclusive, elapsed).is_ok());
        exclusive.unlock().unwrap();

        let shared = open(&path);
        assert!(lock_shared_until(&shared, elapsed).is_ok());
        let second_reader = open(&path);
        assert!(lock_shared_until(&second_reader, elapsed).is_ok());
    }

    #[test]
    fn a_held_lock_times_out_once_the_deadline_passes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admission.lock");
        let holder = open(&path);
        holder.lock().unwrap();
        let waiter = open(&path);

        assert!(matches!(
            lock_until(&waiter, Instant::now()),
            Err(LockAdmissionError::TimedOut)
        ));
        assert!(matches!(
            lock_shared_until(&waiter, Instant::now() + Duration::from_millis(20)),
            Err(LockAdmissionError::TimedOut)
        ));

        holder.unlock().unwrap();
        assert!(lock_until(&waiter, Instant::now()).is_ok());
    }

    #[test]
    fn a_holder_releasing_after_the_deadline_never_admits_a_contended_waiter() {
        let dir = tempfile::tempdir().unwrap();
        let file = open(&dir.path().join("admission.lock"));
        let deadline = Instant::now() + Duration::from_millis(5);
        let released_after_deadline = |_: &File| {
            if Instant::now() < deadline {
                Err(std::fs::TryLockError::WouldBlock)
            } else {
                Ok(())
            }
        };

        assert!(matches!(
            admit_until(&file, deadline, released_after_deadline),
            Err(LockAdmissionError::TimedOut)
        ));
    }
}

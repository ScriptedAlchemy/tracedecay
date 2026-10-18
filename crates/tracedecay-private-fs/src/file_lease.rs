use std::fs::File;
use std::io;
use std::ops::{Deref, DerefMut};

/// A held advisory file lock that is released explicitly, never by closing.
///
/// Closing a descriptor releases an `flock` only once every copy of its open
/// file description is closed. A child forked by any thread holds such a copy
/// until it execs, even under `O_CLOEXEC`, so a lock released only by close can
/// outlive its holder and refuse the next acquisition as busy. Unlocking
/// releases it for every copy.
#[derive(Debug)]
pub struct FileLease {
    file: File,
    label: &'static str,
    released: bool,
}

impl FileLease {
    /// Adopts `file`, whose lock the caller has just acquired. `label` names
    /// the lease in the warning logged if the implicit release on drop fails.
    pub fn held(file: File, label: &'static str) -> Self {
        Self {
            file,
            label,
            released: false,
        }
    }

    /// Releases the lock now, returning the failure instead of logging it.
    pub fn release(mut self) -> io::Result<()> {
        self.released = true;
        self.file.unlock()
    }
}

impl Deref for FileLease {
    type Target = File;

    fn deref(&self) -> &File {
        &self.file
    }
}

impl DerefMut for FileLease {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.file
    }
}

impl Drop for FileLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Err(error) = self.file.unlock() {
            tracing::warn!(lease = self.label, %error, "file lease could not be released");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs::{File, OpenOptions, TryLockError};
    use std::path::Path;

    use super::FileLease;

    /// A forked child that has not exec'd yet: it shares every open file
    /// description of the parent until the returned guard lets it exit.
    struct ForkedChild {
        pid: libc::pid_t,
        release: libc::c_int,
    }

    impl ForkedChild {
        fn spawn() -> Self {
            let mut fds = [0; 2];
            // SAFETY: `fds` is a writable two-element array for `pipe`.
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
            let [read_end, write_end] = fds;
            // SAFETY: the child only calls async-signal-safe `close`, `read`,
            // and `_exit`, so forking a multithreaded test process is sound.
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                let mut byte = 0_u8;
                // SAFETY: both descriptors are the child's copies of the pipe
                // and `byte` is writable for one byte.
                unsafe {
                    libc::close(write_end);
                    libc::read(read_end, (&raw mut byte).cast(), 1);
                    libc::_exit(0);
                }
            }
            // SAFETY: `read_end` is this process's open pipe descriptor.
            unsafe { libc::close(read_end) };
            Self {
                pid,
                release: write_end,
            }
        }
    }

    impl Drop for ForkedChild {
        fn drop(&mut self) {
            let mut status = 0;
            // SAFETY: closing the owned write end lets the child's `read`
            // return; `pid` is this process's unreaped child.
            unsafe {
                libc::close(self.release);
                libc::waitpid(self.pid, &raw mut status, 0);
            }
        }
    }

    fn open(path: &Path) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap()
    }

    fn locked(path: &Path) -> File {
        let file = open(path);
        file.try_lock().unwrap();
        file
    }

    /// One test, so no parallel test's forked child inherits these locks.
    #[test]
    fn lease_is_reacquirable_after_drop_or_release_while_a_forked_child_shares_it() {
        let temp = tempfile::tempdir().unwrap();

        let closed_only = temp.path().join("closed-only.lock");
        let file = locked(&closed_only);
        let child = ForkedChild::spawn();
        drop(file);
        assert!(
            matches!(open(&closed_only).try_lock(), Err(TryLockError::WouldBlock)),
            "control: closing alone must leave the child's shared description locked"
        );
        drop(child);
        open(&closed_only).try_lock().unwrap();

        let dropped = temp.path().join("dropped.lock");
        let lease = FileLease::held(locked(&dropped), "test");
        let child = ForkedChild::spawn();
        drop(lease);
        open(&dropped)
            .try_lock()
            .expect("a dropped lease must not stay held by a forked child");
        drop(child);

        let released = temp.path().join("released.lock");
        let lease = FileLease::held(locked(&released), "test");
        let child = ForkedChild::spawn();
        lease.release().unwrap();
        open(&released)
            .try_lock()
            .expect("a released lease must not stay held by a forked child");
        drop(child);
    }
}

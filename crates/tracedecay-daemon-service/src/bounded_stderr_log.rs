//! Bounded retention for a daemon stderr that lands in a plain file.
//!
//! systemd hands the managed daemon the journal, whose retention journald
//! owns. launchd has no such authority: the generated launch agent points
//! `StandardErrorPath` at `daemon.err.log` and appends to it for the life of
//! the agent, across every restart, so a daemon that warns about a persistent
//! condition grows that file without bound (issue #1981). The daemon owns the
//! bound itself: when its stderr is a regular file, it rotates that file once
//! it passes [`DAEMON_STDERR_LOG_ROTATE_BYTES`], keeping exactly one previous
//! generation, and re-points fd 2 at the fresh file. launchd keeps its own
//! descriptor on the rotated inode, which is only ever reached by the crash
//! output of a process launchd starts, and every restart reopens the path.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tracedecay_runtime_core::logging::log_daemon_event;

/// Size at which the daemon rotates its own stderr file. A generation can
/// overshoot the bound by at most one check interval of writes before it is
/// measured, and one previous generation is retained, so the managed log on
/// disk is bounded at twice (this + one interval of writes).
pub const DAEMON_STDERR_LOG_ROTATE_BYTES: u64 = 32 * 1024 * 1024;

/// How often the daemon measures its stderr file.
pub const DAEMON_STDERR_LOG_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Suffix of the single retained previous generation.
const ROTATED_SUFFIX: &str = ".1";

/// A regular file receiving a process's stderr, and the bound it is held to.
#[derive(Debug)]
pub struct BoundedStderrLog {
    fd: RawFd,
    path: PathBuf,
    rotate_bytes: u64,
}

/// What one bound check did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrLogRotation {
    /// The file is within its bound.
    WithinBound { size_bytes: u64 },
    /// The file passed its bound and was rotated; `size_bytes` moved to the
    /// retained previous generation.
    Rotated { size_bytes: u64 },
}

impl BoundedStderrLog {
    /// The bounded log for fd 2, or `None` when stderr is not a regular file
    /// (a terminal, a pipe, or the journal), which nothing here may touch.
    pub fn detect() -> io::Result<Option<Self>> {
        Self::for_fd(libc::STDERR_FILENO, DAEMON_STDERR_LOG_ROTATE_BYTES)
    }

    /// The bounded log for one descriptor when it is a regular file.
    pub fn for_fd(fd: RawFd, rotate_bytes: u64) -> io::Result<Option<Self>> {
        let file = borrowed_file(fd);
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() {
            return Ok(None);
        }
        let path = fd_path(fd)?;
        Ok(Some(Self {
            fd,
            path,
            rotate_bytes,
        }))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Rotate the file when it has passed its bound.
    ///
    /// Rename is the only operation on the live file, so no line is torn:
    /// writers that raced through the old descriptor land in the retained
    /// generation, and everything after the `dup2` lands in the fresh file.
    /// The previous retained generation is replaced, not kept.
    pub fn rotate_if_oversized(&self) -> io::Result<StderrLogRotation> {
        let size_bytes = borrowed_file(self.fd).metadata()?.len();
        if size_bytes <= self.rotate_bytes {
            return Ok(StderrLogRotation::WithinBound { size_bytes });
        }
        let mut rotated = self.path.clone().into_os_string();
        rotated.push(ROTATED_SUFFIX);
        std::fs::rename(&self.path, &rotated)?;
        let fresh = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // SAFETY: `fresh` is an open descriptor this call owns and `self.fd`
        // is the descriptor the caller handed this log; `dup2` atomically
        // replaces what `self.fd` refers to and closes nothing else.
        if unsafe { libc::dup2(fresh.as_raw_fd(), self.fd) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(StderrLogRotation::Rotated { size_bytes })
    }

    /// Hold fd 2 to its bound for the life of the daemon.
    ///
    /// Runs one check now and then one per [`DAEMON_STDERR_LOG_CHECK_INTERVAL`]
    /// until the returned task is aborted at shutdown. Each rotation is
    /// announced on the fresh file; a failed check is announced once and the
    /// task keeps measuring, so a transient filesystem fault does not silently
    /// end the bound.
    pub fn spawn_rotation(self) -> tokio::task::JoinHandle<()> {
        log_daemon_event(
            "daemon_stderr_log_bounded",
            &[
                ("path", self.path.display().to_string()),
                ("rotate_bytes", self.rotate_bytes.to_string()),
                ("retained_generations", "1".to_owned()),
            ],
        );
        tokio::spawn(async move {
            let mut last_failure: Option<String> = None;
            loop {
                match self.rotate_if_oversized() {
                    Ok(StderrLogRotation::Rotated { size_bytes }) => {
                        last_failure = None;
                        log_daemon_event(
                            "daemon_stderr_log_rotated",
                            &[
                                ("path", self.path.display().to_string()),
                                ("rotated_bytes", size_bytes.to_string()),
                                ("rotate_bytes", self.rotate_bytes.to_string()),
                            ],
                        );
                    }
                    Ok(StderrLogRotation::WithinBound { .. }) => last_failure = None,
                    Err(error) => {
                        let failure = error.to_string();
                        if last_failure.as_deref() != Some(failure.as_str()) {
                            log_daemon_event(
                                "daemon_stderr_log_rotation_failed",
                                &[
                                    ("path", self.path.display().to_string()),
                                    ("error", failure.clone()),
                                ],
                            );
                            last_failure = Some(failure);
                        }
                    }
                }
                tokio::time::sleep(DAEMON_STDERR_LOG_CHECK_INTERVAL).await;
            }
        })
    }
}

/// A `File` view of a descriptor this module does not own. Wrapped in
/// `ManuallyDrop` so dropping the view never closes the caller's descriptor.
fn borrowed_file(fd: RawFd) -> std::mem::ManuallyDrop<File> {
    use std::os::fd::FromRawFd;
    // SAFETY: the caller's descriptor stays open for the life of the log, and
    // `ManuallyDrop` keeps this view from closing it.
    std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn fd_path(fd: RawFd) -> io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{fd}"))
}

#[cfg(target_os = "macos")]
fn fd_path(fd: RawFd) -> io::Result<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let mut buffer = [0_u8; libc::PATH_MAX as usize];
    // SAFETY: `buffer` is `PATH_MAX` bytes, the size `F_GETPATH` writes at
    // most, and `fd` is the caller's open descriptor.
    if unsafe { libc::fcntl(fd, libc::F_GETPATH, buffer.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let path = CStr::from_bytes_until_nul(&buffer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(PathBuf::from(OsStr::from_bytes(path.to_bytes())))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn fd_path(_fd: RawFd) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "resolving a descriptor's path is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    use tracedecay_runtime_core::path_safety::canonical_root_identity;

    use super::{BoundedStderrLog, StderrLogRotation};

    #[test]
    fn a_terminal_or_pipe_is_not_a_bounded_log() {
        let (read, _write) = std::io::pipe().expect("pipe");
        assert!(
            BoundedStderrLog::for_fd(read.as_raw_fd(), 1024)
                .expect("pipe is inspectable")
                .is_none()
        );
    }

    #[test]
    fn an_oversized_stderr_file_rotates_once_and_keeps_one_previous_generation() {
        let root_dir = tempfile::tempdir().expect("log root");
        // A descriptor resolves to the kernel's spelling of its path, which on
        // macOS is `/private/var/...` for a `/var/...` temp dir.
        let root = canonical_root_identity(root_dir.path());
        let path = root.join("daemon.err.log");
        let mut stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open stderr file");
        let log = BoundedStderrLog::for_fd(stderr.as_raw_fd(), 64)
            .expect("regular file is inspectable")
            .expect("regular file is a bounded log");
        assert_eq!(log.path(), path.as_path());

        stderr.write_all(&[b'a'; 40]).expect("write under bound");
        assert_eq!(
            log.rotate_if_oversized().expect("check"),
            StderrLogRotation::WithinBound { size_bytes: 40 }
        );

        stderr.write_all(&[b'a'; 40]).expect("write past bound");
        assert_eq!(
            log.rotate_if_oversized().expect("rotate"),
            StderrLogRotation::Rotated { size_bytes: 80 }
        );
        let rotated = root.join("daemon.err.log.1");
        assert_eq!(std::fs::metadata(&rotated).expect("retained").len(), 80);
        assert_eq!(std::fs::metadata(&path).expect("fresh log").len(), 0);

        // The caller's descriptor now writes into the fresh file, not the
        // retained one.
        stderr
            .write_all(b"after-rotation\n")
            .expect("write through fd");
        assert_eq!(
            std::fs::read_to_string(&path).expect("fresh contents"),
            "after-rotation\n"
        );
        assert_eq!(std::fs::metadata(&rotated).expect("retained").len(), 80);

        // A second rotation replaces the retained generation; nothing older
        // survives.
        stderr
            .write_all(&[b'b'; 70])
            .expect("write past bound again");
        assert_eq!(
            log.rotate_if_oversized().expect("rotate again"),
            StderrLogRotation::Rotated { size_bytes: 85 }
        );
        assert_eq!(std::fs::metadata(&rotated).expect("replaced").len(), 85);
        assert_eq!(
            std::fs::read_dir(&root).expect("log root entries").count(),
            2,
            "exactly the live file and one retained generation"
        );
    }

    /// The reproduced loop from the report: a warning per tick, forever.
    /// Without the bound the file tracks the tick count; with it the file
    /// never holds more than one rotation past the bound plus the live tail.
    #[test]
    fn a_repeating_warning_loop_stays_within_the_documented_bound() {
        let root = tempfile::tempdir().expect("log root");
        let path = root.path().join("daemon.err.log");
        let mut stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open stderr file");
        const ROTATE_BYTES: u64 = 4 * 1024;
        let log = BoundedStderrLog::for_fd(stderr.as_raw_fd(), ROTATE_BYTES)
            .expect("inspectable")
            .expect("bounded");
        let line = "[tracedecay] event=code_index_reconcile_failed terminal=true error=\"the publication authority is corrupt and requires an index reset\"\n";
        let mut unbounded_bytes = 0_u64;
        let mut peak_on_disk = 0_u64;
        for tick in 0..2_000_u64 {
            stderr.write_all(line.as_bytes()).expect("tick line");
            unbounded_bytes += line.len() as u64;
            // The daemon checks on an interval, not per line.
            if tick % 25 == 0 {
                log.rotate_if_oversized().expect("check");
            }
            let live = std::fs::metadata(&path).expect("live").len();
            let retained = std::fs::metadata(root.path().join("daemon.err.log.1"))
                .map_or(0, |metadata| metadata.len());
            peak_on_disk = peak_on_disk.max(live + retained);
        }
        eprintln!(
            "warning loop: {unbounded_bytes} bytes unbounded, {peak_on_disk} bytes peak on disk with a {ROTATE_BYTES}-byte bound"
        );
        let interval_bytes = 25 * line.len() as u64;
        assert!(
            peak_on_disk <= 2 * (ROTATE_BYTES + interval_bytes),
            "peak {peak_on_disk} exceeds twice the bound plus one check interval per generation"
        );
        assert!(unbounded_bytes > 10 * peak_on_disk);
    }
}

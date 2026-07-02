//! Dirty sentinel and sync-lock primitives guarding concurrent or
//! interrupted sync/index operations.

use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};
use crate::storage;

use super::current_timestamp;

/// Creates the active store's dirty sentinel before a sync or index begins.
///
/// This file is intentionally NOT cleaned up by a Drop guard — it must be
/// removed explicitly by `clear_dirty_sentinel` after the operation succeeds.
/// If the process is killed (SIGKILL, OOM), the sentinel survives and signals
/// a potential crash on the next open.
pub(super) fn write_dirty_sentinel_at(path: &Path) {
    let _ = std::fs::write(
        path,
        format!(
            "pid={}\ntime={}\nversion={}",
            std::process::id(),
            current_timestamp(),
            env!("CARGO_PKG_VERSION"),
        ),
    );
}

/// Removes the dirty sentinel after a successful sync/index.
pub(super) fn clear_dirty_sentinel_at(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Returns `true` if the dirty sentinel exists (previous operation was
/// interrupted).
pub(super) fn has_dirty_sentinel_at(path: &Path) -> bool {
    path.exists()
}

/// RAII guard that holds the sync lockfile open. Removing the lockfile on drop
/// is best-effort; if it fails (e.g. permissions), the stale-PID check on the
/// next attempt will reclaim it.
///
/// Internal: exposed for integration tests; not part of the stable public API.
#[doc(hidden)]
pub struct SyncLockGuard {
    path: PathBuf,
}

impl Drop for SyncLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Try to acquire the sync lock for `project_root`'s resolved store.
///
/// Creates the store's `sync.lock` containing the current PID. If the file
/// already exists and the PID inside is still alive, returns a `SyncLock`
/// error. Stale lockfiles (dead PID or unreadable content) are reclaimed
/// automatically.
///
/// Internal: exposed for integration tests; not part of the stable public API.
#[doc(hidden)]
pub fn try_acquire_sync_lock(project_root: &Path) -> Result<SyncLockGuard> {
    let layout = storage::resolve_layout_for_current_profile(project_root)?;
    try_acquire_sync_lock_at(&layout.sync_lock_path)
}

pub(super) fn try_acquire_sync_lock_at(lock_path: &Path) -> Result<SyncLockGuard> {
    use std::io::Write;
    let pid = std::process::id();

    // Fast path: try atomic create.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)
    {
        Ok(mut f) => {
            let _ = write!(f, "{pid}");
            return Ok(SyncLockGuard {
                path: lock_path.to_path_buf(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Fall through to stale-check below.
        }
        Err(e) => {
            return Err(TraceDecayError::SyncLock {
                message: format!("could not create lockfile: {e}"),
            });
        }
    }

    // Lockfile exists — check if the owning process is still alive.
    let contents = std::fs::read_to_string(lock_path).unwrap_or_default();
    if let Ok(existing_pid) = contents.trim().parse::<u32>() {
        if is_pid_alive(existing_pid) {
            return Err(TraceDecayError::SyncLock {
                message: format!(
                    "another sync is already in progress (PID {existing_pid}). \
                     If this is stale, remove {}",
                    lock_path.display()
                ),
            });
        }
    }

    // Stale lock — reclaim it atomically. The previous implementation removed
    // the lockfile and then created a new one in two steps; two processes that
    // both observed the same dead PID could each `remove_file` the other's
    // freshly created lock and both believe they won. Instead, atomically
    // rename the stale entry aside: rename(2) moves one specific directory
    // entry, so at most one racer can move *this* stale file. The real claim is
    // still the O_EXCL create below — the single source of truth for ownership.
    let nonce = RECLAIM_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let reclaim_path = lock_path.with_file_name(format!("sync.lock.reclaim.{pid}.{nonce}"));
    if std::fs::rename(lock_path, &reclaim_path).is_ok() {
        // We won the move. Guard against the race where another process
        // replaced the stale lock with a *live* one between our staleness check
        // and the rename: if what we moved is a live PID, put it back and
        // report contention rather than stealing a valid lock.
        let moved = std::fs::read_to_string(&reclaim_path).unwrap_or_default();
        let moved_is_live = moved.trim().parse::<u32>().is_ok_and(is_pid_alive);
        if moved_is_live {
            let _ = std::fs::rename(&reclaim_path, lock_path);
            return Err(TraceDecayError::SyncLock {
                message: "another sync is already in progress".to_string(),
            });
        }
        let _ = std::fs::remove_file(&reclaim_path);
    }

    // Claim the canonical path via the same atomic O_EXCL create as the fast
    // path. If another racer created it first (won the create after we both
    // cleared the stale file), report contention instead of clobbering it.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)
    {
        Ok(mut f) => {
            let _ = write!(f, "{pid}");
            Ok(SyncLockGuard {
                path: lock_path.to_path_buf(),
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(TraceDecayError::SyncLock {
            message: "another sync is already in progress".to_string(),
        }),
        Err(e) => Err(TraceDecayError::SyncLock {
            message: format!("could not reclaim lockfile: {e}"),
        }),
    }
}

/// Per-process counter making each stale-lock reclaim sidecar path unique, so
/// two threads in the same process never collide on the reclaim filename.
static RECLAIM_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Returns `true` if a process with the given PID is currently running.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

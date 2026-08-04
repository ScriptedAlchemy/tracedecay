//! Dirty sentinel and sync-lock primitives guarding concurrent or
//! interrupted sync/index operations.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::errors::{Result, TraceDecayError};
use crate::storage;

use super::current_timestamp;

const MARKER_SCHEMA: u8 = 2;
static EPOCH_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MarkerOwner {
    pid: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MarkerState {
    Dirty,
    Clean,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DirtyMarker {
    schema: u8,
    owner: MarkerOwner,
    epoch: String,
    state: MarkerState,
    time: i64,
    version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum MarkerIdentity {
    Epoch(String),
    Legacy(Vec<u8>),
}

#[derive(Debug, Serialize)]
struct LockLease<'a> {
    schema: u8,
    owner: MarkerOwner,
    epoch: &'a str,
    state: &'static str,
    time: i64,
    version: &'static str,
}

fn write_dirty_sentinel_for_epoch(path: &Path, epoch: &str) -> std::io::Result<()> {
    let marker = DirtyMarker {
        schema: MARKER_SCHEMA,
        owner: MarkerOwner {
            pid: std::process::id(),
        },
        epoch: epoch.to_string(),
        state: MarkerState::Dirty,
        time: current_timestamp(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let contents = serde_json::to_vec(&marker).map_err(std::io::Error::other)?;
    publish_marker(path, &contents)
}

/// Removes the dirty sentinel after a successful sync/index.
///
/// A clear is authorized only for the exact marker observed while holding its
/// sync lease (or written by this process). This prevents a delayed cleanup
/// from deleting a newer writer's marker after an epoch change.
pub(super) fn clear_dirty_sentinel_at(path: &Path) {
    let contents = match std::fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => return,
    };
    let _ = clear_marker_if_matches(path, &marker_identity(&contents));
}

fn clear_marker_if_matches(path: &Path, expected: &MarkerIdentity) -> std::io::Result<()> {
    let contents = match std::fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if &marker_identity(&contents) != expected {
        return Err(std::io::Error::other(format!(
            "dirty marker epoch changed before commit: {}",
            path.display()
        )));
    }

    if let Ok(mut marker) = serde_json::from_slice::<DirtyMarker>(&contents) {
        if marker.schema == MARKER_SCHEMA {
            marker.state = MarkerState::Clean;
            let clean = serde_json::to_vec(&marker).map_err(std::io::Error::other)?;
            publish_marker(path, &clean)?;
        }
    }

    match std::fs::remove_file(path) {
        Ok(()) => {
            sync_parent_directory(path);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Returns `true` if the dirty sentinel exists (previous operation was
/// interrupted). Legacy unstructured markers remain dirty by definition.
pub(super) fn has_dirty_sentinel_at(path: &Path) -> bool {
    match std::fs::read(path) {
        Ok(contents) => serde_json::from_slice::<DirtyMarker>(&contents)
            .map(|marker| marker.schema != MARKER_SCHEMA || marker.state == MarkerState::Dirty)
            .unwrap_or(true),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// RAII guard that keeps a persistent lockfile kernel-locked. The directory
/// entry is never removed, so a delayed Drop cannot unlink a newer owner's
/// lock. Closing the file releases the lease even after a crash.
///
/// Internal: exposed for integration tests; not part of the stable public API.
#[doc(hidden)]
pub struct SyncLockGuard {
    file: File,
    owner_path: PathBuf,
    epoch: String,
}

pub(super) struct ActiveSyncLockGuard {
    _active: SyncLockGuard,
    _legacy: Option<SyncLockGuard>,
}

pub(super) struct ActiveSyncLease {
    locks: ActiveSyncLockGuard,
    dirty_markers: Vec<(PathBuf, MarkerIdentity)>,
}

impl super::TraceDecay {
    pub(super) fn try_acquire_active_sync_lock(&self) -> Result<ActiveSyncLockGuard> {
        try_acquire_graph_sync_locks(
            &self.active_graph_layout.sync_lock_path,
            &self.store_layout.sync_lock_path,
        )
    }

    pub(super) fn begin_active_sync(&self) -> Result<ActiveSyncLease> {
        let locks = self.try_acquire_active_sync_lock()?;
        self.begin_active_sync_with_locks(locks)
    }

    pub(super) fn begin_active_sync_with_locks(
        &self,
        locks: ActiveSyncLockGuard,
    ) -> Result<ActiveSyncLease> {
        let epoch = next_epoch();
        let mut paths = vec![self.active_graph_layout.dirty_path.clone()];
        if self.active_graph_layout.dirty_path != self.store_layout.dirty_path {
            paths.push(self.store_layout.dirty_path.clone());
        }
        for path in &paths {
            write_dirty_sentinel_for_epoch(path, &epoch).map_err(|error| {
                TraceDecayError::SyncLock {
                    message: format!(
                        "could not publish dirty marker '{}': {error}",
                        path.display()
                    ),
                }
            })?;
        }
        Ok(ActiveSyncLease {
            locks,
            dirty_markers: paths
                .into_iter()
                .map(|path| (path, MarkerIdentity::Epoch(epoch.clone())))
                .collect(),
        })
    }
}

impl ActiveSyncLease {
    /// Marks the operation clean while both active and legacy locks remain
    /// held. Drop without commit intentionally leaves every dirty marker.
    pub(super) fn commit(self) -> Result<()> {
        drop(self.commit_holding_locks()?);
        Ok(())
    }

    pub(super) fn commit_holding_locks(self) -> Result<ActiveSyncLockGuard> {
        // Validate every marker before mutating any of them. A changed epoch
        // fails closed and leaves recovery evidence in place.
        for (path, expected) in &self.dirty_markers {
            let contents = std::fs::read(path).map_err(|error| TraceDecayError::SyncLock {
                message: format!("could not read dirty marker '{}': {error}", path.display()),
            })?;
            if &marker_identity(&contents) != expected {
                return Err(TraceDecayError::SyncLock {
                    message: format!(
                        "dirty marker epoch changed before commit: {}",
                        path.display()
                    ),
                });
            }
        }
        for (path, expected) in &self.dirty_markers {
            clear_marker_if_matches(path, expected).map_err(|error| TraceDecayError::SyncLock {
                message: format!("could not clear dirty marker '{}': {error}", path.display()),
            })?;
        }
        Ok(self.locks)
    }
}

pub(super) fn try_acquire_graph_sync_locks(
    active_path: &Path,
    legacy_path: &Path,
) -> Result<ActiveSyncLockGuard> {
    if active_path == legacy_path {
        return Ok(ActiveSyncLockGuard {
            _active: try_acquire_sync_lock_at(active_path)?,
            _legacy: None,
        });
    }

    // Every caller uses the same total order. This prevents active/legacy
    // lock inversion when different store layouts overlap during migration.
    if active_path < legacy_path {
        let active = try_acquire_sync_lock_at(active_path)?;
        let legacy = try_acquire_sync_lock_at(legacy_path)?;
        Ok(ActiveSyncLockGuard {
            _active: active,
            _legacy: Some(legacy),
        })
    } else {
        let legacy = try_acquire_sync_lock_at(legacy_path)?;
        let active = try_acquire_sync_lock_at(active_path)?;
        Ok(ActiveSyncLockGuard {
            _active: active,
            _legacy: Some(legacy),
        })
    }
}

impl Drop for SyncLockGuard {
    fn drop(&mut self) {
        if clear_lock_owner_if_matches(&self.owner_path, &self.epoch) {
            let _ = self.file.set_len(0);
            let _ = self.file.seek(SeekFrom::Start(0));
            let _ = self.file.sync_all();
        }
        let _ = FileExt::unlock(&self.file);
    }
}

/// Try to acquire the sync lock for `project_root`'s resolved store.
///
/// The store's persistent `sync.lock` is held with an exclusive kernel lease.
/// Its metadata is diagnostic only; lock ownership never depends on PID
/// liveness or removing/recreating a directory entry.
///
/// Internal: exposed for integration tests; not part of the stable public API.
#[doc(hidden)]
pub fn try_acquire_sync_lock(project_root: &Path) -> Result<SyncLockGuard> {
    let layout = storage::resolve_layout_for_current_profile(project_root)?;
    try_acquire_sync_lock_at(&layout.sync_lock_path)
}

#[doc(hidden)]
pub fn try_acquire_sync_lock_at(lock_path: &Path) -> Result<SyncLockGuard> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(lock_path)
        .map_err(|error| TraceDecayError::SyncLock {
            message: format!("could not open lockfile: {error}"),
        })?;

    file.try_lock_exclusive()
        .map_err(|error| TraceDecayError::SyncLock {
            message: if crate::db::is_lock_contended(&error) {
                "another sync is already in progress".to_string()
            } else {
                format!("could not lock sync lockfile: {error}")
            },
        })?;

    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| TraceDecayError::SyncLock {
            message: format!("could not restrict lockfile permissions: {error}"),
        })?;

    // Interoperate conservatively with an old TraceDecay process that created
    // a bare-PID lockfile but does not participate in kernel locking. Dead
    // legacy owners are overwritten in place; the path is never unlinked.
    let mut previous = String::new();
    let _ = file.read_to_string(&mut previous);
    if let Ok(pid) = previous.trim().parse::<u32>() {
        if is_pid_alive(pid) {
            let _ = FileExt::unlock(&file);
            return Err(TraceDecayError::SyncLock {
                message: format!("another sync is already in progress (legacy PID {pid})"),
            });
        }
    }

    let epoch = next_epoch();
    let pid = std::process::id();
    let pid_contents = pid.to_string();
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
        .and_then(|()| file.write_all(pid_contents.as_bytes()))
        .and_then(|()| file.sync_all())
        .map_err(|error| TraceDecayError::SyncLock {
            message: format!("could not publish legacy-compatible lock owner: {error}"),
        })?;

    let lease = LockLease {
        schema: MARKER_SCHEMA,
        owner: MarkerOwner { pid },
        epoch: &epoch,
        state: "locked",
        time: current_timestamp(),
        version: env!("CARGO_PKG_VERSION"),
    };
    let metadata = serde_json::to_vec(&lease).map_err(|error| TraceDecayError::SyncLock {
        message: format!("could not serialize lock lease: {error}"),
    })?;
    let owner_path = lock_owner_path(lock_path);
    publish_marker(&owner_path, &metadata).map_err(|error| TraceDecayError::SyncLock {
        message: format!("could not publish lock lease metadata: {error}"),
    })?;

    Ok(SyncLockGuard {
        file,
        owner_path,
        epoch,
    })
}

fn lock_owner_path(lock_path: &Path) -> PathBuf {
    lock_path.with_extension("lock.owner")
}

fn clear_lock_owner_if_matches(path: &Path, expected_epoch: &str) -> bool {
    let matches = std::fs::read(path)
        .ok()
        .and_then(|contents| serde_json::from_slice::<serde_json::Value>(&contents).ok())
        .and_then(|value| {
            value
                .get("epoch")
                .and_then(|epoch| epoch.as_str())
                .map(str::to_owned)
        })
        .is_some_and(|epoch| epoch == expected_epoch);
    if matches && std::fs::remove_file(path).is_ok() {
        sync_parent_directory(path);
        return true;
    }
    false
}

fn next_epoch() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = EPOCH_NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{now}-{nonce}", std::process::id())
}

fn publish_marker(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    publish_marker_with_replace(path, contents, replace_marker)
}

fn publish_marker_with_replace(
    path: &Path,
    contents: &[u8],
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let temp_path = parent.join(format!(".{name}.{}.tmp", next_epoch()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut temp = options.open(&temp_path)?;
    if let Err(error) = temp.write_all(contents).and_then(|()| temp.sync_all()) {
        drop(temp);
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    drop(temp);

    if let Err(error) = replace(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    sync_parent_directory(path);
    Ok(())
}

fn replace_marker(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    crate::db::DatabaseAuthority::replace_file_atomically(temporary, destination, "sync marker")
        .map_err(|error| std::io::Error::other(error.to_string()))
}

fn marker_identity(contents: &[u8]) -> MarkerIdentity {
    match serde_json::from_slice::<DirtyMarker>(contents) {
        Ok(marker) if marker.schema == MARKER_SCHEMA => MarkerIdentity::Epoch(marker.epoch),
        _ => MarkerIdentity::Legacy(contents.to_vec()),
    }
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
    }
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) {}

/// Returns `true` if a legacy process with the given PID is currently running.
fn is_pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    fn legacy_parser_classifies_stale(path: &Path) -> bool {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| contents.trim().parse::<u32>().ok())
            .is_none_or(|pid| !is_pid_alive(pid))
    }

    #[test]
    fn live_new_owner_blocks_legacy_create_and_new_lockers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync.lock");
        let guard = try_acquire_sync_lock_at(&path).unwrap();

        assert!(is_pid_alive(std::process::id()));
        #[cfg(not(windows))]
        {
            assert!(!legacy_parser_classifies_stale(&path));
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                std::process::id().to_string()
            );
        }
        let legacy_create = OpenOptions::new().write(true).create_new(true).open(&path);
        assert_eq!(
            legacy_create.unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists,
            "an old O_EXCL writer must not create a second canonical lock"
        );
        assert!(try_acquire_sync_lock_at(&path).is_err());

        drop(guard);
        assert!(try_acquire_sync_lock_at(&path).is_ok());
    }

    #[test]
    fn committed_lease_can_retain_sync_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("sync.lock");
        let dirty_path = dir.path().join("dirty");
        let epoch = next_epoch();
        write_dirty_sentinel_for_epoch(&dirty_path, &epoch).unwrap();
        let lease = ActiveSyncLease {
            locks: ActiveSyncLockGuard {
                _active: try_acquire_sync_lock_at(&lock_path).unwrap(),
                _legacy: None,
            },
            dirty_markers: vec![(dirty_path.clone(), MarkerIdentity::Epoch(epoch))],
        };

        let locks = lease.commit_holding_locks().unwrap();
        assert!(!dirty_path.exists());
        assert!(try_acquire_sync_lock_at(&lock_path).is_err());
        drop(locks);
        assert!(try_acquire_sync_lock_at(&lock_path).is_ok());
    }

    #[test]
    fn delayed_drop_preserves_newer_epoch_owner_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync.lock");
        let guard = try_acquire_sync_lock_at(&path).unwrap();
        let owner_path = lock_owner_path(&path);
        let stale_epoch = guard.epoch.clone();
        let replacement_epoch = "replacement-owner-epoch";
        let replacement = LockLease {
            schema: MARKER_SCHEMA,
            owner: MarkerOwner {
                pid: std::process::id(),
            },
            epoch: replacement_epoch,
            state: "locked",
            time: current_timestamp(),
            version: env!("CARGO_PKG_VERSION"),
        };
        publish_marker(&owner_path, &serde_json::to_vec(&replacement).unwrap()).unwrap();

        drop(guard);

        let current: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&owner_path).unwrap()).unwrap();
        assert_ne!(current["epoch"], stale_epoch);
        assert_eq!(current["epoch"], replacement_epoch);
    }

    #[test]
    fn dead_legacy_pid_is_recoverable_without_replacing_lock_inode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync.lock");
        #[cfg(unix)]
        let mut exited = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let mut exited = std::process::Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .unwrap();
        #[cfg(not(any(unix, windows)))]
        return;
        let dead_pid = exited.id();
        assert!(exited.wait().unwrap().success());
        assert!(!is_pid_alive(dead_pid));
        std::fs::write(&path, dead_pid.to_string()).unwrap();
        let before = std::fs::metadata(&path).unwrap();

        let guard = try_acquire_sync_lock_at(&path).unwrap();
        let after = std::fs::metadata(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        }
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
        drop(guard);
        assert!(std::fs::read_to_string(&path).unwrap().is_empty());
    }

    #[test]
    fn dirty_marker_clear_requires_the_published_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db.dirty");
        write_dirty_sentinel_for_epoch(&path, "epoch-one").unwrap();
        let stale = marker_identity(&std::fs::read(&path).unwrap());

        write_dirty_sentinel_for_epoch(&path, "epoch-two").unwrap();
        assert!(clear_marker_if_matches(&path, &stale).is_err());
        let current = std::fs::read(&path).unwrap();
        assert!(matches!(
            marker_identity(&current),
            MarkerIdentity::Epoch(epoch) if epoch == "epoch-two"
        ));

        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        clear_marker_if_matches(&path, &marker_identity(&current)).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn failed_marker_replace_preserves_live_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db.dirty");
        std::fs::write(&path, b"live marker").unwrap();

        let error = publish_marker_with_replace(&path, b"replacement", |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected replacement failure",
            ))
        })
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&path).unwrap(), b"live marker");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

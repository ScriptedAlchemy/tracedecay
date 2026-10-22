//! Locked host-file reads and forward-only UTF-8 mutations.

use std::path::Path;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use same_file::Handle;
use tracedecay_domain::canonical_text::sha256_hex;

use super::HostFileMetadataIdentityV1;
use tracedecay_domain::errors::{Result, TraceDecayError};

/// Sibling lock held across host-file observation and publication.
///
/// The lock file is transient: the holder unlinks it while still holding the
/// exclusive lock, so host-owned directories (a user's project root, a plugin
/// install dir) are not littered with lock artifacts and uninstall can remove
/// a tracedecay-owned directory completely. Only the exclusive holder may
/// unlink; a waiter that acquired the orphaned inode detects the identity
/// mismatch in `lock_host_file_write` and retries on a fresh entry.
pub(super) struct HostFileWriteLock {
    directory: Dir,
    lock_name: String,
    handle: Handle,
}

impl Drop for HostFileWriteLock {
    fn drop(&mut self) {
        // Unlink before releasing: removing the entry after unlock would let
        // two late waiters lock different inodes for the same host file.
        // The lock handle is opened with FILE_SHARE_DELETE on Windows so this
        // unlink can succeed while the exclusive lock is still held. Cleanup
        // is best-effort; a failure is traced rather than swallowed.
        if let Err(error) = self.directory.remove_file(&self.lock_name)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                lock_name = %self.lock_name,
                error = %error,
                "host config lock file could not be unlinked while held"
            );
        }
        if let Err(error) = self.handle.as_file().unlock() {
            tracing::warn!(
                lock_name = %self.lock_name,
                error = %error,
                "host config lock could not be released"
            );
        }
    }
}

/// Each retry means a prior holder unlinked the entry we waited on; bounded so
/// a pathological churn storm surfaces as a typed error instead of a spin.
const HOST_FILE_LOCK_ACQUIRE_ATTEMPTS: usize = 64;

pub(super) fn lock_host_file_write(path: &Path) -> Result<HostFileWriteLock> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| TraceDecayError::Config {
        message: format!("cannot create directory {}: {error}", parent.display()),
    })?;
    let parent = std::fs::canonicalize(parent).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to resolve host config directory {}: {error}",
            parent.display()
        ),
    })?;
    let file_name = path.file_name().ok_or_else(|| TraceDecayError::Config {
        message: format!("host config path has no file name: {}", path.display()),
    })?;
    let file_name_identity =
        serde_json::to_vec(file_name).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to bind host config lock for {}: {error}",
                path.display()
            ),
        })?;
    let lock_name = format!(
        ".tracedecay-host-config-{}.lock",
        sha256_hex(&file_name_identity)
    );
    let directory = Dir::open_ambient_dir(&parent, ambient_authority()).map_err(|error| {
        TraceDecayError::Config {
            message: format!(
                "failed to open host config directory {}: {error}",
                parent.display()
            ),
        }
    })?;
    let mut options = CapOpenOptions::new();
    options
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .follow(FollowSymlinks::No);
    let mut probe = CapOpenOptions::new();
    probe.read(true).follow(FollowSymlinks::No);
    // cap-std already defaults to FILE_SHARE_READ|WRITE|DELETE; set it
    // explicitly so Drop can unlink the still-open lock on Windows even if
    // that default ever changes.
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
        probe.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    for _ in 0..HOST_FILE_LOCK_ACQUIRE_ATTEMPTS {
        let lock = directory
            .open_with(&lock_name, &options)
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to open host config lock {}: {error}",
                    parent.join(&lock_name).display()
                ),
            })?
            .into_std();
        let metadata = lock.metadata().map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to inspect host config lock {}: {error}",
                parent.join(&lock_name).display()
            ),
        })?;
        if !metadata.is_file() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "refusing unsafe host config lock {}",
                    parent.join(&lock_name).display()
                ),
            });
        }
        let locked = Handle::from_file(lock).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to identify host config lock {}: {error}",
                parent.join(&lock_name).display()
            ),
        })?;
        locked
            .as_file()
            .lock()
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to lock host config {}: {error}", path.display()),
            })?;
        // A prior holder may have unlinked the inode we waited on; the lock is
        // valid only while the directory entry still names the locked file.
        let current = match directory.open_with(&lock_name, &probe) {
            Ok(file) => Handle::from_file(file.into_std()).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        };
        match current {
            Ok(Some(current)) if current == locked => {
                return Ok(HostFileWriteLock {
                    directory,
                    lock_name,
                    handle: locked,
                });
            }
            Ok(_) => release_abandoned_lock(&locked, &lock_name),
            Err(error) => {
                release_abandoned_lock(&locked, &lock_name);
                return Err(TraceDecayError::Config {
                    message: format!(
                        "failed to inspect host config lock {}: {error}",
                        parent.join(&lock_name).display()
                    ),
                });
            }
        }
    }
    Err(TraceDecayError::Config {
        message: format!(
            "host config lock {} kept churning while waiting for it",
            parent.join(&lock_name).display()
        ),
    })
}

/// Closing does not release an `flock` while a forked child still shares the
/// descriptor, so a lock that is not kept must be unlocked explicitly.
fn release_abandoned_lock(locked: &Handle, lock_name: &str) {
    if let Err(error) = locked.as_file().unlock() {
        tracing::warn!(
            lock_name = %lock_name,
            error = %error,
            "abandoned host config lock could not be released"
        );
    }
}

/// Brackets one read of a host file. The snapshot carries the bytes and every
/// comparison compares them, so an equal identity is never a proof by itself.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct HostFileObjectIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    device_type: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(unix)]
fn host_file_object_identity(metadata: &std::fs::Metadata) -> HostFileObjectIdentity {
    use std::os::unix::fs::MetadataExt;

    HostFileObjectIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        device_type: metadata.rdev(),
        size: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

#[cfg(not(unix))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct HostFileObjectIdentity {
    size: u64,
    readonly: bool,
    modified: std::time::SystemTime,
}

#[cfg(not(unix))]
fn host_file_object_identity(
    metadata: &std::fs::Metadata,
) -> std::io::Result<HostFileObjectIdentity> {
    Ok(HostFileObjectIdentity {
        size: metadata.len(),
        readonly: metadata.permissions().readonly(),
        modified: metadata.modified()?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HostFileSnapshot {
    Missing,
    Present {
        contents: Vec<u8>,
        metadata: HostFileMetadataIdentityV1,
        object: HostFileObjectIdentity,
    },
}

impl HostFileSnapshot {
    fn contents(&self) -> Option<&[u8]> {
        match self {
            Self::Missing => None,
            Self::Present { contents, .. } => Some(contents),
        }
    }

    fn metadata(&self) -> Option<&HostFileMetadataIdentityV1> {
        match self {
            Self::Missing => None,
            Self::Present { metadata, .. } => Some(metadata),
        }
    }
}

fn capture_host_file_snapshot(path: &Path) -> std::io::Result<HostFileSnapshot> {
    let before = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => {
            return Err(std::io::Error::other(format!(
                "unsafe host metadata path: {}",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HostFileSnapshot::Missing);
        }
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    let before_object = host_file_object_identity(&before);
    #[cfg(not(unix))]
    let before_object = host_file_object_identity(&before)?;
    let contents = std::fs::read(path)?;
    let metadata = super::capture_host_file_metadata(path)?;
    let after = std::fs::symlink_metadata(path)?;
    if !after.file_type().is_file() {
        return Err(std::io::Error::other(format!(
            "unsafe host metadata path: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    let after_object = host_file_object_identity(&after);
    #[cfg(not(unix))]
    let after_object = host_file_object_identity(&after)?;
    if before_object != after_object {
        return Err(std::io::Error::other(format!(
            "host config changed while it was read: {}",
            path.display()
        )));
    }
    Ok(HostFileSnapshot::Present {
        contents,
        metadata,
        object: after_object,
    })
}

fn verify_host_file_snapshot(path: &Path, expected: &HostFileSnapshot) -> std::io::Result<()> {
    let observed = capture_host_file_snapshot(path)?;
    if &observed == expected {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "host config changed since it was read: {}",
            path.display()
        )))
    }
}

/// Lock, snapshot, and atomically publish replacement bytes for a host file.
///
/// This is the single write authority behind `safe_write_bytes_file` and its
/// metadata-preserving variant. The snapshot supplies replacement metadata
/// and detects foreign edits before publication. Writers that bypass the lock
/// can still race between validation and rename.
pub(super) fn write_bytes_file_locked(
    path: &Path,
    contents: &[u8],
    replacement_metadata: Option<&HostFileMetadataIdentityV1>,
) -> Result<()> {
    let _lock = lock_host_file_write(path)?;
    let observed = capture_host_file_snapshot(path).map_err(|error| TraceDecayError::Config {
        message: format!("failed to capture metadata for {}: {error}", path.display()),
    })?;
    safe_write_bytes_file_from_snapshot(path, contents, replacement_metadata, &observed)
}

pub(crate) enum TextFileMutation {
    Unchanged,
    Write(String),
    Remove,
}

/// Run a strict UTF-8 read-transform-mutate while holding the host-file lock.
pub(crate) fn update_text_file_transactionally<T>(
    path: &Path,
    update: impl FnOnce(&str) -> Result<(T, TextFileMutation)>,
) -> Result<T> {
    let _lock = lock_host_file_write(path)?;
    let observed = capture_host_file_snapshot(path).map_err(|error| TraceDecayError::Config {
        message: format!("failed to read {}: {error}", path.display()),
    })?;
    let existing = snapshot_utf8(path, &observed)?;
    let (output, mutation) = update(existing)?;
    apply_file_mutation(path, &observed, &mutation)?;
    Ok(output)
}

/// Hold both path locks while publishing two host documents in order.
/// A failure leaves completed mutations in place.
pub(crate) fn update_two_config_files_locked<T>(
    first_path: &Path,
    second_path: &Path,
    update: impl FnOnce(&str, &str) -> Result<(T, TextFileMutation, TextFileMutation)>,
) -> Result<T> {
    if first_path == second_path {
        return Err(TraceDecayError::Config {
            message: format!(
                "two-file host config transaction repeats {}",
                first_path.display()
            ),
        });
    }
    let (lower, upper) = if first_path < second_path {
        (first_path, second_path)
    } else {
        (second_path, first_path)
    };
    let _locks = [lock_host_file_write(lower)?, lock_host_file_write(upper)?];
    let first_snapshot =
        capture_host_file_snapshot(first_path).map_err(|error| TraceDecayError::Config {
            message: format!("failed to read {}: {error}", first_path.display()),
        })?;
    let second_snapshot =
        capture_host_file_snapshot(second_path).map_err(|error| TraceDecayError::Config {
            message: format!("failed to read {}: {error}", second_path.display()),
        })?;
    let first_existing = snapshot_utf8(first_path, &first_snapshot)?;
    let second_existing = snapshot_utf8(second_path, &second_snapshot)?;
    let (output, first_mutation, second_mutation) = update(first_existing, second_existing)?;
    apply_file_mutation(first_path, &first_snapshot, &first_mutation)?;
    apply_file_mutation(second_path, &second_snapshot, &second_mutation).map_err(|error| {
        TraceDecayError::Config {
            message: format!(
                "{error}; earlier changes to {} remain in place",
                first_path.display()
            ),
        }
    })?;
    Ok(output)
}

fn snapshot_utf8<'a>(path: &Path, snapshot: &'a HostFileSnapshot) -> Result<&'a str> {
    match snapshot.contents() {
        Some(contents) => std::str::from_utf8(contents).map_err(|error| TraceDecayError::Config {
            message: format!("failed to read {} as UTF-8: {error}", path.display()),
        }),
        None => Ok(""),
    }
}

fn apply_file_mutation(
    path: &Path,
    snapshot: &HostFileSnapshot,
    mutation: &TextFileMutation,
) -> Result<()> {
    match mutation {
        TextFileMutation::Unchanged => Ok(()),
        // Republishing identical bytes still replaces the file, churning its
        // inode and mtime under the host that owns it.
        TextFileMutation::Write(replacement)
            if snapshot.contents() == Some(replacement.as_bytes()) =>
        {
            Ok(())
        }
        TextFileMutation::Write(replacement) => {
            safe_write_bytes_file_from_snapshot(path, replacement.as_bytes(), None, snapshot)
        }
        TextFileMutation::Remove => remove_host_file_from_snapshot(path, snapshot),
    }
}

fn remove_host_file_from_snapshot(path: &Path, observed: &HostFileSnapshot) -> Result<()> {
    if matches!(observed, HostFileSnapshot::Missing) {
        return Ok(());
    }
    #[cfg(test)]
    super::host_config_io::test_pause_host_config_write(path);
    verify_host_file_snapshot(path, observed)
        .and_then(|()| std::fs::remove_file(path))
        .and_then(|()| {
            tracedecay_private_fs::framed_log::sync_parent_directory(
                path,
                tracedecay_private_fs::framed_log::DirectorySyncPolicy::TolerateUnsupported,
            )
        })
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to remove {}: {error}; the file may already be removed",
                path.display()
            ),
        })
}

fn safe_write_bytes_file_from_snapshot(
    path: &Path,
    contents: &[u8],
    replacement_metadata: Option<&HostFileMetadataIdentityV1>,
    observed: &HostFileSnapshot,
) -> Result<()> {
    let publish_metadata = replacement_metadata.or(observed.metadata());
    tracedecay_private_fs::framed_log::atomic_write_prepared(
        path,
        "host-config",
        contents,
        |temporary| {
            if let Some(metadata) = publish_metadata {
                super::restore_host_file_metadata(temporary, metadata)?;
            }
            #[cfg(test)]
            super::host_config_io::test_pause_host_config_write(path);
            verify_host_file_snapshot(path, observed)
        },
        |temporary, destination| {
            if matches!(observed, HostFileSnapshot::Missing) {
                tracedecay_private_fs::framed_log::rename_noreplace(temporary, destination)?;
                super::host_config_io::note_created_file(destination);
                Ok(())
            } else {
                tracedecay_private_fs::framed_log::replace_via_rename(temporary, destination)
            }
        },
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to atomically replace {}: {error}; the replacement may already be published",
            path.display()
        ),
    })?;
    #[cfg(feature = "test-transport")]
    super::host_config_io::test_abort_after_host_config_write(path);
    Ok(())
}

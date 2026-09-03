//! Host-file lock, snapshot, and transactional UTF-8 mutation.
//!
//! Ported from Codex's prompt-rule / config publication work so managed
//! prompt files publish only after the exact source snapshot remains valid.

use std::cell::RefCell;
use std::path::Path;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use fs2::FileExt;
use same_file::Handle;
use tracedecay_domain::canonical_text::sha256_hex;

use super::HostFileMetadataIdentityV1;
use crate::errors::{Result, TraceDecayError};

/// Sibling lock held across host-file observation, intent, and rename.
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
        if let Err(error) = FileExt::unlock(self.handle.as_file()) {
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
        lock.lock_exclusive()
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to lock host config {}: {error}", path.display()),
            })?;
        let locked = Handle::from_file(lock).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to identify host config lock {}: {error}",
                parent.join(&lock_name).display()
            ),
        })?;
        // A prior holder may have unlinked the inode we waited on; the lock is
        // valid only while the directory entry still names the locked file.
        let current = match directory.open_with(&lock_name, &probe) {
            Ok(file) => {
                Handle::from_file(file.into_std()).map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "failed to identify host config lock {}: {error}",
                        parent.join(&lock_name).display()
                    ),
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "failed to inspect host config lock {}: {error}",
                        parent.join(&lock_name).display()
                    ),
                });
            }
        };
        if current == locked {
            return Ok(HostFileWriteLock {
                directory,
                lock_name,
                handle: locked,
            });
        }
    }
    Err(TraceDecayError::Config {
        message: format!(
            "host config lock {} kept churning while waiting for it",
            parent.join(&lock_name).display()
        ),
    })
}

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

#[cfg(unix)]
impl HostFileObjectIdentity {
    fn same_after_move(&self, other: &Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.mode == other.mode
            && self.links == other.links
            && self.uid == other.uid
            && self.gid == other.gid
            && self.device_type == other.device_type
            && self.size == other.size
            && self.modified_seconds == other.modified_seconds
            && self.modified_nanoseconds == other.modified_nanoseconds
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

#[cfg(not(unix))]
impl HostFileObjectIdentity {
    fn same_after_move(&self, other: &Self) -> bool {
        self == other
    }
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

    fn same_after_move(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Missing, Self::Missing) => true,
            (
                Self::Present {
                    contents,
                    metadata,
                    object,
                },
                Self::Present {
                    contents: other_contents,
                    metadata: other_metadata,
                    object: other_object,
                },
            ) => {
                contents == other_contents
                    && metadata == other_metadata
                    && object.same_after_move(other_object)
            }
            _ => false,
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
/// metadata-preserving variant: the snapshot arms the publish-time
/// foreign-edit refusal and supplies the metadata identity restored onto the
/// replacement.
pub(super) fn write_bytes_file_locked(
    path: &Path,
    contents: &[u8],
    backup: Option<&Path>,
    replacement_metadata: Option<&HostFileMetadataIdentityV1>,
) -> Result<()> {
    let _lock = lock_host_file_write(path)?;
    let observed = capture_host_file_snapshot(path).map_err(|error| TraceDecayError::Config {
        message: format!("failed to capture metadata for {}: {error}", path.display()),
    })?;
    safe_write_bytes_file_from_snapshot(path, contents, backup, replacement_metadata, &observed)
}

pub(super) fn restore_bytes_file_if_unchanged(
    path: &Path,
    expected_current: Option<&[u8]>,
    original: &[u8],
    original_metadata: &HostFileMetadataIdentityV1,
) -> Result<()> {
    let _lock = lock_host_file_write(path)?;
    let observed = capture_host_file_snapshot(path).map_err(|error| TraceDecayError::Config {
        message: format!("failed to inspect {} before restore: {error}", path.display()),
    })?;
    if observed.contents() != expected_current {
        return Err(TraceDecayError::Config {
            message: format!(
                "refused to restore {} because the host config changed after the failed command",
                path.display()
            ),
        });
    }
    safe_write_bytes_file_from_snapshot(
        path,
        original,
        None,
        Some(original_metadata),
        &observed,
    )
}

pub(crate) enum TextFileMutation {
    Unchanged,
    Write(String),
    Remove,
}

/// Whether a mutating transaction leaves a `.bak` of the observed bytes.
#[derive(Clone, Copy)]
enum MutationBackup {
    /// Prompt/rule files: publish without a recovery copy.
    None,
    /// Structured host configs: every rewrite or removal of an existing file
    /// leaves a `.bak` the operator can restore (issue #63).
    BackupExisting,
}

/// Run a strict UTF-8 read-transform-mutate while holding the host-file lock.
pub(crate) fn update_text_file_transactionally<T>(
    path: &Path,
    update: impl FnOnce(&str) -> Result<(T, TextFileMutation)>,
) -> Result<T> {
    update_file_transactionally(path, MutationBackup::None, update)
}

/// [`update_text_file_transactionally`] for structured host configs
/// (JSON/JSONC/TOML): identical read-under-lock → transform →
/// publish-from-snapshot shape, except that rewriting or removing an existing
/// file first leaves a `.bak` recovery copy whose path is threaded into the
/// publish error hint.
pub(crate) fn update_config_file_transactionally<T>(
    path: &Path,
    update: impl FnOnce(&str) -> Result<(T, TextFileMutation)>,
) -> Result<T> {
    update_file_transactionally(path, MutationBackup::BackupExisting, update)
}

fn update_file_transactionally<T>(
    path: &Path,
    backup: MutationBackup,
    update: impl FnOnce(&str) -> Result<(T, TextFileMutation)>,
) -> Result<T> {
    let _lock = lock_host_file_write(path)?;
    let observed = capture_host_file_snapshot(path).map_err(|error| TraceDecayError::Config {
        message: format!("failed to read {}: {error}", path.display()),
    })?;
    let existing = match observed.contents() {
        Some(contents) => {
            std::str::from_utf8(contents).map_err(|error| TraceDecayError::Config {
                message: format!("failed to read {} as UTF-8: {error}", path.display()),
            })?
        }
        None => "",
    };
    let (output, mutation) = update(existing)?;
    let backup = match (&mutation, backup, &observed) {
        (TextFileMutation::Unchanged, _, _)
        | (_, MutationBackup::None, _)
        | (_, MutationBackup::BackupExisting, HostFileSnapshot::Missing) => None,
        (_, MutationBackup::BackupExisting, HostFileSnapshot::Present { .. }) => {
            super::backup_config_file(path)?
        }
    };
    match mutation {
        TextFileMutation::Unchanged => {}
        TextFileMutation::Write(replacement) => {
            safe_write_bytes_file_from_snapshot(
                path,
                replacement.as_bytes(),
                backup.as_deref(),
                None,
                &observed,
            )?;
        }
        TextFileMutation::Remove => {
            remove_host_file_from_snapshot(path, &observed)?;
        }
    }
    Ok(output)
}

fn remove_host_file_from_snapshot(path: &Path, observed: &HostFileSnapshot) -> Result<()> {
    if matches!(observed, HostFileSnapshot::Missing) {
        return Ok(());
    }
    super::persist_host_config_remove_intent(path)?;
    tracedecay_private_fs::framed_log::remove_conditionally(
        path,
        || {
            #[cfg(test)]
            super::test_pause_host_config_write(
                path,
                super::TestHostConfigWriteBoundary::Publication,
            );
        },
        |displaced| {
            let displaced = capture_host_file_snapshot(displaced)?;
            Ok(displaced.same_after_move(observed))
        },
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to conditionally remove {}: {error}", path.display()),
    })
}

fn safe_write_bytes_file_from_snapshot(
    path: &Path,
    contents: &[u8],
    backup: Option<&Path>,
    replacement_metadata: Option<&HostFileMetadataIdentityV1>,
    observed: &HostFileSnapshot,
) -> Result<()> {
    let publish_metadata = replacement_metadata.or(observed.metadata());
    let staged_snapshot = RefCell::new(None);
    let publish_expectation = match observed {
        HostFileSnapshot::Missing => {
            tracedecay_private_fs::framed_log::ConditionalPublishExpectation::Missing
        }
        HostFileSnapshot::Present { .. } => {
            tracedecay_private_fs::framed_log::ConditionalPublishExpectation::Present
        }
    };
    if let Err(e) = tracedecay_private_fs::framed_log::atomic_write_prepared_conditionally(
        path,
        "host-config",
        contents,
        publish_expectation,
        tracedecay_private_fs::framed_log::ConditionalPublishCallbacks {
            prepare: |temporary: &Path| {
                if let Some(metadata) = publish_metadata {
                    super::restore_host_file_metadata(temporary, metadata)?;
                }
                let expected_metadata = super::capture_host_file_metadata(temporary)?;
                staged_snapshot.replace(Some(capture_host_file_snapshot(temporary)?));
                super::persist_host_config_write_intent(path, contents, Some(&expected_metadata))
                    .map_err(std::io::Error::other)?;
                verify_host_file_snapshot(path, observed)?;
                #[cfg(test)]
                super::test_pause_host_config_write(
                    path,
                    super::TestHostConfigWriteBoundary::Validation,
                );
                verify_host_file_snapshot(path, observed)?;
                Ok(())
            },
            before_publish: || {
                #[cfg(test)]
                super::test_pause_host_config_write(
                    path,
                    super::TestHostConfigWriteBoundary::Publication,
                );
            },
            after_publish: || {
                #[cfg(test)]
                super::test_pause_host_config_write(
                    path,
                    super::TestHostConfigWriteBoundary::Published,
                );
            },
            verify_displaced: |displaced: &Path| {
                let displaced = capture_host_file_snapshot(displaced)?;
                Ok(displaced.same_after_move(observed))
            },
            verify_published: |rolled_back_published: &Path| {
                let rolled_back = capture_host_file_snapshot(rolled_back_published)?;
                let staged = staged_snapshot.borrow();
                Ok(staged
                    .as_ref()
                    .is_some_and(|staged| rolled_back.same_after_move(staged)))
            },
        },
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::TolerateUnsupported,
    ) {
        let hint = if let Some(b) = backup {
            format!(
                "\n  Backup is at: {}\n  \
                 The original file was NOT modified.",
                b.display()
            )
        } else {
            "\n  The original file was NOT modified.".to_string()
        };
        return Err(TraceDecayError::Config {
            message: format!("failed to atomically replace {}: {e}{hint}", path.display()),
        });
    }
    #[cfg(feature = "test-transport")]
    super::test_abort_after_host_config_write(path);
    Ok(())
}

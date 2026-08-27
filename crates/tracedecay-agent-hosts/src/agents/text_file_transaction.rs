//! Host-file lock, snapshot, and transactional UTF-8 mutation.
//!
//! Ported from Codex's prompt-rule / config publication work so managed
//! prompt files publish only after the exact source snapshot remains valid.

use std::cell::RefCell;
use std::fs::File;
use std::path::Path;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use fs2::FileExt;
use tracedecay_domain::canonical_text::sha256_hex;

use super::HostFileMetadataIdentityV1;
use crate::errors::{Result, TraceDecayError};

/// Stable sibling lock held across host-file observation, intent, and rename.
struct HostFileWriteLock(File);

impl Drop for HostFileWriteLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn lock_host_file_write(path: &Path) -> Result<HostFileWriteLock> {
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
    Ok(HostFileWriteLock(lock))
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
    let existing = match observed.contents() {
        Some(contents) => {
            std::str::from_utf8(contents).map_err(|error| TraceDecayError::Config {
                message: format!("failed to read {} as UTF-8: {error}", path.display()),
            })?
        }
        None => "",
    };
    let (output, mutation) = update(existing)?;
    match mutation {
        TextFileMutation::Unchanged => {}
        TextFileMutation::Write(replacement) => {
            safe_write_bytes_file_from_snapshot(
                path,
                replacement.as_bytes(),
                None,
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
        || {},
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
                verify_host_file_snapshot(path, observed)?;
                Ok(())
            },
            before_publish: || {},
            after_publish: || {},
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
    Ok(())
}

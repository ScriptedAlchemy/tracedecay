//! Durable two-phase quarantine for destructive orphan-store retention.

use std::ffi::{CString, OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cap_fs_ext::DirExt;
#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::fence::{
    StoreContentFence, capture_store_content_fence_in_dir_controlled,
    open_store_directory_nofollow, open_store_parent_nofollow,
};
use super::{CollectionControl, CollectionFailureKind};

static QUARANTINE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const QUARANTINE_ATTEMPTS: usize = 32;
const JOURNAL_SUFFIX: &str = ".receipt-v1.json";
const RENAMED_SUFFIX: &str = ".renamed";
const RETIRED_SUFFIX: &str = ".retired";

/// The database decision that must be durable before the quarantined bytes may
/// be removed. `Unregistered` has no row to delete, but it still records the
/// final absence confirmation before its irreversible phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum QuarantineKindV1 {
    Registered,
    Unregistered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct QuarantineRegistryFenceV1 {
    pub(super) store_relpath: String,
    pub(super) created_at: i64,
    pub(super) last_write_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct QuarantineJournalV1 {
    version: u8,
    kind: QuarantineKindV1,
    project_id: String,
    store_id: String,
    original_name: String,
    registry_fence: Option<QuarantineRegistryFenceV1>,
}

/// Test projection of a readable on-disk recovery record.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct PendingQuarantineReceiptV1 {
    pub(crate) quarantine_path: PathBuf,
    /// The live filesystem location observed when the receipt was read. A
    /// rename can succeed before its parent-directory sync fails, leaving the
    /// bytes at `original_path` while the journal remains pending.
    pub(crate) actual_path: PathBuf,
    pub(crate) retirement_committed: bool,
}

/// The result of moving one exact store leaf out of its live name and proving
/// that the moved bytes still equal the census fence.
pub(super) enum QuarantineStoreOutcome {
    Missing,
    Verified(QuarantinedStore),
    Interrupted {
        quarantine_path: PathBuf,
    },
    Restored {
        restored_path: PathBuf,
        journal_pending: bool,
    },
    Retained {
        quarantine_path: PathBuf,
    },
}

/// A durable quarantine found on a later maintenance admission.
pub(super) enum QuarantineRecoveryOutcome {
    Restored {
        restored_path: PathBuf,
        journal_pending: bool,
    },
    Retained {
        quarantine_path: PathBuf,
    },
}

pub(super) enum QuarantineFinalizeOutcome {
    Removed { journal_pending: bool },
    Interrupted { quarantine_path: PathBuf },
    DeleteUnconfirmed { quarantine_path: PathBuf },
}

/// A verified moved directory plus its immutable, sibling journal. The
/// journal is written and synced before this value is returned; after that,
/// no crash can make the quarantine invisible to the production reader.
pub(super) struct QuarantinedStore {
    parent: Dir,
    root: Dir,
    quarantine_path: PathBuf,
    journal_name: String,
}

impl QuarantinedStore {
    pub(super) fn quarantine_path(&self) -> &Path {
        &self.quarantine_path
    }

    /// Publish the database-commit phase before removal. This marker is
    /// additive/no-replace, so a crash cannot turn a committed retirement back
    /// into an apparently prepared one by tearing an overwrite.
    pub(super) fn mark_retirement_committed(&self) -> std::io::Result<()> {
        write_empty_marker(&self.parent, &retired_marker_name(&self.journal_name))
    }

    /// The irreversible phase runs only after the caller's registry commit.
    /// If recursive removal or its parent sync fails, the journal is retained
    /// and reports `DeleteUnconfirmed`; a later reconciliation retries from
    /// the exact same capability boundary rather than claiming reclaimed data.
    pub(super) fn finalize(self, control: CollectionControl<'_>) -> QuarantineFinalizeOutcome {
        let Self {
            parent,
            root,
            quarantine_path,
            journal_name,
            ..
        } = self;
        if control.completion().is_some() {
            return QuarantineFinalizeOutcome::Interrupted { quarantine_path };
        }
        match remove_open_dir_all_controlled(root, control) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                return QuarantineFinalizeOutcome::Interrupted { quarantine_path };
            }
            Err(_) => return QuarantineFinalizeOutcome::DeleteUnconfirmed { quarantine_path },
        }
        // Once the final child disappears, synchronizing the parent is part
        // of the same irreversible operation. It must complete even if the
        // admission is cancelled concurrently; otherwise a completed delete
        // could be reported without its durability boundary.
        if sync_directory(&parent).is_err() {
            return QuarantineFinalizeOutcome::DeleteUnconfirmed { quarantine_path };
        }
        let journal_pending = clear_journal(&parent, &journal_name).is_err();
        QuarantineFinalizeOutcome::Removed { journal_pending }
    }
}

/// Remove an already-open quarantine directory with a cancellation check
/// before every child operation. All descent is capability-relative and
/// no-follow, so an entry swapped for a symlink cannot redirect cleanup out
/// of the quarantined store. An interruption intentionally leaves the journal
/// and remaining bytes in place for the mounted reconciler.
fn remove_open_dir_all_controlled(
    directory: Dir,
    control: CollectionControl<'_>,
) -> std::io::Result<()> {
    if control.completion().is_some() {
        return Err(interrupted_remove_error());
    }
    let entries = directory.read_dir(".")?;
    for entry in entries {
        if control.completion().is_some() {
            return Err(interrupted_remove_error());
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let name = entry.file_name();
            let child = directory.open_dir_nofollow(&name)?;
            remove_open_dir_all_controlled(child, control)?;
        } else {
            entry.remove_file()?;
        }
    }
    if control.completion().is_some() {
        return Err(interrupted_remove_error());
    }
    directory.remove_open_dir()
}

fn interrupted_remove_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "retention quarantine finalization interrupted",
    )
}

/// Atomically moves `data_root` to a unique sibling, persists a prepared
/// journal, then proves its exact content inventory. The caller performs the
/// short registry transaction only after this potentially expensive hashing.
pub(super) fn quarantine_store_for_verified_collection_controlled(
    profile_root: &Path,
    data_root: &Path,
    expected: &StoreContentFence,
    kind: QuarantineKindV1,
    project_id: &str,
    store_id: &str,
    registry_fence: Option<QuarantineRegistryFenceV1>,
    control: CollectionControl<'_>,
) -> Result<QuarantineStoreOutcome, CollectionFailureKind> {
    if control.completion().is_some() {
        return Ok(QuarantineStoreOutcome::Interrupted {
            quarantine_path: data_root.to_path_buf(),
        });
    }
    if *expected == StoreContentFence::Unverifiable {
        return Err(CollectionFailureKind::InspectFailed);
    }
    if *expected == StoreContentFence::Missing {
        return Ok(QuarantineStoreOutcome::Missing);
    }
    let capability = open_store_directory_nofollow(profile_root, data_root)?;
    let original_name = capability
        .leaf_name
        .to_str()
        .ok_or(CollectionFailureKind::InspectFailed)?
        .to_owned();
    let quarantine_name = reserve_quarantine_name(&capability.parent, &capability.leaf_name)
        .ok_or(CollectionFailureKind::RemoveFailed)?;
    let quarantine_path = data_root
        .parent()
        .ok_or(CollectionFailureKind::OutsideProfile)?
        .join(&quarantine_name);
    let journal_name = journal_name(&quarantine_name);
    let journal = QuarantineJournalV1 {
        version: 1,
        kind,
        project_id: project_id.to_owned(),
        store_id: store_id.to_owned(),
        original_name,
        registry_fence,
    };
    // The journal is the intent record for the following destructive rename.
    // Publishing it first eliminates the old crash window where a synced
    // quarantine existed with no discoverable recovery authority.
    write_journal(&capability.parent, &journal_name, &journal)
        .map_err(|_| CollectionFailureKind::RemoveFailed)?;

    if rename_noreplace(
        &capability.parent,
        &capability.leaf_name,
        OsStr::new(&quarantine_name),
    )
    .is_err()
    {
        let _ = clear_journal(&capability.parent, &journal_name);
        return Err(CollectionFailureKind::RemoveFailed);
    }
    if sync_directory(&capability.parent).is_err() {
        return Ok(recover_original_name(
            capability.parent,
            capability.root,
            capability.leaf_name,
            quarantine_name,
            quarantine_path,
            Some(journal_name),
        ));
    }
    if write_empty_marker(&capability.parent, &renamed_marker_name(&journal_name)).is_err() {
        drop(capability.root);
        return Ok(QuarantineStoreOutcome::Interrupted { quarantine_path });
    }
    let moved_root = match capability.parent.open_dir_nofollow(&quarantine_name) {
        Ok(root) => root,
        Err(_) => {
            return Ok(recover_original_name(
                capability.parent,
                capability.root,
                capability.leaf_name,
                quarantine_name,
                quarantine_path,
                Some(journal_name),
            ));
        }
    };
    let verified = capture_store_content_fence_in_dir_controlled(&moved_root, Some(control))
        .map(StoreContentFence::Present);
    match verified {
        Ok(actual) if actual == *expected => {
            Ok(QuarantineStoreOutcome::Verified(QuarantinedStore {
                parent: capability.parent,
                root: moved_root,
                quarantine_path,
                journal_name,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            drop(moved_root);
            Ok(QuarantineStoreOutcome::Interrupted { quarantine_path })
        }
        Ok(_) | Err(_) => {
            drop(moved_root);
            Ok(recover_original_name(
                capability.parent,
                capability.root,
                capability.leaf_name,
                quarantine_name,
                quarantine_path,
                Some(journal_name),
            ))
        }
    }
}

#[cfg(test)]
pub(super) fn quarantine_store_for_verified_collection(
    profile_root: &Path,
    data_root: &Path,
    expected: &StoreContentFence,
) -> Result<QuarantineStoreOutcome, CollectionFailureKind> {
    let cancellation = CancellationToken::new();
    quarantine_store_for_verified_collection_controlled(
        profile_root,
        data_root,
        expected,
        QuarantineKindV1::Unregistered,
        "test-project",
        "test-store",
        None,
        CollectionControl::new(
            &cancellation,
            MonotonicDeadline::at(std::time::Instant::now() + std::time::Duration::from_hours(24)),
        ),
    )
}

fn recover_original_name(
    parent: Dir,
    root: Dir,
    original_name: OsString,
    quarantine_name: String,
    quarantine_path: PathBuf,
    journal_name: Option<String>,
) -> QuarantineStoreOutcome {
    drop(root);
    if rename_noreplace(&parent, OsStr::new(&quarantine_name), &original_name).is_ok() {
        // A directory sync failure occurs after the atomic rename. Preserve
        // any journal and return the true, restored path for that state.
        let journal_pending = sync_directory(&parent).is_err()
            || journal_name.is_some_and(|name| clear_journal(&parent, &name).is_err());
        let restored_path = quarantine_path
            .parent()
            .map_or_else(PathBuf::new, |parent| parent.join(&original_name));
        QuarantineStoreOutcome::Restored {
            restored_path,
            journal_pending,
        }
    } else {
        QuarantineStoreOutcome::Retained { quarantine_path }
    }
}

fn reserve_quarantine_name(parent: &Dir, original: &OsStr) -> Option<String> {
    let original = original.to_str()?;
    for _ in 0..QUARANTINE_ATTEMPTS {
        let sequence = QUARANTINE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = format!(
            ".tracedecay-orphan-quarantine-{original}-{}-{sequence}",
            std::process::id()
        );
        match parent.symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(candidate),
            // The candidate name is taken or unreadable; try the next sequence.
            Ok(_) | Err(_) => {}
        }
    }
    None
}

fn journal_name(quarantine_name: &str) -> String {
    format!("{quarantine_name}{JOURNAL_SUFFIX}")
}

fn retired_marker_name(journal_name: &str) -> String {
    format!("{journal_name}{RETIRED_SUFFIX}")
}

fn renamed_marker_name(journal_name: &str) -> String {
    format!("{journal_name}{RENAMED_SUFFIX}")
}

fn write_journal(parent: &Dir, name: &str, journal: &QuarantineJournalV1) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(journal)
        .map_err(|error| std::io::Error::other(format!("serialize retention journal: {error}")))?;
    let temporary = format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        QUARANTINE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = parent.open_with(&temporary, &options)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = parent.remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = rename_noreplace(parent, OsStr::new(&temporary), OsStr::new(name)) {
        let _ = parent.remove_file(&temporary);
        return Err(error);
    }
    sync_directory(parent)
}

fn write_empty_marker(parent: &Dir, name: &str) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    match parent.open_with(name, &options) {
        Ok(file) => {
            file.sync_all()?;
            sync_directory(parent)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn clear_journal(parent: &Dir, journal_name: &str) -> std::io::Result<()> {
    let renamed = renamed_marker_name(journal_name);
    let marker = retired_marker_name(journal_name);
    for name in [journal_name, renamed.as_str(), marker.as_str()] {
        match parent.remove_file(name) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    sync_directory(parent)
}

/// Legacy pre-journal quarantines are restored. Journal-backed quarantines are
/// retained until the production reconciler proves whether registry retirement
/// committed; blindly restoring after an unknown SQL commit would resurrect a
/// retired store under a live name.
pub(super) fn recover_existing_store_quarantine(
    profile_root: &Path,
    data_root: &Path,
) -> Result<Vec<QuarantineRecoveryOutcome>, CollectionFailureKind> {
    let capability = open_store_parent_nofollow(profile_root, data_root)?;
    let original = capability
        .leaf_name
        .to_str()
        .ok_or(CollectionFailureKind::InspectFailed)?;
    let parent_path = data_root
        .parent()
        .ok_or(CollectionFailureKind::OutsideProfile)?;
    let mut outcomes = Vec::new();
    for entry in capability
        .parent
        .read_dir(".")
        .map_err(|_| CollectionFailureKind::InspectFailed)?
    {
        let entry = entry.map_err(|_| CollectionFailureKind::InspectFailed)?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if quarantine_original_name(name) != Some(original) {
            continue;
        }
        let journal = journal_name(name);
        if capability.parent.symlink_metadata(&journal).is_ok() {
            outcomes.push(QuarantineRecoveryOutcome::Retained {
                quarantine_path: parent_path.join(&file_name),
            });
            continue;
        }
        if let Some(outcome) =
            recover_named_store_quarantine(profile_root, data_root, &file_name, parent_path)?
        {
            outcomes.push(outcome);
        }
    }
    Ok(outcomes)
}

/// Returns the original project id encoded in an orphan-store quarantine name.
pub(super) fn quarantined_project_id(name: &str) -> Option<String> {
    let project_id = quarantine_original_name(name)?;
    crate::storage::validate_project_id(project_id).ok()?;
    Some(project_id.to_owned())
}

fn quarantine_original_name(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(".tracedecay-orphan-quarantine-")?;
    let (rest, sequence) = rest.rsplit_once('-')?;
    sequence.parse::<u64>().ok()?;
    let (original, process_id) = rest.rsplit_once('-')?;
    process_id.parse::<u32>().ok()?;
    (!original.is_empty()).then_some(original)
}

pub(super) fn recover_named_store_quarantine(
    profile_root: &Path,
    data_root: &Path,
    quarantine_name: &OsStr,
    parent_path: &Path,
) -> Result<Option<QuarantineRecoveryOutcome>, CollectionFailureKind> {
    let capability = open_store_parent_nofollow(profile_root, data_root)?;
    let quarantine_path = parent_path.join(quarantine_name);
    match capability.parent.open_dir_nofollow(quarantine_name) {
        Ok(root) => drop(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Ok(Some(QuarantineRecoveryOutcome::Retained {
                quarantine_path,
            }));
        }
    }
    let journal = quarantine_name
        .to_str()
        .map(journal_name)
        .ok_or(CollectionFailureKind::InspectFailed)?;
    if capability.parent.symlink_metadata(&journal).is_ok() {
        return Ok(Some(QuarantineRecoveryOutcome::Retained {
            quarantine_path,
        }));
    }
    if rename_noreplace(&capability.parent, quarantine_name, &capability.leaf_name).is_ok() {
        Ok(Some(QuarantineRecoveryOutcome::Restored {
            restored_path: data_root.to_path_buf(),
            // Legacy quarantines have no journal, but an unsynced rename is
            // still a recovery state that must not be reported as complete.
            journal_pending: sync_directory(&capability.parent).is_err(),
        }))
    } else {
        Ok(Some(QuarantineRecoveryOutcome::Retained {
            quarantine_path,
        }))
    }
}

/// Test helper for asserting that a crash boundary left a readable durable
/// journal. Production recovery is mounted at each store's next admission.
#[cfg(test)]
pub(crate) fn read_pending_quarantine_receipts(
    profile_root: &Path,
) -> Result<Vec<PendingQuarantineReceiptV1>, CollectionFailureKind> {
    read_pending_quarantine_receipts_controlled(profile_root, super::unbounded_collection_control())
}

#[cfg(test)]
pub(super) fn read_pending_quarantine_receipts_controlled(
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> Result<Vec<PendingQuarantineReceiptV1>, CollectionFailureKind> {
    let mut receipts = Vec::new();
    for parent in [profile_root.join("stores"), profile_root.join("projects")] {
        if control.completion().is_some() {
            return Err(CollectionFailureKind::Cancelled);
        }
        let entries = match std::fs::read_dir(&parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(CollectionFailureKind::InspectFailed),
        };
        if control.completion().is_some() {
            return Err(CollectionFailureKind::Cancelled);
        }
        let mut entries = entries;
        loop {
            // `ReadDir` advances lazily. Check before calling `next` so an
            // interrupted admission does not fetch another receipt entry.
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            let Some(entry) = entries.next() else {
                break;
            };
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            let entry = entry.map_err(|_| CollectionFailureKind::InspectFailed)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(quarantine_name) = name.strip_suffix(JOURNAL_SUFFIX) else {
                continue;
            };
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            let file_type = entry
                .file_type()
                .map_err(|_| CollectionFailureKind::InspectFailed)?;
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            if file_type.is_symlink() || !file_type.is_file() {
                return Err(CollectionFailureKind::InspectFailed);
            }
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            let bytes =
                std::fs::read(entry.path()).map_err(|_| CollectionFailureKind::InspectFailed)?;
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            let journal = match serde_json::from_slice::<QuarantineJournalV1>(&bytes) {
                Ok(journal) => journal,
                Err(_) if control.completion().is_some() => {
                    return Err(CollectionFailureKind::Cancelled);
                }
                Err(_) => return Err(CollectionFailureKind::InspectFailed),
            };
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            if journal.version != 1
                || quarantine_original_name(quarantine_name) != Some(journal.original_name.as_str())
            {
                return Err(CollectionFailureKind::InspectFailed);
            }
            let original_path = parent.join(&journal.original_name);
            let quarantine_path = parent.join(quarantine_name);
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            let actual_path = receipt_actual_path(&original_path, &quarantine_path);
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            receipts.push(PendingQuarantineReceiptV1 {
                actual_path,
                quarantine_path,
                retirement_committed: parent.join(retired_marker_name(name)).is_file(),
            });
        }
    }
    Ok(receipts)
}

/// Prefer the quarantined path while it is still a regular directory. Once a
/// restore rename has succeeded, even if its parent sync or journal cleanup
/// failed, expose the original path as the bytes' actual observed location.
#[cfg(test)]
fn receipt_actual_path(original_path: &Path, quarantine_path: &Path) -> PathBuf {
    let quarantine_is_directory = std::fs::symlink_metadata(quarantine_path)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    if quarantine_is_directory {
        quarantine_path.to_path_buf()
    } else {
        let original_is_directory = std::fs::symlink_metadata(original_path)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
        if original_is_directory {
            original_path.to_path_buf()
        } else {
            quarantine_path.to_path_buf()
        }
    }
}

/// Atomically renames one sibling directory without allowing an occupied
/// destination to be replaced. Platforms without a suitable primitive fail
/// closed: retaining bytes is always preferable to risking a replacement.
fn rename_noreplace(parent: &Dir, from: &OsStr, to: &OsStr) -> std::io::Result<()> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(from.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL path"))?;
        let to = CString::new(to.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL path"))?;
        // SAFETY: both C strings are component names, and the borrowed fd is
        // the same parent directory for source and destination.
        let result = unsafe {
            libc::renameat2(
                parent.as_raw_fd(),
                from.as_ptr(),
                parent.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            return Ok(());
        }
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(from.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL path"))?;
        let to = CString::new(to.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL path"))?;
        // SAFETY: both names are component C strings and RENAME_EXCL rejects
        // an occupied destination.
        let result = unsafe {
            libc::renameatx_np(
                parent.as_raw_fd(),
                from.as_ptr(),
                parent.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result == 0 {
            return Ok(());
        }
        Err(std::io::Error::last_os_error())
    }
    #[cfg(windows)]
    {
        return parent.rename(from, parent, to);
    }
    #[cfg(not(any(
        all(target_os = "linux", target_env = "gnu"),
        target_os = "macos",
        windows
    )))]
    {
        let _ = (parent, from, to);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this platform",
        ))
    }
}

fn sync_directory(directory: &Dir) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        directory.dir_metadata().map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let mut options = OpenOptions::new();
        options.read(true).maybe_dir(true);
        directory
            .open_with(".", &options)
            .and_then(|file| file.sync_all())
    }
}

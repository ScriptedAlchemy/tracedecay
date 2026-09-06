//! Durable two-phase quarantine for destructive orphan-store retention.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use tracedecay_private_fs::capability_dir::{
    remove_open_dir_all_nofollow, rename_noreplace, sync_directory,
};
#[cfg(test)]
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::fence::{
    StoreContentFence, capture_store_content_fence_in_dir_controlled,
    open_store_directory_nofollow, open_store_parent_nofollow, profile_relative_store_path,
    store_root_identity,
};
use super::{
    CollectionControl, CollectionFailureKind, CollectionMutationFailure,
    CollectionMutationOperation, StoreRootIdentity,
};

static QUARANTINE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const QUARANTINE_ATTEMPTS: usize = 32;
const MAX_RECOVERY_JOURNAL_BYTES: u64 = 64 * 1024;
const MAX_REGISTERED_QUARANTINE_INTENTS: usize = 16_384;
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
    expected_root_identity: StoreRootIdentity,
    registry_fence: Option<QuarantineRegistryFenceV1>,
}

/// One validated registered retirement intent discovered independently of the
/// current registry census. The registry fence is the only authority the
/// caller may use to classify the interrupted database commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegisteredQuarantineIntentV1 {
    pub(super) project_id: String,
    pub(super) store_id: String,
    pub(super) quarantine_name: String,
    pub(super) quarantine_path: PathBuf,
    pub(super) original_path: PathBuf,
    pub(super) registry_fence: QuarantineRegistryFenceV1,
    pub(super) expected_root_identity: StoreRootIdentity,
}

pub(super) enum RegisteredQuarantineInventoryV1 {
    Complete(Vec<RegisteredQuarantineIntentV1>),
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RegisteredQuarantineDecisionV1 {
    Restore,
    Remove,
    Retain,
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
        failure: Option<CollectionMutationFailure>,
    },
    Restored {
        restored_path: PathBuf,
        failure: Option<CollectionMutationFailure>,
    },
    Retained {
        quarantine_path: PathBuf,
        failure: CollectionMutationFailure,
    },
}

/// A durable quarantine found on a later maintenance admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum QuarantineRecoveryOutcome {
    Removed {
        quarantine_path: PathBuf,
        journal_failure: Option<CollectionMutationFailure>,
    },
    Restored {
        restored_path: PathBuf,
        failure: Option<CollectionMutationFailure>,
    },
    Retained {
        quarantine_path: PathBuf,
        actual_path: PathBuf,
        failure: Option<CollectionMutationFailure>,
    },
}

pub(super) enum QuarantineFinalizeOutcome {
    Removed {
        journal_failure: Option<CollectionMutationFailure>,
    },
    Interrupted {
        quarantine_path: PathBuf,
    },
    DeleteUnconfirmed {
        quarantine_path: PathBuf,
        failure: CollectionMutationFailure,
    },
}

/// A verified moved directory plus its immutable, sibling journal. The
/// journal is written and synced before this value is returned; after that,
/// no crash can make the quarantine invisible to the production reader.
pub(super) struct QuarantinedStore {
    parent: Dir,
    root: Dir,
    quarantine_path: PathBuf,
    journal_name: String,
    expected_root_identity: Option<StoreRootIdentity>,
}

impl QuarantinedStore {
    pub(super) fn quarantine_path(&self) -> &Path {
        &self.quarantine_path
    }

    /// Publish the database-commit phase before removal. This marker is
    /// additive/no-replace, so a crash cannot turn a committed retirement back
    /// into an apparently prepared one by tearing an overwrite.
    pub(super) fn mark_retirement_committed(&self) -> Result<(), CollectionMutationFailure> {
        let marker_name = retired_marker_name(&self.journal_name);
        write_empty_marker(
            &self.parent,
            self.quarantine_path
                .parent()
                .map_or_else(PathBuf::new, Path::to_path_buf)
                .as_path(),
            &marker_name,
            CollectionMutationOperation::MarkRetirementCommitted,
            self.expected_root_identity.clone(),
        )
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
            expected_root_identity,
            ..
        } = self;
        if control.completion().is_some() {
            return QuarantineFinalizeOutcome::Interrupted { quarantine_path };
        }
        // The descent is capability-relative and no-follow; the interrupt
        // check runs before every child operation so a cancelled admission
        // leaves the journal and remaining bytes for the mounted reconciler.
        let interrupted = &mut || {
            if control.completion().is_some() {
                Err(interrupted_remove_error())
            } else {
                Ok(())
            }
        };
        match remove_open_dir_all_nofollow(root, interrupted) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                return QuarantineFinalizeOutcome::Interrupted { quarantine_path };
            }
            Err(error) => {
                let failure = CollectionMutationFailure::from_io_error(
                    CollectionMutationOperation::RecursiveRemove,
                    quarantine_path.clone(),
                    expected_root_identity,
                    &error,
                );
                return QuarantineFinalizeOutcome::DeleteUnconfirmed {
                    quarantine_path,
                    failure,
                };
            }
        }
        // Once the final child disappears, synchronizing the parent is part
        // of the same irreversible operation. It must complete even if the
        // admission is cancelled concurrently; otherwise a completed delete
        // could be reported without its durability boundary.
        if let Err(error) = sync_directory(&parent) {
            let failure = CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::ParentSync,
                quarantine_path
                    .parent()
                    .map_or_else(PathBuf::new, Path::to_path_buf),
                expected_root_identity,
                &error,
            );
            return QuarantineFinalizeOutcome::DeleteUnconfirmed {
                quarantine_path,
                failure,
            };
        }
        let journal_failure = clear_committed_journal(
            &parent,
            quarantine_path
                .parent()
                .map_or_else(PathBuf::new, Path::to_path_buf)
                .as_path(),
            &journal_name,
            expected_root_identity,
        )
        .err();
        QuarantineFinalizeOutcome::Removed { journal_failure }
    }
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
#[hotpath::measure(label = "maintenance.orphan_stores.quarantine")]
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
            failure: None,
        });
    }
    if *expected == StoreContentFence::Unverifiable {
        return Err(CollectionFailureKind::InspectFailed);
    }
    if *expected == StoreContentFence::Missing {
        return Ok(QuarantineStoreOutcome::Missing);
    }
    let expected_root_identity = match expected {
        StoreContentFence::Present(inventory) => Some(inventory.root.clone()),
        StoreContentFence::Missing | StoreContentFence::Unverifiable => None,
    };
    let capability = open_store_directory_nofollow(profile_root, data_root)?;
    let original_name = capability
        .leaf_name
        .to_str()
        .ok_or(CollectionFailureKind::InspectFailed)?
        .to_owned();
    let quarantine_name = reserve_quarantine_name(
        &capability.parent,
        data_root,
        &capability.leaf_name,
        expected_root_identity.clone(),
    )
    .map_err(CollectionFailureKind::RemoveFailed)?;
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
        expected_root_identity: match expected {
            StoreContentFence::Present(inventory) => inventory.root.clone(),
            StoreContentFence::Missing | StoreContentFence::Unverifiable => {
                return Err(CollectionFailureKind::InspectFailed);
            }
        },
        registry_fence,
    };
    // The journal is the intent record for the following destructive rename.
    // Publishing it first eliminates the old crash window where a synced
    // quarantine existed with no discoverable recovery authority.
    write_journal(
        &capability.parent,
        quarantine_path
            .parent()
            .ok_or(CollectionFailureKind::OutsideProfile)?,
        &journal_name,
        &journal,
        expected_root_identity.clone(),
    )
    .map_err(CollectionFailureKind::RemoveFailed)?;

    // cap_std does not open directories with FILE_SHARE_DELETE on Windows, so
    // release our leaf handle before rename. The moved bytes are reopened and
    // revalidated against the expected identity after the rename.
    drop(capability.root);
    if let Err(error) = rename_noreplace(
        &capability.parent,
        &capability.leaf_name,
        &capability.parent,
        OsStr::new(&quarantine_name),
    ) {
        let failure = CollectionMutationFailure::from_io_error(
            CollectionMutationOperation::RenameLiveLeafToQuarantine,
            data_root.to_path_buf(),
            expected_root_identity.clone(),
            &error,
        );
        let _ = clear_journal(
            &capability.parent,
            quarantine_path
                .parent()
                .ok_or(CollectionFailureKind::OutsideProfile)?,
            &journal_name,
            expected_root_identity,
        );
        // The live-leaf rename is the primary failure. Best-effort journal
        // cleanup is secondary and must never replace its operation or code.
        return Err(CollectionFailureKind::RemoveFailed(failure));
    }
    if let Err(error) = sync_directory(&capability.parent) {
        let parent_path = quarantine_path
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        let failure = CollectionMutationFailure::from_io_error(
            CollectionMutationOperation::ParentSync,
            parent_path,
            expected_root_identity.clone(),
            &error,
        );
        return Ok(recover_original_name(
            capability.parent,
            capability.leaf_name,
            quarantine_name,
            quarantine_path,
            Some(journal_name),
            expected_root_identity,
            Some(failure),
        ));
    }
    let renamed_marker = renamed_marker_name(&journal_name);
    if let Err(failure) = write_empty_marker(
        &capability.parent,
        quarantine_path
            .parent()
            .ok_or(CollectionFailureKind::OutsideProfile)?,
        &renamed_marker,
        CollectionMutationOperation::PublishQuarantineRenameMarker,
        expected_root_identity.clone(),
    ) {
        return Ok(QuarantineStoreOutcome::Interrupted {
            quarantine_path,
            failure: Some(failure),
        });
    }
    let moved_root = match capability.parent.open_dir_nofollow(&quarantine_name) {
        Ok(root) => root,
        Err(_) => {
            return Ok(recover_original_name(
                capability.parent,
                capability.leaf_name,
                quarantine_name,
                quarantine_path,
                Some(journal_name),
                expected_root_identity,
                None,
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
                expected_root_identity,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            drop(moved_root);
            Ok(QuarantineStoreOutcome::Interrupted {
                quarantine_path,
                failure: None,
            })
        }
        Ok(_) | Err(_) => {
            drop(moved_root);
            Ok(recover_original_name(
                capability.parent,
                capability.leaf_name,
                quarantine_name,
                quarantine_path,
                Some(journal_name),
                expected_root_identity,
                None,
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
    original_name: OsString,
    quarantine_name: String,
    quarantine_path: PathBuf,
    journal_name: Option<String>,
    expected_root_identity: Option<StoreRootIdentity>,
    primary_failure: Option<CollectionMutationFailure>,
) -> QuarantineStoreOutcome {
    match rename_noreplace(
        &parent,
        OsStr::new(&quarantine_name),
        &parent,
        &original_name,
    ) {
        Ok(()) => {
            // A directory sync failure occurs after the atomic rename. Preserve
            // any journal and return the true, restored path for that state.
            let parent_path = quarantine_path
                .parent()
                .map_or_else(PathBuf::new, Path::to_path_buf);
            let secondary_failure = match sync_directory(&parent) {
                Ok(()) => journal_name.and_then(|name| {
                    clear_journal(&parent, &parent_path, &name, expected_root_identity.clone())
                        .err()
                }),
                Err(error) => Some(CollectionMutationFailure::from_io_error(
                    CollectionMutationOperation::ParentSync,
                    parent_path,
                    expected_root_identity,
                    &error,
                )),
            };
            // One flat failure preserves the initiating error; restore sync
            // and cleanup errors fill the slot only when no primary exists.
            let failure = primary_failure.or(secondary_failure);
            let restored_path = quarantine_path
                .parent()
                .map_or_else(PathBuf::new, |parent| parent.join(&original_name));
            QuarantineStoreOutcome::Restored {
                restored_path,
                failure,
            }
        }
        Err(error) => {
            let restore_failure = CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::RestoreLiveLeafFromQuarantine,
                quarantine_path
                    .parent()
                    .map_or_else(PathBuf::new, |parent| parent.join(&original_name)),
                expected_root_identity,
                &error,
            );
            QuarantineStoreOutcome::Retained {
                quarantine_path,
                failure: primary_failure.unwrap_or(restore_failure),
            }
        }
    }
}

fn reserve_quarantine_name(
    parent: &Dir,
    data_root: &Path,
    original: &OsStr,
    expected_root_identity: Option<StoreRootIdentity>,
) -> Result<String, CollectionMutationFailure> {
    let Some(original) = original.to_str() else {
        return Err(CollectionMutationFailure::without_native_error(
            CollectionMutationOperation::ReserveQuarantineName,
            data_root.to_path_buf(),
            expected_root_identity,
        ));
    };
    reserve_quarantine_name_with_sequence(
        parent,
        data_root,
        original,
        expected_root_identity,
        || QUARANTINE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
    )
}

pub(super) fn reserve_quarantine_name_with_sequence(
    parent: &Dir,
    data_root: &Path,
    original: &str,
    expected_root_identity: Option<StoreRootIdentity>,
    mut next_sequence: impl FnMut() -> u64,
) -> Result<String, CollectionMutationFailure> {
    for _ in 0..QUARANTINE_ATTEMPTS {
        let sequence = next_sequence();
        let candidate = format!(
            ".tracedecay-orphan-quarantine-{original}-{}-{sequence}",
            std::process::id()
        );
        match quarantine_candidate_namespace_available(parent, &candidate) {
            Ok(true) => return Ok(candidate),
            Ok(false) => {}
            Err(error) => {
                return Err(CollectionMutationFailure::from_io_error(
                    CollectionMutationOperation::ReserveQuarantineName,
                    data_root
                        .parent()
                        .map_or_else(PathBuf::new, |parent| parent.join(candidate)),
                    expected_root_identity,
                    &error,
                ));
            }
        }
    }
    Err(CollectionMutationFailure::without_native_error(
        CollectionMutationOperation::ReserveQuarantineName,
        data_root.to_path_buf(),
        expected_root_identity,
    ))
}

pub(super) fn quarantine_candidate_namespace_available(
    parent: &Dir,
    candidate: &str,
) -> std::io::Result<bool> {
    // A journal or marker carries authority over the candidate name even when
    // its directory is gone. Reusing any part of that namespace could let a
    // new quarantine inherit stale rename or retirement authority.
    let journal = journal_name(candidate);
    let renamed_marker = renamed_marker_name(&journal);
    let retired_marker = retired_marker_name(&journal);
    for name in [
        candidate,
        journal.as_str(),
        renamed_marker.as_str(),
        retired_marker.as_str(),
    ] {
        match parent.symlink_metadata(name) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    Ok(true)
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

fn write_journal(
    parent: &Dir,
    parent_path: &Path,
    name: &str,
    journal: &QuarantineJournalV1,
    expected_root_identity: Option<StoreRootIdentity>,
) -> Result<(), CollectionMutationFailure> {
    let target_path = parent_path.join(name);
    let publish_failure = |error: &std::io::Error| {
        CollectionMutationFailure::from_io_error(
            CollectionMutationOperation::PublishQuarantineJournal,
            target_path.clone(),
            expected_root_identity.clone(),
            error,
        )
    };
    let bytes = serde_json::to_vec(journal).map_err(|error| {
        publish_failure(&std::io::Error::other(format!(
            "serialize retention journal: {error}"
        )))
    })?;
    let temporary = format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        QUARANTINE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = parent
        .open_with(&temporary, &options)
        .map_err(|error| publish_failure(&error))?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        // Preserve the publish error; temporary cleanup is best-effort only.
        let _ = parent.remove_file(&temporary);
        return Err(publish_failure(&error));
    }
    drop(file);
    if let Err(error) = rename_noreplace(parent, OsStr::new(&temporary), parent, OsStr::new(name)) {
        // Preserve the publish error; temporary cleanup is best-effort only.
        let _ = parent.remove_file(&temporary);
        return Err(publish_failure(&error));
    }
    sync_directory(parent).map_err(|error| {
        CollectionMutationFailure::from_io_error(
            CollectionMutationOperation::ParentSync,
            parent_path.to_path_buf(),
            expected_root_identity,
            &error,
        )
    })
}

fn write_empty_marker(
    parent: &Dir,
    parent_path: &Path,
    name: &str,
    operation: CollectionMutationOperation,
    expected_root_identity: Option<StoreRootIdentity>,
) -> Result<(), CollectionMutationFailure> {
    let target_path = parent_path.join(name);
    let marker_failure = |error: &std::io::Error| {
        CollectionMutationFailure::from_io_error(
            operation,
            target_path.clone(),
            expected_root_identity.clone(),
            error,
        )
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    match parent.open_with(name, &options) {
        Ok(file) => {
            file.sync_all().map_err(|error| marker_failure(&error))?;
            sync_directory(parent).map_err(|error| {
                CollectionMutationFailure::from_io_error(
                    CollectionMutationOperation::ParentSync,
                    parent_path.to_path_buf(),
                    expected_root_identity,
                    &error,
                )
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(marker_failure(&error)),
    }
}

fn clear_journal(
    parent: &Dir,
    parent_path: &Path,
    journal_name: &str,
    expected_root_identity: Option<StoreRootIdentity>,
) -> Result<(), CollectionMutationFailure> {
    clear_journal_in_order(
        parent,
        parent_path,
        journal_name,
        expected_root_identity,
        JournalCleanupState::Recoverable,
    )
}

fn clear_committed_journal(
    parent: &Dir,
    parent_path: &Path,
    journal_name: &str,
    expected_root_identity: Option<StoreRootIdentity>,
) -> Result<(), CollectionMutationFailure> {
    clear_journal_in_order(
        parent,
        parent_path,
        journal_name,
        expected_root_identity,
        JournalCleanupState::DeletionConfirmed,
    )
}

#[derive(Clone, Copy)]
enum JournalCleanupState {
    Recoverable,
    DeletionConfirmed,
}

fn journal_cleanup_names(journal_name: &str, state: JournalCleanupState) -> [String; 3] {
    let renamed = renamed_marker_name(journal_name);
    let retired = retired_marker_name(journal_name);
    match state {
        // Restore and pre-delete cleanup must keep the journal as the final
        // recovery authority if either marker cleanup is interrupted.
        JournalCleanupState::Recoverable => [renamed, retired, journal_name.to_owned()],
        // Once exact deletion is confirmed, the retired marker must remain
        // authoritative until the journal is removed. It becomes ignorable
        // orphan debris as soon as journal-driven inventory cannot see it.
        JournalCleanupState::DeletionConfirmed => [renamed, journal_name.to_owned(), retired],
    }
}

#[cfg(test)]
pub(super) fn committed_journal_cleanup_names(journal_name: &str) -> [String; 3] {
    journal_cleanup_names(journal_name, JournalCleanupState::DeletionConfirmed)
}

fn clear_journal_in_order(
    parent: &Dir,
    parent_path: &Path,
    journal_name: &str,
    expected_root_identity: Option<StoreRootIdentity>,
    state: JournalCleanupState,
) -> Result<(), CollectionMutationFailure> {
    for name in journal_cleanup_names(journal_name, state) {
        match parent.remove_file(&name) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CollectionMutationFailure::from_io_error(
                    CollectionMutationOperation::ClearRecoveryJournal,
                    parent_path.join(&name),
                    expected_root_identity,
                    &error,
                ));
            }
        }
    }
    sync_directory(parent).map_err(|error| {
        CollectionMutationFailure::from_io_error(
            CollectionMutationOperation::ParentSync,
            parent_path.to_path_buf(),
            expected_root_identity,
            &error,
        )
    })
}

/// Legacy pre-journal quarantines are restored. Unregistered journal recovery
/// uses its durable retirement marker; registered journal recovery remains
/// pending until the caller supplies a decision from the exact global row.
/// Neither path proceeds until the opened quarantine matches the journal's
/// root identity.
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
    let mut recovered_names = HashSet::new();
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
        let quarantine_name = name.strip_suffix(JOURNAL_SUFFIX).unwrap_or(name);
        if quarantine_original_name(quarantine_name) != Some(original)
            || !recovered_names.insert(quarantine_name.to_owned())
        {
            continue;
        }
        if let Some(outcome) = recover_named_store_quarantine(
            profile_root,
            data_root,
            OsStr::new(quarantine_name),
            parent_path,
        )? {
            outcomes.push(outcome);
        }
    }
    Ok(outcomes)
}

/// Returns the original project id encoded in an orphan-store quarantine name.
pub(super) fn quarantined_project_id(name: &str) -> Option<String> {
    let project_id = quarantine_original_name(name)?;
    tracedecay_runtime_core::storage::validate_project_id(project_id).ok()?;
    Some(project_id.to_owned())
}

pub(super) fn quarantine_recovery_entry(name: &str) -> Option<(String, String)> {
    let quarantine_name = name.strip_suffix(JOURNAL_SUFFIX).unwrap_or(name);
    quarantined_project_id(quarantine_name)
        .map(|project_id| (project_id, quarantine_name.to_owned()))
}

/// Inventories registered journal intents directly under `stores/`. Every
/// journal is opened no-follow, size-bounded, and fully validated before its
/// fields are exposed. Unregistered journals belong to the existing projects
/// pager and are deliberately not returned here.
pub(super) fn read_registered_quarantine_intents_controlled(
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> Result<RegisteredQuarantineInventoryV1, CollectionFailureKind> {
    let stores_path = profile_root.join("stores");
    let stores = match open_store_directory_nofollow(profile_root, &stores_path) {
        Ok(capability) => capability.root,
        Err(CollectionFailureKind::PayloadChanged) => {
            return Ok(RegisteredQuarantineInventoryV1::Complete(Vec::new()));
        }
        Err(kind) => return Err(kind),
    };
    let listing = stores
        .open_dir(Path::new("."))
        .map_err(|_| CollectionFailureKind::InspectFailed)?;
    let entries = listing
        .entries()
        .map_err(|_| CollectionFailureKind::InspectFailed)?;
    let mut intents = Vec::new();
    for entry in entries {
        if control.completion().is_some() {
            return Ok(RegisteredQuarantineInventoryV1::Interrupted);
        }
        let entry = entry.map_err(|_| CollectionFailureKind::InspectFailed)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(quarantine_name) = name.strip_suffix(JOURNAL_SUFFIX) else {
            continue;
        };
        let Some(original_name) = quarantine_original_name(quarantine_name) else {
            return Err(CollectionFailureKind::InspectFailed);
        };
        let journal = read_recovery_journal(
            &stores,
            &stores_path,
            &name,
            quarantine_name,
            OsStr::new(original_name),
        )
        .map_err(CollectionFailureKind::RemoveFailed)?
        .ok_or(CollectionFailureKind::InspectFailed)?;
        if journal.kind == QuarantineKindV1::Unregistered {
            continue;
        }
        let Some(registry_fence) = journal.registry_fence else {
            return Err(CollectionFailureKind::RemoveFailed(
                CollectionMutationFailure::without_native_error(
                    CollectionMutationOperation::ProbeRecoveryJournal,
                    stores_path.join(name),
                    Some(journal.expected_root_identity),
                ),
            ));
        };
        let original_path = stores_path.join(&journal.original_name);
        let expected_relpath = profile_relative_store_path(profile_root, &original_path)?;
        if tracedecay_runtime_core::storage::validate_project_id(&journal.project_id).is_err()
            || Path::new(&registry_fence.store_relpath) != expected_relpath
        {
            return Err(CollectionFailureKind::RemoveFailed(
                CollectionMutationFailure::without_native_error(
                    CollectionMutationOperation::ProbeRecoveryJournal,
                    stores_path.join(name),
                    Some(journal.expected_root_identity),
                ),
            ));
        }
        if intents.len() == MAX_REGISTERED_QUARANTINE_INTENTS {
            return Err(CollectionFailureKind::RemoveFailed(
                CollectionMutationFailure::without_native_error(
                    CollectionMutationOperation::ProbeRecoveryJournal,
                    stores_path,
                    None,
                ),
            ));
        }
        intents.push(RegisteredQuarantineIntentV1 {
            project_id: journal.project_id,
            store_id: journal.store_id,
            quarantine_name: quarantine_name.to_owned(),
            quarantine_path: stores_path.join(quarantine_name),
            original_path,
            registry_fence,
            expected_root_identity: journal.expected_root_identity,
        });
    }
    if control.completion().is_some() {
        Ok(RegisteredQuarantineInventoryV1::Interrupted)
    } else {
        Ok(RegisteredQuarantineInventoryV1::Complete(intents))
    }
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
    recover_named_store_quarantine_inner(
        profile_root,
        data_root,
        quarantine_name,
        parent_path,
        None,
        super::unbounded_collection_control(),
        || {},
    )
}

pub(super) fn recover_registered_quarantine_intent_controlled(
    profile_root: &Path,
    intent: &RegisteredQuarantineIntentV1,
    decision: RegisteredQuarantineDecisionV1,
    control: CollectionControl<'_>,
) -> Result<Option<QuarantineRecoveryOutcome>, CollectionFailureKind> {
    let parent_path = intent
        .original_path
        .parent()
        .ok_or(CollectionFailureKind::OutsideProfile)?;
    recover_named_store_quarantine_inner(
        profile_root,
        &intent.original_path,
        OsStr::new(&intent.quarantine_name),
        parent_path,
        Some((intent, decision)),
        control,
        || {},
    )
}

#[cfg(test)]
pub(super) fn recover_named_store_quarantine_controlled(
    profile_root: &Path,
    data_root: &Path,
    quarantine_name: &OsStr,
    parent_path: &Path,
    after_rename: impl FnOnce(),
) -> Result<Option<QuarantineRecoveryOutcome>, CollectionFailureKind> {
    recover_named_store_quarantine_inner(
        profile_root,
        data_root,
        quarantine_name,
        parent_path,
        None,
        super::unbounded_collection_control(),
        after_rename,
    )
}

fn recover_named_store_quarantine_inner(
    profile_root: &Path,
    data_root: &Path,
    quarantine_name: &OsStr,
    parent_path: &Path,
    registered: Option<(
        &RegisteredQuarantineIntentV1,
        RegisteredQuarantineDecisionV1,
    )>,
    control: CollectionControl<'_>,
    after_rename: impl FnOnce(),
) -> Result<Option<QuarantineRecoveryOutcome>, CollectionFailureKind> {
    let capability = open_store_parent_nofollow(profile_root, data_root)?;
    let quarantine_path = parent_path.join(quarantine_name);
    let quarantine_name_str = quarantine_name
        .to_str()
        .ok_or(CollectionFailureKind::InspectFailed)?;
    let journal_name = journal_name(quarantine_name_str);
    let journal = match read_recovery_journal(
        &capability.parent,
        parent_path,
        &journal_name,
        quarantine_name_str,
        &capability.leaf_name,
    ) {
        Ok(journal) => journal,
        Err(failure) => {
            return Ok(Some(QuarantineRecoveryOutcome::Retained {
                actual_path: quarantine_path.clone(),
                quarantine_path,
                failure: Some(failure),
            }));
        }
    };
    let quarantine_root = match capability.parent.open_dir_nofollow(quarantine_name) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let Some(journal) = journal else {
                return Ok(None);
            };
            // The commit authority is independent of either filesystem name.
            // Classify it before requiring a live-name identity: a completed
            // recursive delete legitimately leaves both names absent.
            let decision = match journal.kind {
                QuarantineKindV1::Registered => match registered {
                    Some((intent, decision))
                        if registered_intent_matches_journal(intent, &journal) =>
                    {
                        decision
                    }
                    Some((intent, _)) => {
                        return Ok(Some(QuarantineRecoveryOutcome::Retained {
                            failure: Some(CollectionMutationFailure::without_native_error(
                                CollectionMutationOperation::ProbeRecoveryJournal,
                                parent_path.join(&journal_name),
                                Some(intent.expected_root_identity.clone()),
                            )),
                            actual_path: quarantine_path.clone(),
                            quarantine_path,
                        }));
                    }
                    None => RegisteredQuarantineDecisionV1::Retain,
                },
                QuarantineKindV1::Unregistered => {
                    match probe_regular_recovery_marker(
                        &capability.parent,
                        parent_path,
                        &retired_marker_name(&journal_name),
                        &journal.expected_root_identity,
                    ) {
                        Ok(true) => RegisteredQuarantineDecisionV1::Remove,
                        Ok(false) => RegisteredQuarantineDecisionV1::Restore,
                        Err(failure) => {
                            return Ok(Some(QuarantineRecoveryOutcome::Retained {
                                failure: Some(failure),
                                actual_path: quarantine_path.clone(),
                                quarantine_path,
                            }));
                        }
                    }
                }
            };
            let original_identity = match child_store_identity(
                &capability.parent,
                &capability.leaf_name,
                data_root,
                &journal.expected_root_identity,
            ) {
                Ok(identity) => identity,
                Err(failure) => {
                    return Ok(Some(QuarantineRecoveryOutcome::Retained {
                        failure: Some(failure),
                        actual_path: data_root.to_path_buf(),
                        quarantine_path,
                    }));
                }
            };
            let Some(original_identity) = original_identity else {
                if decision == RegisteredQuarantineDecisionV1::Remove {
                    let journal_failure = clear_committed_journal(
                        &capability.parent,
                        parent_path,
                        &journal_name,
                        Some(journal.expected_root_identity),
                    )
                    .err();
                    return Ok(Some(QuarantineRecoveryOutcome::Removed {
                        quarantine_path,
                        journal_failure,
                    }));
                }
                return Ok(Some(QuarantineRecoveryOutcome::Retained {
                    failure: Some(CollectionMutationFailure::without_native_error(
                        CollectionMutationOperation::ValidateRestoredStoreIdentity,
                        data_root.to_path_buf(),
                        Some(journal.expected_root_identity),
                    )),
                    actual_path: quarantine_path.clone(),
                    quarantine_path,
                }));
            };
            if original_identity != journal.expected_root_identity {
                return Ok(Some(QuarantineRecoveryOutcome::Retained {
                    failure: Some(CollectionMutationFailure::without_native_error(
                        CollectionMutationOperation::ValidateRestoredStoreIdentity,
                        data_root.to_path_buf(),
                        Some(journal.expected_root_identity),
                    )),
                    actual_path: data_root.to_path_buf(),
                    quarantine_path,
                }));
            }
            if decision == RegisteredQuarantineDecisionV1::Restore {
                let failure = clear_journal(
                    &capability.parent,
                    parent_path,
                    &journal_name,
                    Some(journal.expected_root_identity),
                )
                .err();
                return Ok(Some(QuarantineRecoveryOutcome::Restored {
                    restored_path: data_root.to_path_buf(),
                    failure,
                }));
            }
            return Ok(Some(QuarantineRecoveryOutcome::Retained {
                failure: Some(CollectionMutationFailure::without_native_error(
                    CollectionMutationOperation::ValidateRestoredStoreIdentity,
                    data_root.to_path_buf(),
                    Some(journal.expected_root_identity),
                )),
                actual_path: data_root.to_path_buf(),
                quarantine_path,
            }));
        }
        Err(error) => {
            let expected_root_identity = journal
                .as_ref()
                .map(|journal| journal.expected_root_identity.clone());
            return Ok(Some(QuarantineRecoveryOutcome::Retained {
                failure: Some(CollectionMutationFailure::from_io_error(
                    CollectionMutationOperation::ValidateRestoredStoreIdentity,
                    quarantine_path.clone(),
                    expected_root_identity,
                    &error,
                )),
                actual_path: quarantine_path.clone(),
                quarantine_path,
            }));
        }
    };
    let expected_root_identity = match store_root_identity(&quarantine_root) {
        Ok(identity) => identity,
        Err(error) => {
            drop(quarantine_root);
            let expected_root_identity = journal
                .as_ref()
                .map(|journal| journal.expected_root_identity.clone());
            return Ok(Some(QuarantineRecoveryOutcome::Retained {
                failure: Some(CollectionMutationFailure::from_io_error(
                    CollectionMutationOperation::ValidateRestoredStoreIdentity,
                    quarantine_path.clone(),
                    expected_root_identity,
                    &error,
                )),
                actual_path: quarantine_path.clone(),
                quarantine_path,
            }));
        }
    };
    let Some(journal_record) = journal else {
        drop(quarantine_root);
        return Ok(Some(restore_quarantine_name(
            &capability.parent,
            &capability.leaf_name,
            quarantine_name,
            data_root,
            parent_path,
            &quarantine_path,
            &expected_root_identity,
            None,
            after_rename,
        )));
    };
    if let Some((intent, _)) = registered
        && !registered_intent_matches_journal(intent, &journal_record)
    {
        drop(quarantine_root);
        return Ok(Some(QuarantineRecoveryOutcome::Retained {
            failure: Some(CollectionMutationFailure::without_native_error(
                CollectionMutationOperation::ProbeRecoveryJournal,
                parent_path.join(&journal_name),
                Some(intent.expected_root_identity.clone()),
            )),
            actual_path: quarantine_path.clone(),
            quarantine_path,
        }));
    }
    if expected_root_identity != journal_record.expected_root_identity {
        drop(quarantine_root);
        return Ok(Some(QuarantineRecoveryOutcome::Retained {
            failure: Some(CollectionMutationFailure::without_native_error(
                CollectionMutationOperation::ValidateRestoredStoreIdentity,
                quarantine_path.clone(),
                Some(journal_record.expected_root_identity),
            )),
            actual_path: quarantine_path.clone(),
            quarantine_path,
        }));
    }
    if journal_record.kind == QuarantineKindV1::Registered {
        let Some((_, decision)) = registered else {
            drop(quarantine_root);
            return Ok(Some(QuarantineRecoveryOutcome::Retained {
                actual_path: quarantine_path.clone(),
                quarantine_path,
                failure: None,
            }));
        };
        match decision {
            RegisteredQuarantineDecisionV1::Restore => {
                drop(quarantine_root);
                return Ok(Some(restore_quarantine_name(
                    &capability.parent,
                    &capability.leaf_name,
                    quarantine_name,
                    data_root,
                    parent_path,
                    &quarantine_path,
                    &expected_root_identity,
                    Some(&journal_name),
                    after_rename,
                )));
            }
            RegisteredQuarantineDecisionV1::Retain => {
                drop(quarantine_root);
                return Ok(Some(QuarantineRecoveryOutcome::Retained {
                    actual_path: quarantine_path.clone(),
                    quarantine_path,
                    failure: None,
                }));
            }
            RegisteredQuarantineDecisionV1::Remove => {
                let quarantine = QuarantinedStore {
                    parent: capability.parent,
                    root: quarantine_root,
                    quarantine_path: quarantine_path.clone(),
                    journal_name,
                    expected_root_identity: Some(expected_root_identity),
                };
                return Ok(Some(match quarantine.finalize(control) {
                    QuarantineFinalizeOutcome::Removed { journal_failure } => {
                        QuarantineRecoveryOutcome::Removed {
                            quarantine_path,
                            journal_failure,
                        }
                    }
                    QuarantineFinalizeOutcome::Interrupted { quarantine_path } => {
                        QuarantineRecoveryOutcome::Retained {
                            actual_path: quarantine_path.clone(),
                            quarantine_path,
                            failure: None,
                        }
                    }
                    QuarantineFinalizeOutcome::DeleteUnconfirmed {
                        quarantine_path,
                        failure,
                    } => QuarantineRecoveryOutcome::Retained {
                        actual_path: quarantine_path.clone(),
                        quarantine_path,
                        failure: Some(failure),
                    },
                }));
            }
        }
    }
    let retired_name = retired_marker_name(&journal_name);
    let retirement_committed = match probe_regular_recovery_marker(
        &capability.parent,
        parent_path,
        &retired_name,
        &journal_record.expected_root_identity,
    ) {
        Ok(retirement_committed) => retirement_committed,
        Err(failure) => {
            drop(quarantine_root);
            return Ok(Some(QuarantineRecoveryOutcome::Retained {
                actual_path: quarantine_path.clone(),
                quarantine_path,
                failure: Some(failure),
            }));
        }
    };
    if !retirement_committed {
        drop(quarantine_root);
        return Ok(Some(restore_quarantine_name(
            &capability.parent,
            &capability.leaf_name,
            quarantine_name,
            data_root,
            parent_path,
            &quarantine_path,
            &expected_root_identity,
            Some(&journal_name),
            after_rename,
        )));
    }

    let quarantine = QuarantinedStore {
        parent: capability.parent,
        root: quarantine_root,
        quarantine_path: quarantine_path.clone(),
        journal_name,
        expected_root_identity: Some(expected_root_identity),
    };
    Ok(Some(
        match quarantine.finalize(super::unbounded_collection_control()) {
            QuarantineFinalizeOutcome::Removed { journal_failure } => {
                QuarantineRecoveryOutcome::Removed {
                    quarantine_path,
                    journal_failure,
                }
            }
            QuarantineFinalizeOutcome::Interrupted { quarantine_path } => {
                QuarantineRecoveryOutcome::Retained {
                    actual_path: quarantine_path.clone(),
                    quarantine_path,
                    failure: None,
                }
            }
            QuarantineFinalizeOutcome::DeleteUnconfirmed {
                quarantine_path,
                failure,
            } => QuarantineRecoveryOutcome::Retained {
                actual_path: quarantine_path.clone(),
                quarantine_path,
                failure: Some(failure),
            },
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn restore_quarantine_name(
    parent: &Dir,
    live_name: &OsStr,
    quarantine_name: &OsStr,
    data_root: &Path,
    parent_path: &Path,
    quarantine_path: &Path,
    expected_root_identity: &StoreRootIdentity,
    journal_name: Option<&str>,
    after_rename: impl FnOnce(),
) -> QuarantineRecoveryOutcome {
    match rename_noreplace(parent, quarantine_name, parent, live_name) {
        Ok(()) => {
            after_rename();
            let restored_root = match parent.open_dir_nofollow(live_name) {
                Ok(root) => root,
                Err(error) => {
                    let failure = CollectionMutationFailure::from_io_error(
                        CollectionMutationOperation::ValidateRestoredStoreIdentity,
                        data_root.to_path_buf(),
                        Some(expected_root_identity.clone()),
                        &error,
                    );
                    return retain_failed_legacy_restore(
                        parent,
                        live_name,
                        quarantine_name,
                        data_root,
                        quarantine_path,
                        expected_root_identity,
                        failure,
                    );
                }
            };
            let restored_identity = match store_root_identity(&restored_root) {
                Ok(identity) => identity,
                Err(error) => {
                    drop(restored_root);
                    let failure = CollectionMutationFailure::from_io_error(
                        CollectionMutationOperation::ValidateRestoredStoreIdentity,
                        data_root.to_path_buf(),
                        Some(expected_root_identity.clone()),
                        &error,
                    );
                    return retain_failed_legacy_restore(
                        parent,
                        live_name,
                        quarantine_name,
                        data_root,
                        quarantine_path,
                        expected_root_identity,
                        failure,
                    );
                }
            };
            if restored_identity != *expected_root_identity {
                drop(restored_root);
                let failure = CollectionMutationFailure::without_native_error(
                    CollectionMutationOperation::ValidateRestoredStoreIdentity,
                    data_root.to_path_buf(),
                    Some(expected_root_identity.clone()),
                );
                return retain_failed_legacy_restore(
                    parent,
                    live_name,
                    quarantine_name,
                    data_root,
                    quarantine_path,
                    expected_root_identity,
                    failure,
                );
            }
            drop(restored_root);
            let failure = match sync_directory(parent) {
                Ok(()) => journal_name.and_then(|journal_name| {
                    clear_journal(
                        parent,
                        parent_path,
                        journal_name,
                        Some(expected_root_identity.clone()),
                    )
                    .err()
                }),
                Err(error) => Some(CollectionMutationFailure::from_io_error(
                    CollectionMutationOperation::ParentSync,
                    parent_path.to_path_buf(),
                    Some(expected_root_identity.clone()),
                    &error,
                )),
            };
            QuarantineRecoveryOutcome::Restored {
                restored_path: data_root.to_path_buf(),
                failure,
            }
        }
        Err(error) => QuarantineRecoveryOutcome::Retained {
            failure: Some(CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::RestoreLiveLeafFromQuarantine,
                data_root.to_path_buf(),
                Some(expected_root_identity.clone()),
                &error,
            )),
            actual_path: quarantine_path.to_path_buf(),
            quarantine_path: quarantine_path.to_path_buf(),
        },
    }
}

fn retain_failed_legacy_restore(
    parent: &Dir,
    live_name: &OsStr,
    quarantine_name: &OsStr,
    data_root: &Path,
    quarantine_path: &Path,
    expected_root_identity: &StoreRootIdentity,
    primary_failure: CollectionMutationFailure,
) -> QuarantineRecoveryOutcome {
    match rename_noreplace(parent, live_name, parent, quarantine_name) {
        Ok(()) => {
            let failure = match sync_directory(parent) {
                Err(error) if primary_failure.raw_os_error.is_none() => {
                    CollectionMutationFailure::from_io_error(
                        CollectionMutationOperation::ParentSync,
                        quarantine_path
                            .parent()
                            .map_or_else(PathBuf::new, Path::to_path_buf),
                        Some(expected_root_identity.clone()),
                        &error,
                    )
                }
                Ok(()) | Err(_) => primary_failure,
            };
            QuarantineRecoveryOutcome::Retained {
                actual_path: quarantine_path.to_path_buf(),
                quarantine_path: quarantine_path.to_path_buf(),
                failure: Some(failure),
            }
        }
        Err(error) => {
            let reverse_failure = CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::RestoreLiveLeafFromQuarantine,
                quarantine_path.to_path_buf(),
                Some(expected_root_identity.clone()),
                &error,
            );
            let failure = if primary_failure.raw_os_error.is_some() {
                primary_failure
            } else {
                reverse_failure
            };
            if child_has_store_identity(parent, quarantine_name, expected_root_identity) {
                QuarantineRecoveryOutcome::Retained {
                    actual_path: quarantine_path.to_path_buf(),
                    quarantine_path: quarantine_path.to_path_buf(),
                    failure: Some(failure),
                }
            } else if child_has_store_identity(parent, live_name, expected_root_identity)
                || parent.open_dir_nofollow(live_name).is_ok()
            {
                QuarantineRecoveryOutcome::Restored {
                    restored_path: data_root.to_path_buf(),
                    failure: Some(failure),
                }
            } else {
                QuarantineRecoveryOutcome::Retained {
                    actual_path: quarantine_path.to_path_buf(),
                    quarantine_path: quarantine_path.to_path_buf(),
                    failure: Some(failure),
                }
            }
        }
    }
}

fn child_has_store_identity(parent: &Dir, name: &OsStr, expected: &StoreRootIdentity) -> bool {
    let Ok(root) = parent.open_dir_nofollow(name) else {
        return false;
    };
    store_root_identity(&root).is_ok_and(|identity| identity == *expected)
}

fn child_store_identity(
    parent: &Dir,
    name: &OsStr,
    path: &Path,
    expected: &StoreRootIdentity,
) -> Result<Option<StoreRootIdentity>, CollectionMutationFailure> {
    let root = match parent.open_dir_nofollow(name) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::ValidateRestoredStoreIdentity,
                path.to_path_buf(),
                Some(expected.clone()),
                &error,
            ));
        }
    };
    store_root_identity(&root).map(Some).map_err(|error| {
        CollectionMutationFailure::from_io_error(
            CollectionMutationOperation::ValidateRestoredStoreIdentity,
            path.to_path_buf(),
            Some(expected.clone()),
            &error,
        )
    })
}

fn registered_intent_matches_journal(
    intent: &RegisteredQuarantineIntentV1,
    journal: &QuarantineJournalV1,
) -> bool {
    journal.kind == QuarantineKindV1::Registered
        && journal.project_id == intent.project_id
        && journal.store_id == intent.store_id
        && journal.original_name
            == intent
                .original_path
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or_default()
        && journal.registry_fence.as_ref() == Some(&intent.registry_fence)
        && journal.expected_root_identity == intent.expected_root_identity
}

fn read_recovery_journal(
    parent: &Dir,
    parent_path: &Path,
    journal_name: &str,
    quarantine_name: &str,
    expected_original_name: &OsStr,
) -> Result<Option<QuarantineJournalV1>, CollectionMutationFailure> {
    let journal_path = parent_path.join(journal_name);
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = match parent.open_with(journal_name, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::ProbeRecoveryJournal,
                journal_path,
                None,
                &error,
            ));
        }
    };
    let metadata = file.metadata().map_err(|error| {
        CollectionMutationFailure::from_io_error(
            CollectionMutationOperation::ProbeRecoveryJournal,
            journal_path.clone(),
            None,
            &error,
        )
    })?;
    if !metadata.is_file() || metadata.len() > MAX_RECOVERY_JOURNAL_BYTES {
        return Err(CollectionMutationFailure::without_native_error(
            CollectionMutationOperation::ProbeRecoveryJournal,
            journal_path,
            None,
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(MAX_RECOVERY_JOURNAL_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::ProbeRecoveryJournal,
                journal_path.clone(),
                None,
                &error,
            )
        })?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > MAX_RECOVERY_JOURNAL_BYTES {
        return Err(CollectionMutationFailure::without_native_error(
            CollectionMutationOperation::ProbeRecoveryJournal,
            journal_path,
            None,
        ));
    }
    let journal = serde_json::from_slice::<QuarantineJournalV1>(&bytes).map_err(|_| {
        CollectionMutationFailure::without_native_error(
            CollectionMutationOperation::ProbeRecoveryJournal,
            journal_path.clone(),
            None,
        )
    })?;
    if journal.version != 1
        || quarantine_original_name(quarantine_name) != Some(journal.original_name.as_str())
        || expected_original_name != OsStr::new(&journal.original_name)
    {
        return Err(CollectionMutationFailure::without_native_error(
            CollectionMutationOperation::ProbeRecoveryJournal,
            journal_path,
            Some(journal.expected_root_identity),
        ));
    }
    Ok(Some(journal))
}

fn probe_regular_recovery_marker(
    parent: &Dir,
    parent_path: &Path,
    marker_name: &str,
    expected_root_identity: &StoreRootIdentity,
) -> Result<bool, CollectionMutationFailure> {
    let marker_path = parent_path.join(marker_name);
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let marker = match parent.open_with(marker_name, &options) {
        Ok(marker) => marker,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::ProbeRecoveryJournal,
                marker_path,
                Some(expected_root_identity.clone()),
                &error,
            ));
        }
    };
    let metadata = marker.metadata().map_err(|error| {
        CollectionMutationFailure::from_io_error(
            CollectionMutationOperation::ProbeRecoveryJournal,
            marker_path.clone(),
            Some(expected_root_identity.clone()),
            &error,
        )
    })?;
    if !metadata.is_file() || metadata.len() != 0 {
        return Err(CollectionMutationFailure::without_native_error(
            CollectionMutationOperation::ProbeRecoveryJournal,
            marker_path,
            Some(expected_root_identity.clone()),
        ));
    }
    Ok(true)
}

#[cfg(all(test, windows))]
pub(super) fn classify_recovery_journal_probe(
    probe: std::io::Result<cap_std::fs::Metadata>,
    journal_path: PathBuf,
    expected_root_identity: &StoreRootIdentity,
) -> Result<bool, CollectionFailureKind> {
    match probe {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(CollectionFailureKind::RemoveFailed(
            CollectionMutationFailure::from_io_error(
                CollectionMutationOperation::ProbeRecoveryJournal,
                journal_path,
                Some(expected_root_identity.clone()),
                &error,
            ),
        )),
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
        let parent_capability = match open_store_directory_nofollow(profile_root, &parent) {
            Ok(capability) => capability,
            Err(CollectionFailureKind::PayloadChanged) => continue,
            Err(kind) => return Err(kind),
        };
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
            let Some(original_name) = quarantine_original_name(quarantine_name) else {
                return Err(CollectionFailureKind::InspectFailed);
            };
            let journal = read_recovery_journal(
                &parent_capability.root,
                &parent,
                name,
                quarantine_name,
                OsStr::new(original_name),
            )
            .map_err(CollectionFailureKind::RemoveFailed)?
            .ok_or_else(|| {
                CollectionFailureKind::RemoveFailed(
                    CollectionMutationFailure::without_native_error(
                        CollectionMutationOperation::ProbeRecoveryJournal,
                        entry.path(),
                        None,
                    ),
                )
            })?;
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
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
                retirement_committed: probe_regular_recovery_marker(
                    &parent_capability.root,
                    &parent,
                    &retired_marker_name(name),
                    &journal.expected_root_identity,
                )
                .map_err(CollectionFailureKind::RemoveFailed)?,
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

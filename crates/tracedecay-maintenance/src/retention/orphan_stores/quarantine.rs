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
use std::future::Future;
use std::time::Instant;

use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

use super::fence::{
    StoreContentFence, capture_store_content_fence, capture_store_content_fence_controlled,
    capture_store_content_fence_in_dir_controlled, capture_store_directory_fence,
    data_root_fence_matches, open_store_directory_nofollow, open_store_parent_nofollow,
    profile_relative_store_path, store_root_identity,
};
use super::{
    CollectionCompletionV1, CollectionFailure, CollectionFailureKind, CollectionMutationFailure,
    CollectionMutationOperation, CollectionOutcome, CollectionPlan, CollectionRecoveryAction,
    CollectionRecoveryReceipt, CollectedStore, OrphanStoreFinding, StoreCensusEntry,
    StoreRootIdentity, UnregisteredCollectionPlan, UnregisteredStoreFinding,
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
    // The leaf handle only proved the store is present and readable. The
    // content proof re-opens the leaf under its quarantine name, so release
    // the probe before the rename: cap-std opens directories without
    // `FILE_SHARE_DELETE`, and Windows refuses to rename a directory while
    // such a handle is live.
    drop(capability.root);
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

/// Renames the quarantined leaf back to its original name. Every handle on
/// the leaf must already be closed (see the probe release before the
/// forward rename); the callers drop `moved_root` before arriving here.
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
    control: CollectionControl<'_>,
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
        if control.completion().is_some() {
            break;
        }
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
            control,
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
    control: CollectionControl<'_>,
) -> Result<Option<QuarantineRecoveryOutcome>, CollectionFailureKind> {
    recover_named_store_quarantine_inner(
        profile_root,
        data_root,
        quarantine_name,
        parent_path,
        None,
        control,
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
            let actual_path = receipt_actual_path(data_root, &quarantine_path);
            return Ok(Some(QuarantineRecoveryOutcome::Retained {
                actual_path,
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
    Ok(Some(match quarantine.finalize(control) {
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
    }))
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

// High-level registered/unregistered collection orchestration.
pub(crate) struct CollectionControl<'a> {
    cancellation: &'a CancellationToken,
    deadline: MonotonicDeadline,
}

impl<'a> CollectionControl<'a> {
    pub(crate) const fn new(
        cancellation: &'a CancellationToken,
        deadline: MonotonicDeadline,
    ) -> Self {
        Self {
            cancellation,
            deadline,
        }
    }

    pub(crate) fn completion(self) -> Option<CollectionCompletionV1> {
        if self.cancellation.is_cancelled() {
            Some(CollectionCompletionV1::Cancelled)
        } else if self.deadline.is_elapsed_at(Instant::now()) {
            Some(CollectionCompletionV1::DeadlineExceeded)
        } else {
            None
        }
    }

    /// Adapt the retention admission to the canonical `SQLite` read-snapshot
    /// control. The snapshot layer may copy/materialize a foreign database in
    /// `spawn_blocking`, so it must observe the same live cancellation and
    /// deadline rather than an unbounded root-shim control.
    pub(crate) fn snapshot_read_control(
        self,
    ) -> tracedecay_runtime_core::sqlite_read_snapshot::SnapshotReadControl {
        let cancellation = (*self.cancellation).clone();
        tracedecay_runtime_core::sqlite_read_snapshot::SnapshotReadControl::new(
            self.deadline.instant(),
            move || cancellation.is_cancelled(),
        )
    }

    /// Race an awaitable inspection or `SQLite` command against the admission's
    /// cancellation/deadline. Losing the race never authorizes the following
    /// destructive phase: callers retain their quarantine journal and let a
    /// later reconciliation inspect the durable state afresh.
    pub(crate) async fn race<T>(
        self,
        future: impl Future<Output = T>,
    ) -> Result<T, CollectionCompletionV1> {
        if let Some(completion) = self.completion() {
            return Err(completion);
        }
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => Err(CollectionCompletionV1::Cancelled),
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline.instant())) => {
                Err(CollectionCompletionV1::DeadlineExceeded)
            }
            result = future => {
                self.completion().map_or(Ok(result), Err)
            }
        }
    }
}

pub(super) fn unbounded_collection_control() -> CollectionControl<'static> {
    static CANCELLATION: std::sync::OnceLock<CancellationToken> = std::sync::OnceLock::new();
    CollectionControl::new(
        CANCELLATION.get_or_init(CancellationToken::new),
        MonotonicDeadline::at(Instant::now() + std::time::Duration::from_hours(24)),
    )
}

pub(crate) fn store_finding_is_profile_contained(
    finding: &OrphanStoreFinding,
    profile_root: &Path,
) -> bool {
    profile_relative_store_path(profile_root, &finding.data_root)
        .is_ok_and(|relative| relative == Path::new(&finding.expected_store_relpath))
        && matches!(
            capture_store_directory_fence(profile_root, &finding.data_root),
            Ok(StoreDirectoryFence::Missing | StoreDirectoryFence::Present { .. })
        )
}

fn registered_payload_fence_matches(
    finding: &OrphanStoreFinding,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> Result<bool, CollectionFailureKind> {
    if !data_root_fence_matches(
        &finding.expected_data_root_fence,
        profile_root,
        &finding.data_root,
    )? {
        return Ok(false);
    }
    match &finding.expected_data_root_fence {
        StoreDirectoryFence::Missing => Ok(true),
        StoreDirectoryFence::Present { .. } => {
            Ok(newest_mtime_secs_controlled(&finding.data_root, control)?
                == finding.expected_payload_mtime_secs)
        }
        StoreDirectoryFence::Unverifiable => Err(CollectionFailureKind::InspectFailed),
    }
}

fn unregistered_payload_fence_matches(
    finding: &UnregisteredStoreFinding,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> Result<bool, CollectionFailureKind> {
    if !data_root_fence_matches(
        &finding.expected_data_root_fence,
        profile_root,
        &finding.data_root,
    )? {
        return Ok(false);
    }
    match &finding.expected_data_root_fence {
        StoreDirectoryFence::Missing => Ok(true),
        StoreDirectoryFence::Present { .. } => {
            Ok(newest_mtime_secs_controlled(&finding.data_root, control)?
                == finding.expected_payload_mtime_secs)
        }
        StoreDirectoryFence::Unverifiable => Err(CollectionFailureKind::InspectFailed),
    }
}

/// A prepared mutation is private to its same-parent quarantine but remains
/// fully recoverable. The caller must commit registry retirement before it
/// calls [`finalize_verified_quarantine`].
enum QuarantinePreparation {
    Missing,
    Verified(QuarantinedStore),
    Interrupted,
    Failed,
}

fn prepare_verified_quarantine(
    profile_root: &Path,
    data_root: &Path,
    expected_content_fence: &StoreContentFence,
    kind: QuarantineKindV1,
    project_id: &str,
    store_id: &str,
    registry_fence: Option<QuarantineRegistryFenceV1>,
    control: CollectionControl<'_>,
    outcome: &mut CollectionOutcome,
) -> QuarantinePreparation {
    match quarantine_store_for_verified_collection_controlled(
        profile_root,
        data_root,
        expected_content_fence,
        kind,
        project_id,
        store_id,
        registry_fence,
        control,
    ) {
        Ok(QuarantineStoreOutcome::Missing) => QuarantinePreparation::Missing,
        Ok(QuarantineStoreOutcome::Verified(quarantine)) => {
            QuarantinePreparation::Verified(quarantine)
        }
        Ok(QuarantineStoreOutcome::Interrupted {
            quarantine_path,
            failure,
        }) => {
            outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                store_id: store_id.to_owned(),
                original_path: data_root.to_path_buf(),
                actual_path: quarantine_path.clone(),
                quarantine_path,
                action: CollectionRecoveryAction::RetainedForRecovery,
            });
            if let Some(failure) = failure {
                outcome.errors.push(CollectionFailure {
                    store_id: store_id.to_owned(),
                    kind: CollectionFailureKind::RemoveFailed(failure),
                });
            }
            if let Some(completion) = control.completion() {
                outcome.completion = completion;
            }
            QuarantinePreparation::Interrupted
        }
        Ok(QuarantineStoreOutcome::Restored {
            restored_path,
            failure,
        }) => {
            outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                store_id: store_id.to_owned(),
                original_path: data_root.to_path_buf(),
                quarantine_path: data_root.to_path_buf(),
                actual_path: restored_path,
                action: CollectionRecoveryAction::Restored,
            });
            outcome.errors.push(CollectionFailure {
                store_id: store_id.to_owned(),
                kind: CollectionFailureKind::PayloadChanged,
            });
            if let Some(failure) = failure {
                outcome.errors.push(CollectionFailure {
                    store_id: store_id.to_owned(),
                    kind: CollectionFailureKind::RemoveFailed(failure),
                });
            }
            QuarantinePreparation::Failed
        }
        Ok(QuarantineStoreOutcome::Retained {
            quarantine_path,
            failure,
        }) => {
            outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                store_id: store_id.to_owned(),
                original_path: data_root.to_path_buf(),
                actual_path: quarantine_path.clone(),
                quarantine_path,
                action: CollectionRecoveryAction::RetainedForRecovery,
            });
            outcome.errors.push(CollectionFailure {
                store_id: store_id.to_owned(),
                kind: CollectionFailureKind::PayloadChanged,
            });
            outcome.errors.push(CollectionFailure {
                store_id: store_id.to_owned(),
                kind: CollectionFailureKind::RemoveFailed(failure),
            });
            QuarantinePreparation::Failed
        }
        Err(kind) => {
            outcome.errors.push(CollectionFailure {
                store_id: store_id.to_owned(),
                kind,
            });
            QuarantinePreparation::Failed
        }
    }
}

fn retain_interrupted_quarantine(
    quarantine: Option<&QuarantinedStore>,
    data_root: &Path,
    store_id: &str,
    completion: CollectionCompletionV1,
    outcome: &mut CollectionOutcome,
) {
    outcome.completion = completion;
    if let Some(quarantine) = quarantine {
        outcome.recovery_receipts.push(CollectionRecoveryReceipt {
            store_id: store_id.to_owned(),
            original_path: data_root.to_path_buf(),
            quarantine_path: quarantine.quarantine_path().to_path_buf(),
            actual_path: quarantine.quarantine_path().to_path_buf(),
            action: CollectionRecoveryAction::RetainedForRecovery,
        });
    }
}

fn finalize_verified_quarantine(
    quarantine: QuarantinedStore,
    data_root: &Path,
    store_id: &str,
    control: CollectionControl<'_>,
    outcome: &mut CollectionOutcome,
) -> bool {
    if let Some(completion) = control.completion() {
        outcome.completion = completion;
        outcome.recovery_receipts.push(CollectionRecoveryReceipt {
            store_id: store_id.to_owned(),
            original_path: data_root.to_path_buf(),
            quarantine_path: quarantine.quarantine_path().to_path_buf(),
            actual_path: quarantine.quarantine_path().to_path_buf(),
            action: CollectionRecoveryAction::RetainedForRecovery,
        });
        return false;
    }
    if let Err(failure) = quarantine.mark_retirement_committed() {
        outcome.recovery_receipts.push(CollectionRecoveryReceipt {
            store_id: store_id.to_owned(),
            original_path: data_root.to_path_buf(),
            quarantine_path: quarantine.quarantine_path().to_path_buf(),
            actual_path: quarantine.quarantine_path().to_path_buf(),
            action: CollectionRecoveryAction::RetainedForRecovery,
        });
        outcome.errors.push(CollectionFailure {
            store_id: store_id.to_owned(),
            kind: CollectionFailureKind::RemoveFailed(failure),
        });
        return false;
    }
    match quarantine.finalize(control) {
        QuarantineFinalizeOutcome::Removed { journal_failure } => {
            if let Some(failure) = journal_failure {
                outcome.errors.push(CollectionFailure {
                    store_id: store_id.to_owned(),
                    kind: CollectionFailureKind::RemoveFailed(failure),
                });
            }
            true
        }
        QuarantineFinalizeOutcome::Interrupted { quarantine_path } => {
            if let Some(completion) = control.completion() {
                outcome.completion = completion;
            }
            outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                store_id: store_id.to_owned(),
                original_path: data_root.to_path_buf(),
                actual_path: quarantine_path.clone(),
                quarantine_path,
                action: CollectionRecoveryAction::RetainedForRecovery,
            });
            false
        }
        QuarantineFinalizeOutcome::DeleteUnconfirmed {
            quarantine_path,
            failure,
        } => {
            outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                store_id: store_id.to_owned(),
                original_path: data_root.to_path_buf(),
                actual_path: quarantine_path.clone(),
                quarantine_path,
                action: CollectionRecoveryAction::DeleteUnconfirmed,
            });
            outcome.errors.push(CollectionFailure {
                store_id: store_id.to_owned(),
                kind: CollectionFailureKind::RemoveFailed(failure),
            });
            false
        }
    }
}

/// Reconcile a durable interrupted quarantine before applying a fresh plan for
/// this exact live-name. Unregistered journals use their durable retirement
/// marker; registered journals remain pending unless the global-registry
/// inventory pass supplied an exact database decision. A restored or retained
/// quarantine forces a later census/confirmation pass, and recovery never
/// fabricates the old plan's byte count.
fn reconcile_existing_quarantine(
    profile_root: &Path,
    data_root: &Path,
    store_id: &str,
    outcome: &mut CollectionOutcome,
    control: CollectionControl<'_>,
) -> bool {
    let can_continue = match recover_existing_store_quarantine(profile_root, data_root, control) {
        Ok(recoveries) if recoveries.is_empty() => true,
        Ok(recoveries) => {
            let mut retained_or_restored = false;
            for recovery in recoveries {
                let recovery_receipt = match recovery {
                    QuarantineRecoveryOutcome::Removed {
                        journal_failure, ..
                    } => {
                        if let Some(failure) = journal_failure {
                            outcome.errors.push(CollectionFailure {
                                store_id: store_id.to_owned(),
                                kind: CollectionFailureKind::RemoveFailed(failure),
                            });
                        }
                        None
                    }
                    QuarantineRecoveryOutcome::Restored {
                        restored_path,
                        failure,
                    } => {
                        retained_or_restored = true;
                        let action = if failure.is_some() {
                            CollectionRecoveryAction::RetainedForRecovery
                        } else {
                            CollectionRecoveryAction::Restored
                        };
                        Some((data_root.to_path_buf(), restored_path, action, failure))
                    }
                    QuarantineRecoveryOutcome::Retained {
                        quarantine_path,
                        actual_path,
                        failure,
                    } => {
                        retained_or_restored = true;
                        Some((
                            quarantine_path.clone(),
                            actual_path,
                            CollectionRecoveryAction::RetainedForRecovery,
                            failure,
                        ))
                    }
                };
                let Some((quarantine_path, actual_path, action, failure)) = recovery_receipt else {
                    continue;
                };
                outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                    store_id: store_id.to_owned(),
                    original_path: data_root.to_path_buf(),
                    quarantine_path,
                    actual_path,
                    action,
                });
                if let Some(failure) = failure {
                    outcome.errors.push(CollectionFailure {
                        store_id: store_id.to_owned(),
                        kind: CollectionFailureKind::RemoveFailed(failure),
                    });
                }
            }
            if retained_or_restored {
                outcome.errors.push(CollectionFailure {
                    store_id: store_id.to_owned(),
                    kind: CollectionFailureKind::PayloadChanged,
                });
            }
            false
        }
        Err(kind) => {
            outcome.errors.push(CollectionFailure {
                store_id: store_id.to_owned(),
                kind,
            });
            false
        }
    };
    if let Some(completion) = control.completion() {
        outcome.completion = completion;
        false
    } else {
        can_continue
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RegisteredQuarantineRegistryStateV1 {
    Exact,
    Absent,
    Changed,
}

async fn registered_quarantine_registry_state(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    intent: &RegisteredQuarantineIntentV1,
    control: CollectionControl<'_>,
) -> tracedecay_domain::errors::Result<
    Result<RegisteredQuarantineRegistryStateV1, CollectionCompletionV1>,
> {
    let mut rows = match control
        .race(transaction.query(
            "SELECT project_id, store_relpath, created_at, last_write_at
             FROM store_instances
             WHERE store_id = ?1",
            tracedecay_runtime_core::db::engine::params![intent.store_id.as_str()],
        ))
        .await
    {
        Ok(Ok(rows)) => rows,
        Ok(Err(error)) => {
            return Err(orphan_db_error(
                "classify registered quarantine registry row",
                error,
            ));
        }
        Err(completion) => return Ok(Err(completion)),
    };
    let first = match control.race(rows.next()).await {
        Ok(Ok(row)) => row,
        Ok(Err(error)) => {
            return Err(orphan_db_error(
                "read registered quarantine registry row",
                error,
            ));
        }
        Err(completion) => return Ok(Err(completion)),
    };
    let Some(row) = first else {
        return Ok(Ok(RegisteredQuarantineRegistryStateV1::Absent));
    };
    let current = (
        row.get::<String>(0)
            .map_err(|error| orphan_db_error("decode registered quarantine project id", error))?,
        row.get::<String>(1).map_err(|error| {
            orphan_db_error("decode registered quarantine store relpath", error)
        })?,
        row.get::<i64>(2)
            .map_err(|error| orphan_db_error("decode registered quarantine created time", error))?,
        row.get::<Option<i64>>(3)
            .map_err(|error| orphan_db_error("decode registered quarantine last write", error))?,
    );
    let ambiguous = match control.race(rows.next()).await {
        Ok(Ok(row)) => row.is_some(),
        Ok(Err(error)) => {
            return Err(orphan_db_error(
                "confirm registered quarantine registry uniqueness",
                error,
            ));
        }
        Err(completion) => return Ok(Err(completion)),
    };
    if ambiguous {
        return Ok(Ok(RegisteredQuarantineRegistryStateV1::Changed));
    }
    let expected = (
        intent.project_id.clone(),
        intent.registry_fence.store_relpath.clone(),
        intent.registry_fence.created_at,
        intent.registry_fence.last_write_at,
    );
    Ok(Ok(if current == expected {
        RegisteredQuarantineRegistryStateV1::Exact
    } else {
        RegisteredQuarantineRegistryStateV1::Changed
    }))
}

async fn rollback_registered_quarantine_recovery(
    transaction: RegisteredGlobalDbWriteTransaction<'_>,
    operation: &'static str,
) -> tracedecay_domain::errors::Result<()> {
    transaction
        .rollback()
        .await
        .map_err(|error| orphan_db_error(operation, error))
}

fn record_registered_quarantine_recovery(
    intent: &RegisteredQuarantineIntentV1,
    recovery: Option<QuarantineRecoveryOutcome>,
    registry_changed: bool,
    outcome: &mut CollectionOutcome,
) {
    if registry_changed {
        outcome.errors.push(CollectionFailure {
            store_id: intent.store_id.clone(),
            kind: CollectionFailureKind::RegistryChanged,
        });
    }
    let Some(recovery) = recovery else {
        return;
    };
    match recovery {
        QuarantineRecoveryOutcome::Removed {
            journal_failure, ..
        } => {
            if let Some(failure) = journal_failure {
                outcome.errors.push(CollectionFailure {
                    store_id: intent.store_id.clone(),
                    kind: CollectionFailureKind::RemoveFailed(failure),
                });
            }
        }
        QuarantineRecoveryOutcome::Restored {
            restored_path,
            failure,
        } => {
            let action = if failure.is_some() {
                CollectionRecoveryAction::RetainedForRecovery
            } else {
                CollectionRecoveryAction::Restored
            };
            outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                store_id: intent.store_id.clone(),
                original_path: intent.original_path.clone(),
                quarantine_path: intent.quarantine_path.clone(),
                actual_path: restored_path,
                action,
            });
            if let Some(failure) = failure {
                outcome.errors.push(CollectionFailure {
                    store_id: intent.store_id.clone(),
                    kind: CollectionFailureKind::RemoveFailed(failure),
                });
            }
        }
        QuarantineRecoveryOutcome::Retained {
            quarantine_path,
            actual_path,
            failure,
        } => {
            outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                store_id: intent.store_id.clone(),
                original_path: intent.original_path.clone(),
                quarantine_path,
                actual_path,
                action: CollectionRecoveryAction::RetainedForRecovery,
            });
            if let Some(failure) = failure {
                outcome.errors.push(CollectionFailure {
                    store_id: intent.store_id.clone(),
                    kind: CollectionFailureKind::RemoveFailed(failure),
                });
            }
        }
    }
}

async fn reconcile_registered_quarantine_inventory(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    control: CollectionControl<'_>,
    outcome: &mut CollectionOutcome,
) -> tracedecay_domain::errors::Result<()> {
    reconcile_registered_quarantine_inventory_inner(
        db,
        profile_root,
        control,
        outcome,
        #[cfg(test)]
        None,
    )
    .await
}

#[cfg(test)]
pub(super) async fn reconcile_registered_quarantine_inventory_with_classified_hook(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    control: CollectionControl<'_>,
    outcome: &mut CollectionOutcome,
    mut after_classification: impl FnMut(
        &RegisteredQuarantineIntentV1,
        RegisteredQuarantineRegistryStateV1,
    ) + Send,
) -> tracedecay_domain::errors::Result<()> {
    reconcile_registered_quarantine_inventory_inner(
        db,
        profile_root,
        control,
        outcome,
        Some(&mut after_classification),
    )
    .await
}

#[cfg(test)]
type RegisteredQuarantineClassifiedHook<'a> =
    dyn FnMut(&RegisteredQuarantineIntentV1, RegisteredQuarantineRegistryStateV1) + Send + 'a;

async fn reconcile_registered_quarantine_inventory_inner(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    control: CollectionControl<'_>,
    outcome: &mut CollectionOutcome,
    #[cfg(test)] mut after_classification: Option<&mut RegisteredQuarantineClassifiedHook<'_>>,
) -> tracedecay_domain::errors::Result<()> {
    let intents = match read_registered_quarantine_intents_controlled(profile_root, control) {
        Ok(RegisteredQuarantineInventoryV1::Complete(intents)) => intents,
        Ok(RegisteredQuarantineInventoryV1::Interrupted) => {
            outcome.completion = control
                .completion()
                .unwrap_or(CollectionCompletionV1::Cancelled);
            return Ok(());
        }
        Err(CollectionFailureKind::Cancelled) => {
            outcome.completion = control
                .completion()
                .unwrap_or(CollectionCompletionV1::Cancelled);
            return Ok(());
        }
        Err(kind) => {
            outcome.errors.push(CollectionFailure {
                store_id: "registered-quarantine-inventory".to_owned(),
                kind,
            });
            return Ok(());
        }
    };
    for intent in intents {
        if let Some(completion) = control.completion() {
            outcome.completion = completion;
            break;
        }
        let transaction = match control.race(db.begin_write_transaction()).await {
            Ok(Ok(transaction)) => transaction,
            Ok(Err(error)) => return Err(error),
            Err(completion) => {
                outcome.completion = completion;
                break;
            }
        };
        let registry_state =
            match registered_quarantine_registry_state(&transaction, &intent, control).await {
                Ok(Ok(state)) => state,
                Ok(Err(completion)) => {
                    rollback_registered_quarantine_recovery(
                        transaction,
                        "rollback interrupted registered quarantine classification",
                    )
                    .await?;
                    outcome.completion = completion;
                    break;
                }
                Err(error) => {
                    if let Err(rollback_error) = transaction.rollback().await {
                        return Err(orphan_db_error(
                            "rollback failed registered quarantine classification",
                            format!("{error}; rollback failed: {rollback_error}"),
                        ));
                    }
                    return Err(error);
                }
            };
        #[cfg(test)]
        if let Some(after_classification) = after_classification.as_deref_mut() {
            after_classification(&intent, registry_state);
        }
        let (decision, registry_changed) = match registry_state {
            RegisteredQuarantineRegistryStateV1::Exact => {
                (RegisteredQuarantineDecisionV1::Restore, false)
            }
            RegisteredQuarantineRegistryStateV1::Absent => {
                (RegisteredQuarantineDecisionV1::Remove, false)
            }
            RegisteredQuarantineRegistryStateV1::Changed => {
                (RegisteredQuarantineDecisionV1::Retain, true)
            }
        };
        if let Some(completion) = control.completion() {
            rollback_registered_quarantine_recovery(
                transaction,
                "rollback interrupted registered quarantine recovery",
            )
            .await?;
            outcome.completion = completion;
            break;
        }
        let recovery = match recover_registered_quarantine_intent_controlled(
            profile_root,
            &intent,
            decision,
            control,
        ) {
            Ok(recovery) => recovery,
            Err(CollectionFailureKind::Cancelled) => {
                rollback_registered_quarantine_recovery(
                    transaction,
                    "rollback interrupted registered quarantine recovery",
                )
                .await?;
                outcome.completion = control
                    .completion()
                    .unwrap_or(CollectionCompletionV1::Cancelled);
                break;
            }
            Err(kind) => {
                rollback_registered_quarantine_recovery(
                    transaction,
                    "rollback failed registered quarantine recovery",
                )
                .await?;
                outcome.errors.push(CollectionFailure {
                    store_id: intent.store_id.clone(),
                    kind,
                });
                continue;
            }
        };
        rollback_registered_quarantine_recovery(
            transaction,
            "rollback completed registered quarantine recovery",
        )
        .await?;
        record_registered_quarantine_recovery(&intent, recovery, registry_changed, outcome);
        if let Some(completion) = control.completion() {
            outcome.completion = completion;
            break;
        }
    }
    Ok(())
}

/// Executes registered collection in two phases: expensive inspection and a
/// same-parent quarantine run without a writer; a short final transaction then
/// retires the exact registry row before irreversible quarantine deletion.
pub async fn execute_registered_collection(
    db: &RegisteredGlobalDb,
    plan: &CollectionPlan,
    profile_root: &Path,
) -> tracedecay_domain::errors::Result<(CollectionOutcome, usize)> {
    execute_registered_collection_controlled(db, plan, profile_root, unbounded_collection_control())
        .await
}

#[hotpath::measure(label = "maintenance.orphan_stores.collect_registered", future = true)]
pub(crate) async fn execute_registered_collection_controlled(
    db: &RegisteredGlobalDb,
    plan: &CollectionPlan,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> tracedecay_domain::errors::Result<(CollectionOutcome, usize)> {
    let mut outcome = CollectionOutcome::default();
    let mut retired = 0usize;
    reconcile_registered_quarantine_inventory(db, profile_root, control, &mut outcome).await?;
    if outcome.completion != CollectionCompletionV1::Complete {
        return Ok((outcome, retired));
    }
    for finding in &plan.collect {
        if let Some(completion) = control.completion() {
            outcome.completion = completion;
            break;
        }
        if !reconcile_existing_quarantine(
            profile_root,
            &finding.data_root,
            &finding.store_id,
            &mut outcome,
            control,
        ) {
            continue;
        }
        if !store_finding_is_profile_contained(finding, profile_root) {
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::OutsideProfile,
            });
            continue;
        }
        match registered_payload_fence_matches(finding, profile_root, control) {
            Ok(true) => {}
            Ok(false) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::PayloadChanged,
                });
                continue;
            }
            Err(CollectionFailureKind::Cancelled) => {
                outcome.completion = control
                    .completion()
                    .unwrap_or(CollectionCompletionV1::Cancelled);
                break;
            }
            Err(kind) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind,
                });
                continue;
            }
        }

        let current_stores = match control
            .race(db.try_list_store_instances_for_project(&finding.project_id))
            .await
        {
            Ok(Ok(stores)) => stores,
            Ok(Err(error)) => return Err(error),
            Err(completion) => {
                outcome.completion = completion;
                break;
            }
        };
        let current = current_stores
            .into_iter()
            .find(|store| store.store_id == finding.store_id)
            .map(|store| (store.store_relpath, store.created_at, store.last_write_at));
        if current
            != Some((
                finding.expected_store_relpath.clone(),
                finding.expected_created_at,
                finding.expected_last_write_at,
            ))
        {
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::RegistryChanged,
            });
            continue;
        }

        let manifest_path = finding
            .data_root
            .join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
        let current_manifest = match read_regular_file(&manifest_path) {
            RegularFileSnapshot::Bytes(bytes) => Some(bytes),
            RegularFileSnapshot::Missing => None,
            RegularFileSnapshot::Unverifiable => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::InspectFailed,
                });
                continue;
            }
        };
        if current_manifest != finding.expected_manifest_bytes {
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::ManifestChanged,
            });
            continue;
        }
        match registered_payload_fence_matches(finding, profile_root, control) {
            Ok(true) => {}
            Ok(false) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::PayloadChanged,
                });
                continue;
            }
            Err(CollectionFailureKind::Cancelled) => {
                outcome.completion = control
                    .completion()
                    .unwrap_or(CollectionCompletionV1::Cancelled);
                break;
            }
            Err(kind) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind,
                });
                continue;
            }
        }

        let scratch_root = durable_check_scratch_root(profile_root);
        match check_store_durable_memory(
            &finding.data_root,
            finding.expected_manifest_bytes.as_deref(),
            &finding.graph_scope_relpaths,
            &scratch_root,
            control,
        )
        .await
        {
            DurableMemoryCheck::Empty => {}
            DurableMemoryCheck::Present | DurableMemoryCheck::Unverifiable => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::DurableDataProtected,
                });
                continue;
            }
            DurableMemoryCheck::Interrupted => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::Cancelled,
                });
                outcome.completion = control
                    .completion()
                    .unwrap_or(CollectionCompletionV1::Cancelled);
                break;
            }
        }

        // The durable inventory can take a private snapshot and therefore
        // leaves a window for a concurrent replacement. Re-prove the exact
        // directory generation immediately before destructive removal.
        match registered_payload_fence_matches(finding, profile_root, control) {
            Ok(true) => {}
            Ok(false) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind: CollectionFailureKind::PayloadChanged,
                });
                continue;
            }
            Err(CollectionFailureKind::Cancelled) => {
                outcome.completion = control
                    .completion()
                    .unwrap_or(CollectionCompletionV1::Cancelled);
                break;
            }
            Err(kind) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.store_id.clone(),
                    kind,
                });
                continue;
            }
        }

        let quarantine = match prepare_verified_quarantine(
            profile_root,
            &finding.data_root,
            &finding.expected_content_fence,
            QuarantineKindV1::Registered,
            &finding.project_id,
            &finding.store_id,
            Some(QuarantineRegistryFenceV1 {
                store_relpath: finding.expected_store_relpath.clone(),
                created_at: finding.expected_created_at,
                last_write_at: finding.expected_last_write_at,
            }),
            control,
            &mut outcome,
        ) {
            QuarantinePreparation::Missing => None,
            QuarantinePreparation::Verified(quarantine) => Some(quarantine),
            QuarantinePreparation::Interrupted | QuarantinePreparation::Failed => {
                continue;
            }
        };
        let transaction = match control.race(db.begin_write_transaction()).await {
            Ok(Ok(transaction)) => transaction,
            Ok(Err(error)) => return Err(error),
            Err(completion) => {
                retain_interrupted_quarantine(
                    quarantine.as_ref(),
                    &finding.data_root,
                    &finding.store_id,
                    completion,
                    &mut outcome,
                );
                break;
            }
        };
        let mut rows = match control
            .race(transaction.query(
                "SELECT store_relpath, created_at, last_write_at
                 FROM store_instances
                 WHERE project_id = ?1 AND store_id = ?2",
                tracedecay_runtime_core::db::engine::params![
                    finding.project_id.as_str(),
                    finding.store_id.as_str()
                ],
            ))
            .await
        {
            Ok(Ok(rows)) => rows,
            Ok(Err(error)) => {
                return Err(orphan_db_error(
                    "confirm quarantined orphan registry",
                    error,
                ));
            }
            Err(completion) => {
                drop(transaction);
                retain_interrupted_quarantine(
                    quarantine.as_ref(),
                    &finding.data_root,
                    &finding.store_id,
                    completion,
                    &mut outcome,
                );
                break;
            }
        };
        let next = match control.race(rows.next()).await {
            Ok(Ok(next)) => next,
            Ok(Err(error)) => {
                return Err(orphan_db_error("read quarantined orphan registry", error));
            }
            Err(completion) => {
                drop(rows);
                drop(transaction);
                retain_interrupted_quarantine(
                    quarantine.as_ref(),
                    &finding.data_root,
                    &finding.store_id,
                    completion,
                    &mut outcome,
                );
                break;
            }
        };
        let current = match next {
            Some(row) => Some((
                row.get::<String>(0)
                    .map_err(|error| orphan_db_error("decode orphan store relpath", error))?,
                row.get::<i64>(1)
                    .map_err(|error| orphan_db_error("decode orphan store generation", error))?,
                row.get::<Option<i64>>(2)
                    .map_err(|error| orphan_db_error("decode orphan last write", error))?,
            )),
            None => None,
        };
        drop(rows);
        if current
            != Some((
                finding.expected_store_relpath.clone(),
                finding.expected_created_at,
                finding.expected_last_write_at,
            ))
        {
            match control.race(transaction.rollback()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    return Err(orphan_db_error(
                        "rollback changed quarantined orphan",
                        error,
                    ));
                }
                Err(completion) => {
                    retain_interrupted_quarantine(
                        quarantine.as_ref(),
                        &finding.data_root,
                        &finding.store_id,
                        completion,
                        &mut outcome,
                    );
                    break;
                }
            }
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::RegistryChanged,
            });
            continue;
        }
        let deleted = match control
            .race(transaction.execute(
                "DELETE FROM store_instances
                 WHERE project_id = ?1 AND store_id = ?2
                   AND store_relpath = ?3 AND created_at = ?4
                   AND last_write_at IS ?5",
                tracedecay_runtime_core::db::engine::params![
                    finding.project_id.as_str(),
                    finding.store_id.as_str(),
                    finding.expected_store_relpath.as_str(),
                    finding.expected_created_at,
                    finding.expected_last_write_at
                ],
            ))
            .await
        {
            Ok(Ok(deleted)) => deleted,
            Ok(Err(error)) => return Err(orphan_db_error("retire collected orphan store", error)),
            Err(completion) => {
                drop(transaction);
                retain_interrupted_quarantine(
                    quarantine.as_ref(),
                    &finding.data_root,
                    &finding.store_id,
                    completion,
                    &mut outcome,
                );
                break;
            }
        };
        if deleted != 1 {
            match control.race(transaction.rollback()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    return Err(orphan_db_error("rollback raced orphan retirement", error));
                }
                Err(completion) => {
                    retain_interrupted_quarantine(
                        quarantine.as_ref(),
                        &finding.data_root,
                        &finding.store_id,
                        completion,
                        &mut outcome,
                    );
                    break;
                }
            }
            outcome.errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::RegistryChanged,
            });
            continue;
        }
        match control
            .race(transaction.execute(
                "DELETE FROM code_projects
                 WHERE project_id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM store_instances WHERE project_id = ?1
                )",
                tracedecay_runtime_core::db::engine::params![finding.project_id.as_str()],
            ))
            .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err(orphan_db_error("retire empty collected project", error)),
            Err(completion) => {
                drop(transaction);
                retain_interrupted_quarantine(
                    quarantine.as_ref(),
                    &finding.data_root,
                    &finding.store_id,
                    completion,
                    &mut outcome,
                );
                break;
            }
        }
        match control.race(transaction.commit()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(orphan_db_error("commit collected orphan retirement", error));
            }
            Err(completion) => {
                retain_interrupted_quarantine(
                    quarantine.as_ref(),
                    &finding.data_root,
                    &finding.store_id,
                    completion,
                    &mut outcome,
                );
                break;
            }
        }

        retired = retired.saturating_add(1);
        if let Some(quarantine) = quarantine
            && !finalize_verified_quarantine(
                quarantine,
                &finding.data_root,
                &finding.store_id,
                control,
                &mut outcome,
            )
        {
            continue;
        }
        outcome.reclaimed_bytes = outcome.reclaimed_bytes.saturating_add(finding.size_bytes);
        outcome.collected.push(CollectedStore {
            project_id: finding.project_id.clone(),
            store_id: finding.store_id.clone(),
            data_root: finding.data_root.clone(),
            size_bytes: finding.size_bytes,
        });
    }
    Ok((outcome, retired))
}

fn orphan_db_error(
    operation: &'static str,
    error: impl std::fmt::Display,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Database {
        operation: operation.to_string(),
        message: error.to_string(),
    }
}

/// Result of checking a store's graph database for durable memory rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DurableMemoryCheck {
    /// Cooperative cancellation/deadline interrupted recursive discovery or a
    /// bounded database probe before any mutation.
    Interrupted,
    /// No durable memory table has any row (including: none of the tables
    /// exist, or the database file itself does not exist). Safe to collect.
    Empty,
    /// At least one durable memory table has at least one row.
    Present,
    /// The check could not prove the store is free of durable memory rows
    /// (I/O error, corrupt/locked database, the source changed mid-check).
    /// Fails closed: treated exactly like `Present` by every caller.
    Unverifiable,
}

/// Every database under a store that can carry durable rows, or a typed
/// statement that the inventory itself could not be trusted.
///
/// The databases registered as project authorities for durable memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DurableDatabaseInventoryV1 {
    /// The bounded scan stopped before it could establish a complete durable
    /// database inventory. This is not an unverifiable green light: callers
    /// preserve the exact cancellation/deadline state for the coordinator.
    Interrupted,
    /// The complete set of database paths, relative to the store's data root.
    Resolved(Vec<PathBuf>),
    /// The set could not be enumerated — a missing or malformed manifest, or a
    /// directory that could not be listed. Never a green light for deletion.
    Unverifiable,
}

/// A regular-file read that preserves the difference between an absent
/// optional artifact and an unsafe/unreadable one. In particular, `read()`
/// follows symlinks; retention must never turn a symlinked manifest into a
/// trusted manifest snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RegularFileSnapshot {
    Missing,
    Bytes(Vec<u8>),
    Unverifiable,
}

fn read_regular_file(path: &Path) -> RegularFileSnapshot {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RegularFileSnapshot::Missing;
        }
        Err(_) => return RegularFileSnapshot::Unverifiable,
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return RegularFileSnapshot::Unverifiable;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return RegularFileSnapshot::Unverifiable;
    };
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.file_type().is_file() => {
            RegularFileSnapshot::Bytes(bytes)
        }
        _ => RegularFileSnapshot::Unverifiable,
    }
}

/// Store manifests and registry-provided graph scopes are path authorities,
/// not arbitrary filesystem paths. Only normalized, non-empty relative paths
/// made entirely from normal components are accepted; `..`, `.`, roots,
/// prefixes, and empty paths all fail closed before joining.
fn safe_store_relative_path(path: &Path) -> bool {
    let mut saw_normal = false;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if let std::path::Component::Normal(component) = component {
            saw_normal = true;
            normalized.push(component);
        } else {
            return false;
        }
    }
    saw_normal && normalized == path
}

/// Reject symlinked directory components as well as a symlinked final file.
/// A lexical relative-path check alone is insufficient when an intermediate
/// directory redirects outside the store.
fn safe_store_path(data_root: &Path, relative: &Path) -> bool {
    if !safe_store_relative_path(relative) {
        return false;
    }
    let mut current = data_root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return false;
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return false,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        }
    }
    true
}

/// Enumerates every durable database under `data_root`.
///
/// Fails closed. The manifest is the store's own record of where its graph
/// lives; if it is absent or will not parse, guessing the default filename
/// would check the wrong file (or no file) and report "empty" for a store whose
/// real graph sits elsewhere.
pub(super) fn durable_database_inventory(
    data_root: &Path,
    manifest_bytes: Option<&[u8]>,
    graph_scope_relpaths: &[PathBuf],
    control: CollectionControl<'_>,
) -> DurableDatabaseInventoryV1 {
    if control.completion().is_some() {
        return DurableDatabaseInventoryV1::Interrupted;
    }
    let Some(bytes) = manifest_bytes else {
        return DurableDatabaseInventoryV1::Unverifiable;
    };
    let manifest =
        match serde_json::from_slice::<tracedecay_runtime_core::storage::StoreManifest>(bytes) {
            Ok(manifest) => manifest,
            Err(_) if control.completion().is_some() => {
                return DurableDatabaseInventoryV1::Interrupted;
            }
            Err(_) => return DurableDatabaseInventoryV1::Unverifiable,
        };
    if control.completion().is_some() {
        return DurableDatabaseInventoryV1::Interrupted;
    }

    if !safe_store_relative_path(&manifest.graph_db_relpath) {
        return DurableDatabaseInventoryV1::Unverifiable;
    }

    let mut inventory = vec![manifest.graph_db_relpath];
    for relpath in graph_scope_relpaths {
        if control.completion().is_some() {
            return DurableDatabaseInventoryV1::Interrupted;
        }
        if !safe_store_relative_path(relpath) {
            return DurableDatabaseInventoryV1::Unverifiable;
        }
        if !inventory.contains(relpath) {
            inventory.push(relpath.clone());
        }
    }

    // Durable facts are project-wide and outlive the branch they were written
    // on, so a branch database can hold the only surviving rows. The manifest
    // does not name them; an unlistable directory is therefore unverifiable,
    // not empty.
    let branches = data_root.join("branches");
    let branches_metadata = std::fs::symlink_metadata(&branches);
    if control.completion().is_some() {
        return DurableDatabaseInventoryV1::Interrupted;
    }
    match branches_metadata {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() => {
            return DurableDatabaseInventoryV1::Unverifiable;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DurableDatabaseInventoryV1::Resolved(inventory);
        }
        Err(_) => return DurableDatabaseInventoryV1::Unverifiable,
    }
    let branch_entries = std::fs::read_dir(&branches);
    if control.completion().is_some() {
        return DurableDatabaseInventoryV1::Interrupted;
    }
    match branch_entries {
        Ok(entries) => {
            let mut entries = entries;
            loop {
                // `ReadDir` fetches lazily, so control must be checked before
                // every `next` rather than only before opening `branches`.
                if control.completion().is_some() {
                    return DurableDatabaseInventoryV1::Interrupted;
                }
                let Some(entry) = entries.next() else {
                    break;
                };
                if control.completion().is_some() {
                    return DurableDatabaseInventoryV1::Interrupted;
                }
                let Ok(entry) = entry else {
                    return DurableDatabaseInventoryV1::Unverifiable;
                };
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("db") {
                    continue;
                }
                if control.completion().is_some() {
                    return DurableDatabaseInventoryV1::Interrupted;
                }
                let Ok(file_type) = entry.file_type() else {
                    return DurableDatabaseInventoryV1::Unverifiable;
                };
                if control.completion().is_some() {
                    return DurableDatabaseInventoryV1::Interrupted;
                }
                if file_type.is_symlink() || !file_type.is_file() {
                    return DurableDatabaseInventoryV1::Unverifiable;
                }
                let Some(name) = path.file_name() else {
                    continue;
                };
                let relpath = Path::new("branches").join(name);
                if !inventory.contains(&relpath) {
                    inventory.push(relpath);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return DurableDatabaseInventoryV1::Unverifiable,
    }

    if control.completion().is_some() {
        return DurableDatabaseInventoryV1::Interrupted;
    }
    DurableDatabaseInventoryV1::Resolved(inventory)
}

/// Runs [`check_durable_memory_rows`] over every database in the store's
/// inventory. Any single `Present` or `Unverifiable` protects the whole store.
pub(super) async fn check_store_durable_memory(
    data_root: &Path,
    manifest_bytes: Option<&[u8]>,
    graph_scope_relpaths: &[PathBuf],
    scratch_root: &Path,
    control: CollectionControl<'_>,
) -> DurableMemoryCheck {
    if control.completion().is_some() {
        return DurableMemoryCheck::Interrupted;
    }
    let inventory = match durable_database_inventory(
        data_root,
        manifest_bytes,
        graph_scope_relpaths,
        control,
    ) {
        DurableDatabaseInventoryV1::Interrupted => return DurableMemoryCheck::Interrupted,
        DurableDatabaseInventoryV1::Resolved(inventory) => inventory,
        DurableDatabaseInventoryV1::Unverifiable => return DurableMemoryCheck::Unverifiable,
    };
    for relpath in inventory {
        if control.completion().is_some() {
            return DurableMemoryCheck::Interrupted;
        }
        match check_durable_memory_rows(data_root, &relpath, scratch_root, control).await {
            DurableMemoryCheck::Empty => {}
            protected => return protected,
        }
    }
    DurableMemoryCheck::Empty
}

/// The read-snapshot scratch directory for durable-memory checks.
///
/// It lives under the *profile* root, never inside the store being examined.
/// Two reasons, both load-bearing: the store is a deletion candidate, and
/// writing into it bumps the newest mtime that
/// [`walk_store_stats`] uses as the revival fence — a store that failed one
/// check would have its age reset by the check itself and could never mature
/// past the retention window again.
pub(super) fn durable_check_scratch_root(profile_root: &Path) -> PathBuf {
    profile_root.join("scratch").join("sqlite-read")
}

/// Checks whether `data_root`'s graph database carries rows in any canonical
/// `memory_*` table. This intentionally discovers tables from the schema
/// instead of maintaining a fixed list: both legacy memory and Memory V2 add
/// durable tables, and a newly added table must be protected automatically.
/// Side-effect-free with respect to the store: opens the database through
/// [`tracedecay_runtime_core::sqlite_read_snapshot`], so the live store is never mutated or
/// locked against a concurrent writer.
async fn check_durable_memory_rows(
    data_root: &Path,
    graph_db_relpath: &Path,
    scratch_root: &Path,
    control: CollectionControl<'_>,
) -> DurableMemoryCheck {
    if control.completion().is_some() {
        return DurableMemoryCheck::Interrupted;
    }
    if !safe_store_path(data_root, graph_db_relpath) {
        return DurableMemoryCheck::Unverifiable;
    }
    let graph_db_path = data_root.join(graph_db_relpath);
    match std::fs::symlink_metadata(&graph_db_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            return DurableMemoryCheck::Unverifiable;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // No database file at all: there is no schema that could carry
            // durable rows.
            return DurableMemoryCheck::Empty;
        }
        Err(_) => return DurableMemoryCheck::Unverifiable,
    }
    // The snapshot layer creates only the final scratch component, so its
    // parent must exist first. Without this the snapshot fails NotFound, the
    // check fails closed as `Unverifiable`, and — because `Unverifiable` is
    // treated exactly like `Present` — *every* collection is refused. That is
    // safe, but it silently disables orphan reclamation entirely.
    if control.completion().is_some() || std::fs::create_dir_all(scratch_root).is_err() {
        return if control.completion().is_some() {
            DurableMemoryCheck::Interrupted
        } else {
            DurableMemoryCheck::Unverifiable
        };
    }
    let snapshot = match control
        .race(
            tracedecay_runtime_core::sqlite_read_snapshot::open_foreign_in(
                &graph_db_path,
                scratch_root,
                control.snapshot_read_control(),
            ),
        )
        .await
    {
        Err(_) => return DurableMemoryCheck::Interrupted,
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(_)) => return DurableMemoryCheck::Unverifiable,
    };
    if control.completion().is_some() {
        return DurableMemoryCheck::Interrupted;
    }
    let connection = snapshot.connection();
    let mut rows = match control
        .race(connection.query(
            "SELECT name
             FROM pragma_table_list
             WHERE schema = 'main'
               AND type = 'table'
               AND name LIKE ?1 ESCAPE '\\'
             ORDER BY name",
            tracedecay_runtime_core::db::engine::params!["memory\\_%"],
        ))
        .await
    {
        Err(_) => return DurableMemoryCheck::Interrupted,
        Ok(Ok(rows)) => rows,
        Ok(Err(_)) => return DurableMemoryCheck::Unverifiable,
    };
    let mut present_tables = Vec::new();
    loop {
        let next = match control.race(rows.next()).await {
            Err(_) => return DurableMemoryCheck::Interrupted,
            Ok(Ok(next)) => next,
            Ok(Err(_)) => return DurableMemoryCheck::Unverifiable,
        };
        match next {
            Some(row) => match row.get::<String>(0) {
                Ok(name) => present_tables.push(name),
                Err(_) => return DurableMemoryCheck::Unverifiable,
            },
            None => break,
        }
    }
    drop(rows);
    for table in present_tables {
        // `pragma_table_list.type = 'table'` intentionally excludes FTS
        // virtual/shadow tables, whose internal config rows are derived and
        // exist even when there is no durable memory. Identifiers cannot be
        // SQL parameters, so only interpolate TraceDecay's canonical shape;
        // an unexpected name fails closed rather than becoming SQL text.
        if !is_memory_table_identifier(&table) {
            return DurableMemoryCheck::Unverifiable;
        }
        let probe_sql = format!("SELECT 1 FROM \"{table}\" LIMIT 1");
        let mut probe_rows = match control.race(connection.query(&probe_sql, ())).await {
            Err(_) => return DurableMemoryCheck::Interrupted,
            Ok(Ok(rows)) => rows,
            Ok(Err(_)) => return DurableMemoryCheck::Unverifiable,
        };
        match control.race(probe_rows.next()).await {
            Err(_) => return DurableMemoryCheck::Interrupted,
            Ok(Ok(Some(_))) => return DurableMemoryCheck::Present,
            Ok(Ok(None)) => {}
            Ok(Err(_)) => return DurableMemoryCheck::Unverifiable,
        }
    }
    if control.completion().is_some() {
        return DurableMemoryCheck::Interrupted;
    }
    if snapshot.validate_source().is_err() {
        // The file changed under us mid-check: cannot trust an empty result.
        return DurableMemoryCheck::Unverifiable;
    }
    DurableMemoryCheck::Empty
}

fn is_memory_table_identifier(table: &str) -> bool {
    table.strip_prefix("memory_").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}
pub(crate) async fn execute_unregistered_collection(
    db: &RegisteredGlobalDb,
    plan: &UnregisteredCollectionPlan,
    profile_root: &Path,
) -> tracedecay_domain::errors::Result<CollectionOutcome> {
    execute_unregistered_collection_controlled(
        db,
        plan,
        profile_root,
        unbounded_collection_control(),
    )
    .await
}

#[hotpath::measure(
    label = "maintenance.orphan_stores.collect_unregistered",
    future = true
)]
pub(crate) async fn execute_unregistered_collection_controlled(
    db: &RegisteredGlobalDb,
    plan: &UnregisteredCollectionPlan,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> tracedecay_domain::errors::Result<CollectionOutcome> {
    let mut outcome = CollectionOutcome::default();
    for finding in &plan.collect {
        if let Some(completion) = control.completion() {
            outcome.completion = completion;
            break;
        }
        if !reconcile_existing_quarantine(
            profile_root,
            &finding.data_root,
            &finding.project_dir_name,
            &mut outcome,
            control,
        ) {
            continue;
        }
        // Containment + shape: only ever delete an exact, safely-named
        // `<profile>/projects/<id>` leaf.
        let expected = profile_root
            .join("projects")
            .join(&finding.project_dir_name);
        if expected != finding.data_root
            || tracedecay_runtime_core::storage::validate_project_id(&finding.project_dir_name)
                .is_err()
        {
            outcome.errors.push(CollectionFailure {
                store_id: finding.project_dir_name.clone(),
                kind: CollectionFailureKind::OutsideProfile,
            });
            continue;
        }
        match unregistered_payload_fence_matches(finding, profile_root, control) {
            Ok(true) => {}
            Ok(false) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.project_dir_name.clone(),
                    kind: CollectionFailureKind::PayloadChanged,
                });
                continue;
            }
            Err(CollectionFailureKind::Cancelled) => {
                outcome.completion = control
                    .completion()
                    .unwrap_or(CollectionCompletionV1::Cancelled);
                break;
            }
            Err(kind) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.project_dir_name.clone(),
                    kind,
                });
                continue;
            }
        }

        let now_registered = match control
            .race(db.code_project_exists(&finding.project_dir_name))
            .await
        {
            Ok(Ok(exists)) => exists,
            Ok(Err(error)) => return Err(error),
            Err(completion) => {
                outcome.completion = completion;
                break;
            }
        };
        if now_registered {
            outcome.errors.push(CollectionFailure {
                store_id: finding.project_dir_name.clone(),
                kind: CollectionFailureKind::RegistryChanged,
            });
            continue;
        }
        match unregistered_payload_fence_matches(finding, profile_root, control) {
            Ok(true) => {}
            Ok(false) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.project_dir_name.clone(),
                    kind: CollectionFailureKind::PayloadChanged,
                });
                continue;
            }
            Err(CollectionFailureKind::Cancelled) => {
                outcome.completion = control
                    .completion()
                    .unwrap_or(CollectionCompletionV1::Cancelled);
                break;
            }
            Err(kind) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.project_dir_name.clone(),
                    kind,
                });
                continue;
            }
        }

        let scratch_root = durable_check_scratch_root(profile_root);
        // An unreadable manifest must not be swallowed into "no manifest":
        // the inventory then fails closed instead of checking a guessed
        // database. A manifestless directory is different: only an exact
        // empty-tree inventory proves that it carries no durable authority.
        // Arbitrary payload files remain unverifiable, while any discovered
        // `.db` family is inspected directly and remains fail-closed on error.
        let manifest_path = finding
            .data_root
            .join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
        let durable_check = match read_regular_file(&manifest_path) {
            RegularFileSnapshot::Bytes(manifest_bytes) => {
                // An unregistered store has no registry graph scopes by
                // definition; the manifest remains the canonical graph path.
                check_store_durable_memory(
                    &finding.data_root,
                    Some(&manifest_bytes),
                    &[],
                    &scratch_root,
                    control,
                )
                .await
            }
            RegularFileSnapshot::Missing => {
                check_manifestless_store_durable_memory(&finding.data_root, &scratch_root, control)
                    .await
            }
            RegularFileSnapshot::Unverifiable => DurableMemoryCheck::Unverifiable,
        };
        match durable_check {
            DurableMemoryCheck::Empty => {}
            DurableMemoryCheck::Present | DurableMemoryCheck::Unverifiable => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.project_dir_name.clone(),
                    kind: CollectionFailureKind::DurableDataProtected,
                });
                continue;
            }
            DurableMemoryCheck::Interrupted => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.project_dir_name.clone(),
                    kind: CollectionFailureKind::Cancelled,
                });
                outcome.completion = control
                    .completion()
                    .unwrap_or(CollectionCompletionV1::Cancelled);
                break;
            }
        }

        // The durable-data inspection is intentionally fail-closed, but it is
        // not a deletion lock. Re-prove the inspected root generation at the
        // final destructive boundary so an in-profile replacement or symlink
        // swap cannot inherit an old empty-directory decision.
        match unregistered_payload_fence_matches(finding, profile_root, control) {
            Ok(true) => {}
            Ok(false) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.project_dir_name.clone(),
                    kind: CollectionFailureKind::PayloadChanged,
                });
                continue;
            }
            Err(CollectionFailureKind::Cancelled) => {
                outcome.completion = control
                    .completion()
                    .unwrap_or(CollectionCompletionV1::Cancelled);
                break;
            }
            Err(kind) => {
                outcome.errors.push(CollectionFailure {
                    store_id: finding.project_dir_name.clone(),
                    kind,
                });
                continue;
            }
        }

        let quarantine = match prepare_verified_quarantine(
            profile_root,
            &finding.data_root,
            &finding.expected_content_fence,
            QuarantineKindV1::Unregistered,
            &finding.project_dir_name,
            &finding.project_dir_name,
            None,
            control,
            &mut outcome,
        ) {
            QuarantinePreparation::Missing => None,
            QuarantinePreparation::Verified(quarantine) => Some(quarantine),
            QuarantinePreparation::Interrupted | QuarantinePreparation::Failed => {
                continue;
            }
        };
        let transaction = match control.race(db.begin_write_transaction()).await {
            Ok(Ok(transaction)) => transaction,
            Ok(Err(error)) => return Err(error),
            Err(completion) => {
                retain_interrupted_quarantine(
                    quarantine.as_ref(),
                    &finding.data_root,
                    &finding.project_dir_name,
                    completion,
                    &mut outcome,
                );
                break;
            }
        };
        let mut rows = match control
            .race(transaction.query(
                "SELECT 1 FROM code_projects WHERE project_id = ?1",
                tracedecay_runtime_core::db::engine::params![finding.project_dir_name.as_str()],
            ))
            .await
        {
            Ok(Ok(rows)) => rows,
            Ok(Err(error)) => {
                return Err(orphan_db_error(
                    "confirm quarantined unregistered store",
                    error,
                ));
            }
            Err(completion) => {
                drop(transaction);
                retain_interrupted_quarantine(
                    quarantine.as_ref(),
                    &finding.data_root,
                    &finding.project_dir_name,
                    completion,
                    &mut outcome,
                );
                break;
            }
        };
        let next = match control.race(rows.next()).await {
            Ok(Ok(next)) => next,
            Ok(Err(error)) => {
                return Err(orphan_db_error(
                    "read quarantined unregistered store",
                    error,
                ));
            }
            Err(completion) => {
                drop(rows);
                drop(transaction);
                retain_interrupted_quarantine(
                    quarantine.as_ref(),
                    &finding.data_root,
                    &finding.project_dir_name,
                    completion,
                    &mut outcome,
                );
                break;
            }
        };
        let now_registered = next.is_some();
        drop(rows);
        if now_registered {
            match control.race(transaction.rollback()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    return Err(orphan_db_error(
                        "rollback newly-registered quarantined store",
                        error,
                    ));
                }
                Err(completion) => {
                    retain_interrupted_quarantine(
                        quarantine.as_ref(),
                        &finding.data_root,
                        &finding.project_dir_name,
                        completion,
                        &mut outcome,
                    );
                    break;
                }
            }
            outcome.errors.push(CollectionFailure {
                store_id: finding.project_dir_name.clone(),
                kind: CollectionFailureKind::RegistryChanged,
            });
            continue;
        }
        match control.race(transaction.commit()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(orphan_db_error("commit unregistered store fence", error));
            }
            Err(completion) => {
                retain_interrupted_quarantine(
                    quarantine.as_ref(),
                    &finding.data_root,
                    &finding.project_dir_name,
                    completion,
                    &mut outcome,
                );
                break;
            }
        }

        if let Some(quarantine) = quarantine
            && !finalize_verified_quarantine(
                quarantine,
                &finding.data_root,
                &finding.project_dir_name,
                control,
                &mut outcome,
            )
        {
            continue;
        }
        outcome.reclaimed_bytes = outcome.reclaimed_bytes.saturating_add(finding.size_bytes);
        outcome.collected.push(CollectedStore {
            project_id: finding.project_dir_name.clone(),
            store_id: finding.project_dir_name.clone(),
            data_root: finding.data_root.clone(),
            size_bytes: finding.size_bytes,
        });
    }
    Ok(outcome)
}

/// Inspects a manifestless unregistered directory without inventing a graph
/// path. An exactly empty directory is provably free of durable rows. Any
/// arbitrary payload, symlink, or unreadable entry remains unverifiable;
/// when a SQLite-looking file is present, every such file is treated as a
/// possible durable authority and inspected fail-closed.
async fn check_manifestless_store_durable_memory(
    data_root: &Path,
    scratch_root: &Path,
    control: CollectionControl<'_>,
) -> DurableMemoryCheck {
    let mut databases = Vec::new();
    if control.completion().is_some() {
        return DurableMemoryCheck::Interrupted;
    }
    if collect_sqlite_candidates(data_root, data_root, &mut databases, control).is_err() {
        return if control.completion().is_some() {
            DurableMemoryCheck::Interrupted
        } else {
            DurableMemoryCheck::Unverifiable
        };
    }
    if databases.is_empty() {
        return DurableMemoryCheck::Empty;
    }
    for relpath in databases {
        if control.completion().is_some() {
            return DurableMemoryCheck::Interrupted;
        }
        match check_durable_memory_rows(data_root, &relpath, scratch_root, control).await {
            DurableMemoryCheck::Empty => {}
            protected => return protected,
        }
    }
    DurableMemoryCheck::Empty
}

/// Finds only regular `.db` files below a store and never follows symlinks.
/// The manifestless path deliberately does not guess a single filename, so a
/// custom legacy graph cannot be mistaken for payload-only debris. Any other
/// file shape is an unverifiable durable-data candidate, not disposable dust.
fn collect_sqlite_candidates(
    root: &Path,
    current: &Path,
    output: &mut Vec<PathBuf>,
    control: CollectionControl<'_>,
) -> std::io::Result<()> {
    let entries = std::fs::read_dir(current)?;
    for entry in entries {
        if control.completion().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "retention durable-data inventory interrupted",
            ));
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(std::io::Error::other(
                "manifestless store contains a symlink",
            ));
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_sqlite_candidates(root, &path, output, control)?;
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("db")
            && let Ok(relative) = path.strip_prefix(root)
        {
            output.push(relative.to_path_buf());
        } else {
            return Err(std::io::Error::other(
                "manifestless store contains an unrecognized payload",
            ));
        }
    }
    output.sort();
    Ok(())
}

/// Compatibility convenience for one bounded read/apply page. The daemon uses
/// [`sweep_unregistered_store_page`] directly so it can persist the returned
/// cursor across maintenance cadences; Doctor deliberately receives one
/// bounded preview rather than a hidden full-profile traversal.
#[hotpath::measure(label = "maintenance.orphan_stores.sweep_unregistered", future = true)]

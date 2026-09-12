//! Store-level orphan detection and collection.
//!
//! The parent module prunes append-only *rows* inside a live store. This
//! submodule operates one level up: whole profile-sharded store directories
//! whose project identity no longer resolves to a live repository root.
//!
//! A project-root migration re-registers a repository under a new identity and
//! silently strands the prior store on disk. Registry GC removes the
//! stale *registry row* but never the on-disk store *data*, so the payload
//! accumulates invisibly (measured at ~41 GB in one observed profile). This
//! module makes those stores a typed finding — carrying age and size — and
//! collects them under an owner-visible retention window.
//!
//! The contract is "re-link or explicitly retire, never orphan silently": a
//! store whose registry roots are gone but whose manifest points at a
//! *different, currently-live* root is classified [`StoreDisposition::Relinkable`]
//! and is never collected here — an applied sweep atomically transfers its
//! registry identity to that exact live project. Only stores with no live root
//! at all are eligible for collection, and only once older than the retention
//! window.

use std::path::{Path, PathBuf};

use tracedecay_global_db::registry_maintenance::{RootLivenessV1, probe_root};

mod fence;
mod pages;
mod quarantine;
mod unregistered_page;
pub use fence::{
    StoreContentEntry, StoreContentEntryKind, StoreContentFence, StoreContentInventory,
    StoreDirectoryFence, StoreFileIdentity, StoreRootIdentity,
};
#[cfg(test)]
pub(crate) use quarantine::read_pending_quarantine_receipts;
pub use unregistered_page::UnregisteredSweepCompletionV1;
pub use unregistered_page::{
    DEFAULT_UNREGISTERED_STORE_PAGE_LIMIT, UnregisteredStoreSweepReport,
    UnregisteredStoreSweepRequestV1, sweep_unregistered_store_page,
};
pub(super) use unregistered_page::{ProjectDirectoryWorkV1, read_project_directory_page};

/// One profile-sharded store observed on disk, paired with the registry
/// identity that points at it. This is the pure input to classification so the
/// decision logic is testable without a filesystem or database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreCensusEntry {
    pub project_id: String,
    pub store_id: String,
    /// Registry canonical root for the store's identity.
    pub canonical_root: PathBuf,
    /// Registry display root, when distinct from the canonical root.
    pub display_root: Option<PathBuf>,
    /// Git common directory recorded for the project. A linked worktree shares
    /// it with the primary checkout, so it keeps the identity live.
    pub git_common_dir: Option<PathBuf>,
    /// Every registered alias path for the project. Any live alias keeps the
    /// store live even when the canonical root is gone.
    pub alias_roots: Vec<PathBuf>,
    /// Whether the store manifest was read and parsed. A malformed or
    /// unreadable manifest makes the store's project root unverifiable, never
    /// "absent".
    pub manifest_readable: bool,
    /// On-disk store data directory (`profile_root` joined with the store relpath).
    pub data_root: PathBuf,
    /// `project_root` recorded in the store manifest, when the manifest was read.
    pub manifest_root: Option<PathBuf>,
    /// Newest payload mtime under `data_root`, unix seconds. Drives the age.
    pub last_write_secs: i64,
    /// Total bytes on disk under `data_root`.
    pub size_bytes: u64,
    /// Exact registry identity observed with this filesystem census.
    pub expected_store_relpath: String,
    pub expected_created_at: i64,
    pub expected_last_write_at: Option<i64>,
    /// Payload mtime and manifest bytes fence collection against revival.
    pub expected_payload_mtime_secs: i64,
    /// Stable filesystem generation observed for `data_root`. This is carried
    /// from inspection to apply so a same-second replacement cannot inherit a
    /// prior store's eligibility merely by copying its payload mtimes.
    pub expected_data_root_fence: StoreDirectoryFence,
    /// Complete no-follow child content/identity fence. Collection rechecks it
    /// only after atomically moving the store into a same-parent quarantine.
    pub expected_content_fence: StoreContentFence,
    pub expected_manifest_bytes: Option<Vec<u8>>,
    /// Registered graph-scope database paths, relative to `data_root`. Scopes
    /// may sit at custom relative paths, so the durable-data check cannot infer
    /// them from the main graph alone.
    pub graph_scope_relpaths: Vec<PathBuf>,
}

/// Why a store's identity could not be resolved either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnverifiableReason {
    /// A root could not be inspected (permission or I/O failure), so absence
    /// was never proven.
    RootInspectionFailed,
    /// The store manifest was missing, unreadable, or malformed, so the store's
    /// own record of its project root could not be trusted.
    ManifestUnreadable,
}

/// What should happen to a store, decided purely from its census entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreDisposition {
    /// The identity still resolves to a live repository root. Keep.
    Live,
    /// The registry roots are gone but the manifest points at a different,
    /// currently-live root: the repository moved. Re-link, never collect.
    Relinkable { live_root: PathBuf },
    /// Liveness could not be determined. Never collected: retirement requires
    /// proof of absence, and a failed inspection is not proof.
    Unverifiable { reason: UnverifiableReason },
    /// Every root of this identity was *proven* absent. Eligible for collection
    /// once older than the retention window.
    Orphaned,
}

/// A typed finding over one store: its disposition plus the age and size an
/// owner surface (Doctor) reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanStoreFinding {
    pub project_id: String,
    pub store_id: String,
    pub data_root: PathBuf,
    pub disposition: StoreDisposition,
    /// `now - last_write_secs`, clamped at zero.
    pub age_secs: i64,
    pub size_bytes: u64,
    pub expected_store_relpath: String,
    pub expected_created_at: i64,
    pub expected_last_write_at: Option<i64>,
    pub expected_payload_mtime_secs: i64,
    pub expected_data_root_fence: StoreDirectoryFence,
    pub expected_content_fence: StoreContentFence,
    pub expected_manifest_bytes: Option<Vec<u8>>,
    /// Registered graph-scope database paths, relative to `data_root`; carried
    /// through so the durable-data check covers every scope, not just the main
    /// graph.
    pub graph_scope_relpaths: Vec<PathBuf>,
}

/// Every root that can keep this store's identity alive: the registry roots,
/// the git common directory shared with linked worktrees, and every registered
/// alias path. Collecting a store because one checkout vanished, while another
/// checkout of the same repository is still enrolled, destroys live data.
fn identity_roots(entry: &StoreCensusEntry) -> impl Iterator<Item = &Path> {
    std::iter::once(entry.canonical_root.as_path())
        .chain(entry.display_root.as_deref())
        .chain(entry.git_common_dir.as_deref())
        .chain(entry.alias_roots.iter().map(PathBuf::as_path))
}

fn classify_one(entry: &StoreCensusEntry) -> StoreDisposition {
    if entry.expected_data_root_fence == StoreDirectoryFence::Unverifiable {
        return StoreDisposition::Unverifiable {
            reason: UnverifiableReason::RootInspectionFailed,
        };
    }
    let identity = identity_roots(entry).fold(RootLivenessV1::Absent, |liveness, root| {
        liveness.merge(probe_root(root))
    });
    match identity {
        RootLivenessV1::Live => return StoreDisposition::Live,
        // An inspection that failed proves nothing. Retiring on it would delete
        // a store whose repository may be perfectly alive behind an unreadable
        // parent directory or a stale mount.
        RootLivenessV1::Unverifiable => {
            return StoreDisposition::Unverifiable {
                reason: UnverifiableReason::RootInspectionFailed,
            };
        }
        RootLivenessV1::Absent => {}
    }
    // The manifest names this store's project root. If it could not be read or
    // parsed, the identity is unproven and the store is not collectable.
    if !entry.manifest_readable {
        return StoreDisposition::Unverifiable {
            reason: UnverifiableReason::ManifestUnreadable,
        };
    }
    // Registry identity is dead. If the manifest still names a live root the
    // repository moved rather than vanished — re-link instead of collecting.
    if let Some(manifest_root) = entry.manifest_root.as_deref()
        && manifest_root != entry.canonical_root
        && entry.display_root.as_deref() != Some(manifest_root)
    {
        match probe_root(manifest_root) {
            RootLivenessV1::Live => {
                return StoreDisposition::Relinkable {
                    live_root: manifest_root.to_path_buf(),
                };
            }
            RootLivenessV1::Unverifiable => {
                return StoreDisposition::Unverifiable {
                    reason: UnverifiableReason::RootInspectionFailed,
                };
            }
            RootLivenessV1::Absent => {}
        }
    }
    StoreDisposition::Orphaned
}

/// Classify every census entry. Pure: no filesystem writes, no deletion.
pub fn classify_stores(census: &[StoreCensusEntry], now: i64) -> Vec<OrphanStoreFinding> {
    census
        .iter()
        .map(|entry| OrphanStoreFinding {
            project_id: entry.project_id.clone(),
            store_id: entry.store_id.clone(),
            data_root: entry.data_root.clone(),
            disposition: classify_one(entry),
            age_secs: now.saturating_sub(entry.last_write_secs).max(0),
            size_bytes: entry.size_bytes,
            expected_store_relpath: entry.expected_store_relpath.clone(),
            expected_created_at: entry.expected_created_at,
            expected_last_write_at: entry.expected_last_write_at,
            expected_payload_mtime_secs: entry.expected_payload_mtime_secs,
            expected_data_root_fence: entry.expected_data_root_fence.clone(),
            expected_content_fence: entry.expected_content_fence.clone(),
            expected_manifest_bytes: entry.expected_manifest_bytes.clone(),
            graph_scope_relpaths: entry.graph_scope_relpaths.clone(),
        })
        .collect()
}

/// The partitioned collection decision over a set of findings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CollectionPlan {
    /// Orphaned and older than the retention window — collect these.
    pub collect: Vec<OrphanStoreFinding>,
    /// Orphaned but still inside the retention window — kept for now, surfaced.
    pub retained_immature: Vec<OrphanStoreFinding>,
    /// Re-linkable (moved repository) — never collected; an applied sweep
    /// transfers these to the exact registered live project identity.
    pub relink: Vec<OrphanStoreFinding>,
    /// Liveness could not be proven either way — never collected, surfaced so
    /// an owner can resolve the inspection failure instead of losing the store.
    pub unverifiable: Vec<OrphanStoreFinding>,
}

impl CollectionPlan {
    /// Total bytes that collecting [`Self::collect`] would reclaim.
    pub fn collectable_bytes(&self) -> u64 {
        self.collect
            .iter()
            .fold(0u64, |acc, f| acc.saturating_add(f.size_bytes))
    }
}

/// Partition findings under a retention window. Live stores are dropped from
/// the plan entirely — they are never a retention concern. Pure.
pub fn plan_collection(findings: Vec<OrphanStoreFinding>, retention_secs: i64) -> CollectionPlan {
    let mut plan = CollectionPlan::default();
    for finding in findings {
        match &finding.disposition {
            StoreDisposition::Live => {}
            StoreDisposition::Relinkable { .. } => plan.relink.push(finding),
            StoreDisposition::Unverifiable { .. } => plan.unverifiable.push(finding),
            StoreDisposition::Orphaned => {
                if finding.age_secs >= retention_secs {
                    plan.collect.push(finding);
                } else {
                    plan.retained_immature.push(finding);
                }
            }
        }
    }
    plan
}

/// A store directory that was deleted from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedStore {
    pub project_id: String,
    pub store_id: String,
    pub data_root: PathBuf,
    pub size_bytes: u64,
}

/// The exact filesystem mutation that failed during orphan-store retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionMutationOperation {
    ReserveQuarantineName,
    PublishQuarantineJournal,
    PublishQuarantineRenameMarker,
    RenameLiveLeafToQuarantine,
    RestoreLiveLeafFromQuarantine,
    ProbeRecoveryJournal,
    ValidateRestoredStoreIdentity,
    ClearRecoveryJournal,
    MarkRetirementCommitted,
    RecursiveRemove,
    ParentSync,
}

/// Whether a mutation failure is a known external-owner deferral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionMutationFailureClassification {
    RetryableDeferred,
    NonRetryable,
}

/// Structured evidence for a failed orphan-store filesystem mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionMutationFailure {
    pub operation: CollectionMutationOperation,
    pub raw_os_error: Option<i32>,
    pub target_path: PathBuf,
    pub expected_root_identity: Option<StoreRootIdentity>,
    pub classification: CollectionMutationFailureClassification,
}

impl CollectionMutationFailure {
    pub fn retryable(&self) -> bool {
        self.classification == CollectionMutationFailureClassification::RetryableDeferred
    }

    pub(crate) fn from_io_error(
        operation: CollectionMutationOperation,
        target_path: PathBuf,
        expected_root_identity: Option<StoreRootIdentity>,
        error: &std::io::Error,
    ) -> Self {
        let raw_os_error = error.raw_os_error();
        let classification = if cfg!(windows) && matches!(raw_os_error, Some(5 | 32 | 33)) {
            CollectionMutationFailureClassification::RetryableDeferred
        } else {
            CollectionMutationFailureClassification::NonRetryable
        };
        Self {
            operation,
            raw_os_error,
            target_path,
            expected_root_identity,
            classification,
        }
    }

    pub(crate) fn without_native_error(
        operation: CollectionMutationOperation,
        target_path: PathBuf,
        expected_root_identity: Option<StoreRootIdentity>,
    ) -> Self {
        Self {
            operation,
            raw_os_error: None,
            target_path,
            expected_root_identity,
            classification: CollectionMutationFailureClassification::NonRetryable,
        }
    }
}

/// Outcome of executing a [`CollectionPlan`] against the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectionFailureKind {
    /// Cooperative maintenance cancellation/deadline interrupted an expensive
    /// inspection before any irreversible step. The report completion carries
    /// the exact cancelled/deadline distinction.
    Cancelled,
    OutsideProfile,
    InspectFailed,
    RemoveFailed(CollectionMutationFailure),
    RegistryChanged,
    ManifestChanged,
    PayloadChanged,
    /// The store's graph database carries rows in a durable per-project memory
    /// table (or the check could not prove otherwise). Never collected, even
    /// when every other eligibility check passed — see
    /// [`DurableMemoryCheck`]/[`check_durable_memory_rows`].
    DurableDataProtected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionFailure {
    pub store_id: String,
    pub kind: CollectionFailureKind,
}

/// A truthful recovery receipt for a store moved to the retention quarantine.
/// A failed post-move proof never becomes an invisible failure: either the
/// original name was restored, or the moved bytes remain at the named sibling
/// for a later reconciliation pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionRecoveryAction {
    Restored,
    RetainedForRecovery,
    /// Registry retirement committed, but the irreversible delete has not yet
    /// been durably confirmed. A journal-backed retry owns this state.
    DeleteUnconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionRecoveryReceipt {
    pub store_id: String,
    pub original_path: PathBuf,
    pub quarantine_path: PathBuf,
    /// The path that currently owns the bytes (or, after a remove/sync
    /// ambiguity, the exact path whose deletion remains unconfirmed).
    pub actual_path: PathBuf,
    pub action: CollectionRecoveryAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CollectionOutcome {
    pub collected: Vec<CollectedStore>,
    pub reclaimed_bytes: u64,
    pub errors: Vec<CollectionFailure>,
    pub recovery_receipts: Vec<CollectionRecoveryReceipt>,
    /// A bounded pass may have completed only a prefix of its plan. This is
    /// never reported as a successful empty collection.
    pub completion: CollectionCompletionV1,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CollectionCompletionV1 {
    #[default]
    Complete,
    Cancelled,
    DeadlineExceeded,
}

pub use pages::{
    OrphanSweepReport, StoreCensusPageV1, UnregisteredCollectionPlan, UnregisteredStoreFinding,
    build_store_census, build_store_census_page, plan_unregistered_collection,
    sweep_unregistered_stores,
};
#[cfg(test)]
pub(crate) use pages::{census_unregistered_project_dirs, sweep_orphan_stores};
pub(crate) use pages::{
    dir_size_bytes, dir_size_bytes_controlled, manifest_names_abandoned_root,
    newest_mtime_secs_controlled,
};
pub use quarantine::execute_registered_collection;
pub(crate) use quarantine::{CollectionControl, execute_unregistered_collection_controlled};
#[cfg(test)]
pub(crate) use quarantine::{
    execute_registered_collection_controlled, execute_unregistered_collection,
    unbounded_collection_control,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

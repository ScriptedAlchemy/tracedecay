//! Store-level orphan detection and collection (plan 38, §2).
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

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::global_db::RegisteredGlobalDb;
use crate::global_db::registry_maintenance::{RootLivenessV1, probe_root};
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

mod fence;
mod quarantine;
mod unregistered_page;
pub use fence::{
    StoreContentEntry, StoreContentEntryKind, StoreContentFence, StoreContentInventory,
    StoreDirectoryFence, StoreFileIdentity, StoreRootIdentity,
};
use fence::{
    capture_store_content_fence, capture_store_content_fence_controlled,
    capture_store_directory_fence, data_root_fence_matches, profile_relative_store_path,
};
#[cfg(test)]
use quarantine::quarantine_store_for_verified_collection;
#[cfg(test)]
pub(crate) use quarantine::read_pending_quarantine_receipts;
use quarantine::{
    QuarantineFinalizeOutcome, QuarantineKindV1, QuarantineRecoveryOutcome,
    QuarantineRegistryFenceV1, QuarantineStoreOutcome, QuarantinedStore,
    quarantine_store_for_verified_collection_controlled, recover_existing_store_quarantine,
};
pub(crate) use unregistered_page::UnregisteredSweepCompletionV1;
pub(crate) use unregistered_page::{
    DEFAULT_UNREGISTERED_STORE_PAGE_LIMIT, UnregisteredStoreSweepReport,
    UnregisteredStoreSweepRequestV1, sweep_unregistered_store_page,
};

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

impl OrphanStoreFinding {
    pub fn is_orphaned(&self) -> bool {
        matches!(self.disposition, StoreDisposition::Orphaned)
    }

    pub fn is_relinkable(&self) -> bool {
        matches!(self.disposition, StoreDisposition::Relinkable { .. })
    }
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

/// Outcome of executing a [`CollectionPlan`] against the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionFailureKind {
    /// Cooperative maintenance cancellation/deadline interrupted an expensive
    /// inspection before any irreversible step. The report completion carries
    /// the exact cancelled/deadline distinction.
    Cancelled,
    OutsideProfile,
    InspectFailed,
    RemoveFailed,
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

/// Cooperative budget carried through every expensive retention read and
/// apply boundary. The database writer is acquired only after content hashing
/// and durable-memory inspection have completed under this control.
#[derive(Clone, Copy)]
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

    /// Adapt the retention admission to the canonical SQLite read-snapshot
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

    /// Race an awaitable inspection or SQLite command against the admission's
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

fn unbounded_collection_control() -> CollectionControl<'static> {
    static CANCELLATION: std::sync::OnceLock<CancellationToken> = std::sync::OnceLock::new();
    CollectionControl::new(
        CANCELLATION.get_or_init(CancellationToken::new),
        MonotonicDeadline::at(Instant::now() + std::time::Duration::from_secs(86_400)),
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
        Ok(QuarantineStoreOutcome::Interrupted { quarantine_path }) => {
            outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                store_id: store_id.to_owned(),
                original_path: data_root.to_path_buf(),
                actual_path: quarantine_path.clone(),
                quarantine_path,
                action: CollectionRecoveryAction::RetainedForRecovery,
            });
            if let Some(completion) = control.completion() {
                outcome.completion = completion;
            }
            QuarantinePreparation::Interrupted
        }
        Ok(QuarantineStoreOutcome::Restored {
            restored_path,
            journal_pending,
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
            if journal_pending {
                outcome.errors.push(CollectionFailure {
                    store_id: store_id.to_owned(),
                    kind: CollectionFailureKind::RemoveFailed,
                });
            }
            QuarantinePreparation::Failed
        }
        Ok(QuarantineStoreOutcome::Retained { quarantine_path }) => {
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
    if quarantine.mark_retirement_committed().is_err() {
        outcome.recovery_receipts.push(CollectionRecoveryReceipt {
            store_id: store_id.to_owned(),
            original_path: data_root.to_path_buf(),
            quarantine_path: quarantine.quarantine_path().to_path_buf(),
            actual_path: quarantine.quarantine_path().to_path_buf(),
            action: CollectionRecoveryAction::RetainedForRecovery,
        });
        outcome.errors.push(CollectionFailure {
            store_id: store_id.to_owned(),
            kind: CollectionFailureKind::RemoveFailed,
        });
        return false;
    }
    match quarantine.finalize(control) {
        QuarantineFinalizeOutcome::Removed { journal_pending } => {
            if journal_pending {
                outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                    store_id: store_id.to_owned(),
                    original_path: data_root.to_path_buf(),
                    quarantine_path: data_root.to_path_buf(),
                    actual_path: data_root.to_path_buf(),
                    action: CollectionRecoveryAction::DeleteUnconfirmed,
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
        QuarantineFinalizeOutcome::DeleteUnconfirmed { quarantine_path } => {
            outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                store_id: store_id.to_owned(),
                original_path: data_root.to_path_buf(),
                actual_path: quarantine_path.clone(),
                quarantine_path,
                action: CollectionRecoveryAction::DeleteUnconfirmed,
            });
            outcome.errors.push(CollectionFailure {
                store_id: store_id.to_owned(),
                kind: CollectionFailureKind::RemoveFailed,
            });
            false
        }
    }
}

/// Reconcile a durable interrupted quarantine before applying a fresh plan for
/// this exact live-name. Recovery never resumes the old deletion decision: a
/// restored or retained quarantine is an owner-visible receipt and forces a
/// later census/confirmation pass.
fn reconcile_existing_quarantine(
    profile_root: &Path,
    data_root: &Path,
    store_id: &str,
    outcome: &mut CollectionOutcome,
) -> bool {
    match recover_existing_store_quarantine(profile_root, data_root) {
        Ok(recoveries) if recoveries.is_empty() => true,
        Ok(recoveries) => {
            for recovery in recoveries {
                let (quarantine_path, actual_path, action) = match recovery {
                    QuarantineRecoveryOutcome::Restored {
                        restored_path,
                        journal_pending,
                    } => {
                        let action = if journal_pending {
                            CollectionRecoveryAction::RetainedForRecovery
                        } else {
                            CollectionRecoveryAction::Restored
                        };
                        (data_root.to_path_buf(), restored_path, action)
                    }
                    QuarantineRecoveryOutcome::Retained { quarantine_path } => (
                        quarantine_path.clone(),
                        quarantine_path,
                        CollectionRecoveryAction::RetainedForRecovery,
                    ),
                };
                outcome.recovery_receipts.push(CollectionRecoveryReceipt {
                    store_id: store_id.to_owned(),
                    original_path: data_root.to_path_buf(),
                    quarantine_path,
                    actual_path,
                    action,
                });
            }
            outcome.errors.push(CollectionFailure {
                store_id: store_id.to_owned(),
                kind: CollectionFailureKind::PayloadChanged,
            });
            false
        }
        Err(kind) => {
            outcome.errors.push(CollectionFailure {
                store_id: store_id.to_owned(),
                kind,
            });
            false
        }
    }
}

/// Executes registered collection in two phases: expensive inspection and a
/// same-parent quarantine run without a writer; a short final transaction then
/// retires the exact registry row before irreversible quarantine deletion.
pub(crate) async fn execute_registered_collection(
    db: &RegisteredGlobalDb,
    plan: &CollectionPlan,
    profile_root: &Path,
) -> crate::errors::Result<(CollectionOutcome, usize)> {
    execute_registered_collection_controlled(db, plan, profile_root, unbounded_collection_control())
        .await
}

pub(crate) async fn execute_registered_collection_controlled(
    db: &RegisteredGlobalDb,
    plan: &CollectionPlan,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> crate::errors::Result<(CollectionOutcome, usize)> {
    let mut outcome = CollectionOutcome::default();
    let mut retired = 0usize;
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
            .join(crate::storage::STORE_MANIFEST_FILENAME);
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
                crate::db::engine::params![finding.project_id.as_str(), finding.store_id.as_str()],
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
                crate::db::engine::params![
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
                crate::db::engine::params![finding.project_id.as_str()],
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
) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Database {
        operation: operation.to_string(),
        message: error.to_string(),
    }
}

/// Result of checking a store's graph database for durable memory rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableMemoryCheck {
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
enum DurableDatabaseInventoryV1 {
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
fn durable_database_inventory(
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
    let manifest = match serde_json::from_slice::<crate::storage::StoreManifest>(bytes) {
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
async fn check_store_durable_memory(
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
/// [`newest_mtime_secs`] uses as the revival fence — a store that failed one
/// check would have its age reset by the check itself and could never mature
/// past the retention window again.
fn durable_check_scratch_root(profile_root: &Path) -> PathBuf {
    profile_root.join("scratch").join("sqlite-read")
}

/// Checks whether `data_root`'s graph database carries rows in any canonical
/// `memory_*` table. This intentionally discovers tables from the schema
/// instead of maintaining a fixed list: both legacy memory and Memory V2 add
/// durable tables, and a newly added table must be protected automatically.
/// Side-effect-free with respect to the store: opens the database through
/// [`crate::sqlite_read_snapshot`], so the live store is never mutated or
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
            crate::db::engine::params!["memory\\_%"],
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

/// Newest mtime under `dir`, unix seconds, or `0` when nothing is readable.
///
/// Symlinks are not followed, and are stat'd as links rather than targets.
/// This walk is the revival fence for a delete: following a symlink would let
/// a link planted under a store recurse without bound, or make the fence read
/// an unrelated directory's mtime instead of the payload's.
fn newest_mtime_secs(dir: &Path) -> i64 {
    fn walk(path: &Path, newest: &mut i64) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if let Ok(modified) = meta.modified()
                && let Ok(elapsed) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                *newest = (*newest).max(elapsed.as_secs() as i64);
            }
            if meta.is_dir() {
                walk(&entry.path(), newest);
            }
        }
    }
    let mut newest = 0i64;
    walk(dir, &mut newest);
    newest
}

/// Controlled counterpart of [`newest_mtime_secs`]. Every recursive descent
/// checks the maintenance admission before asking the next directory for
/// entries, so a cancellation cannot turn age accounting into an unbounded
/// traversal. Ordinary I/O remains best-effort exactly as in the unbounded
/// census; only the caller-owned interruption is surfaced distinctly.
pub(crate) fn newest_mtime_secs_controlled(
    dir: &Path,
    control: CollectionControl<'_>,
) -> Result<i64, CollectionFailureKind> {
    fn walk(
        path: &Path,
        newest: &mut i64,
        control: CollectionControl<'_>,
    ) -> Result<(), CollectionFailureKind> {
        if control.completion().is_some() {
            return Err(CollectionFailureKind::Cancelled);
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return Ok(());
        };
        for entry in entries {
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if let Ok(modified) = meta.modified()
                && let Ok(elapsed) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                *newest = (*newest).max(elapsed.as_secs() as i64);
            }
            if meta.is_dir() {
                walk(&entry.path(), newest, control)?;
            }
        }
        Ok(())
    }

    let mut newest = 0i64;
    walk(dir, &mut newest, control)?;
    Ok(newest)
}

/// Total size in bytes of every file under `dir`. Best-effort: unreadable
/// entries are skipped. Kept local to the lib because the binary-only
/// `global::tracedecay_dir_size` is not reachable from this crate module.
///
/// Symlinks are never followed. `DirEntry::metadata` follows them, so a
/// symlink pointing at an ancestor would recurse until the stack ran out, and
/// one pointing outside the store would bill another directory's bytes to
/// this one. `file_type` reports the link itself, so the walk stays inside
/// the directory it was given.
pub(crate) fn dir_size_bytes(dir: &Path) -> u64 {
    fn walk(path: &Path, acc: &mut u64) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                walk(&entry.path(), acc);
            } else if file_type.is_file()
                && let Ok(meta) = entry.metadata()
            {
                *acc = acc.saturating_add(meta.len());
            }
        }
    }
    let mut total = 0u64;
    walk(dir, &mut total);
    total
}

/// Controlled counterpart of [`dir_size_bytes`]. It preserves the original
/// best-effort accounting policy for unreadable entries while making the
/// recursive work bounded by the caller's admission control.
pub(crate) fn dir_size_bytes_controlled(
    dir: &Path,
    control: CollectionControl<'_>,
) -> Result<u64, CollectionFailureKind> {
    fn walk(
        path: &Path,
        total: &mut u64,
        control: CollectionControl<'_>,
    ) -> Result<(), CollectionFailureKind> {
        if control.completion().is_some() {
            return Err(CollectionFailureKind::Cancelled);
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return Ok(());
        };
        for entry in entries {
            if control.completion().is_some() {
                return Err(CollectionFailureKind::Cancelled);
            }
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                walk(&entry.path(), total, control)?;
            } else if file_type.is_file()
                && let Ok(meta) = entry.metadata()
            {
                *total = total.saturating_add(meta.len());
            }
        }
        Ok(())
    }

    let mut total = 0u64;
    walk(dir, &mut total, control)?;
    Ok(total)
}

/// Build the on-disk store census from the registry. Reads manifests and sizes
/// directories but never mutates. Only profile-sharded stores are considered;
/// other storage modes are not laid out under the profile root here.
pub(crate) async fn build_store_census(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
) -> crate::errors::Result<Vec<StoreCensusEntry>> {
    let projects = db.list_code_projects(usize::MAX).await?;
    build_store_census_for_projects(db, profile_root, &projects, None)
        .await?
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: "unbounded store census was unexpectedly interrupted".to_owned(),
        })
}

#[derive(Debug, Clone)]
pub(crate) struct StoreCensusPageV1 {
    pub(crate) entries: Vec<StoreCensusEntry>,
    pub(crate) next_cursor: Option<String>,
}

pub(crate) async fn build_store_census_page(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    after_project_id: Option<&str>,
    limit: usize,
) -> crate::errors::Result<StoreCensusPageV1> {
    let limit = limit.clamp(1, 64);
    let mut projects = db
        .list_code_projects_after(after_project_id, limit.saturating_add(1))
        .await?;
    let has_more = projects.len() > limit;
    projects.truncate(limit);
    let next_cursor = has_more
        .then(|| projects.last().map(|project| project.project_id.clone()))
        .flatten();
    let entries = build_store_census_for_projects(db, profile_root, &projects, None)
        .await?
        .ok_or_else(|| crate::errors::TraceDecayError::Config {
            message: "unbounded store census page was unexpectedly interrupted".to_owned(),
        })?;
    Ok(StoreCensusPageV1 {
        entries,
        next_cursor,
    })
}

async fn build_store_census_for_projects(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    projects: &[crate::global_db::CodeProjectRecord],
    control: Option<CollectionControl<'_>>,
) -> crate::errors::Result<Option<Vec<StoreCensusEntry>>> {
    let mut census = Vec::new();
    // Aliases and the git common directory are part of the identity: a linked
    // worktree or a second enrolled checkout keeps the store live even when
    // this row's canonical root is gone.
    let contexts = match control {
        Some(control) => match control
            .race(db.project_registry_contexts_for_projects(projects))
            .await
        {
            Ok(Ok(contexts)) => contexts,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Ok(None),
        },
        None => db.project_registry_contexts_for_projects(projects).await?,
    };
    for context in contexts {
        if control.is_some_and(|control| control.completion().is_some()) {
            return Ok(None);
        }
        let project = &context.project;
        let alias_roots = context
            .aliases
            .iter()
            .map(|alias| PathBuf::from(&alias.alias_path))
            .collect::<Vec<_>>();
        let git_common_dir = project.git_common_dir.as_deref().map(PathBuf::from);
        let stores = match control {
            Some(control) => match control
                .race(db.try_list_store_instances_for_project(&project.project_id))
                .await
            {
                Ok(Ok(stores)) => stores,
                Ok(Err(error)) => return Err(error),
                Err(_) => return Ok(None),
            },
            None => {
                db.try_list_store_instances_for_project(&project.project_id)
                    .await?
            }
        };
        for store in stores {
            if control.is_some_and(|control| control.completion().is_some()) {
                return Ok(None);
            }
            let graph_scope_relpaths = context
                .stores
                .iter()
                .filter(|candidate| candidate.store.store_id == store.store_id)
                .flat_map(|candidate| candidate.graph_scopes.iter())
                .map(|scope| PathBuf::from(&scope.db_relpath))
                .collect::<Vec<_>>();
            if store.storage_mode != "profile_sharded" {
                continue;
            }
            let data_root = profile_root.join(&store.store_relpath);
            let manifest_path = data_root.join(crate::storage::STORE_MANIFEST_FILENAME);
            let expected_manifest_bytes = match read_regular_file(&manifest_path) {
                RegularFileSnapshot::Bytes(bytes) => Some(bytes),
                RegularFileSnapshot::Missing | RegularFileSnapshot::Unverifiable => None,
            };
            // A manifest that is absent or will not parse leaves this store's
            // own record of its project root unknown. Fail closed: record that
            // it is unverifiable rather than defaulting to "no manifest root",
            // which reads downstream as a collectable orphan.
            let parsed_manifest = expected_manifest_bytes
                .as_deref()
                .map(|bytes| serde_json::from_slice::<crate::storage::StoreManifest>(bytes).ok());
            let manifest_readable = matches!(parsed_manifest, Some(Some(_)));
            let manifest_root = parsed_manifest
                .flatten()
                .map(|manifest| manifest.project_root);
            let expected_payload_mtime_secs = match control {
                Some(control) => match newest_mtime_secs_controlled(&data_root, control) {
                    Ok(mtime) => mtime,
                    Err(_) => return Ok(None),
                },
                None => newest_mtime_secs(&data_root),
            };
            let expected_data_root_fence = capture_store_directory_fence(profile_root, &data_root)
                .unwrap_or(StoreDirectoryFence::Unverifiable);
            let expected_content_fence = match control {
                Some(control) => {
                    match capture_store_content_fence_controlled(profile_root, &data_root, control)
                    {
                        Ok(fence) => fence,
                        Err(CollectionFailureKind::Cancelled) => return Ok(None),
                        Err(_) => StoreContentFence::Unverifiable,
                    }
                }
                None => capture_store_content_fence(profile_root, &data_root)
                    .unwrap_or(StoreContentFence::Unverifiable),
            };
            let last_write_secs = store
                .last_write_at
                .filter(|value| *value > 0)
                .unwrap_or(expected_payload_mtime_secs);
            let size_bytes = match control {
                Some(control) => match dir_size_bytes_controlled(&data_root, control) {
                    Ok(size) => size,
                    Err(_) => return Ok(None),
                },
                None => dir_size_bytes(&data_root),
            };
            census.push(StoreCensusEntry {
                project_id: project.project_id.clone(),
                store_id: store.store_id.clone(),
                canonical_root: PathBuf::from(&project.canonical_root),
                display_root: (project.display_root != project.canonical_root)
                    .then(|| PathBuf::from(&project.display_root)),
                git_common_dir: git_common_dir.clone(),
                alias_roots: alias_roots.clone(),
                manifest_readable,
                data_root,
                manifest_root,
                last_write_secs,
                size_bytes,
                expected_store_relpath: store.store_relpath,
                expected_created_at: store.created_at,
                expected_last_write_at: store.last_write_at,
                expected_payload_mtime_secs,
                expected_data_root_fence,
                expected_content_fence,
                expected_manifest_bytes,
                graph_scope_relpaths,
            });
        }
    }
    Ok(Some(census))
}

/// The report returned by a sweep: the full classified plan plus, when
/// applied, what was collected on disk and the registry rows retired.
#[derive(Debug, Clone, Default)]
pub struct OrphanSweepReport {
    pub plan: CollectionPlan,
    pub applied: bool,
    pub outcome: CollectionOutcome,
    /// Registry identities transferred to their exact currently-live project.
    pub relinked_registry_rows: usize,
    /// Registry rows removed for collected stores.
    pub retired_registry_rows: usize,
}

/// Typed daemon/doctor entry point: census → classify → plan → optionally
/// collect. When `apply` is set, orphan store directories older than
/// `retention_secs` are deleted and their now-dangling registry rows retired in
/// the same operation, so an identity migration never leaves a silent orphan.
///
/// The caller (daemon backstop tick or Doctor pass) owns cadence and mutation
/// authority.
#[cfg(test)]
pub(crate) async fn sweep_orphan_stores(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
    apply: bool,
) -> crate::errors::Result<OrphanSweepReport> {
    let census = build_store_census(db, profile_root).await?;
    let findings = classify_stores(&census, now);
    let plan = plan_collection(findings, retention_secs);

    if !apply {
        return Ok(OrphanSweepReport {
            plan,
            applied: false,
            outcome: CollectionOutcome::default(),
            relinked_registry_rows: 0,
            retired_registry_rows: 0,
        });
    }

    let mut relinked_registry_rows = 0usize;
    let mut preflight_errors = Vec::new();
    for finding in &plan.relink {
        let StoreDisposition::Relinkable { live_root } = &finding.disposition else {
            continue;
        };
        if !store_finding_is_profile_contained(finding, profile_root) {
            preflight_errors.push(CollectionFailure {
                store_id: finding.store_id.clone(),
                kind: CollectionFailureKind::OutsideProfile,
            });
            continue;
        }
        if db
            .relink_orphan_store_instance(
                &finding.project_id,
                &finding.store_id,
                live_root,
                profile_root,
                &finding.data_root,
                &finding.expected_store_relpath,
                finding.expected_created_at,
                finding.expected_last_write_at,
                finding.expected_manifest_bytes.as_deref(),
            )
            .await?
        {
            relinked_registry_rows = relinked_registry_rows.saturating_add(1);
        }
    }

    let (mut outcome, retired_registry_rows) =
        execute_registered_collection(db, &plan, profile_root).await?;
    outcome.errors.splice(0..0, preflight_errors);

    Ok(OrphanSweepReport {
        plan,
        applied: true,
        outcome,
        relinked_registry_rows,
        retired_registry_rows,
    })
}

// === Unregistered store directories (plan 38 §2, disjoint audit class) =====
//
// `build_store_census` walks *from* the registry: for every registered
// project, for every one of its registered store instances. A store dir with
// no registry trace at all — no `code_projects` row for its identity, ever —
// is invisible to that walk no matter how large it grows. This is a distinct
// failure mode from [`StoreDisposition::Orphaned`] (whose registry row still
// exists; only its root vanished): here the row itself is gone, e.g. because
// registry GC removed the stale identity row without also removing
// the on-disk payload it pointed at. The owner's audit measured this class at
// 322 directories / 655 MB in one profile. This section is a bottom-up
// counterpart: scan `profile_root/projects/*` (the layout every
// profile-sharded store uses, see [`crate::storage::profile_sharded_data_root`])
// and flag any leaf directory whose name is not a currently-registered
// `project_id`.

/// One store directory found on disk with no registry identity at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnregisteredStoreFinding {
    /// The `projects/` leaf directory name — the project id this store would
    /// have if it were registered.
    pub project_dir_name: String,
    pub data_root: PathBuf,
    /// `now - newest mtime under data_root`, clamped at zero.
    pub age_secs: i64,
    pub size_bytes: u64,
    /// Payload mtime fence captured at census time; re-verified before delete.
    pub(crate) expected_payload_mtime_secs: i64,
    /// Stable data-root generation captured with the inspection finding.
    pub(crate) expected_data_root_fence: StoreDirectoryFence,
    /// Exact no-follow inventory/content identity captured at census time.
    pub(crate) expected_content_fence: StoreContentFence,
}

/// Test-only one-page census convenience. Production callers use
/// [`sweep_unregistered_store_page`] and persist its cursor between bounded
/// daemon admissions.
#[cfg(test)]
pub(crate) async fn census_unregistered_project_dirs(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    now: i64,
) -> crate::errors::Result<Vec<UnregisteredStoreFinding>> {
    let cancellation = CancellationToken::new();
    let report = sweep_unregistered_store_page(
        db,
        profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: DEFAULT_UNREGISTERED_STORE_PAGE_LIMIT,
            retention_secs: i64::MAX,
            now,
            apply: false,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            ),
        },
    )
    .await?;
    Ok(report
        .plan
        .collect
        .into_iter()
        .chain(report.plan.retained_immature)
        .collect())
}

/// The partitioned collection decision over a set of unregistered-store
/// findings. There is no `Live`/`Relinkable` disposition here — an
/// unregistered directory has no registry identity to resolve at all — so
/// every finding is either past the retention window or not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnregisteredCollectionPlan {
    pub collect: Vec<UnregisteredStoreFinding>,
    pub retained_immature: Vec<UnregisteredStoreFinding>,
}

impl UnregisteredCollectionPlan {
    /// Total bytes that collecting [`Self::collect`] would reclaim.
    pub fn collectable_bytes(&self) -> u64 {
        self.collect
            .iter()
            .fold(0u64, |acc, f| acc.saturating_add(f.size_bytes))
    }
}

/// Partition findings under a retention window. Pure.
pub fn plan_unregistered_collection(
    findings: Vec<UnregisteredStoreFinding>,
    retention_secs: i64,
) -> UnregisteredCollectionPlan {
    let mut plan = UnregisteredCollectionPlan::default();
    for finding in findings {
        if finding.age_secs >= retention_secs {
            plan.collect.push(finding);
        } else {
            plan.retained_immature.push(finding);
        }
    }
    plan
}

/// Deletes unregistered directories through the same two-phase boundary:
/// content/durable inspection and quarantine first, then a short final
/// still-unregistered confirmation before the irreversible phase.
#[cfg(test)]
pub(crate) async fn execute_unregistered_collection(
    db: &RegisteredGlobalDb,
    plan: &UnregisteredCollectionPlan,
    profile_root: &Path,
) -> crate::errors::Result<CollectionOutcome> {
    execute_unregistered_collection_controlled(
        db,
        plan,
        profile_root,
        unbounded_collection_control(),
    )
    .await
}

pub(crate) async fn execute_unregistered_collection_controlled(
    db: &RegisteredGlobalDb,
    plan: &UnregisteredCollectionPlan,
    profile_root: &Path,
    control: CollectionControl<'_>,
) -> crate::errors::Result<CollectionOutcome> {
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
        ) {
            continue;
        }
        // Containment + shape: only ever delete an exact, safely-named
        // `<profile>/projects/<id>` leaf.
        let expected = profile_root
            .join("projects")
            .join(&finding.project_dir_name);
        if expected != finding.data_root
            || crate::storage::validate_project_id(&finding.project_dir_name).is_err()
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
            .join(crate::storage::STORE_MANIFEST_FILENAME);
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
                crate::db::engine::params![finding.project_dir_name.as_str()],
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
pub(crate) async fn sweep_unregistered_stores(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
    apply: bool,
) -> crate::errors::Result<UnregisteredStoreSweepReport> {
    let cancellation = CancellationToken::new();
    let report = sweep_unregistered_store_page(
        db,
        profile_root,
        UnregisteredStoreSweepRequestV1 {
            cursor: None,
            limit: DEFAULT_UNREGISTERED_STORE_PAGE_LIMIT,
            retention_secs,
            now,
            apply,
            cancellation: &cancellation,
            deadline: MonotonicDeadline::at(
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            ),
        },
    )
    .await?;
    let completion_is_terminal = report.completion == UnregisteredSweepCompletionV1::Complete;
    let receipt_is_consistent = (!report.applied || apply && completion_is_terminal)
        && (completion_is_terminal || report.next_cursor.is_none())
        && (apply || report.outcome.collected.is_empty());
    if !receipt_is_consistent {
        return Err(crate::errors::TraceDecayError::Config {
            message: "unregistered-store page returned an inconsistent receipt".to_owned(),
        });
    }
    Ok(report)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

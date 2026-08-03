//! Destructive branch-store administration.

use std::path::{Path, PathBuf};

use crate::branch_meta::BranchMeta;

mod transaction;

/// Destructive branch-store operation accepted by the daemon-owned
/// administrative path. The tagged representation is also the wire contract
/// used by the CLI.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BranchAdminAction {
    Remove { branch: String },
    RemoveAll,
    Gc,
}

/// Typed outcome returned to the CLI after a destructive branch operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchAdminOutcome {
    NoTracking,
    NotTracked,
    NoChanges,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BranchAdminReport {
    pub outcome: BranchAdminOutcome,
    #[serde(default)]
    pub removed_branches: Vec<String>,
    #[serde(default)]
    pub removed_orphan_dbs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

/// A branch metadata mutation selected while holding the shared branch lock.
/// The daemon inspects [`Self::database_paths`] and proves every matching store
/// owner is gone before calling the daemon-only transaction method.
pub struct PreparedBranchAdminMutation {
    project_root: PathBuf,
    tracedecay_dir: PathBuf,
    metadata_before: Option<String>,
    metadata_after: Option<String>,
    database_paths: Vec<PathBuf>,
    gc_branches: Vec<String>,
    report: BranchAdminReport,
    _branch_lock: std::fs::File,
}

impl PreparedBranchAdminMutation {
    pub fn database_paths(&self) -> &[PathBuf] {
        &self.database_paths
    }

    pub fn report(&self) -> &BranchAdminReport {
        &self.report
    }

    /// Quarantines the selected `SQLite` families under a durable journal and
    /// publishes branch metadata as the tracked-branch commit point. Any failure
    /// before that point rolls every move back; failures after it retain recovery
    /// evidence for cleanup on the next branch-lock acquisition.
    #[cfg(test)]
    fn commit(self) -> crate::errors::Result<BranchAdminReport> {
        self.commit_with_precommit_hook(None, || Ok(()), |_| Ok(()), || Ok(()), |_| Ok(()))
    }

    pub(crate) fn finish_without_database_deletion(
        self,
    ) -> crate::errors::Result<BranchAdminReport> {
        if !self.database_paths.is_empty() {
            return Err(crate::errors::TraceDecayError::Config {
                message: "branch database deletion requires daemon store administration"
                    .to_string(),
            });
        }
        self.commit_with_precommit_hook(None, || Ok(()), |_| Ok(()), || Ok(()), |_| Ok(()))
    }

    pub(crate) fn commit_registered<V>(
        self,
        validate_quarantined_stores: V,
    ) -> crate::errors::Result<BranchAdminReport>
    where
        V: FnOnce(&[PathBuf]) -> crate::errors::Result<()>,
    {
        self.commit_with_precommit_hook(
            None,
            || Ok(()),
            validate_quarantined_stores,
            || Ok(()),
            |_| Ok(()),
        )
    }

    #[cfg(test)]
    fn commit_with_hook<H>(
        self,
        transaction_id: Option<&str>,
        hook: H,
    ) -> crate::errors::Result<BranchAdminReport>
    where
        H: FnMut(transaction::TransactionPhase) -> crate::errors::Result<()>,
    {
        self.commit_with_precommit_hook(transaction_id, || Ok(()), |_| Ok(()), || Ok(()), hook)
    }

    fn commit_with_precommit_hook<P, V, R, H>(
        self,
        transaction_id: Option<&str>,
        publish_deleting: P,
        validate_quarantined_stores: V,
        rollback_deleting: R,
        hook: H,
    ) -> crate::errors::Result<BranchAdminReport>
    where
        P: FnOnce() -> crate::errors::Result<()>,
        V: FnOnce(&[PathBuf]) -> crate::errors::Result<()>,
        R: FnOnce() -> crate::errors::Result<()>,
        H: FnMut(transaction::TransactionPhase) -> crate::errors::Result<()>,
    {
        if self.report.outcome != BranchAdminOutcome::Removed {
            return Ok(self.report);
        }
        let project_root = self.project_root.clone();
        let gc_branches = self.gc_branches.clone();
        transaction::commit_with_hook(
            transaction::CommitRequest {
                tracedecay_dir: &self.tracedecay_dir,
                supplied_transaction_id: transaction_id,
                database_paths: &self.database_paths,
                metadata_before: self.metadata_before,
                metadata_after: self.metadata_after,
            },
            publish_deleting,
            move |quarantine_paths| {
                validate_quarantined_stores(quarantine_paths)?;
                for branch in &gc_branches {
                    if super::is_branch_ref_present(&project_root, branch) {
                        return Err(crate::errors::TraceDecayError::Config {
                            message: format!(
                                "branch ref '{branch}' reappeared before GC metadata publication; deletion rolled back"
                            ),
                        });
                    }
                }
                Ok(())
            },
            rollback_deleting,
            hook,
        )?;
        Ok(self.report)
    }
}

/// Selects a destructive branch mutation while holding the same lock used by
/// branch add. This function does not mutate metadata or unlink any file.
pub fn prepare_branch_admin_mutation(
    project_root: &Path,
    tracedecay_dir: &Path,
    action: BranchAdminAction,
    branch_gc_days: u64,
    orphan_db_gc_days: u64,
) -> crate::errors::Result<PreparedBranchAdminMutation> {
    let branch_lock = acquire_branch_add_lock_blocking(tracedecay_dir)?;
    let (mut meta, metadata_before) = load_branch_meta_exact(tracedecay_dir)?;
    let default_branch = meta.as_ref().map(|meta| meta.default_branch.clone());
    let mut database_paths = Vec::new();
    let mut removed_branches = Vec::new();
    let mut removed_orphan_dbs = Vec::new();
    let mut gc_branches = Vec::new();
    let mut outcome = BranchAdminOutcome::NoChanges;

    match action {
        BranchAdminAction::Remove { branch } => {
            let Some(branch_meta) = meta.as_mut() else {
                outcome = BranchAdminOutcome::NoTracking;
                return Ok(PreparedBranchAdminMutation {
                    project_root: project_root.to_path_buf(),
                    tracedecay_dir: tracedecay_dir.to_path_buf(),
                    metadata_before: metadata_before.clone(),
                    metadata_after: metadata_before.clone(),
                    database_paths,
                    gc_branches,
                    report: BranchAdminReport {
                        outcome,
                        removed_branches,
                        removed_orphan_dbs,
                        default_branch,
                    },
                    _branch_lock: branch_lock,
                });
            };
            if branch == branch_meta.default_branch {
                return Err(crate::errors::TraceDecayError::Config {
                    message: format!("cannot remove default branch '{branch}'"),
                });
            }
            if let Some(entry) = branch_meta.remove_branch(&branch) {
                database_paths.push(tracedecay_dir.join(entry.db_file));
                removed_branches.push(branch);
                outcome = BranchAdminOutcome::Removed;
            } else {
                outcome = BranchAdminOutcome::NotTracked;
            }
        }
        BranchAdminAction::RemoveAll => {
            let Some(branch_meta) = meta.as_mut() else {
                outcome = BranchAdminOutcome::NoTracking;
                return Ok(PreparedBranchAdminMutation {
                    project_root: project_root.to_path_buf(),
                    tracedecay_dir: tracedecay_dir.to_path_buf(),
                    metadata_before: metadata_before.clone(),
                    metadata_after: metadata_before.clone(),
                    database_paths,
                    gc_branches,
                    report: BranchAdminReport {
                        outcome,
                        removed_branches,
                        removed_orphan_dbs,
                        default_branch,
                    },
                    _branch_lock: branch_lock,
                });
            };
            let mut removed = branch_meta.remove_all_branches();
            removed.sort_by(|left, right| left.0.cmp(&right.0));
            for (branch, entry) in removed {
                removed_branches.push(branch);
                database_paths.push(tracedecay_dir.join(entry.db_file));
            }
            if !removed_branches.is_empty() {
                outcome = BranchAdminOutcome::Removed;
            }
        }
        BranchAdminAction::Gc => {
            let now = super::now_unix_secs();
            if let Some(branch_meta) = meta.as_mut() {
                let branch_grace = branch_gc_days.saturating_mul(86_400);
                let default = branch_meta.default_branch.clone();
                let mut candidates = branch_meta
                    .branches
                    .iter()
                    .filter(|(name, entry)| **name != default && !entry.gc_protected)
                    .filter(|(name, entry)| {
                        !super::is_branch_ref_present(project_root, name)
                            && now.saturating_sub(super::parse_unix_secs(&entry.last_synced_at))
                                >= branch_grace
                    })
                    .map(|(name, entry)| (name.clone(), entry.db_file.clone()))
                    .collect::<Vec<_>>();
                candidates.sort_by(|left, right| left.0.cmp(&right.0));
                for (name, db_file) in candidates {
                    branch_meta.remove_branch(&name);
                    gc_branches.push(name.clone());
                    removed_branches.push(name);
                    database_paths.push(tracedecay_dir.join(db_file));
                }
            }
            let referenced = meta
                .as_ref()
                .map(|meta| {
                    meta.branches
                        .values()
                        .map(|entry| tracedecay_dir.join(&entry.db_file))
                        .collect::<std::collections::HashSet<_>>()
                })
                .unwrap_or_default();
            removed_orphan_dbs =
                select_orphan_dbs(tracedecay_dir, &referenced, orphan_db_gc_days, now);
            database_paths.extend(removed_orphan_dbs.iter().cloned());
            if !database_paths.is_empty() {
                outcome = BranchAdminOutcome::Removed;
            } else if meta.is_none() {
                outcome = BranchAdminOutcome::NoTracking;
            }
        }
    }

    database_paths.sort();
    database_paths.dedup();
    let metadata_after = if removed_branches.is_empty() {
        metadata_before.clone()
    } else {
        Some(crate::branch_meta::serialize_branch_meta(
            meta.as_ref()
                .ok_or_else(|| crate::errors::TraceDecayError::Config {
                    message: "tracked branch deletion lost branch metadata before commit"
                        .to_string(),
                })?,
        )?)
    };
    Ok(PreparedBranchAdminMutation {
        project_root: project_root.to_path_buf(),
        tracedecay_dir: tracedecay_dir.to_path_buf(),
        metadata_before,
        metadata_after,
        database_paths,
        gc_branches,
        report: BranchAdminReport {
            outcome,
            removed_branches,
            removed_orphan_dbs,
            default_branch,
        },
        _branch_lock: branch_lock,
    })
}

/// Removes a branch store that branch-add published but could not sync.
/// The caller must still hold the branch-add lock.
pub(super) fn rollback_published_branch_tracking(
    tracedecay_dir: &Path,
    branch_name: &str,
    db_file: &str,
    database_path: &Path,
) -> crate::errors::Result<()> {
    let (meta, metadata_before) = load_branch_meta_exact(tracedecay_dir)?;
    let mut meta = meta.ok_or_else(|| crate::errors::TraceDecayError::Config {
        message: format!("cannot roll back branch '{branch_name}': branch metadata is missing"),
    })?;
    if meta
        .branches
        .get(branch_name)
        .is_none_or(|entry| entry.db_file != db_file)
    {
        return Err(crate::errors::TraceDecayError::Config {
            message: format!(
                "cannot roll back branch '{branch_name}': published database path changed"
            ),
        });
    }
    meta.remove_branch(branch_name);
    let metadata_after = Some(crate::branch_meta::serialize_branch_meta(&meta)?);
    let database_paths = vec![database_path.to_path_buf()];

    #[cfg(test)]
    let validate_precommit = |_database_paths: &[PathBuf]| Ok(());
    #[cfg(not(test))]
    let validate_precommit = ensure_no_open_store_holders;

    transaction::commit_with_hook(
        transaction::CommitRequest {
            tracedecay_dir,
            supplied_transaction_id: None,
            database_paths: &database_paths,
            metadata_before,
            metadata_after,
        },
        || Ok(()),
        validate_precommit,
        || Ok(()),
        |_| Ok(()),
    )
}

#[cfg(not(test))]
fn ensure_no_open_store_holders(database_paths: &[PathBuf]) -> crate::errors::Result<()> {
    let options = crate::open_store_holders::OpenStoreHolderScanOptions {
        include_current_process: true,
        excluded_current_process_fds: std::collections::BTreeSet::new(),
    };
    let scan = crate::open_store_holders::scan_with_options(database_paths, &options).map_err(
        |error| crate::errors::TraceDecayError::Config {
            message: format!("failed to inspect open branch stores: {error}"),
        },
    )?;
    match scan {
        crate::open_store_holders::OpenStoreHolderScan::Supported(holders)
            if holders.is_empty() =>
        {
            Ok(())
        }
        crate::open_store_holders::OpenStoreHolderScan::Supported(holders) => {
            let details = holders
                .into_iter()
                .map(|holder| format!("pid {} ({})", holder.pid, holder.command))
                .collect::<Vec<_>>()
                .join(", ");
            Err(crate::errors::TraceDecayError::Config {
                message: format!(
                    "cannot delete branch stores while processes still hold them: {details}"
                ),
            })
        }
        crate::open_store_holders::OpenStoreHolderScan::Unsupported { reason } => {
            Err(crate::errors::TraceDecayError::Config {
                message: format!(
                    "cannot prove branch stores are closed: {reason}; destructive branch operation refused"
                ),
            })
        }
    }
}

/// Strict removal entry point used by daemon-owned administrative operations.
pub fn remove_tracked_branch_store_checked(
    _tracedecay_dir: &Path,
    _branch: &str,
) -> crate::errors::Result<BranchAdminReport> {
    Err(crate::errors::TraceDecayError::Config {
        message: "branch database deletion requires daemon store administration; use tracedecay_admin_branch through the managed daemon"
            .to_string(),
    })
}

fn load_branch_meta_exact(
    tracedecay_dir: &Path,
) -> crate::errors::Result<(Option<BranchMeta>, Option<String>)> {
    let path = tracedecay_dir.join(crate::storage::BRANCH_META_FILENAME);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
        Err(error) => {
            return Err(crate::errors::TraceDecayError::Config {
                message: format!(
                    "cannot inspect branch metadata at '{}': {error}",
                    path.display()
                ),
            });
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(crate::errors::TraceDecayError::Config {
            message: format!(
                "cannot administer branch stores with ambiguous metadata path '{}'",
                path.display()
            ),
        });
    }
    let serialized =
        std::fs::read_to_string(&path).map_err(|error| crate::errors::TraceDecayError::Config {
            message: format!(
                "cannot read branch metadata at '{}': {error}",
                path.display()
            ),
        })?;
    let meta = crate::branch_meta::parse(&serialized).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!(
                "cannot administer branch stores with corrupt or unreadable metadata at '{}': {error}",
                path.display()
            ),
        }
    })?;
    Ok((Some(meta), Some(serialized)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BranchAdminRecoveryDisposition {
    PreCommitRollback,
    CommittedCleanup,
}

pub(crate) struct PreparedBranchAdminRecovery {
    tracedecay_dir: PathBuf,
    pending: transaction::PendingRecovery,
    database_paths: Vec<PathBuf>,
    _branch_lock: std::fs::File,
}

impl PreparedBranchAdminRecovery {
    pub(crate) fn disposition(&self) -> BranchAdminRecoveryDisposition {
        match self.pending.disposition() {
            transaction::RecoveryDisposition::PreCommitRollback => {
                BranchAdminRecoveryDisposition::PreCommitRollback
            }
            transaction::RecoveryDisposition::CommittedCleanup => {
                BranchAdminRecoveryDisposition::CommittedCleanup
            }
        }
    }

    pub(crate) fn database_paths(&self) -> &[PathBuf] {
        &self.database_paths
    }

    pub(crate) fn recover<V, T>(
        self,
        validate_stores: V,
        transition_tombstones: T,
    ) -> crate::errors::Result<()>
    where
        V: FnOnce(&[PathBuf]) -> crate::errors::Result<()>,
        T: FnOnce(BranchAdminRecoveryDisposition) -> crate::errors::Result<()>,
    {
        self.pending
            .recover(&self.tracedecay_dir, validate_stores, |disposition| {
                transition_tombstones(match disposition {
                    transaction::RecoveryDisposition::PreCommitRollback => {
                        BranchAdminRecoveryDisposition::PreCommitRollback
                    }
                    transaction::RecoveryDisposition::CommittedCleanup => {
                        BranchAdminRecoveryDisposition::CommittedCleanup
                    }
                })
            })
    }
}

pub(crate) fn prepare_pending_branch_admin_recovery(
    tracedecay_dir: &Path,
) -> crate::errors::Result<Option<PreparedBranchAdminRecovery>> {
    let branch_lock = acquire_branch_add_lock_blocking_raw(tracedecay_dir)?;
    let Some(pending) = transaction::prepare_pending_recovery(tracedecay_dir)? else {
        return Ok(None);
    };
    let database_paths = pending.database_paths(tracedecay_dir);
    Ok(Some(PreparedBranchAdminRecovery {
        tracedecay_dir: tracedecay_dir.to_path_buf(),
        pending,
        database_paths,
        _branch_lock: branch_lock,
    }))
}

pub(super) fn ensure_no_pending_branch_admin_recovery(
    tracedecay_dir: &Path,
) -> crate::errors::Result<()> {
    transaction::ensure_no_pending_recovery(tracedecay_dir)
}

// The lock primitives themselves moved into
// `tracedecay_runtime_core::branch` with `branch_meta`; only the
// pending-recovery gate stayed behind, and it reaches the kernel through
// `tracedecay_runtime_core::ports::branch_admin_recovery`.
use super::{
    acquire_branch_add_lock_blocking_raw,
    acquire_branch_lock_blocking as acquire_branch_add_lock_blocking,
};

fn branch_db_family_paths(db_path: &Path) -> [PathBuf; 3] {
    let mut wal = db_path.to_path_buf();
    wal.set_extension("db-wal");
    let mut shm = db_path.to_path_buf();
    shm.set_extension("db-shm");
    [db_path.to_path_buf(), wal, shm]
}

pub(super) fn remove_branch_db_files_checked(db_path: &Path) -> crate::errors::Result<()> {
    for path in branch_db_family_paths(db_path) {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(crate::errors::TraceDecayError::Config {
                    message: format!(
                        "failed to delete branch store file '{}': {error}",
                        path.display()
                    ),
                });
            }
        }
    }
    Ok(())
}

pub(super) fn select_orphan_dbs(
    tracedecay_dir: &Path,
    referenced: &std::collections::HashSet<PathBuf>,
    orphan_db_gc_days: u64,
    now: u64,
) -> Vec<PathBuf> {
    let mut selected = Vec::new();
    let branches_dir = tracedecay_dir.join("branches");
    let Ok(entries) = std::fs::read_dir(&branches_dir) else {
        return selected;
    };
    let orphan_grace = orphan_db_gc_days.saturating_mul(86_400);
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("db") || referenced.contains(&path) {
            continue;
        }
        let mtime_secs = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        if now.saturating_sub(mtime_secs) >= orphan_grace {
            selected.push(path);
        }
    }
    selected.sort();
    selected
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

//! Destructive branch-store administration.

use std::path::{Path, PathBuf};

use crate::branch_meta::BranchMeta;

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
/// The daemon reserves [`Self::database_paths`] through the store runtime
/// registry's destructive maintenance path before committing.
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

    #[cfg(test)]
    fn commit(self) -> crate::errors::Result<BranchAdminReport> {
        self.commit_with_hook(|_| Ok(()))
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
        Ok(self.report)
    }

    /// CAS-publishes the exact prepared branch metadata, then unlinks every
    /// selected DB/WAL/SHM family. The caller must hold the canonical runtime
    /// destructive reservation until this returns.
    pub(crate) fn commit_destructive(self) -> crate::errors::Result<BranchAdminReport> {
        self.commit_with_hook(|_| Ok(()))
    }

    fn commit_with_hook<H>(self, mut hook: H) -> crate::errors::Result<BranchAdminReport>
    where
        H: FnMut(BranchAdminCommitBoundary) -> crate::errors::Result<()>,
    {
        if self.report.outcome != BranchAdminOutcome::Removed {
            return Ok(self.report);
        }
        let (_, current_metadata) = load_branch_meta_exact(&self.tracedecay_dir)?;
        if current_metadata != self.metadata_before {
            return Err(crate::errors::TraceDecayError::Config {
                message:
                    "branch metadata changed after deletion selection; destructive CAS refused"
                        .to_owned(),
            });
        }
        for branch in &self.gc_branches {
            if super::is_branch_ref_present(&self.project_root, branch) {
                return Err(crate::errors::TraceDecayError::Config {
                    message: format!(
                        "branch ref '{branch}' reappeared before GC metadata publication; deletion refused"
                    ),
                });
            }
        }
        hook(BranchAdminCommitBoundary::BeforeMetadataCas)?;
        if self.metadata_before != self.metadata_after {
            let after = self.metadata_after.as_deref().ok_or_else(|| {
                crate::errors::TraceDecayError::Config {
                    message: "tracked branch deletion cannot remove branch metadata entirely"
                        .to_owned(),
                }
            })?;
            crate::branch_meta::save_branch_meta_serialized(&self.tracedecay_dir, after).map_err(
                |error| crate::errors::TraceDecayError::Config {
                    message: format!(
                        "cannot publish branch metadata '{}': {error}",
                        self.tracedecay_dir
                            .join(crate::storage::BRANCH_META_FILENAME)
                            .display()
                    ),
                },
            )?;
        }
        hook(BranchAdminCommitBoundary::AfterMetadataCas)?;
        for path in &self.database_paths {
            remove_branch_db_files_checked(path)?;
        }
        Ok(self.report)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchAdminCommitBoundary {
    BeforeMetadataCas,
    AfterMetadataCas,
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

/// Retires branch metadata that branch-add published but could not sync.
/// The unreferenced database is left for canonical orphan collection.
/// The caller must still hold the branch-add lock.
pub(super) fn rollback_published_branch_tracking(
    tracedecay_dir: &Path,
    branch_name: &str,
    db_file: &str,
    database_path: &Path,
) -> crate::errors::Result<()> {
    if tracedecay_dir.join(db_file) != database_path {
        return Err(crate::errors::TraceDecayError::Config {
            message: format!(
                "cannot roll back branch '{branch_name}': published database path changed"
            ),
        });
    }
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
    let (_, current_metadata) = load_branch_meta_exact(tracedecay_dir)?;
    if current_metadata != metadata_before {
        return Err(crate::errors::TraceDecayError::Config {
            message: format!("cannot roll back branch '{branch_name}': branch metadata changed"),
        });
    }
    let after = metadata_after.ok_or_else(|| crate::errors::TraceDecayError::Config {
        message: format!("cannot roll back branch '{branch_name}': metadata disappeared"),
    })?;
    crate::branch_meta::save_branch_meta_serialized(tracedecay_dir, &after).map_err(|error| {
        crate::errors::TraceDecayError::Config {
            message: format!("cannot retire failed branch '{branch_name}': {error}"),
        }
    })
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

use super::acquire_branch_lock_blocking as acquire_branch_add_lock_blocking;

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

//! Branch-tracking administration.

use std::path::{Path, PathBuf};

use crate::branch_meta::BranchMeta;
use tracedecay_private_fs::FileLease;

/// Branch-tracking operation accepted by the daemon-owned administrative
/// path. The tagged representation is also the wire contract used by the CLI.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BranchAdminAction {
    Remove { branch: String },
    RemoveAll,
    Gc,
}

/// Typed outcome returned to the CLI after a branch administration operation.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
}

/// Exact single-store provenance selected for cleanup alongside a metadata
/// removal. Branch entries without sealed graph provenance are deliberately
/// absent: destructive Git/worktree cleanup must never guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleStoreBranchRetirementV1 {
    pub branch: String,
    pub source: crate::branch_meta::BranchGraphSourceV1,
}

/// A branch metadata mutation selected while holding the shared branch lock.
/// Every branch is served by the single project store, so the metadata entry
/// is the only store state a removal retires.
pub struct PreparedBranchAdminMutation {
    project_root: PathBuf,
    tracedecay_dir: PathBuf,
    metadata_before: Option<String>,
    metadata_after: Option<String>,
    gc_branches: Vec<String>,
    single_store_retirements: Vec<SingleStoreBranchRetirementV1>,
    report: BranchAdminReport,
    _branch_lock: FileLease,
}

impl PreparedBranchAdminMutation {
    pub fn report(&self) -> &BranchAdminReport {
        &self.report
    }

    pub fn single_store_retirements(&self) -> &[SingleStoreBranchRetirementV1] {
        &self.single_store_retirements
    }

    /// CAS-publishes the exact prepared branch metadata.
    #[hotpath::measure(label = "runtime_core.branch.commit_admin_mutation")]
    pub fn commit(self) -> tracedecay_domain::errors::Result<BranchAdminReport> {
        if self.report.outcome != BranchAdminOutcome::Removed {
            return Ok(self.report);
        }
        let (_, current_metadata) = load_branch_meta_exact(&self.tracedecay_dir)?;
        if current_metadata != self.metadata_before {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "branch metadata changed after selection; branch admin CAS refused"
                    .to_owned(),
            });
        }
        for branch in &self.gc_branches {
            if super::local_branch_exists(&self.project_root, branch) {
                return Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "branch ref '{branch}' reappeared before GC metadata publication; deletion refused"
                    ),
                });
            }
        }
        if self.metadata_before != self.metadata_after {
            let after = self.metadata_after.as_deref().ok_or_else(|| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: "tracked branch deletion cannot remove branch metadata entirely"
                        .to_owned(),
                }
            })?;
            crate::branch_meta::save_branch_meta_serialized(&self.tracedecay_dir, after).map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "cannot publish branch metadata '{}': {error}",
                        self.tracedecay_dir
                            .join(crate::storage::BRANCH_META_FILENAME)
                            .display()
                    ),
                },
            )?;
        }
        Ok(self.report)
    }
}

/// Selects a branch metadata mutation while holding the same lock used by
/// branch add. This function does not mutate metadata.
#[hotpath::measure(label = "runtime_core.branch.prepare_admin_mutation")]
pub fn prepare_branch_admin_mutation(
    project_root: &Path,
    tracedecay_dir: &Path,
    action: BranchAdminAction,
    branch_gc_days: u64,
) -> tracedecay_domain::errors::Result<PreparedBranchAdminMutation> {
    let branch_lock = acquire_branch_add_lock_blocking(tracedecay_dir)?;
    let (mut meta, metadata_before) = load_branch_meta_exact(tracedecay_dir)?;
    let default_branch = meta.as_ref().map(|meta| meta.default_branch.clone());
    let mut removed_branches = Vec::new();
    let mut gc_branches = Vec::new();
    let mut single_store_retirements = Vec::new();
    let mut outcome = BranchAdminOutcome::NoChanges;

    match action {
        BranchAdminAction::Remove { branch } => {
            if let Some(branch_meta) = meta.as_mut() {
                if branch == branch_meta.default_branch {
                    return Err(tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!("cannot remove default branch '{branch}'"),
                    });
                }
                if let Some(entry) = branch_meta.remove_branch(&branch) {
                    if let Some(source) = entry.graph_source {
                        single_store_retirements.push(SingleStoreBranchRetirementV1 {
                            branch: branch.clone(),
                            source,
                        });
                    }
                    removed_branches.push(branch);
                    outcome = BranchAdminOutcome::Removed;
                } else {
                    outcome = BranchAdminOutcome::NotTracked;
                }
            } else {
                outcome = BranchAdminOutcome::NoTracking;
            }
        }
        BranchAdminAction::RemoveAll => {
            if let Some(branch_meta) = meta.as_mut() {
                let mut removed = branch_meta.remove_all_branches();
                removed.sort_by(|left, right| left.0.cmp(&right.0));
                for (branch, entry) in removed {
                    if let Some(source) = entry.graph_source {
                        single_store_retirements.push(SingleStoreBranchRetirementV1 {
                            branch: branch.clone(),
                            source,
                        });
                    }
                    removed_branches.push(branch);
                }
                if !removed_branches.is_empty() {
                    outcome = BranchAdminOutcome::Removed;
                }
            } else {
                outcome = BranchAdminOutcome::NoTracking;
            }
        }
        BranchAdminAction::Gc => {
            if let Some(branch_meta) = meta.as_mut() {
                let now = super::now_unix_secs();
                let branch_grace = branch_gc_days.saturating_mul(86_400);
                let default = branch_meta.default_branch.clone();
                let mut candidates = branch_meta
                    .branches
                    .iter()
                    .filter(|(name, entry)| **name != default && !entry.gc_protected)
                    .filter(|(name, entry)| {
                        !super::local_branch_exists(project_root, name)
                            && now.saturating_sub(super::parse_unix_secs(&entry.last_synced_at))
                                >= branch_grace
                    })
                    .map(|(name, entry)| (name.clone(), entry.graph_source.clone()))
                    .collect::<Vec<_>>();
                candidates.sort_by(|left, right| left.0.cmp(&right.0));
                for (name, source) in candidates {
                    branch_meta.remove_branch(&name);
                    gc_branches.push(name.clone());
                    removed_branches.push(name.clone());
                    if let Some(source) = source {
                        single_store_retirements.push(SingleStoreBranchRetirementV1 {
                            branch: name,
                            source,
                        });
                    }
                }
                if !removed_branches.is_empty() {
                    outcome = BranchAdminOutcome::Removed;
                }
            } else {
                outcome = BranchAdminOutcome::NoTracking;
            }
        }
    }

    let metadata_after = if removed_branches.is_empty() {
        metadata_before.clone()
    } else {
        Some(crate::branch_meta::serialize_branch_meta(
            meta.as_ref()
                .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
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
        gc_branches,
        single_store_retirements,
        report: BranchAdminReport {
            outcome,
            removed_branches,
            default_branch,
        },
        _branch_lock: branch_lock,
    })
}

fn load_branch_meta_exact(
    tracedecay_dir: &Path,
) -> tracedecay_domain::errors::Result<(Option<BranchMeta>, Option<String>)> {
    let path = tracedecay_dir.join(crate::storage::BRANCH_META_FILENAME);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
        Err(error) => {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: format!(
                    "cannot inspect branch metadata at '{}': {error}",
                    path.display()
                ),
            });
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "cannot administer branch stores with ambiguous metadata path '{}'",
                path.display()
            ),
        });
    }
    let serialized = std::fs::read_to_string(&path).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "cannot read branch metadata at '{}': {error}",
                path.display()
            ),
        }
    })?;
    let meta = crate::branch_meta::parse(&serialized).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "cannot administer branch stores with corrupt or unreadable metadata at '{}': {error}",
                path.display()
            ),
        }
    })?;
    Ok((Some(meta), Some(serialized)))
}

use super::acquire_branch_lock_blocking as acquire_branch_add_lock_blocking;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

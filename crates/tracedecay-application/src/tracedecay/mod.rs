//! Transport-neutral store authorities shared by the composition root.
//!
//! Branch diagnostics, fact-owner identity, and store-metadata counters live
//! here so `TraceDecay` can retain owner handles and delegate.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_runtime_core::branch;
use tracedecay_runtime_core::branch_meta;
use tracedecay_runtime_core::config::db_filename;

mod store_meta;

pub use store_meta::{
    add_local_counter, get_local_counter, get_tokens_saved, reset_local_counter, set_tokens_saved,
};

#[derive(Debug, Clone, Serialize)]
pub struct TrackedBranchDiagnostic {
    pub name: String,
    pub db_file: String,
    pub db_path: PathBuf,
    pub db_exists: bool,
    pub size_bytes: u64,
    pub parent: Option<String>,
    pub parent_db_path: Option<PathBuf>,
    pub parent_db_exists: Option<bool>,
    pub created_at: String,
    pub last_synced_at: String,
    pub is_default: bool,
    pub is_current: bool,
    pub is_open_active: bool,
    pub is_serving: bool,
    pub is_ready: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchDiagnostics {
    pub tracking_enabled: bool,
    pub default_branch: Option<String>,
    pub current_branch: Option<String>,
    pub open_active_branch: Option<String>,
    pub serving_branch: Option<String>,
    pub serving_db_path: PathBuf,
    pub serving_db_exists: bool,
    pub branch_drifted: bool,
    pub branch_resolution: String,
    pub is_fallback: bool,
    pub fallback_target: Option<String>,
    pub fallback_warning: Option<String>,
    pub live_branch_tracked: bool,
    pub live_branch_ready: bool,
    pub live_branch_db_path: Option<PathBuf>,
    pub live_branch_db_exists: Option<bool>,
    pub nearest_tracked_ancestor: Option<String>,
    pub nearest_tracked_ancestor_db_path: Option<PathBuf>,
    pub nearest_tracked_ancestor_db_exists: Option<bool>,
    pub tracked_branch_count: usize,
    pub branches: Vec<TrackedBranchDiagnostic>,
    pub warnings: Vec<String>,
}

/// Resolves the only project-memory owner accepted by core routes.
///
/// The ID is supplied by the resolved store layout, never reconstructed
/// from a filesystem path or a caller-provided display label.
pub fn project_memory_owner_from_layout_id(project_id: Option<&str>) -> Result<FactOwnerV1> {
    let project_id = project_id.ok_or_else(|| TraceDecayError::Config {
        message: "active project has no authoritative project_id for memory".to_string(),
    })?;
    let project_id =
        ProjectId::new(project_id.to_owned()).map_err(|error| TraceDecayError::Config {
            message: format!("invalid authoritative project_id for memory: {error}"),
        })?;
    Ok(FactOwnerV1::Project { project_id })
}

/// Resolves the serving-branch provenance for a given live branch.
///
/// Returns `(db_path, serving_branch, fallback_warning)`. Every branch is
/// served by the single project graph store, so `db_path` is always the
/// canonical main database; the branch argument only decides which
/// tracked branch's provenance the open is scoped to and whether the
/// caller must be warned about a fallback.
pub fn resolve_db_for_branch(
    project_root: &Path,
    tracedecay_dir: &Path,
    branch_name: Option<&str>,
) -> (PathBuf, Option<String>, Option<String>) {
    let default_db = tracedecay_dir.join(db_filename(tracedecay_dir));

    let Some(meta) = branch_meta::load_branch_meta(tracedecay_dir) else {
        return (default_db, None, None);
    };

    let Some(branch_name) = branch_name else {
        return (
            default_db,
            Some(meta.default_branch.clone()),
            Some("detached HEAD — using default branch index".to_string()),
        );
    };

    if meta.is_query_eligible(branch_name) {
        return (default_db, Some(branch_name.to_string()), None);
    }

    if let Some(ancestor) = branch::find_nearest_tracked_ancestor(project_root, branch_name, &meta)
    {
        return (
            default_db,
            Some(ancestor.clone()),
            Some(format!(
                "branch '{branch_name}' is not tracked — serving from '{ancestor}'. \
                         Run `tracedecay branch add {branch_name}` to track it."
            )),
        );
    }

    let serving = meta.default_branch.clone();
    (
        default_db,
        Some(serving),
        Some(format!(
            "branch '{branch_name}' is not tracked — serving from '{}'. \
             Run `tracedecay branch add {branch_name}` to track it.",
            meta.default_branch
        )),
    )
}

pub fn build_branch_diagnostics(
    project_root: &Path,
    data_root: &Path,
    open_active_branch: Option<String>,
    serving_branch: Option<String>,
    fallback_warning: Option<String>,
    serving_db_path: PathBuf,
    serving_source: Option<(&str, &str)>,
) -> BranchDiagnostics {
    let meta = branch_meta::load_branch_meta(data_root);
    let observed_serving_branch = serving_source.and_then(|(reference, revision)| {
        meta.as_ref().and_then(|meta| {
            meta.branches.iter().find_map(|(name, entry)| {
                entry
                    .graph_source
                    .as_ref()
                    .filter(|source| {
                        source.reference == reference
                            && source.source_oid == revision
                            && meta.is_query_eligible(name)
                    })
                    .map(|_| name.clone())
            })
        })
    });
    let (open_active_branch, serving_branch, fallback_warning) =
        if let Some(branch) = observed_serving_branch {
            (Some(branch.clone()), Some(branch), None)
        } else {
            (open_active_branch, serving_branch, fallback_warning)
        };
    let current_branch = branch::current_branch(project_root);
    let tracking_enabled = meta.as_ref().is_some_and(|m| !m.branches.is_empty());
    let branch_drifted =
        tracking_enabled && current_branch.as_deref() != open_active_branch.as_deref();
    let is_fallback = fallback_warning.is_some();
    let fallback_target = if is_fallback {
        serving_branch.clone()
    } else {
        None
    };
    let serving_db_exists = serving_db_path.exists();

    let (
        live_branch_tracked,
        live_branch_ready,
        live_branch_db_path,
        live_branch_db_exists,
        nearest_tracked_ancestor,
        nearest_tracked_ancestor_db_path,
        nearest_tracked_ancestor_db_exists,
    ) = if let (Some(meta), Some(current)) = (meta.as_ref(), current_branch.as_deref()) {
        let live_branch_tracked = meta.is_tracked(current);
        let live_branch_ready = meta.is_query_eligible(current);
        let live_branch_db_path = if live_branch_tracked {
            branch::resolve_branch_db_path(data_root, current, meta)
        } else {
            None
        };
        let live_branch_db_exists = live_branch_db_path.as_ref().map(|path| path.exists());
        let nearest_tracked_ancestor = if live_branch_ready {
            None
        } else {
            branch::find_nearest_tracked_ancestor(project_root, current, meta)
        };
        let nearest_tracked_ancestor_db_path = nearest_tracked_ancestor
            .as_deref()
            .and_then(|ancestor| branch::resolve_branch_db_path(data_root, ancestor, meta));
        let nearest_tracked_ancestor_db_exists = nearest_tracked_ancestor_db_path
            .as_ref()
            .map(|path| path.exists());
        (
            live_branch_tracked,
            live_branch_ready,
            live_branch_db_path,
            live_branch_db_exists,
            nearest_tracked_ancestor,
            nearest_tracked_ancestor_db_path,
            nearest_tracked_ancestor_db_exists,
        )
    } else {
        (false, false, None, None, None, None, None)
    };

    let mut warnings = Vec::new();
    if branch_drifted && !(live_branch_tracked && !live_branch_ready) {
        warnings.push(format!(
            "branch drift detected: working tree is on '{}' but this instance opened on '{}' and is still serving '{}'. Reopen the index so reads and writes target the live branch.",
            current_branch.as_deref().unwrap_or("detached HEAD"),
            open_active_branch.as_deref().unwrap_or("detached HEAD"),
            serving_branch.as_deref().unwrap_or("default branch"),
        ));
    }
    if !serving_db_exists {
        warnings.push(format!(
            "serving branch '{}' points at a missing DB: {}",
            serving_branch.as_deref().unwrap_or("default branch"),
            serving_db_path.display(),
        ));
    }
    if let (Some(current), Some(false), Some(path)) = (
        current_branch.as_deref(),
        live_branch_db_exists,
        live_branch_db_path.as_ref(),
    ) {
        warnings.push(format!(
            "tracked branch '{}' is listed in branch metadata but its DB is missing at '{}'; serving '{}' instead.",
            current,
            path.display(),
            serving_branch.as_deref().unwrap_or("default branch"),
        ));
    } else if live_branch_tracked && !live_branch_ready {
        warnings.push(format!(
            "branch '{}' was admitted and its exact index is still building; serving '{}' until publication completes.",
            current_branch.as_deref().unwrap_or("current branch"),
            serving_branch.as_deref().unwrap_or("default branch"),
        ));
    } else if is_fallback {
        match (
            current_branch.as_deref(),
            nearest_tracked_ancestor.as_deref(),
            fallback_target.as_deref(),
        ) {
            (Some(current), Some(ancestor), Some(target)) => warnings.push(format!(
                "branch '{current}' is not tracked; nearest indexed ancestor is '{ancestor}' and tracedecay is serving '{target}' instead."
            )),
            (Some(current), None, Some(target)) => warnings.push(format!(
                "branch '{current}' is not tracked and no indexed ancestor DB was available; tracedecay is serving '{target}' instead."
            )),
            _ => {}
        }
    }

    let branch_resolution = if !tracking_enabled {
        "single_db".to_string()
    } else if live_branch_tracked && !live_branch_ready {
        "indexing".to_string()
    } else if branch_drifted {
        "stale_serving_branch".to_string()
    } else if current_branch.is_none() {
        "detached_default".to_string()
    } else if is_fallback {
        match (
            nearest_tracked_ancestor.as_deref(),
            fallback_target.as_deref(),
        ) {
            (Some(ancestor), Some(target)) if ancestor == target => "fallback_ancestor".to_string(),
            _ => "fallback_default".to_string(),
        }
    } else {
        "exact".to_string()
    };

    let mut branches = Vec::new();
    if let Some(meta) = meta.as_ref() {
        let mut names: Vec<_> = meta.branches.keys().cloned().collect();
        names.sort();
        for name in names {
            let entry = &meta.branches[&name];
            let db_path = data_root.join(&entry.db_file);
            let db_exists = db_path.exists();
            let size_bytes = db_path.metadata().map_or(0, |metadata| metadata.len());
            let parent_db_path = entry
                .parent
                .as_deref()
                .and_then(|parent| branch::resolve_branch_db_path(data_root, parent, meta));
            let parent_db_exists = parent_db_path.as_ref().map(|path| path.exists());
            let mut branch_warnings = Vec::new();
            if !db_exists {
                branch_warnings.push(format!("missing DB at '{}'", db_path.display()));
            }
            if entry.parent.is_some() && parent_db_exists == Some(false) {
                branch_warnings.push("parent DB is missing".to_string());
            }
            branches.push(TrackedBranchDiagnostic {
                name: name.clone(),
                db_file: entry.db_file.clone(),
                db_path,
                db_exists,
                size_bytes,
                parent: entry.parent.clone(),
                parent_db_path,
                parent_db_exists,
                created_at: entry.created_at.clone(),
                last_synced_at: entry.last_synced_at.clone(),
                is_default: name == meta.default_branch,
                is_current: current_branch.as_deref() == Some(name.as_str()),
                is_open_active: open_active_branch.as_deref() == Some(name.as_str()),
                is_serving: serving_branch.as_deref() == Some(name.as_str()),
                is_ready: meta.is_query_eligible(&name),
                warnings: branch_warnings,
            });
        }
    }

    BranchDiagnostics {
        tracking_enabled,
        default_branch: meta.as_ref().map(|m| m.default_branch.clone()),
        current_branch,
        open_active_branch,
        serving_branch,
        serving_db_path,
        serving_db_exists,
        branch_drifted,
        branch_resolution,
        is_fallback,
        fallback_target,
        fallback_warning,
        live_branch_tracked,
        live_branch_ready,
        live_branch_db_path,
        live_branch_db_exists,
        nearest_tracked_ancestor,
        nearest_tracked_ancestor_db_path,
        nearest_tracked_ancestor_db_exists,
        tracked_branch_count: branches.len(),
        branches,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::project_memory_owner_from_layout_id;

    #[test]
    fn project_memory_owner_requires_a_valid_authoritative_layout_id() {
        assert!(project_memory_owner_from_layout_id(None).is_err());
        assert!(project_memory_owner_from_layout_id(Some("")).is_err());
    }
}

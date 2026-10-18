//! Transport-neutral store authorities shared by the composition root.
//!
//! Branch diagnostics, fact-owner identity, and store-metadata counters live
//! here so `TraceDecay` can retain owner handles and delegate.

use std::path::Path;

use serde::Serialize;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_runtime_core::branch;
use tracedecay_runtime_core::branch_meta;

mod store_meta;

pub use store_meta::{
    add_local_counter, get_local_counter, get_tokens_saved, reset_local_counter, set_tokens_saved,
};

#[derive(Debug, Clone, Serialize)]
pub struct TrackedBranchDiagnostic {
    pub name: String,
    pub parent: Option<String>,
    pub created_at: String,
    pub last_synced_at: String,
    pub is_default: bool,
    pub is_current: bool,
    pub is_open_active: bool,
    pub is_serving: bool,
    pub is_ready: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchDiagnostics {
    pub tracking_enabled: bool,
    pub default_branch: Option<String>,
    pub current_branch: Option<String>,
    pub open_active_branch: Option<String>,
    pub serving_branch: Option<String>,
    pub branch_drifted: bool,
    pub branch_resolution: String,
    pub is_fallback: bool,
    pub fallback_target: Option<String>,
    pub fallback_warning: Option<String>,
    pub live_branch_tracked: bool,
    pub live_branch_ready: bool,
    pub nearest_tracked_ancestor: Option<String>,
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
/// Returns `(serving_branch, fallback_warning)`. Every branch is a provenance
/// scope of the one project graph store; the branch argument only decides
/// which tracked branch's provenance the open is scoped to and whether the
/// caller must be warned about a fallback.
pub fn resolve_serving_branch(
    project_root: &Path,
    tracedecay_dir: &Path,
    branch_name: Option<&str>,
) -> (Option<String>, Option<String>) {
    let Some(meta) = branch_meta::load_branch_meta(tracedecay_dir) else {
        return (None, None);
    };

    let Some(branch_name) = branch_name else {
        return (
            Some(meta.default_branch.clone()),
            Some("detached HEAD, using default branch index".to_string()),
        );
    };

    if meta.is_query_eligible(branch_name) {
        return (Some(branch_name.to_string()), None);
    }

    let serving = branch::find_nearest_tracked_ancestor(project_root, branch_name, &meta)
        .unwrap_or_else(|| meta.default_branch.clone());
    let warning = format!(
        "branch '{branch_name}' is not tracked, serving from '{serving}'. \
         Run `tracedecay branch add {branch_name}` to track it."
    );
    (Some(serving), Some(warning))
}

/// The git source the served graph was built from, as the graph reports it.
#[derive(Clone, Copy, Debug)]
pub struct ServingGraphSource<'a> {
    pub reference: &'a str,
    pub revision: Option<&'a str>,
    /// Whether that source still matches the checkout the graph is serving.
    pub is_current: bool,
}

/// The tracked branch the served graph was built for: the one whose published
/// source matches it, else the live branch when the served source is current.
fn observed_serving_branch(
    meta: Option<&branch_meta::BranchMeta>,
    current_branch: Option<&str>,
    serving_source: Option<ServingGraphSource<'_>>,
) -> Option<String> {
    let source = serving_source?;
    let published = source.revision.and_then(|revision| {
        let meta = meta?;
        meta.branches.iter().find_map(|(name, entry)| {
            entry
                .graph_source
                .as_ref()
                .filter(|graph_source| {
                    graph_source.reference == source.reference
                        && graph_source.source_oid == revision
                        && meta.is_query_eligible(name)
                })
                .map(|_| name.clone())
        })
    });
    published.or_else(|| {
        source
            .is_current
            .then(|| source.reference.strip_prefix("refs/heads/"))
            .flatten()
            .filter(|name| current_branch == Some(*name))
            .map(str::to_owned)
    })
}

/// The `(open_active_branch, serving_branch)` pair [`build_branch_diagnostics`]
/// reports, without its drift and tracked-ancestor probes. Compact
/// status answers from this on every poll.
pub fn serving_branch_identity(
    project_root: &Path,
    data_root: &Path,
    open_active_branch: Option<String>,
    serving_branch: Option<String>,
    serving_source: Option<ServingGraphSource<'_>>,
) -> (Option<String>, Option<String>) {
    let meta = branch_meta::load_branch_meta(data_root);
    let current_branch = branch::current_branch(project_root);
    match observed_serving_branch(meta.as_ref(), current_branch.as_deref(), serving_source) {
        Some(branch) => (Some(branch.clone()), Some(branch)),
        None => (open_active_branch, serving_branch),
    }
}

pub fn build_branch_diagnostics(
    project_root: &Path,
    data_root: &Path,
    open_active_branch: Option<String>,
    serving_branch: Option<String>,
    fallback_warning: Option<String>,
    serving_source: Option<ServingGraphSource<'_>>,
) -> BranchDiagnostics {
    let meta = branch_meta::load_branch_meta(data_root);
    let current_branch = branch::current_branch(project_root);
    let observed_serving_branch =
        observed_serving_branch(meta.as_ref(), current_branch.as_deref(), serving_source);
    let observed_current_branch_is_ready = observed_serving_branch == current_branch;
    let (open_active_branch, serving_branch, fallback_warning) =
        if let Some(branch) = observed_serving_branch {
            (Some(branch.clone()), Some(branch), None)
        } else {
            (open_active_branch, serving_branch, fallback_warning)
        };
    let tracking_enabled = meta.as_ref().is_some_and(|m| !m.branches.is_empty());
    let branch_drifted =
        tracking_enabled && current_branch.as_deref() != open_active_branch.as_deref();
    let is_fallback = fallback_warning.is_some();
    let fallback_target = if is_fallback {
        serving_branch.clone()
    } else {
        None
    };

    let (live_branch_tracked, live_branch_ready, nearest_tracked_ancestor) =
        if let (Some(meta), Some(current)) = (meta.as_ref(), current_branch.as_deref()) {
            let live_branch_tracked = meta.is_tracked(current);
            let live_branch_ready =
                meta.is_query_eligible(current) || observed_current_branch_is_ready;
            let nearest_tracked_ancestor = if live_branch_ready {
                None
            } else {
                branch::find_nearest_tracked_ancestor(project_root, current, meta)
            };
            (
                live_branch_tracked,
                live_branch_ready,
                nearest_tracked_ancestor,
            )
        } else {
            (false, false, None)
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
    if live_branch_tracked && !live_branch_ready {
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
                "branch '{current}' is not tracked and has no tracked ancestor; tracedecay is serving '{target}' instead."
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
            branches.push(TrackedBranchDiagnostic {
                name: name.clone(),
                parent: entry.parent.clone(),
                created_at: entry.created_at.clone(),
                last_synced_at: entry.last_synced_at.clone(),
                is_default: name == meta.default_branch,
                is_current: current_branch.as_deref() == Some(name.as_str()),
                is_open_active: open_active_branch.as_deref() == Some(name.as_str()),
                is_serving: serving_branch.as_deref() == Some(name.as_str()),
                is_ready: meta.is_query_eligible(&name),
            });
        }
    }

    BranchDiagnostics {
        tracking_enabled,
        default_branch: meta.as_ref().map(|m| m.default_branch.clone()),
        current_branch,
        open_active_branch,
        serving_branch,
        branch_drifted,
        branch_resolution,
        is_fallback,
        fallback_target,
        fallback_warning,
        live_branch_tracked,
        live_branch_ready,
        nearest_tracked_ancestor,
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
        let Err(missing) = project_memory_owner_from_layout_id(None) else {
            panic!("a missing layout id must not name a project memory owner");
        };
        assert_eq!(
            missing.to_string(),
            "config error: active project has no authoritative project_id for memory"
        );
        let Err(empty) = project_memory_owner_from_layout_id(Some("")) else {
            panic!("an empty layout id must not name a project memory owner");
        };
        assert_eq!(
            empty.to_string(),
            "config error: invalid authoritative project_id for memory: ProjectId must not be empty"
        );
    }
}

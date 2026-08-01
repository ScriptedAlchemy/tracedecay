//! Branch-state accessors and branch-tracking diagnostics for the open
//! store.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::branch;
use crate::branch_meta;
use crate::db::{Database, DatabaseAccessMode};
use crate::errors::{Result, TraceDecayError};
use crate::storage::StoreLayout;

use super::{TraceDecay, TraceDecayOpenOptions};

/// Branch diagnostics are part of the downward graph-runtime port contract, so
/// `tracedecay-usecases` owns the shape and the root engine produces exactly
/// that type rather than a structurally identical twin.
pub use tracedecay_usecases::tracedecay::{BranchDiagnostics, TrackedBranchDiagnostic};

impl TraceDecay {
    pub(crate) fn dashboard_database_guard(&self) -> std::sync::Arc<Database> {
        std::sync::Arc::new(self.db.clone())
    }

    /// Filesystem path of the project's tracedecay directory, for display in
    /// dashboard payloads (mirrors the `path` field of the Hermes plugin API).
    pub(crate) fn dashboard_db_path(&self) -> std::path::PathBuf {
        self.db_path()
    }

    pub(super) fn ensure_branch_writable(&self, operation: &str) -> Result<()> {
        if self.read_only {
            return Err(TraceDecayError::Config {
                message: format!("cannot {operation}: active TraceDecay store is open read-only"),
            });
        }

        if self.is_fallback() {
            let active = self.active_branch.as_deref().unwrap_or("detached HEAD");
            let serving = self.serving_branch.as_deref().unwrap_or("default branch");
            let hint = self.active_branch.as_deref().map_or_else(
                || " Check out a tracked branch before writing.".to_string(),
                |branch| format!(" Run `tracedecay branch add {branch}` before writing."),
            );

            return Err(TraceDecayError::Config {
                message: format!(
                    "cannot {operation}: active branch '{active}' is served from fallback branch \
                     '{serving}'.{hint}"
                ),
            });
        }

        // Branch-drift guard. A long-running MCP server resolves its branch DB
        // once at open time and caches `serving_branch`. If the working tree
        // switched branches since then, this instance still holds the *old*
        // branch's DB — a write would persist the new branch's files into the
        // wrong DB. Re-read the live branch and refuse when it no longer matches
        // the branch we serve. Single-DB mode (no branch metadata) leaves
        // `serving_branch == None` and is exempt: there is only one DB (#2).
        if let Some(serving) = self.serving_branch.as_deref() {
            let live = branch::current_branch(&self.project_root);
            if live.as_deref() != Some(serving) {
                let live_name = live.as_deref().unwrap_or("detached HEAD");
                return Err(TraceDecayError::Config {
                    message: format!(
                        "cannot {operation}: index is open for branch '{serving}' but the working \
                         tree is now on '{live_name}'. Reopen tracedecay so it serves '{live_name}' \
                         (e.g. restart the MCP server)."
                    ),
                });
            }
        }

        Ok(())
    }

    /// Returns `true` when the live git branch differs from the branch this
    /// instance resolved at open time (and branch tracking is active).
    ///
    /// A long-running MCP server resolves its branch DB once at open time; a
    /// mid-session `git checkout` makes the cached DB stale. Callers detect
    /// that here and reopen the correct branch DB via
    /// [`reopen_for_current_branch`](Self::reopen_for_current_branch) before
    /// serving reads or writes. The comparison is against the open-time branch
    /// (`active_branch`), so reopening clears the drift even when the new
    /// branch is untracked and legitimately falls back to an ancestor DB —
    /// avoiding a reopen loop. Returns `false` in single-DB mode (no branch
    /// metadata), where every branch maps to the same DB.
    pub fn branch_drifted(&self) -> bool {
        if self.serving_branch.is_none() {
            return false;
        }
        branch::current_branch(&self.project_root).as_deref() != self.active_branch.as_deref()
    }

    /// Reopens this project for the live git branch, returning a fresh instance
    /// bound to the correct branch DB. Use after [`branch_drifted`](Self::branch_drifted)
    /// reports drift so subsequent reads and writes target the right DB.
    pub async fn reopen_for_current_branch(&self) -> Result<Self> {
        Self::open_with_registered_configuration(
            &self.project_root,
            self.open_options.clone(),
            self.store_layout.clone(),
            self.configuration_runtime.registered_database(),
            Arc::clone(&self.profile_database),
            Arc::clone(&self.store_runtime_registry),
        )
        .await
    }

    /// On-disk path to the `SQLite` DB this instance is serving. Useful for
    /// diagnostics (e.g. WAL/SHM size sampling) — returns the same path that
    /// `Database::open` was called with.
    ///
    /// The inputs (`project_root`, `store_layout.data_root`, and
    /// `serving_branch`) are immutable for the lifetime of a `TraceDecay`
    /// instance — branch changes are served by a freshly constructed
    /// instance rather than mutating an existing one — so the resolved path
    /// is memoized in `db_path_cache` after the first call instead of
    /// re-reading and re-parsing branch metadata from disk on every call.
    pub fn db_path(&self) -> PathBuf {
        self.db_path_cache
            .get_or_init(|| {
                let (path, _, _) = Self::resolve_db_for_branch(
                    &self.project_root,
                    &self.store_layout.data_root,
                    self.serving_branch.as_deref(),
                );
                path
            })
            .clone()
    }

    pub fn store_layout(&self) -> &StoreLayout {
        &self.store_layout
    }

    pub(crate) fn open_options(&self) -> TraceDecayOpenOptions {
        self.open_options.clone()
    }

    #[cfg(unix)]
    pub(crate) fn retained_profile_root(&self) -> Result<PathBuf> {
        self.open_options.resolved_profile_root()
    }

    #[cfg(unix)]
    pub(crate) fn retained_store_runtime_registry(
        &self,
    ) -> std::sync::Arc<
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
    > {
        std::sync::Arc::clone(&self.store_runtime_registry)
    }

    async fn project_memory_mount(&self) -> Result<(tracedecay_domain::ProjectId, Vec<PathBuf>)> {
        let project_id = Self::registered_project_id(&self.store_layout)?;
        let enrollment_roots = Self::registered_enrollment_roots(
            &self.project_root,
            &self.store_layout,
            &project_id,
            self.profile_database.as_ref(),
        )
        .await?;
        Ok((project_id, enrollment_roots))
    }

    pub async fn open_project_store_db(&self) -> Result<Database> {
        if self.read_only {
            return Err(TraceDecayError::Config {
                message: "cannot open project store for writing: active TraceDecay store is open read-only"
                    .to_string(),
            });
        }
        if self.db_path() == self.store_layout.graph_db_path {
            return Ok(self.db.clone());
        }
        let (project_id, enrollment_roots) = self.project_memory_mount().await?;
        self.store_runtime_registry
            .project_memory(project_id, enrollment_roots)
            .await
            .map(|database| database.as_ref().clone())
    }

    pub async fn open_project_store_db_read_only(&self) -> Result<Database> {
        if self.db_path() == self.store_layout.graph_db_path {
            return Database::publish_runtime(
                self.db.retained_runtime().clone(),
                DatabaseAccessMode::ReadOnly,
            )
            .await;
        }
        let (project_id, enrollment_roots) = self.project_memory_mount().await?;
        self.store_runtime_registry
            .project_memory_read_only(project_id, enrollment_roots)
            .await
    }

    fn build_branch_diagnostics(
        project_root: &Path,
        data_root: &Path,
        open_active_branch: Option<String>,
        serving_branch: Option<String>,
        fallback_warning: Option<String>,
        serving_db_path: PathBuf,
    ) -> BranchDiagnostics {
        let meta = branch_meta::load_branch_meta(data_root);
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
            live_branch_db_path,
            live_branch_db_exists,
            nearest_tracked_ancestor,
            nearest_tracked_ancestor_db_path,
            nearest_tracked_ancestor_db_exists,
        ) = if let (Some(meta), Some(current)) = (meta.as_ref(), current_branch.as_deref()) {
            let live_branch_tracked = meta.is_tracked(current);
            let live_branch_db_path = if live_branch_tracked {
                branch::resolve_branch_db_path(data_root, current, meta)
            } else {
                None
            };
            let live_branch_db_exists = live_branch_db_path.as_ref().map(|path| path.exists());
            let nearest_tracked_ancestor = if live_branch_tracked {
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
                live_branch_db_path,
                live_branch_db_exists,
                nearest_tracked_ancestor,
                nearest_tracked_ancestor_db_path,
                nearest_tracked_ancestor_db_exists,
            )
        } else {
            (false, None, None, None, None, None)
        };

        let mut warnings = Vec::new();
        if branch_drifted {
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
        } else if branch_drifted {
            "stale_serving_branch".to_string()
        } else if current_branch.is_none() {
            "detached_default".to_string()
        } else if is_fallback {
            match (
                nearest_tracked_ancestor.as_deref(),
                fallback_target.as_deref(),
            ) {
                (Some(ancestor), Some(target)) if ancestor == target => {
                    "fallback_ancestor".to_string()
                }
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

    pub fn branch_diagnostics(&self) -> BranchDiagnostics {
        Self::build_branch_diagnostics(
            &self.project_root,
            &self.store_layout.data_root,
            self.active_branch.clone(),
            self.serving_branch.clone(),
            self.fallback_warning.clone(),
            self.db_path(),
        )
    }

    /// Returns the active git branch, if any.
    pub fn active_branch(&self) -> Option<&str> {
        self.active_branch.as_deref()
    }

    /// Returns the branch whose DB is actually being served.
    pub fn serving_branch(&self) -> Option<&str> {
        self.serving_branch.as_deref()
    }

    /// Returns a fallback warning if serving from an ancestor branch DB.
    pub fn fallback_warning(&self) -> Option<&str> {
        self.fallback_warning.as_deref()
    }

    /// Returns true if serving from a fallback (ancestor) DB.
    pub fn is_fallback(&self) -> bool {
        self.fallback_warning.is_some()
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
}

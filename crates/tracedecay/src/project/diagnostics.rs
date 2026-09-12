//! Branch-state accessors and branch-tracking diagnostics for the open
//! store.

use std::path::PathBuf;

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::branch;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::storage::StoreLayout;

use super::TraceDecay;

/// Branch diagnostics are part of the downward graph-runtime port contract, so
/// `tracedecay-application` owns the shape and the root engine produces exactly
/// that type rather than a structurally identical twin.
pub use tracedecay_application::tracedecay::{BranchDiagnostics, TrackedBranchDiagnostic};

impl TraceDecay {
    pub(crate) fn dashboard_database_guard(&self) -> std::sync::Arc<Database> {
        std::sync::Arc::new(self.db.clone())
    }

    /// Filesystem path of the project's tracedecay directory, for display in
    /// dashboard payloads (mirrors the `path` field of the Hermes plugin API).
    pub(crate) fn dashboard_db_path(&self) -> std::path::PathBuf {
        self.db_path()
    }

    /// A fresh branch memo rooted at this instance's project root.
    ///
    /// Create one at a request or write-gate entry and thread it through every
    /// drift check and gate that request performs, so a single `gix` HEAD read
    /// (or, for a linked worktree, a single `git` spawn) serves all of them.
    /// Never store it: a checkout must be visible to the next request.
    #[must_use]
    pub fn branch_memo(&self) -> branch::BranchMemo {
        branch::BranchMemo::new(&self.project_root)
    }

    /// Returns `true` when the live git branch differs from the branch this
    /// instance resolved at open time (and branch tracking is active).
    ///
    /// A long-running MCP server resolves its branch provenance once at open
    /// time; a mid-session `git checkout` leaves it scoped to the previous
    /// branch's publication epoch. Callers detect that here and reopen onto
    /// the live branch via
    /// [`reopen_for_current_branch`](Self::reopen_for_current_branch) before
    /// serving reads or writes. The comparison is against the open-time branch
    /// (`active_branch`), so reopening clears the drift even when the new
    /// branch is untracked and legitimately falls back to an ancestor's
    /// provenance — avoiding a reopen loop. Returns `false` when the store
    /// publishes no branch metadata at all, where there is no branch identity
    /// to drift from.
    pub fn branch_drifted(&self) -> bool {
        self.branch_drifted_with(&self.branch_memo())
    }

    /// [`branch_drifted`](Self::branch_drifted) against a branch resolution
    /// this request already made.
    ///
    /// `serving_branch` is `Some` exactly when the store published branch
    /// metadata, so an untracked store still short-circuits without resolving
    /// anything.
    pub fn branch_drifted_with(&self, live_branch: &branch::BranchMemo) -> bool {
        if self.serving_branch.is_none() {
            return false;
        }
        live_branch.resolve_for(&self.project_root).as_deref() != self.active_branch.as_deref()
    }

    /// Reopens this project for the live git branch, returning a fresh instance
    /// bound to the correct branch DB. Use after [`branch_drifted`](Self::branch_drifted)
    /// reports drift so subsequent reads and writes target the right DB.
    #[hotpath::skip]
    pub async fn reopen_for_current_branch(&self) -> Result<Self> {
        Self::open_with_registered_configuration(
            &self.project_root,
            self.open_options.clone(),
            self.store_layout.clone(),
            self.configuration_runtime.registered_database(),
            self.profile_database.clone(),
            self.store_runtime_registry.clone(),
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

    pub(crate) fn retained_project_store_db(&self) -> Result<Database> {
        if !tracedecay_runtime_core::path_safety::same_canonical_path(
            self.db.canonical_database_path(),
            &self.store_layout.graph_db_path,
        ) {
            return Err(TraceDecayError::Config {
                message: format!(
                    "mounted project database '{}' differs from canonical StoreLayout locator '{}'",
                    self.db.canonical_database_path().display(),
                    self.store_layout.graph_db_path.display()
                ),
            });
        }
        Ok(self.db.clone())
    }

    #[hotpath::skip]
    pub fn open_project_store_db(&self) -> Result<Database> {
        if self.read_only {
            return Err(TraceDecayError::Config {
                message: "cannot open project store for writing: active TraceDecay store is open read-only"
                    .to_string(),
            });
        }
        self.retained_project_store_db()
    }

    #[hotpath::skip]
    pub fn open_project_store_db_read_only(&self) -> Result<Database> {
        let database = self.retained_project_store_db()?;
        if database.is_writable() {
            return Err(TraceDecayError::Config {
                message:
                    "cannot issue a read-only project store client from a writable database lease"
                        .to_string(),
            });
        }
        Ok(database)
    }

    pub fn branch_diagnostics(&self) -> BranchDiagnostics {
        tracedecay_application::tracedecay::build_branch_diagnostics(
            &self.project_root,
            &self.store_layout.data_root,
            self.active_branch.clone(),
            self.serving_branch.clone(),
            self.fallback_warning.clone(),
            self.db_path(),
            None,
            false,
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

//! Project-store and Git snapshot diagnostics.

use std::path::{Path, PathBuf};

use crate::branch;
use crate::db::{Database, DatabaseAccessMode};
use crate::errors::{Result, TraceDecayError};
use crate::storage::StoreLayout;

use super::{TraceDecay, TraceDecayOpenOptions};

pub use tracedecay_usecases::tracedecay::{BranchDiagnostics, BranchSnapshotDiagnostic};

impl TraceDecay {
    pub(crate) fn dashboard_database_guard(&self) -> std::sync::Arc<Database> {
        std::sync::Arc::new(self.db.clone())
    }

    pub(crate) fn dashboard_db_path(&self) -> PathBuf {
        self.db_path()
    }

    #[must_use]
    pub fn branch_memo(&self) -> branch::BranchMemo {
        branch::BranchMemo::new(&self.project_root)
    }

    pub(super) fn ensure_writable(&self, operation: &str) -> Result<()> {
        if self.read_only {
            return Err(TraceDecayError::Config {
                message: format!("cannot {operation}: active TraceDecay store is open read-only"),
            });
        }
        Ok(())
    }

    pub fn db_path(&self) -> PathBuf {
        self.store_layout.graph_db_path.clone()
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

    pub async fn open_project_store_db(&self) -> Result<Database> {
        if self.read_only {
            return Err(TraceDecayError::Config {
                message: "cannot open project store for writing: active TraceDecay store is open read-only"
                    .to_string(),
            });
        }
        Ok(self.db.clone())
    }

    pub async fn open_project_store_db_read_only(&self) -> Result<Database> {
        Database::publish_runtime(
            self.db.retained_runtime().clone(),
            DatabaseAccessMode::ReadOnly,
        )
        .await
    }

    fn branch_snapshots(project_root: &Path) -> (Vec<BranchSnapshotDiagnostic>, Vec<String>) {
        let current = branch::current_branch(project_root);
        let branch_snapshots = match branch::local_branch_snapshots(project_root) {
            Ok(snapshots) => snapshots,
            Err(error) => {
                return (
                    Vec::new(),
                    vec![format!("git branch snapshot listing unavailable: {error}")],
                );
            }
        };
        (
            branch_snapshots
                .into_iter()
                .map(|snapshot| BranchSnapshotDiagnostic {
                    is_current: current.as_deref() == Some(snapshot.name.as_str()),
                    name: snapshot.name,
                    commit: snapshot.commit,
                })
                .collect(),
            Vec::new(),
        )
    }

    pub fn branch_diagnostics(&self) -> BranchDiagnostics {
        let current_branch = branch::current_branch(&self.project_root);
        let (snapshots, warnings) = Self::branch_snapshots(&self.project_root);
        BranchDiagnostics {
            current_branch,
            selected_branch: self.active_branch.clone(),
            project_store_path: self.store_layout.graph_db_path.clone(),
            project_store_exists: self.store_layout.graph_db_path.exists(),
            snapshot_count: snapshots.len(),
            snapshots,
            warnings,
        }
    }

    /// The branch snapshot selected for this graph view.
    pub fn active_branch(&self) -> Option<&str> {
        self.active_branch.as_deref()
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
}

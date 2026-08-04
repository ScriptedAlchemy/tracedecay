//! Project-store accessors and write guards.

use std::path::PathBuf;

use crate::db::{Database, DatabaseAccessMode};
use crate::errors::{Result, TraceDecayError};
use crate::storage::StoreLayout;

use super::{TraceDecay, TraceDecayOpenOptions};

impl TraceDecay {
    pub(crate) fn dashboard_database_guard(&self) -> std::sync::Arc<Database> {
        std::sync::Arc::new(self.db.clone())
    }

    /// Filesystem path of the one project-wide graph store.
    pub(crate) fn dashboard_db_path(&self) -> PathBuf {
        self.db_path()
    }

    /// Rejects writes through a read-only snapshot handle. Branch/ref changes
    /// do not select a different database; exact snapshot identity belongs to
    /// immutable generation provenance.
    pub(super) fn ensure_branch_writable(&self, operation: &str) -> Result<()> {
        if self.read_only {
            return Err(TraceDecayError::Config {
                message: format!("cannot {operation}: active TraceDecay store is open read-only"),
            });
        }
        Ok(())
    }

    /// On-disk path of the authoritative project graph store.
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
                message:
                    "cannot open project store for writing: active TraceDecay store is open read-only"
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

    /// Native Git branch observed when this checkout handle was opened.
    /// This is provenance only and never selects a physical database.
    pub fn active_branch(&self) -> Option<&str> {
        self.active_branch.as_deref()
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
}

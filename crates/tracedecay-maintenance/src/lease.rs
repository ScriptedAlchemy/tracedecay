//! Admitted project-store lease for maintenance kernels.
//!
//! Callers extract these fields from a mounted project store. Kernels never
//! name the composition-root aggregate.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_configuration::ProjectConfigurationRuntime;
use tracedecay_domain::ProjectId;
use tracedecay_domain::errors::Result;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::storage::{self, StoreLayout};
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

/// Registered store lease for one mounted project's maintenance journey.
#[derive(Clone)]
pub struct ProjectStoreMaintenanceLeaseV1 {
    project_root: PathBuf,
    store_layout: StoreLayout,
    graph_db: Database,
    store_runtime: Arc<DaemonSessionRuntimeRegistryV1>,
    configuration_runtime: Arc<ProjectConfigurationRuntime>,
    profile_database: RegisteredGlobalDbLeaseV1,
}

impl ProjectStoreMaintenanceLeaseV1 {
    #[must_use]
    pub fn new(
        project_root: PathBuf,
        store_layout: StoreLayout,
        graph_db: Database,
        store_runtime: Arc<DaemonSessionRuntimeRegistryV1>,
        configuration_runtime: Arc<ProjectConfigurationRuntime>,
        profile_database: RegisteredGlobalDbLeaseV1,
    ) -> Self {
        Self {
            project_root,
            store_layout,
            graph_db,
            store_runtime,
            configuration_runtime,
            profile_database,
        }
    }

    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    #[must_use]
    pub fn store_layout(&self) -> &StoreLayout {
        &self.store_layout
    }

    #[must_use]
    pub fn graph_db(&self) -> &Database {
        &self.graph_db
    }

    #[must_use]
    pub fn store_runtime(&self) -> &Arc<DaemonSessionRuntimeRegistryV1> {
        &self.store_runtime
    }

    #[must_use]
    pub fn configuration_runtime(&self) -> &Arc<ProjectConfigurationRuntime> {
        &self.configuration_runtime
    }

    #[must_use]
    pub fn profile_database(&self) -> &RegisteredGlobalDbLeaseV1 {
        &self.profile_database
    }
}

/// Filter candidate roots to those whose on-disk identity names `project_id`.
pub fn enrolled_project_roots(
    candidates: impl IntoIterator<Item = PathBuf>,
    project_id: &ProjectId,
) -> Result<Vec<PathBuf>> {
    let mut candidates = candidates.into_iter().collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();

    let mut roots = Vec::new();
    for candidate in candidates {
        let candidate = tracedecay_runtime_core::worktree::repository_identity_root(&candidate)
            .unwrap_or(candidate);
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if roots.contains(&canonical) {
            continue;
        }
        let named_id = match storage::read_repository_identity_marker(&canonical)? {
            Some(marker) => marker.project_id,
            None => storage::default_profile_project_id(&canonical),
        };
        if named_id == project_id.as_str() {
            roots.push(canonical);
        }
    }
    Ok(roots)
}

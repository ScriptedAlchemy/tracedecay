//! Admitted project-store lease for maintenance kernels.
//!
//! Callers extract these fields from a mounted project store. Kernels never
//! name the composition-root aggregate.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_configuration::ProjectConfigurationRuntime;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::storage::StoreLayout;
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

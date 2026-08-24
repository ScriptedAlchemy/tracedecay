//! Dashboard-facing graph and memory runtime seams.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_agent_hosts::ports::project_runtime::ProjectRuntime;
pub use tracedecay_code_index::is_test_file;
use tracedecay_runtime_core::db::{Database, DatabaseStorageTelemetryHandle};
use tracedecay_runtime_core::errors::Result;
use tracedecay_runtime_core::storage::StoreLayout;
use tracedecay_usecases::configuration::UserSettingsDaemonClient;

use crate::config::RetentionConfig;

pub trait DashboardProjectRuntime: Send + Sync {
    fn project_root(&self) -> &Path;
    fn store_layout(&self) -> &StoreLayout;
    fn automation_runtime(&self) -> &(dyn ProjectRuntime + 'static);
    fn dashboard_db_path(&self) -> PathBuf;
    fn dashboard_database_guard(&self) -> Arc<Database>;
    fn storage_telemetry_handle(&self) -> Result<DatabaseStorageTelemetryHandle>;
    fn retention_config(&self) -> RetentionConfig;
    fn user_settings_client(&self) -> Arc<dyn UserSettingsDaemonClient>;
}

pub type TraceDecay = dyn DashboardProjectRuntime;

pub mod facts {
    // The shared resolvers live in `tracedecay_usecases::memory` — the crate
    // that owns `MemoryApplication`/`MemoryApplicationError` — rather than a
    // copy kept in sync by hand here. `tracedecay::facts::memory_application_for_db`
    // remains the stable call-site path for this crate's ~20 dashboard routes.
    pub use tracedecay_usecases::memory::memory_application_for_db;
}

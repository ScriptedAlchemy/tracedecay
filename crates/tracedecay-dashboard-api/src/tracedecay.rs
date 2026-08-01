//! Dashboard-facing graph and memory runtime seams.

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_agent_hosts::ports::project_runtime::ProjectRuntime;
pub use tracedecay_code_index::is_test_file;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::errors::Result;
use tracedecay_usecases::configuration::UserSettingsDaemonClient;
pub use tracedecay_usecases::tracedecay::GraphRuntimePort;

use crate::config::RetentionConfig;

pub trait DashboardProjectRuntime: GraphRuntimePort {
    fn automation_runtime(&self) -> &(dyn ProjectRuntime + 'static);
    fn dashboard_db_path(&self) -> PathBuf;
    fn dashboard_database_guard(&self) -> Arc<Database>;
    fn storage_telemetry_handle(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::migration_sql::MigrationSqlHandle>;
    fn retention_config(&self) -> RetentionConfig;
    fn user_settings_client(&self) -> Arc<dyn UserSettingsDaemonClient>;
}

pub type TraceDecay = dyn DashboardProjectRuntime;

pub mod facts {
    use tracedecay_domain::FactOwnerV1;
    use tracedecay_runtime_core::db::Database;
    use tracedecay_runtime_core::errors::{Result, TraceDecayError};
    use tracedecay_runtime_core::store::memory::DatabaseFactStore;
    use tracedecay_usecases::memory::{MemoryApplication, MemoryApplicationError};

    fn memory_application_error(error: MemoryApplicationError) -> TraceDecayError {
        TraceDecayError::database_operation("memory application", error)
    }

    pub fn memory_application_for_db(
        owner: FactOwnerV1,
        db: &Database,
    ) -> Result<MemoryApplication<DatabaseFactStore<'_>>> {
        MemoryApplication::new(owner, DatabaseFactStore::new(db)).map_err(memory_application_error)
    }
}

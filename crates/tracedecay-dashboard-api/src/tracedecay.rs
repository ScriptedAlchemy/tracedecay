//! Dashboard-facing graph and memory runtime seams.

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_automation_runtime::automation::host_io::HostIo;
pub use tracedecay_code_index::is_test_file;
use tracedecay_configuration::UserSettingsDaemonClient;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::storage::StoreLayout;

use tracedecay_configuration::RetentionConfig;

/// Immutable project values captured by the composition root for dashboard
/// state construction.
#[derive(Clone)]
pub struct DashboardProjectContext {
    pub store_layout: StoreLayout,
    pub dashboard_db_path: PathBuf,
    pub dashboard_database: Arc<Database>,
    pub retention_config: RetentionConfig,
    pub host_io: HostIo,
    pub user_settings_client: Arc<dyn UserSettingsDaemonClient>,
}

pub mod facts {
    // The shared resolvers live in `tracedecay_session_memory::memory` — the crate
    // that owns `MemoryApplication`/`MemoryApplicationError` — rather than a
    // copy kept in sync by hand here. `tracedecay::facts::memory_application_for_db`
    // remains the stable call-site path for this crate's ~20 dashboard routes.
    pub use tracedecay_session_memory::memory::memory_application_for_db;
}

//! Dashboard-facing graph and memory runtime seams.

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_automation_runtime::automation::host_io::HostIo;
use tracedecay_configuration::UserSettingsDaemonClient;
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::storage::StoreLayout;

use tracedecay_configuration::RetentionConfig;

/// Immutable project values captured by the composition root for dashboard
/// state construction.
#[derive(Clone)]
pub struct DashboardProjectContext {
    /// The profile that owns the dashboard's daemon: its data directory,
    /// home, and global database.
    pub profile: ProfileRoot,
    pub store_layout: StoreLayout,
    pub dashboard_db_path: PathBuf,
    pub dashboard_database: Arc<Database>,
    pub retention_config: RetentionConfig,
    pub host_io: HostIo,
    pub user_settings_client: Arc<dyn UserSettingsDaemonClient>,
}

pub mod facts {
    // Resolvers live with `MemoryApplication`. This re-export is the stable
    // call-site path for this crate's dashboard routes.
    pub use tracedecay_session_memory::memory::memory_application_for_db;
}

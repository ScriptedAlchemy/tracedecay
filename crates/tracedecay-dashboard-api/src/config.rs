//! Dashboard configuration values and injected root-owned read authority.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use tracedecay_application::storage::{StorageByteSizeV1, StoreKeyV1, StoreSizeBudgetV1};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

pub use tracedecay_usecases::config::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionConfig {
    pub store_soft_budgets_bytes: BTreeMap<String, u64>,
}

impl RetentionConfig {
    pub fn store_soft_budget(&self, store: &str) -> Result<Option<StoreSizeBudgetV1>> {
        let Some(bytes) = self.store_soft_budgets_bytes.get(store).copied() else {
            return Ok(None);
        };
        let budget = StoreSizeBudgetV1 {
            store: StoreKeyV1::new(store.to_owned()).map_err(config_error)?,
            soft_limit_bytes: StorageByteSizeV1(bytes),
        };
        budget.validate().map_err(config_error)?;
        Ok(Some(budget))
    }
}

pub trait DashboardConfigurationReadPort: Send + Sync {
    fn cached_runtime_configuration(
        &self,
        project_root: &Path,
    ) -> Result<PinnedRuntimeConfiguration>;
    fn is_in_gitignore(&self, project_root: &Path) -> bool;
}

static CONFIGURATION_READ_PORT: OnceLock<Arc<dyn DashboardConfigurationReadPort>> = OnceLock::new();

pub fn install_dashboard_configuration_read_port(
    port: Arc<dyn DashboardConfigurationReadPort>,
) -> Result<()> {
    CONFIGURATION_READ_PORT
        .set(port)
        .map_err(|_| config_error("dashboard configuration read port is already installed"))
}

pub fn cached_runtime_configuration(project_root: &Path) -> Result<PinnedRuntimeConfiguration> {
    configuration_read_port()?.cached_runtime_configuration(project_root)
}

pub fn is_in_gitignore(project_root: &Path) -> bool {
    CONFIGURATION_READ_PORT
        .get()
        .is_some_and(|port| port.is_in_gitignore(project_root))
}

fn configuration_read_port() -> Result<&'static dyn DashboardConfigurationReadPort> {
    CONFIGURATION_READ_PORT
        .get()
        .map(Arc::as_ref)
        .ok_or_else(|| config_error("dashboard configuration read port is not installed"))
}

fn config_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: error.to_string(),
    }
}

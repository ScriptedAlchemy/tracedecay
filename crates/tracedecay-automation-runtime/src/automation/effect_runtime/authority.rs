//! Blocking journal cleanup after a durable terminal.

use std::path::Path;

use super::{contract_error, recovery_index};
use tracedecay_domain::errors::Result;

#[hotpath::measure(label = "daemon.automation.effect.housekeeping", future = true)]
pub async fn finalize_terminal_housekeeping(dashboard_root: &Path, journal_path: &Path) -> Result<()> {
    let dashboard_root = dashboard_root.to_path_buf();
    let journal_path = journal_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        recovery_index::remove_pending_blocking(&dashboard_root, &journal_path)
    })
    .await
    .map_err(|error| {
        contract_error(format!(
            "retained automation terminal housekeeping task failed: {error}"
        ))
    })?
}

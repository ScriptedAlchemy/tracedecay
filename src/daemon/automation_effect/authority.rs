//! Blocking journal cleanup and retirement finalization for the authority facade.

use std::path::Path;

use super::{contract_error, recovery_index, retirement};
use crate::errors::Result;

pub(super) async fn remove_pending_index(dashboard_root: &Path, journal_path: &Path) -> Result<()> {
    let dashboard_root = dashboard_root.to_path_buf();
    let journal_path = journal_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        recovery_index::remove_pending_blocking(&dashboard_root, &journal_path)
    })
    .await
    .map_err(|error| contract_error(format!("automation pending index cleanup failed: {error}")))?
}

pub(super) async fn finalize_retirement(
    dashboard_root: &Path,
    binding: retirement::RetirementBinding,
    live_plan: Option<retirement::RetirementPlan>,
) -> Result<()> {
    let dashboard_root = dashboard_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        retirement::finalize_after_terminal(&dashboard_root, &binding, live_plan.as_ref())
    })
    .await
    .map_err(|error| contract_error(format!("proposal retirement finalizer failed: {error}")))?
}

//! Compatibility façade for runtime branch topology.

pub mod admin;

pub use admin::{
    BranchAdminAction, BranchAdminOutcome, BranchAdminReport, PreparedBranchAdminMutation,
    prepare_branch_admin_mutation, remove_tracked_branch_store_checked,
};
pub(crate) use admin::{BranchAdminRecoveryDisposition, prepare_pending_branch_admin_recovery};
pub use tracedecay_runtime_core::branch::*;

pub(crate) fn try_acquire_branch_add_lock(
    tracedecay_dir: &std::path::Path,
) -> crate::errors::Result<std::fs::File> {
    let file = tracedecay_runtime_core::branch::try_acquire_branch_add_lock_raw(tracedecay_dir)?;
    admin::ensure_no_pending_branch_admin_recovery(tracedecay_dir)?;
    Ok(file)
}

/// Compatibility wrapper for the PR-autotrack lifecycle. Administrative CLI
/// removal uses [`prepare_branch_admin_mutation`] through the daemon so failures
/// are surfaced instead of collapsed to `false`.
pub fn remove_tracked_branch_store(tracedecay_dir: &std::path::Path, branch: &str) -> bool {
    remove_tracked_branch_store_checked(tracedecay_dir, branch)
        .is_ok_and(|report| report.outcome == BranchAdminOutcome::Removed)
}

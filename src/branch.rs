//! Compatibility façade for runtime branch topology.

pub mod admin;

pub use admin::{
    BranchAdminAction, BranchAdminOutcome, BranchAdminReport, PreparedBranchAdminMutation,
    prepare_branch_admin_mutation, remove_tracked_branch_store_checked,
};
pub(crate) use admin::{BranchAdminRecoveryDisposition, prepare_pending_branch_admin_recovery};
pub use tracedecay_runtime_core::branch::{
    BranchAddOutcome, BranchTrackingPreparation, GcReport, PreparedBranchTracking, current_branch,
    detect_default_branch, finalize_prepared_branch_tracking, find_nearest_tracked_ancestor,
    gc_dead_branch_stores, is_branch_ref_present, local_branch_exists, resolve_branch_db_path,
    rollback_prepared_branch_tracking, sanitize_branch_name,
};
pub(crate) use tracedecay_runtime_core::branch::{
    BRANCH_LOCK_RETRY_ATTEMPTS, BRANCH_LOCK_RETRY_INTERVAL, now_unix_secs, parse_unix_secs,
    try_acquire_branch_add_lock_raw,
};

pub(crate) fn try_acquire_branch_add_lock(
    tracedecay_dir: &std::path::Path,
) -> crate::errors::Result<std::fs::File> {
    let file = tracedecay_runtime_core::branch::try_acquire_branch_add_lock_raw(tracedecay_dir)?;
    admin::ensure_no_pending_branch_admin_recovery(tracedecay_dir)?;
    Ok(file)
}

pub(crate) fn acquire_branch_lock_blocking(
    tracedecay_dir: &std::path::Path,
) -> crate::errors::Result<std::fs::File> {
    let mut last_contention = None;
    for _ in 0..BRANCH_LOCK_RETRY_ATTEMPTS {
        match try_acquire_branch_add_lock(tracedecay_dir) {
            Ok(lock) => return Ok(lock),
            Err(error @ crate::errors::TraceDecayError::SyncLock { .. }) => {
                last_contention = Some(error);
                std::thread::sleep(BRANCH_LOCK_RETRY_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
    Err(
        last_contention.unwrap_or_else(|| crate::errors::TraceDecayError::SyncLock {
            message: format!(
                "timed out waiting for branch metadata lock at {}",
                tracedecay_dir.join(".branch-add.lock").display()
            ),
        }),
    )
}

pub async fn prepare_branch_tracking_in_layout(
    project_root: &std::path::Path,
    branch_name: &str,
    tracedecay_dir: &std::path::Path,
) -> crate::errors::Result<tracedecay_runtime_core::branch::BranchTrackingPreparation> {
    tracedecay_runtime_core::branch::prepare_branch_tracking_in_layout_with_lock(
        project_root,
        branch_name,
        tracedecay_dir,
        try_acquire_branch_add_lock,
    )
    .await
}

/// Compatibility wrapper for the PR-autotrack lifecycle. Administrative CLI
/// removal uses [`prepare_branch_admin_mutation`] through the daemon so failures
/// are surfaced instead of collapsed to `false`.
pub fn remove_tracked_branch_store(tracedecay_dir: &std::path::Path, branch: &str) -> bool {
    remove_tracked_branch_store_checked(tracedecay_dir, branch)
        .is_ok_and(|report| report.outcome == BranchAdminOutcome::Removed)
}

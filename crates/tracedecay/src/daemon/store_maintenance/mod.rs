//! Daemon sequencing for store-administration GC.
//!
//! Retention, compaction, and generation kernels live in
//! `tracedecay-maintenance`. This module keeps the pass that must hold
//! [`StoreAdministration`] for the writer-gated branch-admin action.

use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_runtime_core::branch::BranchAdminAction;

use super::branch_admin::StoreAdministration;
use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::logging::log_daemon_event;

#[cfg(test)]
mod vector_retention_tests;

/// Runs branch-store GC for a project through the daemon administration
/// coordinator, logging what it removed. Returns `false` when layout resolution
/// or administration fails so the maintenance owner keeps the GC cadence
/// eligible for a retry.
#[hotpath::measure(label = "daemon.git.maintenance.branch_gc", future = true)]
pub(super) async fn run_gc(
    administration: &StoreAdministration,
    schedulers: &CodeIndexSchedulerRegistryV1,
    branch_gc_days: u64,
    orphan_db_gc_days: u64,
    cg: &TraceDecay,
) -> bool {
    let root = cg.project_root();
    let data_root = &cg.store_layout().data_root;

    // The coordinator owns the writer gate and its process/store-holder
    // safety checks; GC never runs beside a content writer on the same store.
    let report = administration
        .execute_branch_admin_in_layout(
            schedulers,
            root,
            data_root,
            BranchAdminAction::Gc,
            branch_gc_days,
            orphan_db_gc_days,
        )
        .await;
    let report = match report {
        Ok(report) => report,
        Err(_) => {
            log_daemon_event(
                "retention_degraded",
                &[
                    ("pass", "branch_gc".to_string()),
                    ("project", root.display().to_string()),
                    ("failure", "branch_administration_failed".to_string()),
                ],
            );
            return false;
        }
    };

    if !report.removed_branches.is_empty() || !report.removed_orphan_dbs.is_empty() {
        log_daemon_event(
            "retention_branch_gc",
            &[
                ("project", root.display().to_string()),
                ("removed_tracked", report.removed_branches.len().to_string()),
                (
                    "removed_orphans",
                    report.removed_orphan_dbs.len().to_string(),
                ),
            ],
        );
    }
    true
}

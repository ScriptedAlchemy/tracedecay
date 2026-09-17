//! Ordered generation retention for one mounted project.

use crate::compaction_receipt::record_live_compaction_outcome;
use crate::lease::ProjectStoreMaintenanceLeaseV1;
use crate::store_maintenance::{
    CodeGenerationRetentionOutcomeV1, run_branch_compaction, run_code_generation_retention,
};
use crate::telemetry::StoreTelemetrySamplingRegistry;
use crate::tick::{MaintenanceContinuation, MaintenanceTickOutcome};
use tracedecay_contracts::storage::compaction::CompactionThresholdConfig;

/// Run the production generation-maintenance journey for one admitted store lease.
///
/// A fresh full tick runs bounded code-generation retention and then the
/// independent compaction passes. A code-generation continuation runs only
/// the bounded code-generation unit — draining a superseded backlog on the
/// short cadence without re-running compaction.
#[hotpath::measure(label = "daemon.maintenance.generation", future = true)]
pub async fn run_project_generation_maintenance(
    lease: &ProjectStoreMaintenanceLeaseV1,
    code_index_schedulers: &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    maintenance_observations: &StoreTelemetrySamplingRegistry,
    cancellation: &tracedecay_session_memory::context::CancellationToken,
    compaction: Option<&CompactionThresholdConfig>,
    continuation: Option<MaintenanceContinuation>,
) -> MaintenanceTickOutcome {
    // Each ordered phase gets its own wall span: the outer generation span is
    // inclusive, so a slow tick is attributed to code generation retention or
    // compaction — not guessed.
    let code_generation = if cancellation.is_cancelled() {
        CodeGenerationRetentionOutcomeV1::Failed
    } else {
        hotpath::measure_block!(
            "daemon.maintenance.code_generation_retention",
            run_code_generation_retention(
                lease,
                code_index_schedulers,
                maintenance_observations,
                cancellation,
            )
            .await
        )
    };
    let mut outcome = match code_generation {
        CodeGenerationRetentionOutcomeV1::Complete => MaintenanceTickOutcome::Complete,
        CodeGenerationRetentionOutcomeV1::MoreWork => {
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::CodeGenerationRetention)
        }
        CodeGenerationRetentionOutcomeV1::Failed => MaintenanceTickOutcome::Retry,
    };
    if continuation == Some(MaintenanceContinuation::CodeGenerationRetention) {
        return finalize_generation_outcome(outcome, cancellation);
    }
    if !cancellation.is_cancelled()
        && let Some(compaction) = compaction
    {
        hotpath::measure_block!("daemon.maintenance.compaction", {
            let project_compacted = record_live_compaction_outcome(
                tracedecay_runtime_core::config::DB_FILENAME,
                crate::retention::live_compaction::compact_project_store(
                    lease.graph_db(),
                    compaction,
                )
                .await,
            );
            if !project_compacted {
                outcome = MaintenanceTickOutcome::Retry;
            }
            if !cancellation.is_cancelled() {
                let branch_compacted = run_branch_compaction(lease, compaction);
                if !branch_compacted {
                    outcome = MaintenanceTickOutcome::Retry;
                }
            }
        });
    }
    finalize_generation_outcome(outcome, cancellation)
}

/// Cancelled and degraded ticks are recorded too: a maintenance lane that
/// silently retries forever is exactly the waste being diagnosed.
fn finalize_generation_outcome(
    outcome: MaintenanceTickOutcome,
    cancellation: &tracedecay_session_memory::context::CancellationToken,
) -> MaintenanceTickOutcome {
    if cancellation.is_cancelled() {
        hotpath::gauge!("daemon.maintenance.generation.cancelled_total").inc(1_u64);
        MaintenanceTickOutcome::Retry
    } else {
        if matches!(outcome, MaintenanceTickOutcome::Retry) {
            hotpath::gauge!("daemon.maintenance.generation.retry_total").inc(1_u64);
        }
        outcome
    }
}

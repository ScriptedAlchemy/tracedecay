//! Ordered generation retention for one mounted project, and code-generation
//! retention for the unmounted scopes of one registered project.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::compaction_receipt::record_live_compaction_outcome;
use crate::lease::ProjectStoreMaintenanceLeaseV1;
use crate::store_maintenance::{
    CodeGenerationRetentionOutcomeV1, RegisteredProjectStoreV1, run_code_generation_retention,
    run_code_index_scope_reconciliation, run_registered_code_generation_retention,
};
use crate::telemetry::StoreTelemetrySamplingRegistry;
use crate::tick::{MaintenanceContinuation, MaintenanceTickOutcome};
use tracedecay_contracts::storage::compaction::CompactionThresholdConfig;

/// Run the production generation-maintenance journey for one admitted store lease.
///
/// A fresh full tick runs bounded code-generation retention, then collects
/// the scopes of removed worktrees, then the independent compaction passes.
/// A collected scope continues on the short cadence so the next
/// code-generation pass sweeps the shared segments and text artifacts only it
/// named. A code-generation continuation runs only the bounded
/// code-generation unit, draining a superseded backlog without re-running
/// scope reconciliation or compaction.
#[hotpath::measure(label = "daemon.maintenance.generation", future = true)]
pub async fn run_project_generation_maintenance(
    lease: &ProjectStoreMaintenanceLeaseV1,
    code_index_schedulers: &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    maintenance_observations: &StoreTelemetrySamplingRegistry,
    cancellation: &tracedecay_runtime_core::cancellation::CancellationToken,
    compaction: Option<&CompactionThresholdConfig>,
    continuation: Option<MaintenanceContinuation>,
) -> MaintenanceTickOutcome {
    // Each ordered phase gets its own wall span: the outer generation span is
    // inclusive, so a slow tick is attributed to code generation retention or
    // compaction, not guessed.
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
    if !cancellation.is_cancelled() {
        outcome = outcome.combine(hotpath::measure_block!(
            "daemon.maintenance.scope_reconciliation",
            run_code_index_scope_reconciliation(lease, code_index_schedulers).await
        ));
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
        });
    }
    finalize_generation_outcome(outcome, cancellation)
}

/// Run code-generation retention over the scopes of one registered project
/// that no mounted graph owns. It is the whole unit on a full tick and on a
/// code-generation continuation alike: an unmounted store has no compaction
/// pass of its own here.
#[hotpath::measure(label = "daemon.maintenance.registered_generation", future = true)]
pub async fn run_registered_project_generation_maintenance(
    store: &RegisteredProjectStoreV1,
    mounted_store_roots: &BTreeSet<PathBuf>,
    code_index_schedulers: &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    profile_database: &tracedecay_global_db::RegisteredGlobalDb,
    maintenance_observations: &StoreTelemetrySamplingRegistry,
    cancellation: &tracedecay_runtime_core::cancellation::CancellationToken,
) -> MaintenanceTickOutcome {
    let outcome = match run_registered_code_generation_retention(
        store,
        mounted_store_roots,
        code_index_schedulers,
        profile_database,
        maintenance_observations,
        cancellation,
    )
    .await
    {
        CodeGenerationRetentionOutcomeV1::Complete => MaintenanceTickOutcome::Complete,
        CodeGenerationRetentionOutcomeV1::MoreWork => {
            MaintenanceTickOutcome::Continue(MaintenanceContinuation::CodeGenerationRetention)
        }
        CodeGenerationRetentionOutcomeV1::Failed => MaintenanceTickOutcome::Retry,
    };
    finalize_generation_outcome(outcome, cancellation)
}

/// Cancelled and degraded ticks are recorded too: a maintenance lane that
/// silently retries forever is exactly the waste being diagnosed.
fn finalize_generation_outcome(
    outcome: MaintenanceTickOutcome,
    cancellation: &tracedecay_runtime_core::cancellation::CancellationToken,
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

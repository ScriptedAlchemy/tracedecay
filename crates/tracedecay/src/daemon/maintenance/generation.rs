//! Ordered generation retention for one mounted project.

use super::{MaintenanceContinuation, MaintenanceTickOutcome, StoreTelemetrySamplingRegistry};
use crate::daemon::store_maintenance::CodeGenerationRetentionOutcomeV1;

/// Run the production generation-maintenance journey for one mounted project.
///
/// Vector generations converge before their source code generations can be
/// collected. Scope deletion is admitted only from a complete
/// post-convergence vector census. Code-generation retention still runs when
/// vector retention failed: it resolves its own vector protection inventory,
/// and an unreadable inventory reports its degradation and collects nothing
/// so a mounted activation lease keeps its exact source generation. A daemon
/// without a seated semantic runtime (the default-off state) sweeps under the
/// offline protection set quietly — as an ordinary success, not a degraded
/// retry loop — and an in-progress census defers the sweep until its exact
/// pin set completes. A fresh full tick intentionally preserves this
/// ordered journey, including independent compaction. A semantic continuation
/// returns after its owning phase, while a code-generation continuation runs
/// the bounded semantic-vector page and the bounded code-generation unit —
/// draining a superseded backlog on the short cadence without re-running
/// scope reconciliation or compaction.
#[hotpath::measure(label = "daemon.maintenance.generation", future = true)]
pub(in crate::daemon) async fn run_project_generation_maintenance(
    graph: &crate::tracedecay::TraceDecay,
    code_index_schedulers: &tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    maintenance_observations: &StoreTelemetrySamplingRegistry,
    cancellation: &tracedecay_session_memory::context::CancellationToken,
    retention: &crate::config::RetentionConfig,
    continuation: Option<MaintenanceContinuation>,
) -> MaintenanceTickOutcome {
    // Each ordered phase gets its own wall span: the outer generation span is
    // inclusive, so a slow tick is attributed to vector retention, code
    // generation retention, scope reconciliation, or compaction — not guessed.
    let mut outcome = hotpath::measure_block!(
        "daemon.maintenance.vector_retention",
        crate::daemon::store_maintenance::run_semantic_vector_generation_retention(
            graph,
            code_index_schedulers,
            maintenance_observations,
            cancellation,
        )
        .await
    );
    if continuation == Some(MaintenanceContinuation::SemanticVectorRetention) {
        return outcome;
    }
    let semantic_collection_complete = outcome.is_complete();
    let code_generation = if cancellation.is_cancelled() {
        CodeGenerationRetentionOutcomeV1::Failed
    } else {
        hotpath::measure_block!(
            "daemon.maintenance.code_generation_retention",
            crate::daemon::store_maintenance::run_code_generation_retention(
                graph,
                code_index_schedulers,
                maintenance_observations,
                cancellation,
            )
            .await
        )
    };
    match code_generation {
        CodeGenerationRetentionOutcomeV1::Complete => {}
        CodeGenerationRetentionOutcomeV1::MoreWork => {
            outcome = outcome.combine(MaintenanceTickOutcome::Continue(
                MaintenanceContinuation::CodeGenerationRetention,
            ));
        }
        CodeGenerationRetentionOutcomeV1::Failed => {
            if !cancellation.is_cancelled() {
                outcome = MaintenanceTickOutcome::Retry;
            }
        }
    }
    if continuation == Some(MaintenanceContinuation::CodeGenerationRetention) {
        return finalize_generation_outcome(outcome, cancellation);
    }
    if semantic_collection_complete
        && code_generation == CodeGenerationRetentionOutcomeV1::Complete
        && !cancellation.is_cancelled()
        && maintenance_observations.semantic_vector_scope_collection_ready(graph.project_root())
    {
        let scope_reconciled = hotpath::measure_block!(
            "daemon.maintenance.scope_reconciliation",
            crate::daemon::store_maintenance::run_code_index_scope_reconciliation(
                graph,
                code_index_schedulers,
                maintenance_observations,
            )
            .await
        );
        if !scope_reconciled {
            outcome = MaintenanceTickOutcome::Retry;
        }
    }
    if !cancellation.is_cancelled()
        && let Some(compaction) = &retention.compaction
    {
        hotpath::measure_block!("daemon.maintenance.compaction", {
            let project_compacted =
                crate::daemon::store_maintenance::run_project_compaction(graph.db(), compaction)
                    .await;
            if !project_compacted {
                outcome = MaintenanceTickOutcome::Retry;
            }
            if !cancellation.is_cancelled() {
                let branch_compacted =
                    crate::daemon::store_maintenance::run_branch_compaction(graph, compaction)
                        .await;
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

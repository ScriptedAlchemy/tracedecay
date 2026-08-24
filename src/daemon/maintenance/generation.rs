//! Ordered generation retention for one mounted project.

use super::{MaintenanceContinuation, MaintenanceTickOutcome, StoreTelemetrySamplingRegistry};

/// Run the production generation-maintenance journey for one mounted project.
///
/// Vector generations converge before their source code generations can be
/// collected. Scope deletion is admitted only from a complete
/// post-convergence vector census. Code-generation retention still runs when
/// vector retention failed: it resolves its own vector protection inventory
/// and degrades to the offline protection set when the graph runtime is
/// unavailable, so sealed files cannot grow without bound while the graph is
/// dark. A daemon without a seated semantic runtime (the default-off state)
/// takes the same offline sweep quietly — as an ordinary success, not a
/// degraded retry loop — and an in-progress census defers the sweep until
/// its exact pin set completes. A fresh full tick intentionally preserves this
/// ordered journey, including independent compaction; a later semantic
/// continuation returns after its owning phase so its short cadence cannot
/// re-run unrelated generation-maintenance work.
pub(in crate::daemon) async fn run_project_generation_maintenance(
    graph: &crate::tracedecay::TraceDecay,
    code_index_schedulers: &crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    maintenance_observations: &StoreTelemetrySamplingRegistry,
    cancellation: &tracedecay_usecases::context::CancellationToken,
    retention: &crate::config::RetentionConfig,
    continuation: Option<MaintenanceContinuation>,
) -> MaintenanceTickOutcome {
    let mut outcome = crate::daemon::store_maintenance::run_semantic_vector_generation_retention(
        graph,
        code_index_schedulers,
        maintenance_observations,
        cancellation,
    )
    .await;
    if continuation.is_some() {
        return outcome;
    }
    let semantic_collection_complete = outcome.is_complete();
    let code_generation_succeeded = if !cancellation.is_cancelled() {
        crate::daemon::store_maintenance::run_code_generation_retention(
            graph,
            code_index_schedulers,
            maintenance_observations,
            cancellation,
        )
        .await
    } else {
        false
    };
    if !code_generation_succeeded && !cancellation.is_cancelled() {
        outcome = MaintenanceTickOutcome::Retry;
    }
    if semantic_collection_complete
        && code_generation_succeeded
        && !cancellation.is_cancelled()
        && maintenance_observations.semantic_vector_scope_collection_ready(graph.project_root())
    {
        let scope_reconciled =
            crate::daemon::store_maintenance::run_code_index_scope_reconciliation(
                graph,
                code_index_schedulers,
                maintenance_observations,
            )
            .await;
        if !scope_reconciled {
            outcome = MaintenanceTickOutcome::Retry;
        }
    }
    if !cancellation.is_cancelled()
        && let Some(compaction) = &retention.compaction
    {
        let project_compacted =
            crate::daemon::store_maintenance::run_project_compaction(graph.db(), compaction).await;
        if !project_compacted {
            outcome = MaintenanceTickOutcome::Retry;
        }
        if !cancellation.is_cancelled() {
            let branch_compacted =
                crate::daemon::store_maintenance::run_branch_compaction(graph, compaction).await;
            if !branch_compacted {
                outcome = MaintenanceTickOutcome::Retry;
            }
        }
    }
    if cancellation.is_cancelled() {
        MaintenanceTickOutcome::Retry
    } else {
        outcome
    }
}

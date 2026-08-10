//! Ordered generation retention for one mounted project.

use super::StoreTelemetrySamplingRegistry;

/// Run the production generation-maintenance journey for one mounted project.
///
/// Vector generations converge before their source code generations can be
/// collected. Scope deletion is admitted only from a complete
/// post-convergence vector census.
pub(in crate::daemon) async fn run_project_generation_maintenance(
    graph: &crate::tracedecay::TraceDecay,
    code_index_schedulers: &crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    maintenance_observations: &StoreTelemetrySamplingRegistry,
    cancellation: &tracedecay_usecases::context::CancellationToken,
    retention: &crate::config::RetentionConfig,
) -> bool {
    let mut unit_succeeded =
        crate::daemon::store_maintenance::run_semantic_vector_generation_retention(
            graph,
            code_index_schedulers,
            maintenance_observations,
            cancellation,
        )
        .await;
    if super::code_generation_retention_is_eligible(unit_succeeded, cancellation.is_cancelled()) {
        unit_succeeded &= crate::daemon::store_maintenance::run_code_generation_retention(
            graph,
            code_index_schedulers,
            maintenance_observations,
            cancellation,
        )
        .await;
    }
    if unit_succeeded
        && !cancellation.is_cancelled()
        && maintenance_observations.semantic_vector_scope_collection_ready(graph.project_root())
    {
        unit_succeeded &= crate::daemon::store_maintenance::run_code_index_scope_reconciliation(
            graph,
            code_index_schedulers,
            maintenance_observations,
        )
        .await;
    }
    if !cancellation.is_cancelled()
        && let Some(compaction) = &retention.compaction
    {
        unit_succeeded &=
            crate::daemon::store_maintenance::run_project_compaction(graph.db(), compaction).await;
        if !cancellation.is_cancelled() {
            unit_succeeded &=
                crate::daemon::store_maintenance::run_branch_compaction(graph, compaction).await;
        }
    }
    unit_succeeded && !cancellation.is_cancelled()
}

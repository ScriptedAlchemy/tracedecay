use tracedecay_domain::ProjectId;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::shard_runtime::VerifiedGraphRuntimeWeakProxyV1;
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

/// Returns the verified graph runtime that `registry.project_memory` binds to
/// `database` in the background. Waits on the mount's own graph settlement,
/// which is terminal whether the open attached or detached, so it needs no
/// deadline.
pub async fn await_bound_graph_runtime(
    registry: &DaemonSessionRuntimeRegistryV1,
    project_id: &ProjectId,
    database: &Database,
    operation: &'static str,
) -> Result<VerifiedGraphRuntimeWeakProxyV1> {
    registry.settle_project_memory_graph(project_id).await?;
    database
        .memory_graph_runtime()
        .ok_or_else(|| TraceDecayError::Database {
            operation: operation.to_owned(),
            message: "project memory graph settled without binding its verified graph runtime"
                .to_owned(),
        })
}

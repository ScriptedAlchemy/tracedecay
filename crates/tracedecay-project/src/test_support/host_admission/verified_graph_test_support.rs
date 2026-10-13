use std::time::{Duration, Instant};

use tracedecay_runtime_core::shard_runtime::VerifiedGraphRuntimeWeakProxyV1;

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::db::Database;

/// Awaited from both Tokio and Asupersync tests, so the deadline is a clock
/// check rather than either executor's timer.
pub async fn await_bound_graph_runtime(
    database: &Database,
    operation: &'static str,
) -> Result<VerifiedGraphRuntimeWeakProxyV1> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(runtime) = database.memory_graph_runtime() {
            return Ok(runtime);
        }
        if Instant::now() >= deadline {
            return Err(TraceDecayError::Database {
                operation: operation.to_owned(),
                message: "project memory database did not publish its verified graph runtime"
                    .to_owned(),
            });
        }
        asupersync::runtime::yield_now().await;
    }
}

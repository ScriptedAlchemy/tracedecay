use std::time::Duration;

use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimeWeakProxyV1;

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::db::Database;

pub(crate) async fn await_bound_graph_runtime(
    database: &Database,
    operation: &'static str,
) -> Result<VerifiedGraphRuntimeWeakProxyV1> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(runtime) = database.memory_graph_runtime() {
                break runtime;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| TraceDecayError::Database {
        operation: operation.to_owned(),
        message: "project memory database did not publish its verified graph runtime".to_owned(),
    })
}

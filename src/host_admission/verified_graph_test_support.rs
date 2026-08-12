use std::sync::Arc;

use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;

use crate::db::Database;
use crate::errors::{Result, TraceDecayError};

pub(super) fn bound_graph_runtime(
    database: &Database,
    operation: &'static str,
) -> Result<Arc<dyn VerifiedGraphRuntimePortV1>> {
    database
        .memory_graph_runtime()
        .ok_or_else(|| TraceDecayError::Database {
            operation: operation.to_owned(),
            message: "project memory database has no verified graph runtime".to_owned(),
        })
}

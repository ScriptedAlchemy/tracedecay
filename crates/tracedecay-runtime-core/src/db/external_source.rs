//! Additive external-source state schema owned by the canonical database.

use crate::db::engine::Executor;
use crate::errors::{Result, TraceDecayError};

pub async fn install_external_source_schema(
    connection: &impl Executor,
    operation: &str,
) -> Result<()> {
    connection
        .execute_batch(tracedecay_rusqlite_runtime::repository::EXTERNAL_SOURCE_SCHEMA_V1)
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("{operation}: failed to install external source state: {error}"),
            operation: operation.to_owned(),
        })?;
    Ok(())
}

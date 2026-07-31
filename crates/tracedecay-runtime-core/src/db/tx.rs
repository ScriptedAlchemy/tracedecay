// Rust guideline compliant 2025-10-17
use super::connection::DatabaseEngineReadSnapshot;
use crate::errors::{Result, TraceDecayError};

pub(super) async fn commit(transaction: DatabaseEngineReadSnapshot, operation: &str) -> Result<()> {
    transaction
        .commit()
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to commit transaction: {error}"),
            operation: operation.to_string(),
        })
}

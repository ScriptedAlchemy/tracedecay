// Rust guideline compliant 2025-10-17
use super::connection::Database;
use crate::errors::{Result, TraceDecayError};

impl Database {
    /// Removes all data from every table.
    pub async fn clear(&self) -> Result<()> {
        self.conn()
            .execute_batch(
                "DELETE FROM vectors;
                 DELETE FROM unresolved_refs;
                 DELETE FROM edges;
                 DELETE FROM nodes;
                 DELETE FROM files;",
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to clear database: {e}"),
                operation: "clear".to_string(),
            })?;
        Ok(())
    }
}

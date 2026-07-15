// Rust guideline compliant 2025-10-17
use super::connection::{Database, DatabaseWriteTransaction};
use crate::errors::{Result, TraceDecayError};

impl Database {
    /// Removes all data from every table.
    pub async fn clear(&self) -> Result<()> {
        let transaction = self.begin_write_transaction("clear").await?;
        self.clear_unguarded(&transaction).await?;
        transaction.commit().await
    }

    pub(crate) async fn clear_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        transaction
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

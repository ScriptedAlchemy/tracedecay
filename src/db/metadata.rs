// Rust guideline compliant 2025-10-17
use crate::db::engine::params;

use super::connection::{Database, DatabaseWriteTransaction};
use crate::errors::{Result, TraceDecayError};

impl Database {
    /// Reads a metadata value by key, returning `None` if not set.
    pub async fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        let mut rows = self
            .engine_conn()
            .query("SELECT value FROM metadata WHERE key = ?1", params![key])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query metadata: {e}"),
                operation: "get_metadata".to_string(),
            })?;

        match rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read metadata row: {e}"),
            operation: "get_metadata".to_string(),
        })? {
            Some(row) => {
                let value: String = row.get(0).map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read metadata value: {e}"),
                    operation: "get_metadata".to_string(),
                })?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// Reads a metadata value through an already-open canonical write
    /// transaction. Compound durable operations use this to keep their
    /// compare-and-set and metadata update on one writer lane.
    pub(crate) async fn get_metadata_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        key: &str,
    ) -> Result<Option<String>> {
        let mut rows = transaction
            .query_engine("SELECT value FROM metadata WHERE key = ?1", params![key])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query transactional metadata: {e}"),
                operation: "get_metadata_unguarded".to_string(),
            })?;

        match rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read transactional metadata row: {e}"),
            operation: "get_metadata_unguarded".to_string(),
        })? {
            Some(row) => {
                let value: String = row.get(0).map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read transactional metadata value: {e}"),
                    operation: "get_metadata_unguarded".to_string(),
                })?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// Sets a metadata value, creating or replacing the entry.
    pub async fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        let transaction = self.begin_write_transaction("set_metadata").await?;
        self.set_metadata_unguarded(&transaction, key, value)
            .await?;
        transaction.commit().await
    }

    pub(crate) async fn set_metadata_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        key: &str,
        value: &str,
    ) -> Result<()> {
        transaction
            .execute_engine(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to set metadata: {e}"),
                operation: "set_metadata".to_string(),
            })?;
        Ok(())
    }
}

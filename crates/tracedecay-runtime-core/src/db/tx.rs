// Rust guideline compliant 2025-10-17
use super::connection::Database;
use crate::errors::{Result, TraceDecayError};

impl Database {
    pub(super) async fn with_batch_transaction<T>(
        &self,
        operation: &str,
        work: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        self.conn()
            .execute("BEGIN", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to begin: {e}"),
                operation: operation.to_string(),
            })?;

        match work.await {
            Ok(value) => {
                if let Err(e) = self.conn().execute("COMMIT", ()).await {
                    let _ = self.conn().execute("ROLLBACK", ()).await;
                    return Err(TraceDecayError::Database {
                        message: format!("failed to commit: {e}"),
                        operation: operation.to_string(),
                    });
                }
                Ok(value)
            }
            Err(e) => {
                let _ = self.conn().execute("ROLLBACK", ()).await;
                Err(e)
            }
        }
    }
}

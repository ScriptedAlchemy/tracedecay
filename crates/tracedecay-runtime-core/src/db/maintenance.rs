// Rust guideline compliant 2025-10-17
use super::connection::{Database, DatabaseWriteTransaction};
use crate::errors::{Result, TraceDecayError};

impl Database {
    pub fn storage_telemetry_handle(
        &self,
    ) -> Result<tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle> {
        self.retained_runtime()
            .telemetry_read_handle()
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to attach graph-store telemetry reader: {error:?}"),
                operation: "attach graph-store telemetry reader".to_string(),
            })
    }

    pub async fn storage_page_counts(&self) -> Result<(u64, u64, u64)> {
        self.retained_runtime()
            .storage_page_counts(std::time::Duration::from_secs(5))
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to sample graph-store pages: {error:?}"),
                operation: "sample graph-store pages".to_string(),
            })
    }

    /// Runs a bounded incremental vacuum through the canonical writer lane.
    pub async fn run_incremental_vacuum(&self, pages: u64) -> Result<()> {
        let authority = self.write_authority()?;
        self.retained_runtime()
            .run_bounded_incremental_compaction(pages, authority)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to compact graph store: {error:?}"),
                operation: "run bounded graph-store compaction".to_string(),
            })
    }

    /// Removes all data from every table.
    pub async fn clear(&self) -> Result<()> {
        let transaction = self.begin_write_transaction("clear").await?;
        self.clear_unguarded(&transaction).await?;
        transaction.commit().await
    }

    pub async fn clear_unguarded(&self, transaction: &DatabaseWriteTransaction<'_>) -> Result<()> {
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

    #[tokio::test]
    async fn incremental_vacuum_reclaims_freelist_pages() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority =
            DatabaseAuthority::acquire_test(&path, "graph compaction regression").unwrap();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        db.execute_write_batch(
            "create compaction fixture",
            "CREATE TABLE compaction_fixture (id INTEGER PRIMARY KEY, payload BLOB);",
        )
        .await
        .unwrap();
        let payload = vec![7u8; 64 * 1024];
        for id in 0..16i64 {
            db.execute_write_engine(
                "seed compaction fixture",
                "INSERT INTO compaction_fixture (id, payload) VALUES (?1, ?2)",
                crate::db::engine::params![id, payload.clone()],
            )
            .await
            .unwrap();
        }
        db.execute_write_batch(
            "delete compaction fixture",
            "DELETE FROM compaction_fixture;",
        )
        .await
        .unwrap();
        let (_, _, freelist_before) = db.storage_page_counts().await.unwrap();
        assert!(freelist_before > 0, "fixture must create reclaimable pages");

        db.run_incremental_vacuum(freelist_before).await.unwrap();

        let (_, _, freelist_after) = db.storage_page_counts().await.unwrap();
        assert!(
            freelist_after < freelist_before,
            "successful incremental vacuum must reclaim pages"
        );
    }
}

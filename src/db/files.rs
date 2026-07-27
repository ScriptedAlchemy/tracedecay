// Rust guideline compliant 2025-10-17
use crate::db::engine::params;

use super::connection::{Database, DatabaseWriteTransaction};
use super::rows::row_to_file;
use super::sql::collect_rows;
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

impl Database {
    /// Inserts or replaces a file record.
    /// Batch upserts multiple file records using raw SQL for throughput.
    pub async fn upsert_files(&self, files: &[FileRecord]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }

        let transaction = self.begin_write_transaction("upsert_files").await?;
        self.upsert_files_unguarded(&transaction, files).await?;
        transaction.commit().await
    }

    pub(crate) async fn upsert_files_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        files: &[FileRecord],
    ) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }

        let stmt = transaction
            .prepare_engine("INSERT OR REPLACE INTO files (path,content_hash,size,modified_at,indexed_at,node_count) VALUES (?1,?2,?3,?4,?5,?6)")
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to prepare: {e}"),
                operation: "upsert_files".to_string(),
            })?;

        for file in files {
            if let Err(e) = stmt
                .execute(params![
                    file.path.as_str(),
                    file.content_hash.as_str(),
                    file.size as i64,
                    file.modified_at,
                    file.indexed_at,
                    i64::from(file.node_count),
                ])
                .await
            {
                stmt.reset();
                return Err(TraceDecayError::Database {
                    message: format!("failed to upsert file: {e}"),
                    operation: "upsert_files".to_string(),
                });
            }
            stmt.reset();
        }

        drop(stmt);
        Ok(())
    }

    pub async fn upsert_file(&self, file: &FileRecord) -> Result<()> {
        let transaction = self.begin_write_transaction("upsert_file").await?;
        self.upsert_file_unguarded(&transaction, file).await?;
        transaction.commit().await
    }

    pub(crate) async fn upsert_file_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        file: &FileRecord,
    ) -> Result<()> {
        transaction
            .execute_engine(
                "INSERT OR REPLACE INTO files
                (path, content_hash, size, modified_at, indexed_at, node_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    file.path.as_str(),
                    file.content_hash.as_str(),
                    file.size as i64,
                    file.modified_at,
                    file.indexed_at,
                    i64::from(file.node_count),
                ],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to upsert file: {e}"),
                operation: "upsert_file".to_string(),
            })?;
        Ok(())
    }

    /// Retrieves a file record by path, returning `None` if not found.
    pub async fn get_file(&self, path: &str) -> Result<Option<FileRecord>> {
        let mut rows = self
            .engine_conn()
            .query(
                "SELECT path, content_hash, size, modified_at, indexed_at, node_count
                 FROM files WHERE path = ?1",
                params![path],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query file: {e}"),
                operation: "get_file".to_string(),
            })?;

        match rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read file row: {e}"),
            operation: "get_file".to_string(),
        })? {
            Some(row) => {
                let file = row_to_file(&row).map_err(|e| TraceDecayError::Database {
                    message: format!("failed to map file row: {e}"),
                    operation: "get_file".to_string(),
                })?;
                Ok(Some(file))
            }
            None => Ok(None),
        }
    }

    /// Returns all file records.
    pub async fn get_all_files(&self) -> Result<Vec<FileRecord>> {
        let mut rows = self
            .engine_conn()
            .query(
                "SELECT path, content_hash, size, modified_at, indexed_at, node_count FROM files",
                (),
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query all files: {e}"),
                operation: "get_all_files".to_string(),
            })?;

        collect_rows(&mut rows, row_to_file, "get_all_files").await
    }

    /// Returns only indexed logical paths.
    ///
    /// Startup language discovery needs names, not hashes or metadata. Keeping
    /// that query narrow avoids materializing every full file record for large
    /// projects.
    pub async fn get_all_file_paths(&self) -> Result<Vec<String>> {
        let mut rows = self
            .engine_conn()
            .query("SELECT path FROM files ORDER BY path", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query all file paths: {e}"),
                operation: "get_all_file_paths".to_string(),
            })?;
        let mut paths = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read file path row: {e}"),
            operation: "get_all_file_paths".to_string(),
        })? {
            paths.push(row.get(0).map_err(|e| TraceDecayError::Database {
                message: format!("failed to map file path row: {e}"),
                operation: "get_all_file_paths".to_string(),
            })?);
        }
        Ok(paths)
    }

    /// Deletes a file record and its graph data atomically.
    pub async fn delete_file(&self, path: &str) -> Result<()> {
        self.delete_file_transaction(path, std::future::ready(()))
            .await
    }

    /// Deletes a file while the caller holds the database writer lane.
    pub(crate) async fn delete_file_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        path: &str,
    ) -> Result<()> {
        Self::delete_nodes_by_file_in_transaction(transaction, path).await?;
        transaction
            .execute_engine("DELETE FROM files WHERE path = ?1", params![path])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to delete file: {e}"),
                operation: "delete_file".to_string(),
            })?;
        Ok(())
    }

    async fn delete_file_transaction<F>(&self, path: &str, after_node_delete: F) -> Result<()>
    where
        F: std::future::Future<Output = ()>,
    {
        let transaction = self.begin_write_transaction("delete_file").await?;
        Self::delete_nodes_by_file_in_transaction(&transaction, path).await?;
        after_node_delete.await;
        transaction
            .execute_engine("DELETE FROM files WHERE path = ?1", params![path])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to delete file: {e}"),
                operation: "delete_file".to_string(),
            })?;
        transaction.commit().await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

    async fn seeded_database() -> (tempfile::TempDir, Database) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "delete file tests").unwrap();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        db.upsert_file(&FileRecord {
            path: "src/lib.rs".to_string(),
            content_hash: "hash".to_string(),
            size: 1,
            modified_at: 1,
            indexed_at: 1,
            node_count: 1,
        })
        .await
        .unwrap();
        db.insert_node(&Node {
            id: "node".to_string(),
            kind: NodeKind::Function,
            name: "f".to_string(),
            qualified_name: "crate::f".to_string(),
            file_path: "src/lib.rs".to_string(),
            start_line: 1,
            attrs_start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 1,
            signature: None,
            docstring: None,
            visibility: Visibility::Private,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 1,
            parent_id: None,
        })
        .await
        .unwrap();
        (temp, db)
    }

    async fn row_count(db: &Database, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let mut rows = db.engine_conn().query(&sql, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    #[tokio::test]
    async fn cancelled_delete_file_rolls_back_nodes_and_file() {
        let (_temp, db) = seeded_database().await;
        let (started_tx, entered_rx) = tokio::sync::oneshot::channel();
        let delete_db = db.clone();
        let delete = tokio::spawn(async move {
            delete_db
                .delete_file_transaction("src/lib.rs", async move {
                    started_tx.send(()).unwrap();
                    std::future::pending::<()>().await;
                })
                .await
        });

        entered_rx.await.unwrap();
        assert_eq!(row_count(&db, "nodes").await, 1);
        assert_eq!(row_count(&db, "files").await, 1);
        delete.abort();
        assert!(delete.await.unwrap_err().is_cancelled());
        assert_eq!(row_count(&db, "nodes").await, 1);
        assert_eq!(row_count(&db, "files").await, 1);
    }

    #[tokio::test]
    async fn delete_file_waits_for_writer_lane() {
        let (_temp, db) = seeded_database().await;
        let writer = db.writer().await;
        let delete_db = db.clone();
        let mut delete = tokio::spawn(async move { delete_db.delete_file("src/lib.rs").await });

        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut delete)
                .await
                .is_err()
        );
        drop(writer);
        delete.await.unwrap().unwrap();
        assert_eq!(row_count(&db, "nodes").await, 0);
        assert_eq!(row_count(&db, "files").await, 0);
    }
}

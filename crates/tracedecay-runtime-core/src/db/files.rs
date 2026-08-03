// Rust guideline compliant 2025-10-17
use libsql::params;

use super::connection::Database;
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

        self.with_batch_transaction("upsert_files", async {
            let stmt = self.conn()
                .prepare("INSERT OR REPLACE INTO files (path,content_hash,size,modified_at,indexed_at,node_count) VALUES (?1,?2,?3,?4,?5,?6)")
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

            Ok(())
        })
        .await
    }

    pub async fn upsert_file(&self, file: &FileRecord) -> Result<()> {
        self.conn()
            .execute(
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
            .conn()
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
            .conn()
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

    /// Deletes a file record and cascades to delete its nodes first.
    pub async fn delete_file(&self, path: &str) -> Result<()> {
        self.delete_nodes_by_file(path).await?;
        self.conn()
            .execute("DELETE FROM files WHERE path = ?1", params![path])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to delete file: {e}"),
                operation: "delete_file".to_string(),
            })?;
        Ok(())
    }
}

// Rust guideline compliant 2025-10-17
use std::collections::HashMap;

use crate::db::engine::params;

use super::connection::{Database, DatabaseWriteTransaction};
use super::rows::row_to_file;
use super::sql::collect_rowid_pages;
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

/// Columns `row_to_file` reads, and therefore the index of the trailing
/// `rowid` cursor column in a paged file scan.
const FILE_COLUMNS: i32 = 6;

/// One `rowid` keyset page of just the indexed paths. Shared with
/// [`Database::get_stats`], which counts the same paths by language.
pub(super) const FILE_PATH_PAGE_SQL: &str =
    "SELECT path, rowid FROM files WHERE rowid > ?1 ORDER BY rowid LIMIT ?2";

/// Keyset page size for file token-map construction.
///
/// Token accounting only needs `(path, size)`. Paging keeps peak allocation
/// proportional to this bound rather than the full `files` table width/row
/// count during daemon / MCP startup.
pub const FILE_TOKEN_MAP_PAGE_SIZE: usize = 1_024;

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

    pub async fn upsert_files_unguarded(
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

    pub async fn upsert_file_unguarded(
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
    /// Read through `rowid` keyset pages: whole-table reads on a real project
    /// exceed what the `SQLite` runtime will materialize for one query.
    pub async fn get_all_files(&self) -> Result<Vec<FileRecord>> {
        collect_rowid_pages(
            &self.engine_conn(),
            "SELECT path, content_hash, size, modified_at, indexed_at, node_count, rowid
             FROM files WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            FILE_COLUMNS,
            row_to_file,
            "get_all_files",
        )
        .await
    }

    /// Returns only indexed logical paths.
    ///
    /// Startup language discovery needs names, not hashes or metadata. Keeping
    /// that query narrow avoids materializing every full file record for large
    /// projects.
    /// Read through `rowid` keyset pages: whole-table reads on a real project
    /// exceed what the `SQLite` runtime will materialize for one query. The
    /// pages arrive in `rowid` order, so the path ordering callers see is
    /// restored here rather than by the database.
    pub async fn get_all_file_paths(&self) -> Result<Vec<String>> {
        let mut paths = collect_rowid_pages(
            &self.engine_conn(),
            FILE_PATH_PAGE_SQL,
            1,
            |row| row.get::<String>(0),
            "get_all_file_paths",
        )
        .await?;
        paths.sort_unstable();
        Ok(paths)
    }

    /// Returns one keyset page of `(path, size)` pairs for token accounting.
    ///
    /// `after_path` is exclusive: pass `None` for the first page, then the last
    /// path from the previous page. Rows are ordered by `path` ascending.
    pub async fn get_file_token_sizes_page(
        &self,
        after_path: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, u64)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut rows = match after_path {
            Some(after) => self
                .engine_conn()
                .query(
                    "SELECT path, size FROM files WHERE path > ?1 ORDER BY path LIMIT ?2",
                    params![after, limit],
                )
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query file token sizes page: {e}"),
                    operation: "get_file_token_sizes_page".to_string(),
                })?,
            None => self
                .engine_conn()
                .query(
                    "SELECT path, size FROM files ORDER BY path LIMIT ?1",
                    params![limit],
                )
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query file token sizes page: {e}"),
                    operation: "get_file_token_sizes_page".to_string(),
                })?,
        };

        let mut page = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read file token size row: {e}"),
            operation: "get_file_token_sizes_page".to_string(),
        })? {
            let path: String = row.get(0).map_err(|e| TraceDecayError::Database {
                message: format!("failed to map file token path: {e}"),
                operation: "get_file_token_sizes_page".to_string(),
            })?;
            let size: i64 = row.get(1).map_err(|e| TraceDecayError::Database {
                message: format!("failed to map file token size: {e}"),
                operation: "get_file_token_sizes_page".to_string(),
            })?;
            page.push((path, size.max(0) as u64));
        }
        Ok(page)
    }

    /// Builds path → approximate token count (size / 4) via bounded keyset pages.
    ///
    /// Equivalent to mapping [`Self::get_all_files`] through `size / 4`, but never
    /// materializes full `FileRecord` rows (including `content_hash`) at once.
    pub async fn get_file_token_map(&self) -> Result<HashMap<String, u64>> {
        let mut map = HashMap::new();
        let mut after: Option<String> = None;
        loop {
            let page = self
                .get_file_token_sizes_page(after.as_deref(), FILE_TOKEN_MAP_PAGE_SIZE)
                .await?;
            let page_len = page.len();
            if page_len == 0 {
                break;
            }
            let next_after = page.last().map(|(path, _)| path.clone());
            for (path, size) in page {
                map.insert(path, size / 4);
            }
            if page_len < FILE_TOKEN_MAP_PAGE_SIZE {
                break;
            }
            after = next_after;
        }
        Ok(map)
    }

    /// Deletes a file record and its graph data atomically.
    pub async fn delete_file(&self, path: &str) -> Result<()> {
        self.delete_file_transaction(path, std::future::ready(()))
            .await
    }

    /// Deletes a file while the caller holds the database writer lane.
    pub async fn delete_file_unguarded(
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
    use std::collections::HashMap;
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

    async fn seed_token_map_files(db: &Database, count: usize) {
        let files: Vec<FileRecord> = (0..count)
            .map(|i| FileRecord {
                path: format!("src/f{i:04}.rs"),
                content_hash: format!("hash-{i}"),
                size: ((i as u64) + 1) * 40,
                modified_at: i as i64,
                indexed_at: i as i64,
                node_count: 1,
            })
            .collect();
        db.upsert_files(&files).await.unwrap();
    }

    #[tokio::test]
    async fn get_file_token_map_matches_full_file_materialization() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "token map tests").unwrap();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        seed_token_map_files(&db, 2_500).await;

        let expected: HashMap<String, u64> = db
            .get_all_files()
            .await
            .unwrap()
            .into_iter()
            .map(|f| (f.path, f.size / 4))
            .collect();
        let actual = db.get_file_token_map().await.unwrap();

        assert_eq!(actual.len(), expected.len());
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn get_file_token_sizes_page_stays_within_limit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "token page tests").unwrap();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        seed_token_map_files(&db, 5).await;

        let page_limit = 2;
        let mut after: Option<String> = None;
        let mut seen = Vec::new();
        let mut max_page_len = 0usize;
        let mut pages = 0usize;
        loop {
            let page = db
                .get_file_token_sizes_page(after.as_deref(), page_limit)
                .await
                .unwrap();
            pages += 1;
            max_page_len = max_page_len.max(page.len());
            assert!(
                page.len() <= page_limit,
                "page exceeded bound: got {} > {}",
                page.len(),
                page_limit
            );
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            after = page.last().map(|(path, _)| path.clone());
            seen.extend(page.into_iter().map(|(path, _)| path));
            if page_len < page_limit {
                break;
            }
        }

        assert_eq!(pages, 3); // 2 + 2 + 1
        assert_eq!(max_page_len, page_limit);
        assert_eq!(
            seen,
            vec![
                "src/f0000.rs",
                "src/f0001.rs",
                "src/f0002.rs",
                "src/f0003.rs",
                "src/f0004.rs",
            ]
        );
    }
}

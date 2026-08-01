// Rust guideline compliant 2025-10-17
use crate::db::engine::{Value, params};

use super::connection::{Database, DatabaseWriteTransaction};
use super::rows::row_to_unresolved_ref;
use super::sql::collect_rowid_pages;
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

const UNRESOLVED_REF_WRITE_PAGE_ROWS: usize = 256;

/// Index of the trailing `rowid` page cursor in a whole-table unresolved-ref
/// scan: it follows the six columns [`row_to_unresolved_ref`] reads.
const UNRESOLVED_REF_COLUMNS: i32 = 6;

/// One `rowid` keyset page of the whole `unresolved_refs` table.
const UNRESOLVED_REF_PAGE_SQL: &str =
    "SELECT from_node_id, reference_name, reference_kind, line, col, file_path, rowid
     FROM unresolved_refs
     WHERE rowid > ?1 ORDER BY rowid LIMIT ?2";

impl Database {
    /// Inserts a single unresolved reference.
    pub async fn insert_unresolved_ref(&self, uref: &UnresolvedRef) -> Result<()> {
        let transaction = self
            .begin_write_transaction("insert_unresolved_ref")
            .await?;
        self.insert_unresolved_ref_unguarded(&transaction, uref)
            .await?;
        transaction.commit().await
    }

    pub(crate) async fn insert_unresolved_ref_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        uref: &UnresolvedRef,
    ) -> Result<()> {
        transaction
            .execute_engine(
                "INSERT INTO unresolved_refs
                (from_node_id, reference_name, reference_kind, line, col, file_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    uref.from_node_id.as_str(),
                    uref.reference_name.as_str(),
                    uref.reference_kind.as_str(),
                    i64::from(uref.line),
                    i64::from(uref.column),
                    uref.file_path.as_str(),
                ],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to insert unresolved ref: {e}"),
                operation: "insert_unresolved_ref".to_string(),
            })?;
        Ok(())
    }

    /// Inserts a batch of unresolved references using a prepared statement.
    pub async fn insert_unresolved_refs(&self, refs: &[UnresolvedRef]) -> Result<()> {
        self.insert_unresolved_refs_paged_with_pause(refs, std::future::ready(()))
            .await
    }

    async fn insert_unresolved_refs_paged_with_pause<F>(
        &self,
        refs: &[UnresolvedRef],
        after_first_page: F,
    ) -> Result<()>
    where
        F: std::future::Future<Output = ()>,
    {
        if refs.is_empty() {
            return Ok(());
        }

        let mut after_first_page = Some(after_first_page);
        for page in refs.chunks(UNRESOLVED_REF_WRITE_PAGE_ROWS) {
            let transaction = self
                .begin_write_transaction("insert_unresolved_refs")
                .await?;
            self.insert_unresolved_refs_unguarded(&transaction, page)
                .await?;
            transaction.commit().await?;
            if let Some(pause) = after_first_page.take() {
                pause.await;
            }
        }
        Ok(())
    }

    pub async fn insert_unresolved_refs_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        refs: &[UnresolvedRef],
    ) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }

        // `Statement::execute` is intentionally a fresh runtime request, not a
        // pinned SQLite statement. Submitting one request per extracted
        // reference made a large checkout pay nearly a million
        // spawn_blocking/channel round-trips. Keep each SQL statement under
        // SQLite's conservative 999-parameter floor (100 rows × 6 values) and
        // retain the surrounding atomic full-index transaction.
        const ROWS_PER_INSERT: usize = 100;
        for chunk in refs.chunks(ROWS_PER_INSERT) {
            let values_clause = (0..chunk.len())
                .map(|row| {
                    let first = row * 6 + 1;
                    format!(
                        "(?{first},?{},?{},?{},?{},?{})",
                        first + 1,
                        first + 2,
                        first + 3,
                        first + 4,
                        first + 5
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "INSERT INTO unresolved_refs \
                 (from_node_id,reference_name,reference_kind,line,col,file_path) \
                 VALUES {values_clause}"
            );
            let mut values = Vec::with_capacity(chunk.len() * 6);
            for unresolved in chunk {
                values.push(Value::Text(unresolved.from_node_id.as_str().to_owned()));
                values.push(Value::Text(unresolved.reference_name.as_str().to_owned()));
                values.push(Value::Text(unresolved.reference_kind.as_str().to_owned()));
                values.push(Value::Integer(i64::from(unresolved.line)));
                values.push(Value::Integer(i64::from(unresolved.column)));
                values.push(Value::Text(unresolved.file_path.clone()));
            }
            transaction
                .execute_engine(&sql, values)
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to insert unresolved ref: {e}"),
                    operation: "insert_unresolved_refs".to_string(),
                })?;
        }
        Ok(())
    }

    /// Returns all unresolved references.
    ///
    /// Read through `rowid` keyset pages. A first index of a real repository
    /// leaves far more unresolved references than the `SQLite` runtime will
    /// materialize for a single query, and the runtime rejects an oversized
    /// query outright rather than truncating it, so the whole-table read has to
    /// arrive as a sequence of pages.
    pub async fn get_unresolved_refs(&self) -> Result<Vec<UnresolvedRef>> {
        collect_rowid_pages(
            &self.engine_conn(),
            UNRESOLVED_REF_PAGE_SQL,
            UNRESOLVED_REF_COLUMNS,
            row_to_unresolved_ref,
            "get_unresolved_refs",
        )
        .await
    }

    /// Removes all unresolved references.
    pub async fn clear_unresolved_refs(&self) -> Result<()> {
        let transaction = self
            .begin_write_transaction("clear_unresolved_refs")
            .await?;
        self.clear_unresolved_refs_unguarded(&transaction).await?;
        transaction.commit().await
    }

    pub(crate) async fn clear_unresolved_refs_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        transaction
            .execute_engine("DELETE FROM unresolved_refs", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to clear unresolved refs: {e}"),
                operation: "clear_unresolved_refs".to_string(),
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

    #[tokio::test]
    async fn unresolved_ref_batch_commits_durable_pages() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority =
            DatabaseAuthority::acquire_test(&path, "unresolved ref batch test").unwrap();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        db.insert_node(&Node {
            id: "source".to_string(),
            kind: NodeKind::Function,
            name: "source".to_string(),
            qualified_name: "crate::source".to_string(),
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
        let refs: Vec<_> = (0..=(UNRESOLVED_REF_WRITE_PAGE_ROWS as u32))
            .map(|index| UnresolvedRef {
                from_node_id: "source".to_string(),
                reference_name: format!("target_{index}"),
                reference_kind: EdgeKind::Calls,
                line: index,
                column: 0,
                file_path: "src/lib.rs".to_string(),
            })
            .collect();

        let (page_committed_tx, page_committed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let insert_db = db.clone();
        let insert_refs = refs.clone();
        let insert = tokio::spawn(async move {
            insert_db
                .insert_unresolved_refs_paged_with_pause(&insert_refs, async move {
                    page_committed_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                })
                .await
        });

        page_committed_rx.await.unwrap();
        assert_eq!(
            db.get_unresolved_refs().await.unwrap().len(),
            UNRESOLVED_REF_WRITE_PAGE_ROWS
        );

        release_tx.send(()).unwrap();
        insert.await.unwrap().unwrap();
        assert_eq!(db.get_unresolved_refs().await.unwrap().len(), refs.len());
    }

    /// A first index of a real repository leaves far more unresolved references
    /// than the `SQLite` runtime will materialize for one query, and the
    /// runtime refuses an oversized query outright instead of truncating it —
    /// which failed branch sync with "migration SQL query materialization
    /// exceeded its limit (operation: `get_unresolved_refs`)".
    ///
    /// Both directions matter: the whole-table statement the read used to
    /// issue must still be refused, and the `rowid` keyset statement it issues
    /// now must return every row.
    #[tokio::test]
    async fn unresolved_ref_scan_pages_past_the_runtime_query_limit() {
        // The runtime refuses a single query that materializes this many rows.
        const RUNTIME_QUERY_ROW_LIMIT: i64 = 10_000;
        const REFS: i64 = RUNTIME_QUERY_ROW_LIMIT + 1;

        let directory = tempfile::TempDir::new().expect("unresolved ref scan tempdir");
        let conn = crate::db::engine::TestConnection::open(&directory.path().join("scan.db"));
        conn.execute_batch(
            "CREATE TABLE unresolved_refs (
                 from_node_id TEXT NOT NULL,
                 reference_name TEXT NOT NULL,
                 reference_kind TEXT NOT NULL,
                 line INTEGER NOT NULL,
                 col INTEGER NOT NULL,
                 file_path TEXT NOT NULL
             );",
        )
        .await
        .expect("create unresolved_refs");
        conn.execute(
            &format!(
                "WITH RECURSIVE fixture(value) AS (
                     SELECT 0 UNION ALL SELECT value + 1 FROM fixture WHERE value < {}
                 )
                 INSERT INTO unresolved_refs
                     (from_node_id, reference_name, reference_kind, line, col, file_path)
                 SELECT 'source', printf('target_%05d', value), 'calls', value, 0, 'src/lib.rs'
                 FROM fixture",
                REFS - 1
            ),
            (),
        )
        .await
        .expect("seed unresolved refs");

        let unpaged = conn
            .query(
                "SELECT from_node_id, reference_name, reference_kind, line, col, file_path
                 FROM unresolved_refs",
                (),
            )
            .await;
        assert!(
            unpaged.is_err(),
            "the whole-table statement must still be refused, or this fixture no longer \
             reproduces the branch-sync failure"
        );

        let refs = collect_rowid_pages(
            &*conn,
            UNRESOLVED_REF_PAGE_SQL,
            UNRESOLVED_REF_COLUMNS,
            row_to_unresolved_ref,
            "get_unresolved_refs",
        )
        .await
        .expect("a paged scan must not exceed the runtime materialization limit");

        assert_eq!(i64::try_from(refs.len()).expect("row count"), REFS);
        assert_eq!(
            refs.first().map(|uref| uref.reference_name.as_str()),
            Some("target_00000")
        );
        assert_eq!(
            refs.last().map(|uref| uref.reference_name.as_str()),
            Some("target_10000")
        );
    }
}

// Rust guideline compliant 2025-10-17
use std::collections::HashSet;

use super::connection::{Database, DatabaseWriteTransaction};
use super::engine::{Value, params_from_iter};
use super::sql::{build_qmark_placeholders, collect_rowid_pages};
use crate::errors::{Result, TraceDecayError};

/// One `rowid` keyset page of the file paths that hold a test-annotated
/// function or method. Paged over the annotated node's `rowid`, so the same
/// path can repeat across pages; the caller dedupes into a [`HashSet`].
pub(super) const TEST_ANNOTATION_FILE_PAGE_SQL: &str = "SELECT t.file_path, t.rowid \
     FROM edges e \
     JOIN nodes n ON e.source = n.id \
     JOIN nodes t ON e.target = t.id \
     WHERE n.kind = 'annotation_usage' \
       AND n.name = 'test' \
       AND e.kind = 'annotates' \
       AND t.kind IN ('function', 'method') \
       AND t.rowid > ?1 \
     ORDER BY t.rowid LIMIT ?2";

/// One `rowid` keyset page of the nodes whose docstring opts out of test
/// coverage.
pub(super) const SKIP_TEST_COVERAGE_PAGE_SQL: &str = "SELECT id, rowid FROM nodes WHERE docstring LIKE '%skip-test-coverage%' \
     AND rowid > ?1 ORDER BY rowid LIMIT ?2";

/// One `rowid` keyset page of the `annotation_usage` nodes whose name marks a
/// function as a test.
pub(super) const TEST_MARKER_PAGE_SQL: &str = "SELECT id, rowid FROM nodes
                   WHERE kind = 'annotation_usage'
                     AND (
                         name = 'test'
                         OR name LIKE '%::test'
                         OR name = 'wasm_bindgen_test'
                         OR name LIKE '%::wasm_bindgen_test'
                     )
                     AND rowid > ?1
                   ORDER BY rowid LIMIT ?2";

impl Database {
    /// Returns the subset of `candidate_ids` that are annotated with `#[test]`
    /// (i.e. targeted by an `Annotates` edge from an `annotation_usage` node
    /// named `"test"`).
    pub async fn get_test_annotated_node_ids(
        &self,
        candidate_ids: &[String],
    ) -> Result<HashSet<String>> {
        // Keep each IN list well below SQLite variable limits. Large
        // repos can pass tens of thousands of function ids from test-risk
        // analysis, and one giant placeholder list fails before the query runs.
        const CHUNK_SIZE: usize = 500;

        if candidate_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let mut result = HashSet::new();

        for chunk in candidate_ids.chunks(CHUNK_SIZE) {
            let placeholders = build_qmark_placeholders(chunk.len());
            let sql = format!(
                "SELECT DISTINCT e.target \
                 FROM edges e \
                 JOIN nodes n ON e.source = n.id \
                 WHERE n.kind = 'annotation_usage' \
                   AND n.name = 'test' \
                   AND e.kind = 'annotates' \
                   AND e.target IN ({placeholders})",
            );
            let param_values: Vec<Value> = chunk.iter().map(|id| Value::Text(id.clone())).collect();
            let mut rows = self
                .engine_conn()
                .query(&sql, params_from_iter(param_values))
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query test-annotated nodes: {e}"),
                    operation: "get_test_annotated_node_ids".to_string(),
                })?;
            while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
                message: format!("failed to read test-annotated row: {e}"),
                operation: "get_test_annotated_node_ids".to_string(),
            })? {
                if let Ok(id) = row.get::<String>(0) {
                    result.insert(id);
                }
            }
        }
        Ok(result)
    }

    /// Returns all file paths that contain at least one node annotated with
    /// `#[test]` (useful for detecting inline test modules in source files).
    ///
    /// Read through `rowid` keyset pages over the annotated node. A real
    /// repository annotates far more functions than the `SQLite` runtime will
    /// materialize for one query, and the runtime refuses an oversized query
    /// outright rather than truncating it. Paging drops the SQL `DISTINCT` —
    /// a page cursor and a distinct projection cannot coexist — so the same
    /// path arrives once per annotated function and the [`HashSet`] dedupes.
    pub async fn get_files_with_test_annotations(&self) -> Result<HashSet<String>> {
        let paths = collect_rowid_pages(
            &self.engine_conn(),
            TEST_ANNOTATION_FILE_PAGE_SQL,
            1,
            |row| row.get::<String>(0),
            "get_files_with_test_annotations",
        )
        .await?;
        Ok(paths.into_iter().collect())
    }

    /// Returns all node IDs whose docstring contains `skip-test-coverage`.
    ///
    /// Read through `rowid` keyset pages: the leading-wildcard `LIKE` has no
    /// index to narrow it, so the statement is a whole-`nodes` scan whose
    /// result set is bounded only by how often the marker appears.
    pub async fn get_skip_test_coverage_node_ids(&self) -> Result<HashSet<String>> {
        let ids = collect_rowid_pages(
            &self.engine_conn(),
            SKIP_TEST_COVERAGE_PAGE_SQL,
            1,
            |row| row.get::<String>(0),
            "get_skip_test_coverage_node_ids",
        )
        .await?;
        Ok(ids.into_iter().collect())
    }

    /// Resolves the set of `annotation_usage` node ids whose name marks a
    /// function as a test (`#[test]`, `#[tokio::test]`, `#[async_std::test]`,
    /// `#[wasm_bindgen_test]`, …). Runs the leading-wildcard `LIKE` scan
    /// exactly once over the `kind = 'annotation_usage'` partition.
    ///
    /// `find_dead_code` uses this in a two-step "resolve + use" pattern
    /// (push the ids into a TEMP table, then probe by id) so the LIKE never
    /// runs in a correlated subquery — the per-row degenerate plan from the
    /// reverted 4.14.8 CTE attempt timed out at >60s on scirs; the original
    /// JOIN+LIKE form times out at >25s on chromium. Both pathologies stem
    /// from re-running the wildcard scan per candidate row.
    pub async fn collect_test_marker_ids(&self) -> Result<Vec<String>> {
        let transaction = self
            .begin_write_transaction("collect test marker ids")
            .await?;
        let result = self.collect_test_marker_ids_on(&transaction).await;
        let _ = transaction.rollback().await;
        result
    }

    /// Read through `rowid` keyset pages. A real repository carries far more
    /// test markers than the `SQLite` runtime will materialize for one query,
    /// and the runtime refuses an oversized query outright rather than
    /// truncating it — which failed `tracedecay_health` with "failed to query
    /// test marker ids: … exact SQL query materialization exceeded its
    /// limit".
    ///
    /// Paging keeps the property the two-step "resolve + use" pattern depends
    /// on: each page resumes at the previous page's cursor, so the wildcard
    /// scan still crosses the `nodes` table exactly once in aggregate rather
    /// than once per candidate row.
    pub async fn collect_test_marker_ids_on(
        &self,
        conn: &DatabaseWriteTransaction<'_>,
    ) -> Result<Vec<String>> {
        collect_rowid_pages(
            conn,
            TEST_MARKER_PAGE_SQL,
            1,
            |row| row.get::<String>(0),
            "collect_test_marker_ids",
        )
        .await
    }

    /// Drops, recreates, and bulk-inserts `ids` into `temp.test_markers`.
    ///
    /// The temp table has a `PRIMARY KEY` on `id` so `SQLite` builds a real
    /// rowid B-tree — `IN (SELECT id FROM temp.test_markers)` in downstream
    /// queries probes via that index, not a wildcard scan. Inserts are
    /// chunked under `SQLite`'s 999-parameter limit.
    ///
    /// Always drops first, so a previous call on the same connection
    /// (e.g. consecutive `find_dead_code` from the same MCP client) does
    /// not collide. The caller should also drop the table when done — see
    /// `find_dead_code` for the wrapping pattern.
    pub async fn populate_test_marker_temp_table_unlocked(
        &self,
        conn: &DatabaseWriteTransaction<'_>,
        ids: &[String],
    ) -> Result<()> {
        // `SQLite`'s default parameter limit is 999. Chunk well under that.
        const CHUNK_SIZE: usize = 500;

        let op = "populate_test_marker_temp_table";
        // Drop + recreate so we always start from an empty table.
        self.drop_test_marker_temp_table_unlocked(conn).await?;
        conn.execute_engine("CREATE TEMP TABLE test_markers (id TEXT PRIMARY KEY)", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to create temp.test_markers: {e}"),
                operation: op.to_string(),
            })?;

        if ids.is_empty() {
            return Ok(());
        }

        for chunk in ids.chunks(CHUNK_SIZE) {
            let mut sql = String::from("INSERT INTO temp.test_markers (id) VALUES ");
            for i in 0..chunk.len() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push_str("(?)");
            }
            let params: Vec<Value> = chunk.iter().map(|id| Value::Text(id.clone())).collect();
            conn.execute_engine(&sql, params_from_iter(params))
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to bulk-insert test markers: {e}"),
                    operation: op.to_string(),
                })?;
        }
        Ok(())
    }

    /// Drops `temp.test_markers` if it exists. Used as cleanup by
    /// `find_dead_code` so the table does not leak to other queries on the
    /// same connection.
    ///
    /// Safe to call even if the table doesn't exist (uses `IF EXISTS`).
    pub async fn drop_test_marker_temp_table_unlocked(
        &self,
        conn: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        conn.execute_engine("DROP TABLE IF EXISTS temp.test_markers", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to drop temp.test_markers: {e}"),
                operation: "drop_test_marker_temp_table".to_string(),
            })?;
        Ok(())
    }

    /// Materialises the set of node ids that are targets of a test-marker
    /// `annotates` edge into `temp.test_annotated_targets`.
    ///
    /// This is the second step of the dead-code test-exclusion pipeline:
    /// 1. `populate_test_marker_temp_table` fills `temp.test_markers`.
    /// 2. THIS fn pre-resolves "which nodes are annotated by any test
    ///    marker" into a small lookup table with a PK on `target`.
    /// 3. `find_dead_code`'s outer SELECT then uses
    ///    `id NOT IN (SELECT target FROM temp.test_annotated_targets)` —
    ///    an indexed PK probe per candidate.
    ///
    /// Why two tables instead of `IN (SELECT id FROM temp.test_markers)`
    /// inside a correlated `NOT EXISTS`: on chromium (~13 K markers,
    /// ~134 K dead-code candidates, ~411 K annotates edges) `SQLite` picked
    /// `idx_edges_unique (source, target, kind)` for the correlated
    /// subquery, iterating every marker as the outer driver for every
    /// candidate. That's ~1.7 billion index probes and a >25 s timeout
    /// on the MCP probe. Pre-materialising the *target* set means the
    /// per-candidate probe becomes a single indexed lookup against a
    /// table with ~15 K rows. Real measurement on chromium 7.5 GB DB:
    /// 0.75 s end-to-end (vs. >60 s for the single-temp-table form).
    pub async fn populate_test_annotated_targets_temp_table_unlocked(
        &self,
        conn: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        let op = "populate_test_annotated_targets_temp_table";

        // Drop + recreate so we always start from an empty table — same
        // hygiene as the test_markers temp table.
        self.drop_test_annotated_targets_temp_table_unlocked(conn)
            .await?;
        conn.execute_engine(
            "CREATE TEMP TABLE test_annotated_targets (target TEXT PRIMARY KEY)",
            (),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to create temp.test_annotated_targets: {e}"),
            operation: op.to_string(),
        })?;

        // `INSERT OR IGNORE` because a single function can have multiple
        // test markers (e.g. `#[test] #[cfg(target_os = "linux")]`) — one
        // row per target, not per (target, marker) pair.
        conn.execute_engine(
            "INSERT OR IGNORE INTO temp.test_annotated_targets (target)
             SELECT e.target FROM edges e
             WHERE e.kind = 'annotates'
               AND e.source IN (SELECT id FROM temp.test_markers)",
            (),
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to populate temp.test_annotated_targets: {e}"),
            operation: op.to_string(),
        })?;
        Ok(())
    }

    /// Drops `temp.test_annotated_targets` if it exists. Cleanup pair for
    /// `populate_test_annotated_targets_temp_table`.
    pub async fn drop_test_annotated_targets_temp_table_unlocked(
        &self,
        conn: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        conn.execute_engine("DROP TABLE IF EXISTS temp.test_annotated_targets", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to drop temp.test_annotated_targets: {e}"),
                operation: "drop_test_annotated_targets_temp_table".to_string(),
            })?;
        Ok(())
    }
}

// `graph::queries` sits above this kernel, so the one test that drives a real
// graph query through the writer lane is gated off here and must be re-homed in
// the crate that owns `GraphQueryManager`. The whole module — helper included —
// travels with that gate so it stays warning-free until the move happens.
#[cfg(all(test, tracedecay_graph_query_tests))]
mod tests {
    use std::future::Future;
    use std::time::Duration;

    use super::super::access::DatabaseAuthority;
    use super::*;
    use crate::db::TestDatabaseRuntimeMode;
    use crate::graph::queries::GraphQueryManager;

    async fn assert_waits_for_writer<Operation, OperationFuture>(
        db: &Database,
        operation: Operation,
    ) where
        Operation: FnOnce(Database) -> OperationFuture + Send + 'static,
        OperationFuture: Future<Output = Result<()>> + Send + 'static,
    {
        let writer = db.writer().await;
        let other = db.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut task = tokio::spawn(async move {
            let _ = started_tx.send(());
            operation(other).await
        });
        started_rx.await.unwrap();

        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut task)
                .await
                .is_err(),
            "TEMP-table mutation bypassed the database writer"
        );

        drop(writer);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("TEMP-table mutation did not resume after releasing the writer")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn temp_table_lifecycle_uses_the_database_writer() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "coverage writer").unwrap();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();

        assert_waits_for_writer(&db, |db| async move {
            GraphQueryManager::new(&db)
                .find_dead_code(&[], true, None)
                .await
                .map(|_| ())
        })
        .await;
    }
}

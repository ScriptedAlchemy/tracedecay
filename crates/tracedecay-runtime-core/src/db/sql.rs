// Rust guideline compliant 2025-10-17
use crate::db::engine::{Error, Row, Value};
use crate::errors::{Result, TraceDecayError};

// ---------------------------------------------------------------------------
// Helper: build SQL placeholder string `?, ?, ?, …` in one allocation.
// ---------------------------------------------------------------------------

/// Returns a SQL placeholder string of `n` anonymous `?` markers separated by
/// `, `. Used to construct `IN ($qmarks)` clauses without allocating one
/// `String` per id (`format!("?{i}")` previously did that).
pub fn build_qmark_placeholders(n: usize) -> String {
    debug_assert!(n > 0, "build_qmark_placeholders called with n == 0");
    // Each "?, " occupies 3 bytes; the last one drops the trailing ", ".
    let mut s = String::with_capacity(n * 3);
    for i in 0..n {
        if i > 0 {
            s.push_str(", ");
        }
        s.push('?');
    }
    s
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Rows requested per keyset page of a whole-table scan.
///
/// The `SQLite` runtime admits a bounded number of rows per query and rejects
/// anything larger outright, so a whole-table read has to arrive as a sequence
/// of pages. This budget stays well under that admission limit while keeping
/// the number of round trips low.
pub const FULL_SCAN_PAGE_ROWS: i64 = 2_000;

/// Reads an entire table through `rowid` keyset pages and returns every row.
///
/// `page_sql` must append `rowid` after the columns `map_fn` reads, bind the
/// exclusive `rowid` cursor to `?1` and the page row budget to `?2`, and order
/// by `rowid` so each page resumes exactly where the previous one stopped.
/// `cursor_index` is the position of that trailing `rowid` column. The result
/// is the complete scan: paging is a transport concern here, never a silent
/// truncation.
pub async fn collect_rowid_pages<T, C>(
    conn: &C,
    page_sql: &str,
    cursor_index: i32,
    map_fn: fn(&Row) -> std::result::Result<T, Error>,
    operation: &str,
) -> Result<Vec<T>>
where
    C: crate::db::engine::QueryExecutor + ?Sized,
{
    collect_rowid_pages_with(conn, page_sql, &[], cursor_index, map_fn, operation).await
}

/// Like [`collect_rowid_pages`], but for a filtered scan that also binds its own
/// parameters.
///
/// `leading_params` are bound first as `?1..?N`; `page_sql` must then bind the
/// exclusive `rowid` cursor to `?{N+1}` and the page row budget to `?{N+2}`.
/// Everything else matches [`collect_rowid_pages`]: `page_sql` appends `rowid`
/// after the columns `map_fn` reads, orders by `rowid`, and `cursor_index` is
/// the position of that trailing cursor column.
///
/// A `WHERE` clause is not a bound. A partition of a graph table — one node
/// kind, one edge kind, one path prefix — routinely holds far more rows on a
/// real repository than the `SQLite` runtime will materialize for one query,
/// and the runtime refuses an oversized query outright rather than truncating
/// it. Filtered whole-partition reads therefore have to page exactly like
/// whole-table reads do.
pub async fn collect_rowid_pages_with<T, C>(
    conn: &C,
    page_sql: &str,
    leading_params: &[Value],
    cursor_index: i32,
    map_fn: fn(&Row) -> std::result::Result<T, Error>,
    operation: &str,
) -> Result<Vec<T>>
where
    C: crate::db::engine::QueryExecutor + ?Sized,
{
    collect_rowid_pages_with_controlled(
        conn,
        page_sql,
        leading_params,
        cursor_index,
        map_fn,
        operation,
        || Ok(()),
    )
    .await
}

/// [`collect_rowid_pages_with`] with one cooperative checkpoint around every
/// bounded query page.
pub async fn collect_rowid_pages_with_controlled<T, C, F>(
    conn: &C,
    page_sql: &str,
    leading_params: &[Value],
    cursor_index: i32,
    map_fn: fn(&Row) -> std::result::Result<T, Error>,
    operation: &str,
    mut checkpoint: F,
) -> Result<Vec<T>>
where
    C: crate::db::engine::QueryExecutor + ?Sized,
    F: FnMut() -> Result<()>,
{
    let mut items = Vec::new();
    let mut after_rowid = i64::MIN;
    loop {
        checkpoint()?;
        let mut page_params: Vec<Value> = Vec::with_capacity(leading_params.len() + 2);
        page_params.extend_from_slice(leading_params);
        page_params.push(Value::Integer(after_rowid));
        page_params.push(Value::Integer(FULL_SCAN_PAGE_ROWS));
        let mut rows = conn
            .query(page_sql, crate::db::engine::params_from_iter(page_params))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query page: {e}"),
                operation: operation.to_string(),
            })?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: operation.to_string(),
        })? {
            let rowid: i64 = row
                .get(cursor_index)
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read page cursor: {e}"),
                    operation: operation.to_string(),
                })?;
            if rowid <= after_rowid {
                return Err(TraceDecayError::Database {
                    message: "table scan page did not advance".to_string(),
                    operation: operation.to_string(),
                });
            }
            after_rowid = rowid;
            page_rows += 1;
            items.push(map_fn(&row).map_err(|e| TraceDecayError::Database {
                message: format!("failed to map row: {e}"),
                operation: operation.to_string(),
            })?);
        }
        checkpoint()?;
        if page_rows < FULL_SCAN_PAGE_ROWS {
            return Ok(items);
        }
    }
}

/// Outcome of a cap-preserving keyset scan.
///
/// `exceeded` is the whole point: it is proven by reading one row past `cap`,
/// never inferred from a truncated `items`. When `exceeded` is `false`, `items`
/// is the complete set and may be used as a measurement; when it is `true`,
/// `items` holds exactly `cap` rows and is only good for reporting that the
/// budget was passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedRowidScan<T> {
    /// Rows read, never more than the requested cap.
    pub items: Vec<T>,
    /// Whether at least one row beyond the cap exists.
    pub exceeded: bool,
}

/// Reads a table through `rowid` keyset pages, stopping as soon as one row past
/// `cap` has been seen.
///
/// A bounded read that proves its own budget with a single
/// `… LIMIT cap + 1` statement does not work here: the `SQLite` runtime refuses
/// any query that materializes more than its per-query row limit outright, so a
/// budget larger than that limit turns the budget check itself into a hard
/// failure at exactly the scale it exists to protect. Paging fixes that, but a
/// naive paged rewrite reintroduces the opposite defect — reading the whole
/// table just to discover it was too large, or worse, silently returning a
/// truncated result that reads as a complete measurement.
///
/// This helper keeps both properties: every query stays within one page, and
/// the scan stops at `cap + 1` rows with [`CappedRowidScan::exceeded`] set.
///
/// `page_sql` has the same shape [`collect_rowid_pages_with`] requires:
/// `leading_params` bind first as `?1..?N`, then the exclusive `rowid` cursor
/// and the page row budget, with `rowid` appended after the columns `map_fn`
/// reads at position `cursor_index` and an `ORDER BY rowid`.
pub async fn collect_rowid_pages_capped_with<T, C>(
    conn: &C,
    page_sql: &str,
    leading_params: &[Value],
    cursor_index: i32,
    map_fn: fn(&Row) -> std::result::Result<T, Error>,
    operation: &str,
    cap: usize,
) -> Result<CappedRowidScan<T>>
where
    C: crate::db::engine::QueryExecutor + ?Sized,
{
    let mut items: Vec<T> = Vec::new();
    let mut after_rowid = i64::MIN;
    loop {
        // Never request more than one page, and never more than the single row
        // past the cap that the over-budget proof needs.
        let remaining = cap.saturating_sub(items.len()).saturating_add(1);
        let page_budget = i64::try_from(remaining)
            .unwrap_or(FULL_SCAN_PAGE_ROWS)
            .min(FULL_SCAN_PAGE_ROWS);
        let mut page_params: Vec<Value> = Vec::with_capacity(leading_params.len() + 2);
        page_params.extend_from_slice(leading_params);
        page_params.push(Value::Integer(after_rowid));
        page_params.push(Value::Integer(page_budget));
        let mut rows = conn
            .query(page_sql, crate::db::engine::params_from_iter(page_params))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query capped page: {e}"),
                operation: operation.to_string(),
            })?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read capped row: {e}"),
            operation: operation.to_string(),
        })? {
            let rowid: i64 = row
                .get(cursor_index)
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read capped page cursor: {e}"),
                    operation: operation.to_string(),
                })?;
            if rowid <= after_rowid {
                return Err(TraceDecayError::Database {
                    message: "capped table scan page did not advance".to_string(),
                    operation: operation.to_string(),
                });
            }
            after_rowid = rowid;
            page_rows += 1;
            if items.len() == cap {
                // The row past the cap proves the budget was exceeded. It is
                // deliberately not collected: `items` stays exactly `cap` long.
                return Ok(CappedRowidScan {
                    items,
                    exceeded: true,
                });
            }
            items.push(map_fn(&row).map_err(|e| TraceDecayError::Database {
                message: format!("failed to map capped row: {e}"),
                operation: operation.to_string(),
            })?);
        }
        if page_rows < page_budget {
            return Ok(CappedRowidScan {
                items,
                exceeded: false,
            });
        }
    }
}

/// [`collect_rowid_pages_capped_with`] for a scan that binds no parameters of
/// its own.
pub async fn collect_rowid_pages_capped<T, C>(
    conn: &C,
    page_sql: &str,
    cursor_index: i32,
    map_fn: fn(&Row) -> std::result::Result<T, Error>,
    operation: &str,
    cap: usize,
) -> Result<CappedRowidScan<T>>
where
    C: crate::db::engine::QueryExecutor + ?Sized,
{
    collect_rowid_pages_capped_with(conn, page_sql, &[], cursor_index, map_fn, operation, cap).await
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{FULL_SCAN_PAGE_ROWS, collect_rowid_pages};
    use crate::db::engine::TestConnection;

    /// A single query over this many rows is refused by the `SQLite` runtime, so
    /// a whole-table read has to page. The scan must still return every row,
    /// exactly once, in `rowid` order.
    #[tokio::test]
    async fn rowid_pages_complete_a_scan_larger_than_the_runtime_limit() {
        const ROWS: i64 = 10_001;
        let directory = TempDir::new().expect("scan tempdir");
        let conn = TestConnection::open(&directory.path().join("scan.db"));
        conn.execute_batch("CREATE TABLE sample (label TEXT NOT NULL);")
            .await
            .expect("create table");
        conn.execute(
            &format!(
                "WITH RECURSIVE fixture(value) AS (
                     SELECT 1 UNION ALL SELECT value + 1 FROM fixture WHERE value < {ROWS}
                 )
                 INSERT INTO sample(label) SELECT printf('row-%05d', value) FROM fixture"
            ),
            (),
        )
        .await
        .expect("seed rows");

        let labels = collect_rowid_pages(
            &*conn,
            "SELECT label, rowid FROM sample WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            1,
            |row| row.get::<String>(0),
            "sample_scan",
        )
        .await
        .expect("a paged scan must not exceed the runtime materialization limit");

        assert!(
            std::hint::black_box(ROWS) > FULL_SCAN_PAGE_ROWS,
            "the fixture must span pages"
        );
        assert_eq!(i64::try_from(labels.len()).expect("row count"), ROWS);
        assert_eq!(labels.first().map(String::as_str), Some("row-00001"));
        assert_eq!(labels.last().map(String::as_str), Some("row-10001"));
    }
}

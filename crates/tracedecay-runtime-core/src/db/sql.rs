// Rust guideline compliant 2025-10-17
use crate::db::engine::{Error, Row, Rows, Value};
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

/// Converts `Option<String>` to an engine [`Value`] for use in parameters.
pub(super) fn opt_str(opt: Option<&str>) -> Value {
    match opt {
        Some(s) => Value::Text(s.to_string()),
        None => Value::Null,
    }
}

/// Builds a bound-parameter value for literal path-prefix `LIKE` filters.
///
/// Keep caller-provided prefixes out of SQL text. The `%` suffix is the only
/// wildcard added by query helpers; quotes, comments, and semicolons inside the
/// prefix stay plain data when bound through `SQLite` parameters.
pub(super) fn path_prefix_like_value(prefix: &str) -> Value {
    Value::Text(format!("{prefix}%"))
}

/// Appends a SQL-safe single-quoted string literal to `buf`, escaping `'` as `''`.
///
/// This is only for bulk value literals in `execute_batch` paths. Do not use it
/// for identifiers, column names, table names, predicates, or new dynamic query
/// surfaces; prefer prepared statements and bound parameters whenever possible.
pub(super) fn push_quoted(buf: &mut String, s: &str) {
    buf.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            buf.push_str("''");
        } else {
            buf.push(ch);
        }
    }
    buf.push('\'');
}

/// Appends a SQL-safe quoted string literal or NULL for Option<String>.
pub(super) fn push_opt_quoted(buf: &mut String, opt: Option<&str>) {
    match opt {
        Some(s) => push_quoted(buf, s),
        None => buf.push_str("NULL"),
    }
}

/// Appends an integer literal to the buffer.
pub(super) fn push_int(buf: &mut String, val: i64) {
    use std::fmt::Write;
    let _ = write!(buf, "{val}");
}

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
    let mut items = Vec::new();
    let mut after_rowid = i64::MIN;
    loop {
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
        if page_rows < FULL_SCAN_PAGE_ROWS {
            return Ok(items);
        }
    }
}

/// Collects all rows from a `Rows` iterator into a `Vec<T>` using the given
/// row-mapping function. This helper never constructs SQL; callers must build
/// and parameterize queries before invoking it.
pub(super) async fn collect_rows<T>(
    rows: &mut Rows,
    map_fn: fn(&Row) -> std::result::Result<T, Error>,
    operation: &str,
) -> Result<Vec<T>> {
    let mut items = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
        message: format!("failed to read row: {e}"),
        operation: operation.to_string(),
    })? {
        items.push(map_fn(&row).map_err(|e| TraceDecayError::Database {
            message: format!("failed to map row: {e}"),
            operation: operation.to_string(),
        })?);
    }
    Ok(items)
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

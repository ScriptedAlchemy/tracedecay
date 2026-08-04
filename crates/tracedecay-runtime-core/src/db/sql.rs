// Rust guideline compliant 2025-10-17
use crate::errors::{Result, TraceDecayError};

// ---------------------------------------------------------------------------
// Helper: build SQL placeholder string `?, ?, ?, …` in one allocation.
// ---------------------------------------------------------------------------

/// Returns a SQL placeholder string of `n` anonymous `?` markers separated by
/// `, `. Used to construct `IN ($qmarks)` clauses without allocating one
/// `String` per id (`format!("?{i}")` previously did that).
pub(super) fn build_qmark_placeholders(n: usize) -> String {
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

/// Converts `Option<String>` to a `libsql::Value` for use in params.
pub(super) fn opt_str(opt: Option<&str>) -> libsql::Value {
    match opt {
        Some(s) => libsql::Value::Text(s.to_string()),
        None => libsql::Value::Null,
    }
}

/// Builds a bound-parameter value for literal path-prefix `LIKE` filters.
///
/// Keep caller-provided prefixes out of SQL text. The `%` suffix is the only
/// wildcard added by query helpers; quotes, comments, and semicolons inside the
/// prefix stay plain data when bound through libSQL parameters.
pub(super) fn path_prefix_like_value(prefix: &str) -> libsql::Value {
    libsql::Value::Text(format!("{prefix}%"))
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

/// Collects all rows from a `Rows` iterator into a `Vec<T>` using the given
/// row-mapping function. This helper never constructs SQL; callers must build
/// and parameterize queries before invoking it.
pub(super) async fn collect_rows<T>(
    rows: &mut libsql::Rows,
    map_fn: fn(&libsql::Row) -> std::result::Result<T, libsql::Error>,
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

//! Small SQL→JSON helpers shared by the dashboard API handlers.
//!
//! The original Hermes plugin APIs are thin Python layers that run SQL and
//! return row dicts; these helpers reproduce that style (`rows_to_json` is
//! the moral equivalent of `_rowdict`) so the endpoint ports stay close to
//! their reference implementations.

use libsql::{Connection, Rows, Value as DbValue};
use serde_json::{json, Map, Number, Value};

pub(crate) fn db_value_to_json(value: DbValue) -> Value {
    match value {
        DbValue::Null | DbValue::Blob(_) => Value::Null,
        DbValue::Integer(i) => Value::Number(i.into()),
        DbValue::Real(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
        DbValue::Text(s) => Value::String(s),
    }
}

/// Drains `rows` into an array of `{column_name: value}` objects.
pub(crate) async fn collect_rows(mut rows: Rows) -> std::result::Result<Vec<Value>, libsql::Error> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let mut obj = Map::new();
        for idx in 0..rows.column_count() {
            let name = rows
                .column_name(idx)
                .map_or_else(|| format!("col{idx}"), ToOwned::to_owned);
            let value = row.get_value(idx).unwrap_or(DbValue::Null);
            obj.insert(name, db_value_to_json(value));
        }
        out.push(Value::Object(obj));
    }
    Ok(out)
}

/// Runs a query and collects all rows as JSON objects. On SQL errors returns
/// the error message so handlers can surface it in the payload's `error`
/// field (mirroring the Python APIs, which never 500 on a bad/missing DB).
pub(crate) async fn query_rows(
    conn: &Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> std::result::Result<Vec<Value>, String> {
    let rows = conn.query(sql, params).await.map_err(|e| e.to_string())?;
    collect_rows(rows).await.map_err(|e| e.to_string())
}

/// Runs a scalar `SELECT COUNT(*)`-style query; errors and missing rows
/// collapse to 0 (these feed overview cards, not critical paths).
pub(crate) async fn query_i64(
    conn: &Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> i64 {
    let Ok(mut rows) = conn.query(sql, params).await else {
        return 0;
    };
    match rows.next().await {
        Ok(Some(row)) => row.get::<i64>(0).unwrap_or(0),
        _ => 0,
    }
}

/// Clamps a user-supplied limit (mirrors `_coerce_limit` in the Python APIs).
pub(crate) fn coerce_limit(value: Option<i64>, default: i64, maximum: i64) -> i64 {
    value.unwrap_or(default).clamp(1, maximum)
}

/// Escapes `%`/`_`/`\` for a `LIKE ? ESCAPE '\'` pattern.
pub(crate) fn like_pattern(query: &str) -> String {
    let escaped = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// Builds a safe FTS5 MATCH expression from raw user text (port of
/// `_build_fts_match` in the hermes-lcm plugin API). Returns `None` when no
/// usable token remains, in which case callers fall back to LIKE.
pub(crate) fn build_fts_match(raw: &str) -> Option<String> {
    let mut tokens: Vec<String> = Vec::new();
    for chunk in raw.split_whitespace() {
        let cleaned: String = chunk.chars().filter(|c| *c != '"').collect();
        if !cleaned.chars().any(char::is_alphanumeric) {
            continue;
        }
        tokens.push(cleaned);
    }
    let last = tokens.len().checked_sub(1)?;
    let quoted: Vec<String> = tokens
        .iter()
        .enumerate()
        .map(|(i, tok)| {
            if i == last {
                format!("\"{tok}\"*")
            } else {
                format!("\"{tok}\"")
            }
        })
        .collect();
    Some(quoted.join(" "))
}

/// JSON error body matching `FastAPI`'s `HTTPException` shape, which the UIs'
/// error paths already understand.
pub(crate) fn http_detail(detail: &str) -> Value {
    json!({ "detail": detail })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_match_quotes_tokens_and_prefixes_last() {
        assert_eq!(
            build_fts_match("hello world").as_deref(),
            Some("\"hello\" \"world\"*")
        );
        assert_eq!(
            build_fts_match("a-b c:d").as_deref(),
            Some("\"a-b\" \"c:d\"*")
        );
        assert_eq!(build_fts_match("-- !!"), None);
        assert_eq!(build_fts_match(""), None);
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("a%b_c"), "%a\\%b\\_c%");
    }

    #[test]
    fn coerce_limit_clamps() {
        assert_eq!(coerce_limit(None, 25, 100), 25);
        assert_eq!(coerce_limit(Some(0), 25, 100), 1);
        assert_eq!(coerce_limit(Some(500), 25, 100), 100);
    }

    #[allow(clippy::unwrap_used)]
    async fn test_conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        db.connect().unwrap()
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn query_rows_returns_named_json_objects() {
        let conn = test_conn().await;
        conn.execute(
            "CREATE TABLE t (id INTEGER, name TEXT, score REAL, data BLOB)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO t VALUES (1, 'alpha', 0.5, X'00'), (2, NULL, NULL, NULL)",
            (),
        )
        .await
        .unwrap();

        let rows = query_rows(&conn, "SELECT id, name, score, data FROM t ORDER BY id", ())
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], 1);
        assert_eq!(rows[0]["name"], "alpha");
        assert_eq!(rows[0]["score"], 0.5);
        // Blobs (like NULLs) collapse to JSON null per db_value_to_json.
        assert!(rows[0]["data"].is_null());
        assert!(rows[1]["name"].is_null());
        assert_eq!(rows[1]["id"], 2);
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn query_rows_binds_params_and_reports_sql_errors() {
        let conn = test_conn().await;
        conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", ())
            .await
            .unwrap();
        conn.execute("INSERT INTO t VALUES (1, 'a'), (2, 'b')", ())
            .await
            .unwrap();

        let rows = query_rows(
            &conn,
            "SELECT name FROM t WHERE id = ?1",
            libsql::params![2],
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "b");

        // SQL errors come back as Err(message) so handlers can surface them
        // in the payload instead of panicking or returning a 500.
        let err = query_rows(&conn, "SELECT * FROM missing_table", ()).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("missing_table"));
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn query_i64_returns_scalar_and_collapses_failures_to_zero() {
        let conn = test_conn().await;
        conn.execute("CREATE TABLE c (v INTEGER)", ())
            .await
            .unwrap();
        conn.execute("INSERT INTO c VALUES (7), (8)", ())
            .await
            .unwrap();

        assert_eq!(query_i64(&conn, "SELECT COUNT(*) FROM c", ()).await, 2);
        assert_eq!(
            query_i64(&conn, "SELECT v FROM c WHERE v = ?1", libsql::params![7]).await,
            7
        );
        // Bad SQL and empty result sets both collapse to 0 (overview-card semantics).
        assert_eq!(
            query_i64(&conn, "SELECT COUNT(*) FROM missing", ()).await,
            0
        );
        assert_eq!(
            query_i64(&conn, "SELECT v FROM c WHERE v = 999", ()).await,
            0
        );
    }
}

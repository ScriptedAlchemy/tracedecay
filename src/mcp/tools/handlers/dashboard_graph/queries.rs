//! Canonical code-graph topology reads backing the dashboard graph read port.
//!
//! These are the exact bounded SQL projections the pre-cutover dashboard
//! graph service ran; the daemon adapter owns them now so HTTP adapters only
//! ever receive complete typed read models. Every failure is preserved as a
//! failed read — never an empty result.

use serde_json::{Map, Number, Value};

use tracedecay_runtime_core::db::build_qmark_placeholders;
use tracedecay_runtime_core::db::engine::{
    IntoParams, QueryExecutor, Rows, Value as DbValue, params, params_from_iter,
};

pub(super) type GraphReadResult<T> = std::result::Result<T, String>;

pub(super) const NODE_COLUMNS: &str = "id, kind, name, qualified_name, file_path,
       start_line, end_line, start_column, end_column, attrs_start_line,
       docstring AS doc, signature, visibility, is_async,
       branches, loops, returns, max_nesting, unsafe_blocks,
       unchecked_calls, assertions, updated_at, parent_id";

/// `NODE_COLUMNS` qualified with the `n.` alias for joined queries
/// (`edges e JOIN nodes n ...`), where bare `id`/`kind` would be ambiguous
/// between the two tables.
pub(super) const NODE_COLUMNS_N: &str = "n.id, n.kind, n.name, n.qualified_name, n.file_path,
       n.start_line, n.end_line, n.start_column, n.end_column, n.attrs_start_line,
       n.docstring AS doc, n.signature, n.visibility, n.is_async,
       n.branches, n.loops, n.returns, n.max_nesting, n.unsafe_blocks,
       n.unchecked_calls, n.assertions, n.updated_at, n.parent_id";

const ALL_DEGREE_UNION_SQL: &str = "SELECT source AS node_id FROM edges
             UNION ALL
             SELECT target AS node_id FROM edges";

fn filtered_degree_union_sql(placeholders: &str) -> String {
    format!(
        "SELECT source AS node_id FROM edges WHERE source IN ({placeholders})
         UNION ALL
         SELECT target AS node_id FROM edges WHERE target IN ({placeholders})"
    )
}

fn db_value_to_json(value: DbValue) -> Value {
    match value {
        DbValue::Null | DbValue::Blob(_) => Value::Null,
        DbValue::Integer(i) => Value::Number(i.into()),
        DbValue::Real(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
        DbValue::Text(s) => Value::String(s),
    }
}

async fn collect_rows(
    mut rows: Rows,
) -> std::result::Result<Vec<Value>, tracedecay_runtime_core::db::engine::Error> {
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let mut object = Map::new();
        for idx in 0..rows.column_count() {
            let name = rows
                .column_name(idx)
                .map_or_else(|| format!("col{idx}"), ToOwned::to_owned);
            let value = row.get::<DbValue>(idx).unwrap_or(DbValue::Null);
            object.insert(name, db_value_to_json(value));
        }
        out.push(Value::Object(object));
    }
    Ok(out)
}

async fn query_rows(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    params: impl IntoParams,
) -> GraphReadResult<Vec<Value>> {
    let rows = conn.query(sql, params).await.map_err(|e| e.to_string())?;
    collect_rows(rows).await.map_err(|e| e.to_string())
}

/// Runs a scalar integer query while preserving SQL, row-iteration, empty-row,
/// and conversion failures for read models where zero carries domain meaning.
async fn query_i64_result(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    params: impl IntoParams,
) -> GraphReadResult<i64> {
    let mut rows = conn.query(sql, params).await.map_err(|e| e.to_string())?;
    let row = rows
        .next()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "scalar query returned no rows".to_string())?;
    row.get::<i64>(0).map_err(|e| e.to_string())
}

/// Escapes `%`/`_`/`\` for a `LIKE ? ESCAPE '\'` pattern.
fn like_pattern(query: &str) -> String {
    let escaped = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

pub(super) async fn total_nodes(conn: &(impl QueryExecutor + ?Sized)) -> GraphReadResult<i64> {
    query_i64_result(conn, "SELECT COUNT(*) FROM nodes", ()).await
}

pub(super) async fn total_edges(conn: &(impl QueryExecutor + ?Sized)) -> GraphReadResult<i64> {
    query_i64_result(conn, "SELECT COUNT(*) FROM edges", ()).await
}

pub(super) async fn max_edge_id(conn: &(impl QueryExecutor + ?Sized)) -> GraphReadResult<i64> {
    query_i64_result(conn, "SELECT COALESCE(MAX(id), 0) FROM edges", ()).await
}

pub(super) async fn total_files(conn: &(impl QueryExecutor + ?Sized)) -> GraphReadResult<i64> {
    query_i64_result(conn, "SELECT COUNT(*) FROM files", ()).await
}

pub(super) async fn last_node_update(conn: &(impl QueryExecutor + ?Sized)) -> GraphReadResult<i64> {
    query_i64_result(conn, "SELECT COALESCE(MAX(updated_at), 0) FROM nodes", ()).await
}

pub(super) async fn first_node_for_query(
    conn: &(impl QueryExecutor + ?Sized),
    query: &str,
) -> GraphReadResult<Option<String>> {
    let trimmed = query.trim();
    let like = like_pattern(trimmed);
    let rows = query_rows(
        conn,
        "SELECT id
         FROM nodes
         WHERE name LIKE ?1 ESCAPE '\\'
            OR qualified_name LIKE ?1 ESCAPE '\\'
         ORDER BY CASE WHEN name = ?2 THEN 0 ELSE 1 END,
                  LENGTH(qualified_name) ASC,
                  qualified_name ASC
         LIMIT 1",
        params![like, trimmed],
    )
    .await?;
    Ok(rows
        .first()
        .and_then(|row| row.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

pub(super) async fn search_total(
    conn: &(impl QueryExecutor + ?Sized),
    query: &str,
) -> GraphReadResult<i64> {
    if query.is_empty() {
        total_nodes(conn).await
    } else {
        let like = like_pattern(query);
        query_i64_result(
            conn,
            "SELECT COUNT(*)
             FROM nodes
             WHERE name LIKE ?1 ESCAPE '\\'
                OR qualified_name LIKE ?1 ESCAPE '\\'
                OR COALESCE(signature, '') LIKE ?1 ESCAPE '\\'
                OR file_path LIKE ?1 ESCAPE '\\'",
            params![like],
        )
        .await
    }
}

pub(super) async fn search_rows(
    conn: &(impl QueryExecutor + ?Sized),
    query: &str,
    limit: i64,
    offset: i64,
) -> GraphReadResult<Vec<Value>> {
    if query.is_empty() {
        query_rows(
            conn,
            &format!(
                "SELECT {NODE_COLUMNS}
                 FROM nodes
                 ORDER BY updated_at DESC, qualified_name ASC
                 LIMIT ?1 OFFSET ?2"
            ),
            params![limit, offset],
        )
        .await
    } else {
        let like = like_pattern(query);
        query_rows(
            conn,
            &format!(
                "SELECT {NODE_COLUMNS}
                 FROM nodes
                 WHERE name LIKE ?1 ESCAPE '\\'
                    OR qualified_name LIKE ?1 ESCAPE '\\'
                    OR COALESCE(signature, '') LIKE ?1 ESCAPE '\\'
                    OR file_path LIKE ?1 ESCAPE '\\'
                 ORDER BY CASE
                    WHEN name = ?2 THEN 0
                    WHEN qualified_name = ?2 THEN 1
                    WHEN name LIKE ?1 ESCAPE '\\' THEN 2
                    ELSE 3
                 END,
                 LENGTH(qualified_name) ASC,
                 qualified_name ASC
                 LIMIT ?3 OFFSET ?4"
            ),
            params![like, query, limit, offset],
        )
        .await
    }
}

pub(super) async fn node_rows_by_ids(
    conn: &(impl QueryExecutor + ?Sized),
    ids: &[String],
) -> GraphReadResult<Vec<Value>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = build_qmark_placeholders(ids.len());
    let sql = format!(
        "SELECT {NODE_COLUMNS}
         FROM nodes
         WHERE id IN ({placeholders})"
    );
    let params = ids.iter().cloned().map(DbValue::Text);
    query_rows(conn, &sql, params_from_iter(params)).await
}

pub(super) async fn edge_rows_for_ids(
    conn: &(impl QueryExecutor + ?Sized),
    ids: &[String],
    limit: i64,
) -> GraphReadResult<Vec<Value>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = build_qmark_placeholders(ids.len());
    // One row per (source, target, kind): the edges table stores one row per
    // call site, and duplicates would only burn the edge cap (the canvas
    // dedups by that key anyway).
    let sql = format!(
        "SELECT source, target, kind, MIN(line) AS line
         FROM edges
         WHERE source IN ({placeholders}) AND target IN ({placeholders})
         GROUP BY source, target, kind
         ORDER BY kind ASC, source ASC, target ASC
         LIMIT ?"
    );
    let mut params: Vec<DbValue> = ids.iter().cloned().map(DbValue::Text).collect();
    params.extend(ids.iter().cloned().map(DbValue::Text));
    params.push(DbValue::Integer(limit));
    query_rows(conn, &sql, params_from_iter(params)).await
}

pub(super) async fn degree_rows_for_ids(
    conn: &(impl QueryExecutor + ?Sized),
    ids: &[String],
) -> GraphReadResult<Vec<Value>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = build_qmark_placeholders(ids.len());
    let degree_union = filtered_degree_union_sql(&placeholders);
    let sql = format!(
        "SELECT node_id, COUNT(*) AS degree
         FROM ({degree_union})
         GROUP BY node_id"
    );
    let mut params: Vec<DbValue> = ids.iter().cloned().map(DbValue::Text).collect();
    params.extend(ids.iter().cloned().map(DbValue::Text));
    query_rows(conn, &sql, params_from_iter(params)).await
}

pub(super) async fn degree_pool_rows(
    conn: &(impl QueryExecutor + ?Sized),
    limit: i64,
) -> GraphReadResult<Vec<Value>> {
    query_rows(
        conn,
        &format!(
            "SELECT n.id, COALESCE(d.degree, 0) AS degree
             FROM nodes n
             LEFT JOIN (
                 SELECT node_id, COUNT(*) AS degree
                 FROM ({ALL_DEGREE_UNION_SQL})
                 GROUP BY node_id
             ) d ON d.node_id = n.id
             ORDER BY degree DESC, n.qualified_name ASC
             LIMIT ?1"
        ),
        params![limit],
    )
    .await
}

pub(super) async fn top_connected_rows(
    conn: &(impl QueryExecutor + ?Sized),
) -> GraphReadResult<Vec<Value>> {
    query_rows(
        conn,
        &format!(
            "SELECT {NODE_COLUMNS_N}, d.degree
             FROM (
                 SELECT node_id, COUNT(*) AS degree
                 FROM ({ALL_DEGREE_UNION_SQL})
                 GROUP BY node_id
                 ORDER BY degree DESC
                 LIMIT 12
             ) d
             JOIN nodes n ON n.id = d.node_id
             ORDER BY d.degree DESC, n.qualified_name ASC"
        ),
        (),
    )
    .await
}

pub(super) async fn node_row(
    conn: &(impl QueryExecutor + ?Sized),
    node_id: &str,
) -> GraphReadResult<Option<Value>> {
    Ok(query_rows(
        conn,
        &format!("SELECT {NODE_COLUMNS} FROM nodes WHERE id = ?1 LIMIT 1"),
        params![node_id],
    )
    .await?
    .into_iter()
    .next())
}

/// Node rows for neighbor hydration, selected by qualified name. Adjacency for
/// the neighborhood read comes from the verified code graph projection; this
/// lookup only maps projection symbols back onto the relational id-space the
/// other read operations still serve.
///
/// The qualified name is a SELECTION key here, not an identity: it is not
/// unique, so the caller narrows each result by kind and refuses a key that
/// still matches more than one row. This whole lookup is provisional. Once
/// `nodes.symbol_occurrence_id` lands, the projection's occurrence joins the
/// node table directly and this name-keyed query is superseded — the
/// occurrence is the identity, the qualified name never was.
pub(super) async fn node_rows_by_qualified_names(
    conn: &(impl QueryExecutor + ?Sized),
    qualified_names: &[String],
) -> GraphReadResult<Vec<Value>> {
    if qualified_names.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = build_qmark_placeholders(qualified_names.len());
    let sql = format!(
        "SELECT {NODE_COLUMNS}
         FROM nodes
         WHERE qualified_name IN ({placeholders})
         ORDER BY qualified_name ASC, id ASC"
    );
    let params = qualified_names.iter().cloned().map(DbValue::Text);
    query_rows(conn, &sql, params_from_iter(params)).await
}

pub(super) async fn subgraph_candidate_rows(
    conn: &(impl QueryExecutor + ?Sized),
    seed_id: &str,
) -> GraphReadResult<Vec<Value>> {
    query_rows(
        conn,
        "SELECT id, MIN(rank) AS rank
         FROM (
             SELECT ?1 AS id, 0 AS rank
             UNION ALL SELECT source AS id, 1 AS rank FROM edges WHERE target = ?1
             UNION ALL SELECT target AS id, 2 AS rank FROM edges WHERE source = ?1
         )
         GROUP BY id
         ORDER BY rank ASC, id ASC",
        params![seed_id],
    )
    .await
}

pub(super) async fn frontier_edge_rows(
    conn: &(impl QueryExecutor + ?Sized),
    frontier: &[String],
) -> GraphReadResult<Vec<Value>> {
    if frontier.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = build_qmark_placeholders(frontier.len());
    let sql = format!(
        "SELECT source, target, kind, line FROM edges
         WHERE source IN ({placeholders}) OR target IN ({placeholders})"
    );
    let mut bind: Vec<DbValue> = frontier.iter().cloned().map(DbValue::Text).collect();
    bind.extend(frontier.iter().cloned().map(DbValue::Text));
    query_rows(conn, &sql, params_from_iter(bind)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("a%b_c"), "%a\\%b\\_c%");
    }

    #[allow(clippy::unwrap_used)]
    fn test_conn() -> (
        tempfile::TempDir,
        tracedecay_runtime_core::db::engine::TestConnection,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let connection = tracedecay_runtime_core::db::engine::TestConnection::open(
            &directory.path().join("dashboard-graph-queries.db"),
        );
        (directory, connection)
    }

    #[tokio::test]
    async fn failed_search_rows_are_not_reported_as_empty() {
        let (_directory, conn) = test_conn();

        let rows = search_rows(&conn, "missing", 10, 0).await;

        assert!(rows.is_err(), "query failure should remain a failed read");
    }

    #[tokio::test]
    async fn failed_node_lookup_is_not_reported_as_absent() {
        let (_directory, conn) = test_conn();

        let node = node_row(&conn, "missing").await;

        assert!(node.is_err(), "query failure should remain a failed read");
    }

    #[tokio::test]
    async fn failed_node_count_is_not_reported_as_zero() {
        let (_directory, conn) = test_conn();

        let total = total_nodes(&conn).await;

        assert!(total.is_err(), "query failure should remain a failed read");
    }
}

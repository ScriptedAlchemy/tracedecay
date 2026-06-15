use libsql::Connection;
use serde_json::Value;

use super::util::{like_pattern, qmarks, query_i64, query_rows};

pub(crate) const NODE_COLUMNS: &str = "id, kind, name, qualified_name, file_path,
       start_line, end_line, start_column, end_column, attrs_start_line,
       docstring AS doc, signature, visibility, is_async,
       branches, loops, returns, max_nesting, unsafe_blocks,
       unchecked_calls, assertions, updated_at, parent_id";

/// `NODE_COLUMNS` qualified with the `n.` alias for joined queries
/// (`edges e JOIN nodes n ...`), where bare `id`/`kind` would be ambiguous
/// between the two tables.
pub(crate) const NODE_COLUMNS_N: &str = "n.id, n.kind, n.name, n.qualified_name, n.file_path,
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

pub(crate) async fn overview_file_rows(conn: &Connection) -> Vec<Value> {
    query_rows(
        conn,
        "SELECT path, node_count FROM files ORDER BY path ASC",
        (),
    )
    .await
    .unwrap_or_default()
}

pub(crate) async fn total_nodes(conn: &Connection) -> i64 {
    query_i64(conn, "SELECT COUNT(*) FROM nodes", ()).await
}

pub(crate) async fn total_edges(conn: &Connection) -> i64 {
    query_i64(conn, "SELECT COUNT(*) FROM edges", ()).await
}

pub(crate) async fn total_files(conn: &Connection) -> i64 {
    query_i64(conn, "SELECT COUNT(*) FROM files", ()).await
}

pub(crate) async fn max_edge_id(conn: &Connection) -> i64 {
    query_i64(conn, "SELECT COALESCE(MAX(id), 0) FROM edges", ()).await
}

pub(crate) async fn node_counts_by_kind(conn: &Connection) -> Vec<Value> {
    query_rows(
        conn,
        "SELECT kind, COUNT(*) AS count
         FROM nodes
         GROUP BY kind
         ORDER BY count DESC, kind ASC",
        (),
    )
    .await
    .unwrap_or_default()
}

pub(crate) async fn edge_counts_by_kind(conn: &Connection) -> Vec<Value> {
    query_rows(
        conn,
        "SELECT kind, COUNT(*) AS count
         FROM edges
         GROUP BY kind
         ORDER BY count DESC, kind ASC",
        (),
    )
    .await
    .unwrap_or_default()
}

pub(crate) async fn largest_files(conn: &Connection) -> Vec<Value> {
    query_rows(
        conn,
        "SELECT path, node_count, size
         FROM files
         ORDER BY node_count DESC, path ASC
         LIMIT 12",
        (),
    )
    .await
    .unwrap_or_default()
}

pub(crate) async fn first_node_for_query(conn: &Connection, query: &str) -> Option<String> {
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
        libsql::params![like, trimmed],
    )
    .await
    .ok()?;
    rows.first()
        .and_then(|row| row.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub(crate) async fn search_total(conn: &Connection, query: &str) -> i64 {
    if query.is_empty() {
        total_nodes(conn).await
    } else {
        let like = like_pattern(query);
        query_i64(
            conn,
            "SELECT COUNT(*)
             FROM nodes
             WHERE name LIKE ?1 ESCAPE '\\'
                OR qualified_name LIKE ?1 ESCAPE '\\'
                OR COALESCE(signature, '') LIKE ?1 ESCAPE '\\'
                OR file_path LIKE ?1 ESCAPE '\\'",
            libsql::params![like],
        )
        .await
    }
}

pub(crate) async fn search_rows(
    conn: &Connection,
    query: &str,
    limit: i64,
    offset: i64,
) -> Vec<Value> {
    if query.is_empty() {
        query_rows(
            conn,
            &format!(
                "SELECT {NODE_COLUMNS}
                 FROM nodes
                 ORDER BY updated_at DESC, qualified_name ASC
                 LIMIT ?1 OFFSET ?2"
            ),
            libsql::params![limit, offset],
        )
        .await
        .unwrap_or_default()
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
            libsql::params![like, query, limit, offset],
        )
        .await
        .unwrap_or_default()
    }
}

pub(crate) async fn node_rows_by_ids(conn: &Connection, ids: &[String]) -> Vec<Value> {
    if ids.is_empty() {
        return Vec::new();
    }
    let placeholders = qmarks(ids.len());
    let sql = format!(
        "SELECT {NODE_COLUMNS}
         FROM nodes
         WHERE id IN ({placeholders})"
    );
    let params = ids.iter().cloned().map(libsql::Value::Text);
    query_rows(conn, &sql, libsql::params_from_iter(params))
        .await
        .unwrap_or_default()
}

pub(crate) async fn edge_rows_for_ids(conn: &Connection, ids: &[String], limit: i64) -> Vec<Value> {
    if ids.is_empty() {
        return Vec::new();
    }
    let placeholders = qmarks(ids.len());
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
    let mut params: Vec<libsql::Value> = ids.iter().cloned().map(libsql::Value::Text).collect();
    params.extend(ids.iter().cloned().map(libsql::Value::Text));
    params.push(libsql::Value::Integer(limit));
    query_rows(conn, &sql, libsql::params_from_iter(params))
        .await
        .unwrap_or_default()
}

pub(crate) async fn degree_rows_for_ids(conn: &Connection, ids: &[String]) -> Vec<Value> {
    if ids.is_empty() {
        return Vec::new();
    }
    let placeholders = qmarks(ids.len());
    let degree_union = filtered_degree_union_sql(&placeholders);
    let sql = format!(
        "SELECT node_id, COUNT(*) AS degree
         FROM ({degree_union})
         GROUP BY node_id"
    );
    let mut params: Vec<libsql::Value> = ids.iter().cloned().map(libsql::Value::Text).collect();
    params.extend(ids.iter().cloned().map(libsql::Value::Text));
    query_rows(conn, &sql, libsql::params_from_iter(params))
        .await
        .unwrap_or_default()
}

pub(crate) async fn degree_pool_rows(conn: &Connection, limit: i64) -> Vec<Value> {
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
        libsql::params![limit],
    )
    .await
    .unwrap_or_default()
}

pub(crate) async fn top_connected_rows(conn: &Connection) -> Vec<Value> {
    query_rows(
        conn,
        &format!(
            "SELECT n.id, n.name, n.kind, n.file_path, d.degree
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
    .unwrap_or_default()
}

pub(crate) async fn node_row(conn: &Connection, node_id: &str) -> Option<Value> {
    query_rows(
        conn,
        &format!("SELECT {NODE_COLUMNS} FROM nodes WHERE id = ?1 LIMIT 1"),
        libsql::params![node_id],
    )
    .await
    .unwrap_or_default()
    .into_iter()
    .next()
}

pub(crate) async fn node_exists(conn: &Connection, node_id: &str) -> bool {
    query_i64(
        conn,
        "SELECT COUNT(*) FROM nodes WHERE id = ?1",
        libsql::params![node_id],
    )
    .await
        > 0
}

pub(crate) async fn caller_rows(conn: &Connection, node_id: &str, limit: i64) -> Vec<Value> {
    query_rows(
        conn,
        &format!(
            "SELECT {NODE_COLUMNS_N}, e.kind AS edge_kind, e.line AS edge_line
             FROM edges e
             JOIN nodes n ON n.id = e.source
             WHERE e.target = ?1 AND e.kind = 'calls'
             ORDER BY n.qualified_name ASC
             LIMIT ?2"
        ),
        libsql::params![node_id, limit],
    )
    .await
    .unwrap_or_default()
}

pub(crate) async fn callee_rows(conn: &Connection, node_id: &str, limit: i64) -> Vec<Value> {
    query_rows(
        conn,
        &format!(
            "SELECT {NODE_COLUMNS_N}, e.kind AS edge_kind, e.line AS edge_line
             FROM edges e
             JOIN nodes n ON n.id = e.target
             WHERE e.source = ?1 AND e.kind = 'calls'
             ORDER BY n.qualified_name ASC
             LIMIT ?2"
        ),
        libsql::params![node_id, limit],
    )
    .await
    .unwrap_or_default()
}

pub(crate) async fn neighborhood_edge_rows(
    conn: &Connection,
    node_id: &str,
    limit: i64,
) -> Vec<Value> {
    query_rows(
        conn,
        "SELECT e.source, e.target, e.kind, e.line,
                source_node.name AS source_name,
                target_node.name AS target_name
         FROM edges e
         JOIN nodes source_node ON source_node.id = e.source
         JOIN nodes target_node ON target_node.id = e.target
         WHERE e.source = ?1 OR e.target = ?1
         ORDER BY e.kind ASC, source_node.qualified_name ASC, target_node.qualified_name ASC
         LIMIT ?2",
        libsql::params![node_id, limit],
    )
    .await
    .unwrap_or_default()
}

pub(crate) async fn neighborhood_edge_counts(conn: &Connection, node_id: &str) -> Vec<Value> {
    query_rows(
        conn,
        "SELECT kind, COUNT(*) AS count
         FROM edges
         WHERE source = ?1 OR target = ?1
         GROUP BY kind
         ORDER BY count DESC, kind ASC",
        libsql::params![node_id],
    )
    .await
    .unwrap_or_default()
}

pub(crate) async fn subgraph_candidate_rows(conn: &Connection, seed_id: &str) -> Vec<Value> {
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
        libsql::params![seed_id],
    )
    .await
    .unwrap_or_default()
}

pub(crate) async fn frontier_edge_rows(conn: &Connection, frontier: &[String]) -> Vec<Value> {
    if frontier.is_empty() {
        return Vec::new();
    }
    let placeholders = qmarks(frontier.len());
    let sql = format!(
        "SELECT source, target, kind, line FROM edges
         WHERE source IN ({placeholders}) OR target IN ({placeholders})"
    );
    let mut bind: Vec<libsql::Value> = frontier.iter().cloned().map(libsql::Value::Text).collect();
    bind.extend(frontier.iter().cloned().map(libsql::Value::Text));
    query_rows(conn, &sql, libsql::params_from_iter(bind))
        .await
        .unwrap_or_default()
}

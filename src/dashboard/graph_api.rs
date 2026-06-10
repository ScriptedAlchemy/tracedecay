//! Code graph dashboard API, backed by tokensave's indexed graph tables.
//!
//! The explorer reads the project-local `nodes`, `edges`, and `files` tables
//! directly and returns compact payloads suitable for search, inspection,
//! progressive subgraph expansion, and shortest-path queries. Every endpoint
//! is bounded: subgraphs cap node/edge counts, search is paginated, and the
//! path BFS caps depth and visited-set size, so responses stay interactive
//! even on graphs with tens of thousands of nodes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::util::{coerce_limit, collect_rows, http_detail, like_pattern, query_i64, query_rows};
use super::DashboardState;

const NODE_COLUMNS: &str = "id, kind, name, qualified_name, file_path,
       start_line, end_line, start_column, end_column, attrs_start_line,
       docstring AS doc, signature, visibility, is_async,
       branches, loops, returns, max_nesting, unsafe_blocks,
       unchecked_calls, assertions, updated_at, parent_id";

/// `NODE_COLUMNS` qualified with the `n.` alias for joined queries
/// (`edges e JOIN nodes n ...`), where bare `id`/`kind` would be ambiguous
/// between the two tables.
const NODE_COLUMNS_N: &str = "n.id, n.kind, n.name, n.qualified_name, n.file_path,
       n.start_line, n.end_line, n.start_column, n.end_column, n.attrs_start_line,
       n.docstring AS doc, n.signature, n.visibility, n.is_async,
       n.branches, n.loops, n.returns, n.max_nesting, n.unsafe_blocks,
       n.unchecked_calls, n.assertions, n.updated_at, n.parent_id";

/// Safety cap on the BFS visited set for `GET /path`.
const PATH_VISITED_CAP: usize = 20_000;

#[derive(Deserialize)]
pub(crate) struct SearchParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct NeighborParams {
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct SubgraphParams {
    node_id: Option<String>,
    #[serde(default)]
    q: String,
    limit_nodes: Option<i64>,
    limit_edges: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct PathParams {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    max_depth: Option<i64>,
}

fn language_for_path(path: &str) -> &'static str {
    let Some((_, ext)) = path.rsplit_once('.') else {
        return "unknown";
    };
    match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "scala" | "sc" => "scala",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "zig" => "zig",
        "sh" | "bash" | "zsh" => "shell",
        "md" | "mdx" => "markdown",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sql" => "sql",
        "html" | "css" => "web",
        _ => "other",
    }
}

fn i64_field(row: &Value, key: &str) -> i64 {
    row.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn str_field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

fn rows_by_language(files: &[Value]) -> Vec<Value> {
    let mut counts: BTreeMap<&'static str, i64> = BTreeMap::new();
    for file in files {
        let language = language_for_path(str_field(file, "path"));
        let count = counts.entry(language).or_insert(0);
        *count += 1;
    }
    let mut rows: Vec<Value> = counts
        .into_iter()
        .map(|(language, count)| json!({ "language": language, "count": count }))
        .collect();
    rows.sort_by(|a, b| {
        i64_field(b, "count")
            .cmp(&i64_field(a, "count"))
            .then_with(|| str_field(a, "language").cmp(str_field(b, "language")))
    });
    rows
}

fn add_span(row: &mut Map<String, Value>) {
    let span = json!({
        "start_line": row.get("start_line").and_then(Value::as_i64).unwrap_or(0),
        "end_line": row.get("end_line").and_then(Value::as_i64).unwrap_or(0),
        "start_column": row.get("start_column").and_then(Value::as_i64).unwrap_or(0),
        "end_column": row.get("end_column").and_then(Value::as_i64).unwrap_or(0),
        "attrs_start_line": row.get("attrs_start_line").and_then(Value::as_i64).unwrap_or(0),
    });
    row.insert("span".into(), span);
}

fn node_with_span(row: Value) -> Value {
    let Value::Object(mut obj) = row else {
        return row;
    };
    add_span(&mut obj);
    Value::Object(obj)
}

async fn first_node_for_query(state: &DashboardState, query: &str) -> Option<String> {
    let like = like_pattern(query.trim());
    let rows = query_rows(
        &state.mem_conn,
        "SELECT id
         FROM nodes
         WHERE name LIKE ?1 ESCAPE '\\'
            OR qualified_name LIKE ?1 ESCAPE '\\'
         ORDER BY CASE WHEN name = ?2 THEN 0 ELSE 1 END,
                  LENGTH(qualified_name) ASC,
                  qualified_name ASC
         LIMIT 1",
        libsql::params![like, query.trim()],
    )
    .await
    .ok()?;
    rows.first()
        .and_then(|row| row.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn qmarks(count: usize) -> String {
    vec!["?"; count].join(",")
}

async fn nodes_by_ids(state: &DashboardState, ids: &[String]) -> Vec<Value> {
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
    let Ok(rows) = state
        .mem_conn
        .query(&sql, libsql::params_from_iter(params))
        .await
    else {
        return Vec::new();
    };
    collect_rows(rows)
        .await
        .map(|rows| rows.into_iter().map(node_with_span).collect())
        .unwrap_or_default()
}

async fn edges_for_ids(state: &DashboardState, ids: &[String], limit: i64) -> Vec<Value> {
    if ids.is_empty() {
        return Vec::new();
    }
    let placeholders = qmarks(ids.len());
    let sql = format!(
        "SELECT source, target, kind, line
         FROM edges
         WHERE source IN ({placeholders}) AND target IN ({placeholders})
         ORDER BY kind ASC, source ASC, target ASC
         LIMIT ?"
    );
    let mut params: Vec<libsql::Value> = ids.iter().cloned().map(libsql::Value::Text).collect();
    params.extend(ids.iter().cloned().map(libsql::Value::Text));
    params.push(libsql::Value::Integer(limit));
    let Ok(rows) = state
        .mem_conn
        .query(&sql, libsql::params_from_iter(params))
        .await
    else {
        return Vec::new();
    };
    collect_rows(rows).await.unwrap_or_default()
}

/// Total (in + out) edge count per node, for the given ids. Drives the UI's
/// size encoding and the "+N collapsed neighbors" affordance.
async fn degrees_for_ids(state: &DashboardState, ids: &[String]) -> BTreeMap<String, i64> {
    let mut degrees = BTreeMap::new();
    if ids.is_empty() {
        return degrees;
    }
    let placeholders = qmarks(ids.len());
    let sql = format!(
        "SELECT node_id, COUNT(*) AS degree
         FROM (
             SELECT source AS node_id FROM edges WHERE source IN ({placeholders})
             UNION ALL
             SELECT target AS node_id FROM edges WHERE target IN ({placeholders})
         )
         GROUP BY node_id"
    );
    let mut params: Vec<libsql::Value> = ids.iter().cloned().map(libsql::Value::Text).collect();
    params.extend(ids.iter().cloned().map(libsql::Value::Text));
    let Ok(rows) = state
        .mem_conn
        .query(&sql, libsql::params_from_iter(params))
        .await
    else {
        return degrees;
    };
    if let Ok(rows) = collect_rows(rows).await {
        for row in rows {
            if let (Some(id), Some(degree)) = (
                row.get("node_id").and_then(Value::as_str),
                row.get("degree").and_then(Value::as_i64),
            ) {
                degrees.insert(id.to_string(), degree);
            }
        }
    }
    degrees
}

fn attach_degrees(nodes: Vec<Value>, degrees: &BTreeMap<String, i64>) -> Vec<Value> {
    nodes
        .into_iter()
        .map(|node| match node {
            Value::Object(mut obj) => {
                let degree = obj
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| degrees.get(id))
                    .copied()
                    .unwrap_or(0);
                obj.insert("degree".into(), json!(degree));
                Value::Object(obj)
            }
            other => other,
        })
        .collect()
}

fn collect_node_ids(nodes: &[Value]) -> Vec<String> {
    nodes
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

/// `GET /api/plugins/graph/overview`
pub(crate) async fn overview(State(state): State<DashboardState>) -> Json<Value> {
    let files = query_rows(
        &state.mem_conn,
        "SELECT path, node_count FROM files ORDER BY path ASC",
        (),
    )
    .await
    .unwrap_or_default();

    Json(json!({
        "path": state.mem_db_path,
        "totals": {
            "nodes": query_i64(&state.mem_conn, "SELECT COUNT(*) FROM nodes", ()).await,
            "edges": query_i64(&state.mem_conn, "SELECT COUNT(*) FROM edges", ()).await,
            "files": query_i64(&state.mem_conn, "SELECT COUNT(*) FROM files", ()).await,
        },
        "nodes_by_kind": query_rows(
            &state.mem_conn,
            "SELECT kind, COUNT(*) AS count
             FROM nodes
             GROUP BY kind
             ORDER BY count DESC, kind ASC",
            (),
        )
        .await
        .unwrap_or_default(),
        "edges_by_kind": query_rows(
            &state.mem_conn,
            "SELECT kind, COUNT(*) AS count
             FROM edges
             GROUP BY kind
             ORDER BY count DESC, kind ASC",
            (),
        )
        .await
        .unwrap_or_default(),
        "files_by_language": rows_by_language(&files),
        "top_connected": query_rows(
            &state.mem_conn,
            "SELECT n.id, n.name, n.kind, n.file_path, d.degree
             FROM (
                 SELECT node_id, COUNT(*) AS degree
                 FROM (
                     SELECT source AS node_id FROM edges
                     UNION ALL
                     SELECT target AS node_id FROM edges
                 )
                 GROUP BY node_id
                 ORDER BY degree DESC
                 LIMIT 12
             ) d
             JOIN nodes n ON n.id = d.node_id
             ORDER BY d.degree DESC, n.qualified_name ASC",
            (),
        )
        .await
        .unwrap_or_default(),
        "largest_files": query_rows(
            &state.mem_conn,
            "SELECT path, node_count, size
             FROM files
             ORDER BY node_count DESC, path ASC
             LIMIT 12",
            (),
        )
        .await
        .unwrap_or_default(),
    }))
}

/// `GET /api/plugins/graph/search?q=...&limit=50&offset=0`
pub(crate) async fn search(
    State(state): State<DashboardState>,
    Query(params): Query<SearchParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 50, 200);
    let offset = params.offset.unwrap_or(0).max(0);
    let query = params.q.trim();

    let (total, results) = if query.is_empty() {
        let total = query_i64(&state.mem_conn, "SELECT COUNT(*) FROM nodes", ()).await;
        let results = query_rows(
            &state.mem_conn,
            &format!(
                "SELECT {NODE_COLUMNS}
                 FROM nodes
                 ORDER BY updated_at DESC, qualified_name ASC
                 LIMIT ?1 OFFSET ?2"
            ),
            libsql::params![limit, offset],
        )
        .await
        .unwrap_or_default();
        (total, results)
    } else {
        let like = like_pattern(query);
        let total = query_i64(
            &state.mem_conn,
            "SELECT COUNT(*)
             FROM nodes
             WHERE name LIKE ?1 ESCAPE '\\'
                OR qualified_name LIKE ?1 ESCAPE '\\'
                OR COALESCE(signature, '') LIKE ?1 ESCAPE '\\'
                OR file_path LIKE ?1 ESCAPE '\\'",
            libsql::params![like.clone()],
        )
        .await;
        let results = query_rows(
            &state.mem_conn,
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
        .unwrap_or_default();
        (total, results)
    };

    let ids = collect_node_ids(&results);
    let degrees = degrees_for_ids(&state, &ids).await;
    let results: Vec<Value> =
        attach_degrees(results.into_iter().map(node_with_span).collect(), &degrees);

    Json(json!({
        "query": query,
        "limit": limit,
        "offset": offset,
        "total": total,
        "count": results.len(),
        "results": results,
    }))
}

/// `GET /api/plugins/graph/node/{node_id}`
pub(crate) async fn node(
    State(state): State<DashboardState>,
    Path(node_id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let rows = query_rows(
        &state.mem_conn,
        &format!("SELECT {NODE_COLUMNS} FROM nodes WHERE id = ?1 LIMIT 1"),
        libsql::params![node_id.clone()],
    )
    .await
    .unwrap_or_default();
    let Some(row) = rows.into_iter().next() else {
        return (
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("node not found: {node_id}"))),
        );
    };
    let degrees = degrees_for_ids(&state, std::slice::from_ref(&node_id)).await;
    let node = attach_degrees(vec![node_with_span(row)], &degrees)
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
    (StatusCode::OK, Json(json!({ "node": node })))
}

/// `GET /api/plugins/graph/node/{node_id}/neighbors`
pub(crate) async fn neighbors(
    State(state): State<DashboardState>,
    Path(node_id): Path<String>,
    Query(params): Query<NeighborParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 50, 200);
    let callers = query_rows(
        &state.mem_conn,
        &format!(
            "SELECT {NODE_COLUMNS_N}, e.kind AS edge_kind, e.line AS edge_line
             FROM edges e
             JOIN nodes n ON n.id = e.source
             WHERE e.target = ?1 AND e.kind = 'calls'
             ORDER BY n.qualified_name ASC
             LIMIT ?2"
        ),
        libsql::params![node_id.clone(), limit],
    )
    .await
    .unwrap_or_default();
    let callees = query_rows(
        &state.mem_conn,
        &format!(
            "SELECT {NODE_COLUMNS_N}, e.kind AS edge_kind, e.line AS edge_line
             FROM edges e
             JOIN nodes n ON n.id = e.target
             WHERE e.source = ?1 AND e.kind = 'calls'
             ORDER BY n.qualified_name ASC
             LIMIT ?2"
        ),
        libsql::params![node_id.clone(), limit],
    )
    .await
    .unwrap_or_default();
    let edges = query_rows(
        &state.mem_conn,
        "SELECT e.source, e.target, e.kind, e.line,
                source_node.name AS source_name,
                target_node.name AS target_name
         FROM edges e
         JOIN nodes source_node ON source_node.id = e.source
         JOIN nodes target_node ON target_node.id = e.target
         WHERE e.source = ?1 OR e.target = ?1
         ORDER BY e.kind ASC, source_node.qualified_name ASC, target_node.qualified_name ASC
         LIMIT ?2",
        libsql::params![node_id.clone(), limit],
    )
    .await
    .unwrap_or_default();
    let edges_by_kind = query_rows(
        &state.mem_conn,
        "SELECT kind, COUNT(*) AS count
         FROM edges
         WHERE source = ?1 OR target = ?1
         GROUP BY kind
         ORDER BY count DESC, kind ASC",
        libsql::params![node_id.clone()],
    )
    .await
    .unwrap_or_default();

    let mut neighbor_ids = collect_node_ids(&callers);
    neighbor_ids.extend(collect_node_ids(&callees));
    neighbor_ids.sort();
    neighbor_ids.dedup();
    let degrees = degrees_for_ids(&state, &neighbor_ids).await;
    let callers = attach_degrees(callers.into_iter().map(node_with_span).collect(), &degrees);
    let callees = attach_degrees(callees.into_iter().map(node_with_span).collect(), &degrees);

    Json(json!({
        "node_id": node_id,
        "depth": 1,
        "limit": limit,
        "callers": callers,
        "callees": callees,
        "edges": edges,
        "edges_by_kind": edges_by_kind,
    }))
}

/// `GET /api/plugins/graph/subgraph?node_id=...&limit_nodes=80&limit_edges=120`
///
/// One-hop neighborhood of the seed, capped, with per-node total degrees so
/// the UI can show how many neighbors remain unexpanded.
pub(crate) async fn subgraph(
    State(state): State<DashboardState>,
    Query(params): Query<SubgraphParams>,
) -> Json<Value> {
    let node_limit = coerce_limit(params.limit_nodes, 80, 250);
    let edge_limit = coerce_limit(params.limit_edges, 120, 500);
    let seed_id = match params.node_id.filter(|id| !id.trim().is_empty()) {
        Some(id) => Some(id),
        None if !params.q.trim().is_empty() => first_node_for_query(&state, &params.q).await,
        None => None,
    };
    let Some(seed_id) = seed_id else {
        return Json(json!({
            "seed_id": null,
            "nodes": [],
            "edges": [],
            "capped": { "nodes": false, "edges": false },
            "limits": { "nodes": node_limit, "edges": edge_limit },
        }));
    };

    let candidate_rows = query_rows(
        &state.mem_conn,
        "SELECT id, MIN(rank) AS rank
         FROM (
             SELECT ?1 AS id, 0 AS rank
             UNION ALL SELECT source AS id, 1 AS rank FROM edges WHERE target = ?1
             UNION ALL SELECT target AS id, 2 AS rank FROM edges WHERE source = ?1
         )
         GROUP BY id
         ORDER BY rank ASC, id ASC",
        libsql::params![seed_id.clone()],
    )
    .await
    .unwrap_or_default();

    let mut all_ids = Vec::new();
    let mut seen = BTreeSet::new();
    for row in candidate_rows {
        if let Some(id) = row.get("id").and_then(Value::as_str) {
            if seen.insert(id.to_string()) {
                all_ids.push(id.to_string());
            }
        }
    }

    let selected_ids: Vec<String> = all_ids.iter().take(node_limit as usize).cloned().collect();
    let degrees = degrees_for_ids(&state, &selected_ids).await;
    let nodes = attach_degrees(nodes_by_ids(&state, &selected_ids).await, &degrees);
    let edges = edges_for_ids(&state, &selected_ids, edge_limit + 1).await;
    let edge_count = edges.len();
    let capped_edges = edge_count > edge_limit as usize;
    let visible_edges: Vec<Value> = edges.into_iter().take(edge_limit as usize).collect();

    Json(json!({
        "seed_id": seed_id,
        "nodes": nodes,
        "edges": visible_edges,
        "capped": {
            "nodes": all_ids.len() > node_limit as usize,
            "edges": capped_edges,
        },
        "limits": {
            "nodes": node_limit,
            "edges": edge_limit,
        },
    }))
}

/// `GET /api/plugins/graph/path?from=<id>&to=<id>&max_depth=6`
///
/// Undirected shortest path between two nodes via breadth-first search over
/// the edges table. Depth defaults to 6 (max 10); the visited set is capped
/// so pathological graphs cannot stall the server.
pub(crate) async fn path(
    State(state): State<DashboardState>,
    Query(params): Query<PathParams>,
) -> Json<Value> {
    let max_depth = coerce_limit(params.max_depth, 6, 10);
    let from = params.from.trim().to_string();
    let to = params.to.trim().to_string();

    let mut payload = json!({
        "from": from,
        "to": to,
        "found": false,
        "path": [],
        "nodes": [],
        "edges": [],
        "max_depth": max_depth,
    });
    if from.is_empty() || to.is_empty() {
        return Json(payload);
    }

    // child -> (parent, edge row) back-pointers for path reconstruction.
    let mut parents: HashMap<String, (String, Value)> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(from.clone());
    let mut frontier = vec![from.clone()];
    let mut found = from == to;

    'search: for _ in 0..max_depth {
        if found || frontier.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for chunk in frontier.chunks(400) {
            let placeholders = qmarks(chunk.len());
            let sql = format!(
                "SELECT source, target, kind, line FROM edges
                 WHERE source IN ({placeholders}) OR target IN ({placeholders})"
            );
            let mut bind: Vec<libsql::Value> =
                chunk.iter().cloned().map(libsql::Value::Text).collect();
            bind.extend(chunk.iter().cloned().map(libsql::Value::Text));
            let Ok(rows) = state
                .mem_conn
                .query(&sql, libsql::params_from_iter(bind))
                .await
            else {
                continue;
            };
            let Ok(rows) = collect_rows(rows).await else {
                continue;
            };
            for row in rows {
                let Some(source) = row.get("source").and_then(Value::as_str) else {
                    continue;
                };
                let Some(target) = row.get("target").and_then(Value::as_str) else {
                    continue;
                };
                let (known, discovered) = if visited.contains(source) && !visited.contains(target) {
                    (source.to_string(), target.to_string())
                } else if visited.contains(target) && !visited.contains(source) {
                    (target.to_string(), source.to_string())
                } else {
                    continue;
                };
                visited.insert(discovered.clone());
                parents.insert(discovered.clone(), (known, row.clone()));
                if discovered == to {
                    found = true;
                    break 'search;
                }
                next.push(discovered);
                if visited.len() > PATH_VISITED_CAP {
                    break 'search;
                }
            }
        }
        frontier = next;
    }

    if !found {
        return Json(payload);
    }

    let mut path_ids = vec![to.clone()];
    let mut path_edges = Vec::new();
    let mut cursor = to.clone();
    while cursor != from {
        let Some((parent, edge)) = parents.get(&cursor) else {
            break;
        };
        path_edges.push(edge.clone());
        cursor = parent.clone();
        path_ids.push(cursor.clone());
    }
    path_ids.reverse();
    path_edges.reverse();

    let degrees = degrees_for_ids(&state, &path_ids).await;
    let nodes = attach_degrees(nodes_by_ids(&state, &path_ids).await, &degrees);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("found".into(), json!(true));
        obj.insert("path".into(), json!(path_ids));
        obj.insert("nodes".into(), json!(nodes));
        obj.insert("edges".into(), json!(path_edges));
    }
    Json(payload)
}

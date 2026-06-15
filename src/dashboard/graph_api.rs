//! Code graph dashboard API, backed by tracedecay's indexed graph tables.
//!
//! The explorer reads the project-local `nodes`, `edges`, and `files` tables
//! directly and returns compact payloads suitable for search, inspection,
//! progressive subgraph expansion, and shortest-path queries. Every endpoint
//! is bounded: subgraphs cap node/edge counts, search is paginated, and the
//! path BFS caps depth and visited-set size, so responses stay interactive
//! even on graphs with tens of thousands of nodes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::util::{
    coerce_limit, collect_rows, http_detail, i64_field, like_pattern, qmarks, query_i64,
    query_rows, str_field, JsonPath, JsonQuery,
};
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

/// Cap on the cached top-degree pool: the default subgraph's candidate pool
/// is at most `node_limit * 2 = 500`, and the overview needs the top 12.
const DEGREE_POOL_CAP: i64 = 500;

/// Cached whole-graph degree aggregation feeding the overview's
/// `top_connected` and the default subgraph's candidate pool. Both used to
/// double-scan the full `edges` table (UNION ALL + GROUP BY) on every Graph
/// tab open/reset, for a result that only changes when the index is synced.
struct DegreeSummary {
    /// `(COUNT(*), MAX(id))` of `edges` at compute time. Edge ids are
    /// AUTOINCREMENT (never reused), so inserts raise the max and deletes
    /// shrink the count; node-only edits without any edge change are not
    /// detected until the next sync rewrites edges.
    fingerprint: (i64, i64),
    /// Top [`DEGREE_POOL_CAP`] `(node_id, degree)` rows, ordered by degree
    /// descending then qualified name ascending (zero-degree nodes included,
    /// like the pool query they replace).
    pool: Vec<(String, i64)>,
    /// Overview `top_connected` rows (top 12 by degree, joined to `nodes`).
    top_connected: Vec<Value>,
}

static DEGREE_CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<DegreeSummary>>>> =
    OnceLock::new();

async fn degree_summary(state: &DashboardState) -> Arc<DegreeSummary> {
    let fingerprint = (
        query_i64(&state.mem_conn, "SELECT COUNT(*) FROM edges", ()).await,
        query_i64(
            &state.mem_conn,
            "SELECT COALESCE(MAX(id), 0) FROM edges",
            (),
        )
        .await,
    );
    let cache = DEGREE_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    // Held across the rebuild so concurrent requests share one aggregation.
    let mut guard = cache.lock().await;
    if let Some(existing) = guard.get(&state.mem_db_path) {
        if existing.fingerprint == fingerprint {
            return existing.clone();
        }
    }

    let pool_rows = query_rows(
        &state.mem_conn,
        "SELECT n.id, COALESCE(d.degree, 0) AS degree
         FROM nodes n
         LEFT JOIN (
             SELECT node_id, COUNT(*) AS degree
             FROM (
                 SELECT source AS node_id FROM edges
                 UNION ALL
                 SELECT target AS node_id FROM edges
             )
             GROUP BY node_id
         ) d ON d.node_id = n.id
         ORDER BY degree DESC, n.qualified_name ASC
         LIMIT ?1",
        libsql::params![DEGREE_POOL_CAP],
    )
    .await
    .unwrap_or_default();
    let pool: Vec<(String, i64)> = pool_rows
        .iter()
        .filter_map(|row| {
            row.get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), i64_field(row, "degree")))
        })
        .collect();

    let top_connected = query_rows(
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
    .unwrap_or_default();

    let summary = Arc::new(DegreeSummary {
        fingerprint,
        pool,
        top_connected,
    });
    guard.insert(state.mem_db_path.clone(), summary.clone());
    summary
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
    let summary = degree_summary(&state).await;

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
        "top_connected": summary.top_connected,
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
    JsonQuery(params): JsonQuery<SearchParams>,
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
    JsonPath(node_id): JsonPath<String>,
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
    JsonPath(node_id): JsonPath<String>,
    JsonQuery(params): JsonQuery<NeighborParams>,
) -> (StatusCode, Json<Value>) {
    let limit = coerce_limit(params.limit, 50, 200);
    let exists = query_i64(
        &state.mem_conn,
        "SELECT COUNT(*) FROM nodes WHERE id = ?1",
        libsql::params![node_id.clone()],
    )
    .await;
    if exists == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("node not found: {node_id}"))),
        );
    }
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

    (
        StatusCode::OK,
        Json(json!({
            "node_id": node_id,
            "depth": 1,
            "limit": limit,
            "callers": callers,
            "callees": callees,
            "edges": edges,
            "edges_by_kind": edges_by_kind,
        })),
    )
}

/// Cap on edges fetched among the default-mode candidate pool before the
/// per-response `limit_edges` cap is applied.
const DEFAULT_POOL_EDGE_CAP: i64 = 4_000;

/// Seedless "project overview" slice: the most-connected symbols plus the
/// edges among them, so the canvas has something informative to show before
/// the user searches.
///
/// Selection grows greedily by adjacency over a top-degree candidate pool:
/// prefer the highest-degree pool node touching the current selection
/// (recording the edge that connects it, so the edge cap can never strand
/// it), seed a new cluster from the highest-degree connected node when
/// nothing touches the selection, and let isolated nodes fill whatever
/// capacity is left (which also keeps tiny or edge-free indexes non-empty).
async fn default_subgraph(state: &DashboardState, node_limit: i64, edge_limit: i64) -> Json<Value> {
    // Candidate pool: 2x the node budget so selection has room to work with,
    // served as a prefix of the cached top-degree summary.
    let pool_limit = usize::try_from((node_limit * 2).min(DEGREE_POOL_CAP)).unwrap_or(0);
    let summary = degree_summary(state).await;

    let mut pool_ids = Vec::new();
    let mut degrees: BTreeMap<String, i64> = BTreeMap::new();
    for (id, degree) in summary.pool.iter().take(pool_limit) {
        pool_ids.push(id.clone());
        degrees.insert(id.clone(), *degree);
    }

    let pool_edges = edges_for_ids(state, &pool_ids, DEFAULT_POOL_EDGE_CAP).await;

    // Adjacency over the pool: node id -> indices of touching edges
    // (self-loops don't make a node "connected" for selection purposes).
    let mut adjacency: HashMap<&str, Vec<usize>> = HashMap::new();
    for (idx, edge) in pool_edges.iter().enumerate() {
        let source = str_field(edge, "source");
        let target = str_field(edge, "target");
        if source == target {
            continue;
        }
        adjacency.entry(source).or_default().push(idx);
        adjacency.entry(target).or_default().push(idx);
    }

    let budget = usize::try_from(node_limit).unwrap_or(0);
    let mut selected: Vec<String> = Vec::new();
    let mut selected_set: HashSet<&str> = HashSet::new();
    // Edges recorded while growing the selection; emitted first so the edge
    // cap cannot leave a selected node without any visible edge.
    let mut connecting_edges: Vec<usize> = Vec::new();
    while selected.len() < budget {
        let mut adjacent_pick: Option<(&str, usize)> = None;
        let mut seed_pick: Option<&str> = None;
        for id in pool_ids.iter().map(String::as_str) {
            if selected_set.contains(id) {
                continue;
            }
            let Some(edge_idxs) = adjacency.get(id) else {
                continue;
            };
            let touching = edge_idxs.iter().copied().find(|&idx| {
                let edge = &pool_edges[idx];
                let source = str_field(edge, "source");
                let other = if source == id {
                    str_field(edge, "target")
                } else {
                    source
                };
                selected_set.contains(other)
            });
            if let Some(idx) = touching {
                adjacent_pick = Some((id, idx));
                break;
            }
            if seed_pick.is_none() {
                seed_pick = Some(id);
            }
        }
        let Some(id) = adjacent_pick.map(|(id, _)| id).or(seed_pick) else {
            break;
        };
        if let Some((_, edge_idx)) = adjacent_pick {
            connecting_edges.push(edge_idx);
        }
        selected.push(id.to_string());
        selected_set.insert(id);
    }
    if selected.len() < budget {
        for id in &pool_ids {
            if selected.len() >= budget {
                break;
            }
            if !selected_set.contains(id.as_str()) {
                selected.push(id.clone());
                selected_set.insert(id);
            }
        }
    }

    let mut edge_order = connecting_edges;
    let used: HashSet<usize> = edge_order.iter().copied().collect();
    for (idx, edge) in pool_edges.iter().enumerate() {
        if used.contains(&idx) {
            continue;
        }
        if selected_set.contains(str_field(edge, "source"))
            && selected_set.contains(str_field(edge, "target"))
        {
            edge_order.push(idx);
        }
    }
    let capped_edges = edge_order.len() > edge_limit as usize;
    let visible_edges: Vec<Value> = edge_order
        .into_iter()
        .take(edge_limit as usize)
        .map(|idx| pool_edges[idx].clone())
        .collect();

    let nodes = attach_degrees(nodes_by_ids(state, &selected).await, &degrees);
    let total_nodes = query_i64(&state.mem_conn, "SELECT COUNT(*) FROM nodes", ()).await;

    Json(json!({
        "seed_id": null,
        "mode": "default",
        "nodes": nodes,
        "edges": visible_edges,
        "capped": {
            "nodes": total_nodes > selected.len() as i64,
            "edges": capped_edges,
        },
        "limits": {
            "nodes": node_limit,
            "edges": edge_limit,
        },
    }))
}

/// `GET /api/plugins/graph/subgraph?node_id=...&limit_nodes=80&limit_edges=120`
///
/// One-hop neighborhood of the seed, capped, with per-node total degrees so
/// the UI can show how many neighbors remain unexpanded. Without a seed
/// (`node_id` / `q` both absent) it returns the default overview slice
/// instead: top-degree hubs plus the edges among them.
pub(crate) async fn subgraph(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<SubgraphParams>,
) -> Json<Value> {
    let node_limit = coerce_limit(params.limit_nodes, 80, 250);
    let edge_limit = coerce_limit(params.limit_edges, 120, 500);
    let query = params.q.trim();
    let seed_id = match params.node_id.filter(|id| !id.trim().is_empty()) {
        Some(id) => Some(id),
        None if !query.is_empty() => {
            let Some(id) = first_node_for_query(&state, query).await else {
                // Explicit query with no hit: an empty payload, not the
                // default slice, so a failed search reads as "no match".
                return Json(json!({
                    "seed_id": null,
                    "mode": "seeded",
                    "nodes": [],
                    "edges": [],
                    "capped": { "nodes": false, "edges": false },
                    "limits": { "nodes": node_limit, "edges": edge_limit },
                }));
            };
            Some(id)
        }
        None => None,
    };
    let Some(seed_id) = seed_id else {
        return default_subgraph(&state, node_limit, edge_limit).await;
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
        "mode": "seeded",
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
    JsonQuery(params): JsonQuery<PathParams>,
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

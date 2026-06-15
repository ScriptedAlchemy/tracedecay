use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use serde_json::{json, Map, Value};

use super::graph_queries;
use super::util::{i64_field, str_field};
use super::DashboardState;

/// Safety cap on the BFS visited set for `GET /path`.
const PATH_VISITED_CAP: usize = 20_000;

/// Cap on the cached top-degree pool: the default subgraph's candidate pool
/// is at most `node_limit * 2 = 500`, and the overview needs the top 12.
const DEGREE_POOL_CAP: i64 = 500;

/// Cap on edges fetched among the default-mode candidate pool before the
/// per-response `limit_edges` cap is applied.
const DEFAULT_POOL_EDGE_CAP: i64 = 4_000;

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
        "attrs_start_line": row
            .get("attrs_start_line")
            .and_then(Value::as_i64)
            .unwrap_or(0),
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

async fn nodes_by_ids(state: &DashboardState, ids: &[String]) -> Vec<Value> {
    graph_queries::node_rows_by_ids(&state.mem_conn, ids)
        .await
        .into_iter()
        .map(node_with_span)
        .collect()
}

async fn edges_for_ids(state: &DashboardState, ids: &[String], limit: i64) -> Vec<Value> {
    graph_queries::edge_rows_for_ids(&state.mem_conn, ids, limit).await
}

/// Total (in + out) edge count per node, for the given ids. Drives the UI's
/// size encoding and the "+N collapsed neighbors" affordance.
async fn degrees_for_ids(state: &DashboardState, ids: &[String]) -> BTreeMap<String, i64> {
    let mut degrees = BTreeMap::new();
    for row in graph_queries::degree_rows_for_ids(&state.mem_conn, ids).await {
        if let (Some(id), Some(degree)) = (
            row.get("node_id").and_then(Value::as_str),
            row.get("degree").and_then(Value::as_i64),
        ) {
            degrees.insert(id.to_string(), degree);
        }
    }
    degrees
}

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
        graph_queries::total_edges(&state.mem_conn).await,
        graph_queries::max_edge_id(&state.mem_conn).await,
    );
    let cache = DEGREE_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    // Held across the rebuild so concurrent requests share one aggregation.
    let mut guard = cache.lock().await;
    if let Some(existing) = guard.get(&state.mem_db_path) {
        if existing.fingerprint == fingerprint {
            return existing.clone();
        }
    }

    let pool = graph_queries::degree_pool_rows(&state.mem_conn, DEGREE_POOL_CAP)
        .await
        .iter()
        .filter_map(|row| {
            row.get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), i64_field(row, "degree")))
        })
        .collect();
    let top_connected = graph_queries::top_connected_rows(&state.mem_conn).await;

    let summary = Arc::new(DegreeSummary {
        fingerprint,
        pool,
        top_connected,
    });
    guard.insert(state.mem_db_path.clone(), summary.clone());
    summary
}

pub(crate) async fn overview_payload(state: &DashboardState) -> Value {
    let files = graph_queries::overview_file_rows(&state.mem_conn).await;
    let summary = degree_summary(state).await;

    json!({
        "path": state.mem_db_path,
        "totals": {
            "nodes": graph_queries::total_nodes(&state.mem_conn).await,
            "edges": graph_queries::total_edges(&state.mem_conn).await,
            "files": graph_queries::total_files(&state.mem_conn).await,
        },
        "nodes_by_kind": graph_queries::node_counts_by_kind(&state.mem_conn).await,
        "edges_by_kind": graph_queries::edge_counts_by_kind(&state.mem_conn).await,
        "files_by_language": rows_by_language(&files),
        "top_connected": summary.top_connected,
        "largest_files": graph_queries::largest_files(&state.mem_conn).await,
    })
}

pub(crate) async fn search_payload(
    state: &DashboardState,
    query: &str,
    limit: i64,
    offset: i64,
) -> Value {
    let total = graph_queries::search_total(&state.mem_conn, query).await;
    let results = graph_queries::search_rows(&state.mem_conn, query, limit, offset).await;
    let ids = collect_node_ids(&results);
    let degrees = degrees_for_ids(state, &ids).await;
    let results = attach_degrees(results.into_iter().map(node_with_span).collect(), &degrees);

    json!({
        "query": query,
        "limit": limit,
        "offset": offset,
        "total": total,
        "count": results.len(),
        "results": results,
    })
}

pub(crate) async fn node_exists(state: &DashboardState, node_id: &str) -> bool {
    graph_queries::node_exists(&state.mem_conn, node_id).await
}

pub(crate) async fn node_payload(state: &DashboardState, node_id: &str) -> Option<Value> {
    let row = graph_queries::node_row(&state.mem_conn, node_id).await?;
    let degrees = degrees_for_ids(state, &[node_id.to_string()]).await;
    let node = attach_degrees(vec![node_with_span(row)], &degrees)
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
    Some(json!({ "node": node }))
}

pub(crate) async fn neighbors_payload(state: &DashboardState, node_id: &str, limit: i64) -> Value {
    let callers = graph_queries::caller_rows(&state.mem_conn, node_id, limit).await;
    let callees = graph_queries::callee_rows(&state.mem_conn, node_id, limit).await;
    let edges = graph_queries::neighborhood_edge_rows(&state.mem_conn, node_id, limit).await;
    let edges_by_kind = graph_queries::neighborhood_edge_counts(&state.mem_conn, node_id).await;

    let mut neighbor_ids = collect_node_ids(&callers);
    neighbor_ids.extend(collect_node_ids(&callees));
    neighbor_ids.sort();
    neighbor_ids.dedup();
    let degrees = degrees_for_ids(state, &neighbor_ids).await;
    let callers = attach_degrees(callers.into_iter().map(node_with_span).collect(), &degrees);
    let callees = attach_degrees(callees.into_iter().map(node_with_span).collect(), &degrees);

    json!({
        "node_id": node_id,
        "depth": 1,
        "limit": limit,
        "callers": callers,
        "callees": callees,
        "edges": edges,
        "edges_by_kind": edges_by_kind,
    })
}

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
async fn default_subgraph(state: &DashboardState, node_limit: i64, edge_limit: i64) -> Value {
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
    let total_nodes = graph_queries::total_nodes(&state.mem_conn).await;

    json!({
        "seed_id": Value::Null,
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
    })
}

pub(crate) async fn subgraph_payload(
    state: &DashboardState,
    node_id: Option<String>,
    query: &str,
    node_limit: i64,
    edge_limit: i64,
) -> Value {
    let seed_id = match node_id.filter(|id| !id.trim().is_empty()) {
        Some(id) => Some(id),
        None if !query.is_empty() => {
            let Some(id) = graph_queries::first_node_for_query(&state.mem_conn, query).await else {
                // Explicit query with no hit: an empty payload, not the
                // default slice, so a failed search reads as "no match".
                return json!({
                    "seed_id": Value::Null,
                    "mode": "seeded",
                    "nodes": [],
                    "edges": [],
                    "capped": { "nodes": false, "edges": false },
                    "limits": { "nodes": node_limit, "edges": edge_limit },
                });
            };
            Some(id)
        }
        None => None,
    };
    let Some(seed_id) = seed_id else {
        return default_subgraph(state, node_limit, edge_limit).await;
    };

    let candidate_rows = graph_queries::subgraph_candidate_rows(&state.mem_conn, &seed_id).await;
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
    let degrees = degrees_for_ids(state, &selected_ids).await;
    let nodes = attach_degrees(nodes_by_ids(state, &selected_ids).await, &degrees);
    let edges = edges_for_ids(state, &selected_ids, edge_limit + 1).await;
    let edge_count = edges.len();
    let capped_edges = edge_count > edge_limit as usize;
    let visible_edges: Vec<Value> = edges.into_iter().take(edge_limit as usize).collect();

    json!({
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
    })
}

/// Undirected shortest path between two nodes via breadth-first search over
/// the edges table. Depth defaults to 6 (max 10); the visited set is capped
/// so pathological graphs cannot stall the server.
pub(crate) async fn path_payload(
    state: &DashboardState,
    from: &str,
    to: &str,
    max_depth: i64,
) -> Value {
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
        return payload;
    }

    // child -> (parent, edge row) back-pointers for path reconstruction.
    let mut parents: HashMap<String, (String, Value)> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(from.to_string());
    let mut frontier = vec![from.to_string()];
    let mut found = from == to;

    'search: for _ in 0..max_depth {
        if found || frontier.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for chunk in frontier.chunks(400) {
            for row in graph_queries::frontier_edge_rows(&state.mem_conn, chunk).await {
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
        return payload;
    }

    let mut path_ids = vec![to.to_string()];
    let mut path_edges = Vec::new();
    let mut cursor = to.to_string();
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

    let degrees = degrees_for_ids(state, &path_ids).await;
    let nodes = attach_degrees(nodes_by_ids(state, &path_ids).await, &degrees);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("found".into(), json!(true));
        obj.insert("path".into(), json!(path_ids));
        obj.insert("nodes".into(), json!(nodes));
        obj.insert("edges".into(), json!(path_edges));
    }
    payload
}

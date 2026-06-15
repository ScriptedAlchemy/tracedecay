//! Code graph dashboard API, backed by tracedecay's indexed graph tables.
//!
//! The explorer reads the project-local `nodes`, `edges`, and `files` tables
//! directly and returns compact payloads suitable for search, inspection,
//! progressive subgraph expansion, and shortest-path queries. Every endpoint
//! is bounded: subgraphs cap node/edge counts, search is paginated, and the
//! path BFS caps depth and visited-set size, so responses stay interactive
//! even on graphs with tens of thousands of nodes.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::Value;

use super::graph_service;
use super::util::{coerce_limit, http_detail, JsonPath, JsonQuery};
use super::DashboardState;

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

/// `GET /api/plugins/graph/overview`
pub(crate) async fn overview(State(state): State<DashboardState>) -> Json<Value> {
    Json(graph_service::overview_payload(&state).await)
}

/// `GET /api/plugins/graph/search?q=...&limit=50&offset=0`
pub(crate) async fn search(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<SearchParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 50, 200);
    let offset = params.offset.unwrap_or(0).max(0);
    Json(graph_service::search_payload(&state, params.q.trim(), limit, offset).await)
}

/// `GET /api/plugins/graph/node/{node_id}`
pub(crate) async fn node(
    State(state): State<DashboardState>,
    JsonPath(node_id): JsonPath<String>,
) -> (StatusCode, Json<Value>) {
    let Some(payload) = graph_service::node_payload(&state, &node_id).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("node not found: {node_id}"))),
        );
    };
    (StatusCode::OK, Json(payload))
}

/// `GET /api/plugins/graph/node/{node_id}/neighbors`
pub(crate) async fn neighbors(
    State(state): State<DashboardState>,
    JsonPath(node_id): JsonPath<String>,
    JsonQuery(params): JsonQuery<NeighborParams>,
) -> (StatusCode, Json<Value>) {
    if !graph_service::node_exists(&state, &node_id).await {
        return (
            StatusCode::NOT_FOUND,
            Json(http_detail(&format!("node not found: {node_id}"))),
        );
    }
    let limit = coerce_limit(params.limit, 50, 200);
    (
        StatusCode::OK,
        Json(graph_service::neighbors_payload(&state, &node_id, limit).await),
    )
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
    Json(
        graph_service::subgraph_payload(
            &state,
            params.node_id,
            params.q.trim(),
            node_limit,
            edge_limit,
        )
        .await,
    )
}

/// `GET /api/plugins/graph/path?from=<id>&to=<id>&max_depth=6`
pub(crate) async fn path(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<PathParams>,
) -> Json<Value> {
    let max_depth = coerce_limit(params.max_depth, 6, 10);
    Json(graph_service::path_payload(&state, params.from.trim(), params.to.trim(), max_depth).await)
}

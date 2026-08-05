//! Code graph dashboard API, backed by tracedecay's indexed graph tables.
//!
//! The explorer reads the resolved project graph `nodes`, `edges`, and
//! `files` tables directly and returns compact payloads suitable for search,
//! inspection, progressive subgraph expansion, and shortest-path queries.
//! Every endpoint is bounded: subgraphs cap node/edge counts, search is
//! paginated, and the path BFS caps depth and visited-set size, so responses
//! stay interactive even on graphs with tens of thousands of nodes.

use axum::extract::State;
use axum::response::Json;
use serde::Deserialize;

use super::DashboardState;
use super::graph_service;
use super::read_model::{DashboardCoverageV1, DashboardEnvelopeV1, scope_from_state};
use super::util::{JsonPath, JsonQuery, coerce_limit};

#[derive(Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
pub struct NeighborParams {
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SubgraphParams {
    node_id: Option<String>,
    #[serde(default)]
    q: String,
    limit_nodes: Option<i64>,
    limit_edges: Option<i64>,
}

#[derive(Deserialize)]
pub struct PathParams {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    max_depth: Option<i64>,
}

/// `GET /api/plugins/graph/overview`
pub async fn overview(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphOverviewPayloadV1>>> {
    graph_response(&state, graph_service::overview_payload(&state).await)
}

/// `GET /api/plugins/graph/search?q=...&limit=50&offset=0`
pub async fn search(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<SearchParams>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphSearchPayloadV1>>> {
    let limit = coerce_limit(params.limit, 50, 200);
    let offset = params.offset.unwrap_or(0).max(0);
    graph_response(
        &state,
        graph_service::search_payload(&state, params.q.trim(), limit, offset).await,
    )
}

/// `GET /api/plugins/graph/node/{node_id}`
pub async fn node(
    State(state): State<DashboardState>,
    JsonPath(node_id): JsonPath<String>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphNodePayloadV1>>> {
    match graph_service::node_payload(&state, &node_id).await {
        Ok(Some(payload)) => graph_ready(&state, payload),
        Ok(None) => Json(DashboardEnvelopeV1::complete_zero_findings(
            scope_from_state(&state),
            DashboardCoverageV1::complete(1, "nodes"),
            None,
        )),
        Err(error) => graph_read_failed(&state, error),
    }
}

/// `GET /api/plugins/graph/node/{node_id}/neighbors`
pub async fn neighbors(
    State(state): State<DashboardState>,
    JsonPath(node_id): JsonPath<String>,
    JsonQuery(params): JsonQuery<NeighborParams>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphNeighborsPayloadV1>>> {
    match graph_service::node_exists(&state, &node_id).await {
        Ok(true) => {}
        Ok(false) => {
            return Json(DashboardEnvelopeV1::complete_zero_findings(
                scope_from_state(&state),
                DashboardCoverageV1::complete(1, "nodes"),
                None,
            ));
        }
        Err(error) => return graph_read_failed(&state, error),
    }
    let limit = coerce_limit(params.limit, 50, 200);
    graph_response(
        &state,
        graph_service::neighbors_payload(&state, &node_id, limit).await,
    )
}

/// `GET /api/plugins/graph/subgraph?node_id=...&limit_nodes=80&limit_edges=120`
///
/// One-hop neighborhood of the seed, capped, with per-node total degrees so
/// the UI can show how many neighbors remain unexpanded. Without a seed
/// (`node_id` / `q` both absent) it returns the default overview slice
/// instead: top-degree hubs plus the edges among them.
pub async fn subgraph(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<SubgraphParams>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphSubgraphPayloadV1>>> {
    let node_limit = coerce_limit(params.limit_nodes, 80, 250);
    let edge_limit = coerce_limit(params.limit_edges, 120, 500);
    graph_response(
        &state,
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
pub async fn path(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<PathParams>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphPathPayloadV1>>> {
    let max_depth = coerce_limit(params.max_depth, 6, 10);
    graph_response(
        &state,
        graph_service::path_payload(&state, params.from.trim(), params.to.trim(), max_depth).await,
    )
}

fn graph_response<T>(
    state: &DashboardState,
    result: Result<T, String>,
) -> Json<DashboardEnvelopeV1<Option<T>>> {
    match result {
        Ok(payload) => graph_ready(state, payload),
        Err(error) => graph_read_failed(state, error),
    }
}

fn graph_ready<T>(state: &DashboardState, payload: T) -> Json<DashboardEnvelopeV1<Option<T>>> {
    Json(DashboardEnvelopeV1::ready(
        scope_from_state(state),
        DashboardCoverageV1::unknown(),
        Some(payload),
    ))
}

fn graph_read_failed<T>(
    state: &DashboardState,
    error: String,
) -> Json<DashboardEnvelopeV1<Option<T>>> {
    Json(DashboardEnvelopeV1::error(
        scope_from_state(state),
        None,
        error,
    ))
}

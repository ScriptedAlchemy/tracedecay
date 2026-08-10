//! Code graph dashboard API over the daemon/application graph read authority.
//!
//! Every result is bound to one exact recovered-state-verified generation.
//! The adapter receives no graph store path, connection, or query handle.

use axum::extract::{Extension, State};
use axum::response::Json;
use serde::Deserialize;

use super::graph_service;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardVersionV1, scope_from_state,
};
use super::util::{JsonPath, JsonQuery, coerce_limit};
use super::{DashboardHttpRequestControlV1, DashboardState};
use crate::graph::CodeGraphReadError;

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
    control: Option<Extension<DashboardHttpRequestControlV1>>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphOverviewPayloadV1>>> {
    let Some(Extension(control)) = control else {
        return graph_read_failed(&state, CodeGraphReadError::MissingRegistry);
    };
    graph_response(
        &state,
        graph_service::overview_payload(&state, &control).await,
    )
}

/// `GET /api/plugins/graph/search?q=...&limit=50&offset=0`
pub async fn search(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonQuery(params): JsonQuery<SearchParams>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphSearchPayloadV1>>> {
    let limit = coerce_limit(params.limit, 50, 200);
    let offset = params.offset.unwrap_or(0).max(0);
    let Some(Extension(control)) = control else {
        return graph_read_failed(&state, CodeGraphReadError::MissingRegistry);
    };
    graph_response(
        &state,
        graph_service::search_payload(&state, &control, params.q.trim(), limit, offset).await,
    )
}

/// `GET /api/plugins/graph/node/{node_id}`
pub async fn node(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonPath(node_id): JsonPath<String>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphNodePayloadV1>>> {
    let Some(Extension(control)) = control else {
        return graph_read_failed(&state, CodeGraphReadError::MissingRegistry);
    };
    match graph_service::node_payload(&state, &control, &node_id).await {
        Ok(read) if read.payload.is_some() => graph_ready(&state, read.payload, read.generation),
        Ok(read) => Json(
            DashboardEnvelopeV1::complete_zero_findings(
                scope_from_state(&state),
                DashboardCoverageV1::complete(1, "nodes"),
                None,
            )
            .with_version(graph_version(read.generation)),
        ),
        Err(error) => graph_read_failed(&state, error),
    }
}

/// `GET /api/plugins/graph/node/{node_id}/neighbors`
pub async fn neighbors(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonPath(node_id): JsonPath<String>,
    JsonQuery(params): JsonQuery<NeighborParams>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphNeighborsPayloadV1>>> {
    let limit = coerce_limit(params.limit, 50, 200);
    let Some(Extension(control)) = control else {
        return graph_read_failed(&state, CodeGraphReadError::MissingRegistry);
    };
    match graph_service::neighbors_payload(&state, &control, &node_id, limit).await {
        Ok(read) if read.payload.is_some() => graph_ready(&state, read.payload, read.generation),
        Ok(read) => Json(
            DashboardEnvelopeV1::complete_zero_findings(
                scope_from_state(&state),
                DashboardCoverageV1::complete(1, "nodes"),
                None,
            )
            .with_version(graph_version(read.generation)),
        ),
        Err(error) => graph_read_failed(&state, error),
    }
}

/// `GET /api/plugins/graph/subgraph?node_id=...&limit_nodes=80&limit_edges=120`
///
/// One-hop neighborhood of the seed, capped, with per-node total degrees so
/// the UI can show how many neighbors remain unexpanded. Without a seed
/// (`node_id` / `q` both absent) it returns the default overview slice
/// instead: top-degree hubs plus the edges among them.
pub async fn subgraph(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonQuery(params): JsonQuery<SubgraphParams>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphSubgraphPayloadV1>>> {
    let node_limit = coerce_limit(params.limit_nodes, 80, 250);
    let edge_limit = coerce_limit(params.limit_edges, 120, 500);
    let Some(Extension(control)) = control else {
        return graph_read_failed(&state, CodeGraphReadError::MissingRegistry);
    };
    graph_response(
        &state,
        graph_service::subgraph_payload(
            &state,
            &control,
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
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonQuery(params): JsonQuery<PathParams>,
) -> Json<DashboardEnvelopeV1<Option<graph_service::GraphPathPayloadV1>>> {
    let max_depth = coerce_limit(params.max_depth, 6, 10);
    let Some(Extension(control)) = control else {
        return graph_read_failed(&state, CodeGraphReadError::MissingRegistry);
    };
    graph_response(
        &state,
        graph_service::path_payload(
            &state,
            &control,
            params.from.trim(),
            params.to.trim(),
            max_depth,
        )
        .await,
    )
}

fn graph_response<T>(
    state: &DashboardState,
    result: Result<graph_service::GraphServiceReadV1<T>, CodeGraphReadError>,
) -> Json<DashboardEnvelopeV1<Option<T>>> {
    match result {
        Ok(read) => graph_ready(state, Some(read.payload), read.generation),
        Err(error) => graph_read_failed(state, error),
    }
}

fn graph_ready<T>(
    state: &DashboardState,
    payload: Option<T>,
    generation: String,
) -> Json<DashboardEnvelopeV1<Option<T>>> {
    Json(
        DashboardEnvelopeV1::ready(
            scope_from_state(state),
            DashboardCoverageV1::unknown(),
            payload,
        )
        .with_version(graph_version(generation)),
    )
}

fn graph_read_failed<T>(
    state: &DashboardState,
    error: CodeGraphReadError,
) -> Json<DashboardEnvelopeV1<Option<T>>> {
    let scope = scope_from_state(state);
    let envelope = match error {
        CodeGraphReadError::MissingRegistry => {
            DashboardEnvelopeV1::unavailable(scope, None, "missing_registry")
        }
        CodeGraphReadError::Unavailable { detail } => {
            DashboardEnvelopeV1::unavailable(scope, None, detail)
        }
        CodeGraphReadError::Stale { detail } => {
            let mut coverage = DashboardCoverageV1::unknown();
            coverage.omission_reasons.push(detail);
            DashboardEnvelopeV1::stale(scope, coverage, None)
        }
        CodeGraphReadError::Cancelled => DashboardEnvelopeV1::new(
            scope,
            DashboardDomainStateV1::Cancelled,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            None,
        ),
        CodeGraphReadError::TimedOut => DashboardEnvelopeV1::new(
            scope,
            DashboardDomainStateV1::TimedOut,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            None,
        ),
        CodeGraphReadError::Denied => DashboardEnvelopeV1::denied(scope, None),
        CodeGraphReadError::InvalidRequest { detail }
        | CodeGraphReadError::Corrupt { detail }
        | CodeGraphReadError::ResetRequired { detail }
        | CodeGraphReadError::BudgetExhausted { detail } => {
            DashboardEnvelopeV1::error(scope, None, detail)
        }
    };
    Json(envelope)
}

fn graph_version(generation: String) -> DashboardVersionV1 {
    DashboardVersionV1 {
        entity_version: None,
        graph_version: Some(generation),
    }
}

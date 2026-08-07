use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_application::{
    DashboardGraphReadErrorV1, DashboardGraphReadOperationV1, DashboardGraphReadPayloadV1,
    DashboardGraphReadRequestV1, DashboardGraphReadV1, VerifiedDashboardGraphGenerationV1,
};

use super::DashboardState;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct GraphSpanV1 {
    start_line: i64,
    end_line: i64,
    start_column: i64,
    end_column: i64,
    attrs_start_line: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct GraphNodeV1 {
    id: String,
    kind: String,
    name: Option<String>,
    qualified_name: Option<String>,
    file_path: Option<String>,
    start_line: Option<i64>,
    end_line: Option<i64>,
    start_column: Option<i64>,
    end_column: Option<i64>,
    attrs_start_line: Option<i64>,
    doc: Option<String>,
    signature: Option<String>,
    visibility: Option<String>,
    is_async: Option<i64>,
    branches: Option<i64>,
    loops: Option<i64>,
    returns: Option<i64>,
    max_nesting: Option<i64>,
    unsafe_blocks: Option<i64>,
    unchecked_calls: Option<i64>,
    assertions: Option<i64>,
    updated_at: Option<i64>,
    parent_id: Option<String>,
    degree: Option<i64>,
    span: Option<GraphSpanV1>,
    edge_kind: Option<String>,
    edge_line: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct GraphEdgeV1 {
    source: String,
    target: String,
    kind: String,
    line: Option<i64>,
    source_name: Option<String>,
    target_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct GraphKindCountV1 {
    kind: String,
    count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct GraphLanguageCountV1 {
    language: String,
    count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct GraphLargestFileV1 {
    path: String,
    node_count: i64,
    size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct GraphTotalsV1 {
    nodes: u64,
    edges: u64,
    files: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct GraphOverviewPayloadV1 {
    totals: GraphTotalsV1,
    nodes_by_kind: Vec<GraphKindCountV1>,
    edges_by_kind: Vec<GraphKindCountV1>,
    files_by_language: Vec<GraphLanguageCountV1>,
    largest_files: Vec<GraphLargestFileV1>,
    path: String,
    top_connected: Vec<GraphNodeV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct GraphSearchPayloadV1 {
    query: String,
    limit: i64,
    offset: i64,
    pub(super) total: i64,
    count: usize,
    pub(super) results: Vec<GraphNodeV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct GraphNodePayloadV1 {
    pub(super) node: GraphNodeV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct GraphNeighborsPayloadV1 {
    node_id: String,
    depth: i64,
    limit: i64,
    callers: Vec<GraphNodeV1>,
    callees: Vec<GraphNodeV1>,
    edges: Vec<GraphEdgeV1>,
    edges_by_kind: Vec<GraphKindCountV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct GraphCappedV1 {
    nodes: bool,
    edges: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct GraphLimitsV1 {
    nodes: i64,
    edges: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct GraphSubgraphPayloadV1 {
    seed_id: Option<String>,
    mode: String,
    nodes: Vec<GraphNodeV1>,
    edges: Vec<GraphEdgeV1>,
    capped: GraphCappedV1,
    limits: GraphLimitsV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct GraphPathPayloadV1 {
    from: String,
    to: String,
    found: bool,
    path: Vec<String>,
    nodes: Vec<GraphNodeV1>,
    edges: Vec<GraphEdgeV1>,
    max_depth: i64,
}

pub(super) struct GraphServiceReadV1<T> {
    pub(super) payload: T,
    pub(super) generation: VerifiedDashboardGraphGenerationV1,
}

fn decode_payload<T: DeserializeOwned>(payload: Value) -> Result<T, DashboardGraphReadErrorV1> {
    serde_json::from_value(payload).map_err(|error| DashboardGraphReadErrorV1::Corrupt {
        detail: format!("dashboard graph application read model is invalid: {error}"),
    })
}

pub(super) async fn read_graph(
    state: &DashboardState,
    operation: DashboardGraphReadOperationV1,
) -> Result<DashboardGraphReadV1, DashboardGraphReadErrorV1> {
    let scope = state
        .resolved_scope
        .clone()
        .ok_or(DashboardGraphReadErrorV1::MissingRegistry)?;
    let authority = state
        .graph_read_authority
        .as_ref()
        .ok_or(DashboardGraphReadErrorV1::MissingRegistry)?;
    let result = authority
        .read(DashboardGraphReadRequestV1 {
            scope: scope.clone(),
            operation,
        })
        .await?;
    if !result.generation.matches_scope(&scope) {
        return Err(DashboardGraphReadErrorV1::Corrupt {
            detail: "graph authority returned a generation for another exact scope".to_owned(),
        });
    }
    Ok(result)
}

fn expected_payload<T: DeserializeOwned>(
    read: DashboardGraphReadV1,
    select: impl FnOnce(DashboardGraphReadPayloadV1) -> Option<Value>,
) -> Result<GraphServiceReadV1<T>, DashboardGraphReadErrorV1> {
    let generation = read.generation;
    let payload = select(read.payload).ok_or_else(|| DashboardGraphReadErrorV1::Corrupt {
        detail: "graph authority returned the wrong read-model variant".to_owned(),
    })?;
    Ok(GraphServiceReadV1 {
        payload: decode_payload(payload)?,
        generation,
    })
}

pub async fn overview_payload(
    state: &DashboardState,
) -> Result<GraphServiceReadV1<GraphOverviewPayloadV1>, DashboardGraphReadErrorV1> {
    let read = read_graph(state, DashboardGraphReadOperationV1::Overview).await?;
    let projection = read.generation.projection_id.clone();
    expected_payload(read, |payload| match payload {
        DashboardGraphReadPayloadV1::Overview(overview) => Some(json!({
            "totals": overview.totals,
            "nodes_by_kind": overview.nodes_by_kind,
            "edges_by_kind": overview.edges_by_kind,
            "files_by_language": overview.files_by_language,
            "largest_files": overview.largest_files,
            // Retained wire field: it now identifies the verified projection,
            // never a database path.
            "path": projection,
            "top_connected": overview.top_connected,
        })),
        _ => None,
    })
}

pub async fn search_payload(
    state: &DashboardState,
    query: &str,
    limit: i64,
    offset: i64,
) -> Result<GraphServiceReadV1<GraphSearchPayloadV1>, DashboardGraphReadErrorV1> {
    let read = read_graph(
        state,
        DashboardGraphReadOperationV1::Search {
            query: query.to_owned(),
            limit,
            offset,
        },
    )
    .await?;
    expected_payload(read, |payload| match payload {
        DashboardGraphReadPayloadV1::Search(search) => {
            let count = search.results.len();
            Some(json!({
                "query": search.query,
                "limit": search.limit,
                "offset": search.offset,
                "total": search.total,
                "count": count,
                "results": search.results,
            }))
        }
        _ => None,
    })
}

pub async fn node_payload(
    state: &DashboardState,
    node_id: &str,
) -> Result<GraphServiceReadV1<Option<GraphNodePayloadV1>>, DashboardGraphReadErrorV1> {
    let read = read_graph(
        state,
        DashboardGraphReadOperationV1::Node {
            node_id: node_id.to_owned(),
        },
    )
    .await?;
    expected_payload(read, |payload| match payload {
        DashboardGraphReadPayloadV1::Node(node) => {
            Some(json!(node.map(|node| { json!({ "node": node }) })))
        }
        _ => None,
    })
}

pub async fn neighbors_payload(
    state: &DashboardState,
    node_id: &str,
    limit: i64,
) -> Result<GraphServiceReadV1<Option<GraphNeighborsPayloadV1>>, DashboardGraphReadErrorV1> {
    let read = read_graph(
        state,
        DashboardGraphReadOperationV1::Neighbors {
            node_id: node_id.to_owned(),
            limit,
        },
    )
    .await?;
    expected_payload(read, |payload| match payload {
        DashboardGraphReadPayloadV1::Neighbors(neighbors) => {
            Some(json!(neighbors.map(|neighbors| {
                json!({
                    "node_id": neighbors.node_id,
                    "depth": 1,
                    "limit": limit,
                    "callers": neighbors.callers,
                    "callees": neighbors.callees,
                    "edges": neighbors.edges,
                    "edges_by_kind": neighbors.edges_by_kind,
                })
            })))
        }
        _ => None,
    })
}

pub async fn subgraph_payload(
    state: &DashboardState,
    node_id: Option<String>,
    query: &str,
    node_limit: i64,
    edge_limit: i64,
) -> Result<GraphServiceReadV1<GraphSubgraphPayloadV1>, DashboardGraphReadErrorV1> {
    let read = read_graph(
        state,
        DashboardGraphReadOperationV1::Subgraph {
            node_id,
            query: query.to_owned(),
            node_limit,
            edge_limit,
        },
    )
    .await?;
    expected_payload(read, |payload| match payload {
        DashboardGraphReadPayloadV1::Subgraph(subgraph) => Some(json!({
            "seed_id": subgraph.seed_id,
            "mode": subgraph.mode,
            "nodes": subgraph.nodes,
            "edges": subgraph.edges,
            "capped": {
                "nodes": subgraph.nodes_capped,
                "edges": subgraph.edges_capped,
            },
            "limits": {
                "nodes": node_limit,
                "edges": edge_limit,
            },
        })),
        _ => None,
    })
}

pub async fn path_payload(
    state: &DashboardState,
    from: &str,
    to: &str,
    max_depth: i64,
) -> Result<GraphServiceReadV1<GraphPathPayloadV1>, DashboardGraphReadErrorV1> {
    let read = read_graph(
        state,
        DashboardGraphReadOperationV1::Path {
            from: from.to_owned(),
            to: to.to_owned(),
            max_depth,
        },
    )
    .await?;
    expected_payload(read, |payload| match payload {
        DashboardGraphReadPayloadV1::Path(path) => Some(json!({
            "from": path.from,
            "to": path.to,
            "found": path.found,
            "path": path.path,
            "nodes": path.nodes,
            "edges": path.edges,
            "max_depth": max_depth,
        })),
        _ => None,
    })
}

//! Typed read models for the structure-visualization graph surfaces.
//!
//! These routes preserve the distinction between a measured empty result, an
//! unavailable authority, and a failed read. The existing graph Explorer JSON
//! remains unchanged; new structure consumers receive `DashboardEnvelopeV1`.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardVersionV1, scope_from_state,
};
use super::util::{JsonPath, JsonQuery, query_rows};
use crate::graph::health::{dependency_depth, dsm_clusters};
use crate::graph::queries::GraphQueryManager;
use tracedecay_domain::code_intelligence::{Edge, Node};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::memory::entities::normalize_entity;

const MAX_CALL_CHAIN_DEPTH: usize = 20;
const TEST_CALLER_DEPTH: usize = 3;
const FACT_MATCH_LIMIT: usize = 100;
const STRATA_MAX_FILES: usize = 50_000;
const STRATA_MAX_DEPENDENCY_EDGES: usize = 250_000;
const STRATA_SCAN_BUDGET: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CallChainParamsV1 {
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    max_depth: Option<usize>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum StructureReadV1<T> {
    Measured {
        measurement: T,
    },
    Unmeasured {
        reason: &'static str,
        detail: String,
    },
    Failed {
        code: &'static str,
        detail: String,
        retryable: bool,
    },
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct NodeRefV1 {
    id: String,
    name: String,
    qualified_name: String,
    kind: String,
    file_path: String,
    start_line: u32,
}

impl From<&Node> for NodeRefV1 {
    fn from(node: &Node) -> Self {
        Self {
            id: node.id.clone(),
            name: node.name.clone(),
            qualified_name: node.qualified_name.clone(),
            kind: node.kind.as_str().to_string(),
            file_path: node.file_path.clone(),
            start_line: node.start_line,
        }
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct IncomingCallEdgeV1 {
    source: String,
    target: String,
    kind: &'static str,
    line: Option<u32>,
}

impl From<&Edge> for IncomingCallEdgeV1 {
    fn from(edge: &Edge) -> Self {
        Self {
            source: edge.source.clone(),
            target: edge.target.clone(),
            kind: "calls",
            line: edge.line,
        }
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct CallChainStepV1 {
    node: NodeRefV1,
    incoming_edge: Option<IncomingCallEdgeV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct CallChainMeasurementV1 {
    from_node_id: String,
    to_node_id: String,
    max_depth: usize,
    directed: bool,
    edge_kind: &'static str,
    selection: &'static str,
    found: bool,
    hop_count: Option<usize>,
    steps: Vec<CallChainStepV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct StrataFileV1 {
    path: String,
    depth: usize,
    scc_size: usize,
    chain: Vec<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct StrataClusterV1 {
    order: usize,
    directory: String,
    file_count: usize,
    internal_edges: usize,
    outgoing_edges: usize,
    incoming_edges: usize,
    boundary_edges: usize,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct StrataScanV1 {
    cache_scope: &'static str,
    cache_state: &'static str,
    budget_ms: u64,
    max_files: usize,
    max_dependency_edges: usize,
    files_examined: usize,
    dependency_edges_examined: usize,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct StrataMeasurementV1 {
    graph_generation: String,
    granularity: &'static str,
    dependency_edge_kinds: [&'static str; 2],
    algorithm: &'static str,
    cluster_ordering: &'static str,
    max_depth: usize,
    ideal_depth: usize,
    files: Vec<StrataFileV1>,
    clusters: Vec<StrataClusterV1>,
    scan: StrataScanV1,
}

#[derive(Clone, Debug)]
struct CachedStrataV1 {
    graph_generation: String,
    max_depth: usize,
    ideal_depth: usize,
    files: Vec<StrataFileV1>,
    clusters: Vec<StrataClusterV1>,
    files_examined: usize,
    dependency_edges_examined: usize,
}

static STRATA_CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<CachedStrataV1>>>> =
    OnceLock::new();

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct FactArmCoverageV1 {
    completeness: &'static str,
    returned: usize,
    truncated: bool,
    limit: usize,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct FactMatchArmV1 {
    match_basis: &'static str,
    strength: &'static str,
    collision_warning: &'static str,
    coverage: FactArmCoverageV1,
    facts: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct FactMatchesMeasurementV1 {
    node: NodeRefV1,
    name: String,
    normalized_name: String,
    granularity: &'static str,
    identity_semantics: &'static str,
    caption: &'static str,
    same_name_collision_possible: bool,
    entity_matches: Vec<Value>,
    payload_fts_matches: Vec<Value>,
    arms: Vec<FactMatchArmV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct CoveringTestV1 {
    id: String,
    name: String,
    file_path: String,
    start_line: u32,
    qualification: &'static str,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct TestMapMeasurementV1 {
    node: NodeRefV1,
    granularity: &'static str,
    algorithm: &'static str,
    caller_depth: usize,
    applicable: bool,
    reason: Option<&'static str>,
    tests: Vec<CoveringTestV1>,
    test_files: Vec<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct NodeSessionsMeasurementV1 {
    node: NodeRefV1,
    linkage: super::loom_api::LoomFileSessionProjectionV1,
    available_granularities: Vec<&'static str>,
    symbol_granularity_available: bool,
    symbol_granularity_reason: &'static str,
}

pub(super) struct RegisteredDashboardRouteContractV1 {
    pub(super) method: &'static str,
    pub(super) path: &'static str,
    pub(super) response_schema_name: fn() -> Cow<'static, str>,
}

fn response_schema_name<T: JsonSchema>() -> Cow<'static, str> {
    T::schema_name()
}

macro_rules! contracted_graph_routes {
    ($($path:literal => $handler:ident => $response:ty),+ $(,)?) => {
        pub(super) fn contracted_routes() -> Router<DashboardState> {
            Router::new()$(.route($path, get($handler)))+
        }

        pub(super) fn registered_route_contracts(
        ) -> &'static [RegisteredDashboardRouteContractV1] {
            &[
                $(RegisteredDashboardRouteContractV1 {
                    method: "GET",
                    path: $path,
                    response_schema_name: response_schema_name::<$response>,
                },)+
            ]
        }
    };
}

contracted_graph_routes! {
    "/api/plugins/graph/call-chain"
        => call_chain
        => StructureReadV1<CallChainMeasurementV1>,
    "/api/plugins/graph/strata"
        => strata
        => StructureReadV1<StrataMeasurementV1>,
    "/api/plugins/graph/node/{node_id}/facts"
        => node_facts
        => StructureReadV1<FactMatchesMeasurementV1>,
    "/api/plugins/graph/node/{node_id}/tests"
        => node_tests
        => StructureReadV1<TestMapMeasurementV1>,
    "/api/plugins/graph/node/{node_id}/sessions"
        => node_sessions
        => StructureReadV1<NodeSessionsMeasurementV1>,
}

/// `GET /api/plugins/graph/call-chain`
async fn call_chain(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<CallChainParamsV1>,
) -> Response {
    let from = params.from.trim();
    let to = params.to.trim();
    if from.is_empty() || to.is_empty() {
        return unmeasured_response::<CallChainMeasurementV1>(
            &state,
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "both from and to node ids are required",
        );
    }
    let max_depth = params
        .max_depth
        .unwrap_or(MAX_CALL_CHAIN_DEPTH)
        .clamp(1, MAX_CALL_CHAIN_DEPTH);
    let Some(graph) = state.project_graph.as_deref() else {
        return unmeasured_response::<CallChainMeasurementV1>(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_authority_unavailable",
            "the retained project graph is unavailable",
        );
    };

    let (from_node, to_node) = match tokio::try_join!(graph.get_node(from), graph.get_node(to)) {
        Ok(nodes) => nodes,
        Err(error) => {
            return failed_response::<CallChainMeasurementV1>(
                &state,
                "call_chain_endpoint_read_failed",
                error.to_string(),
                true,
            );
        }
    };
    if from_node.is_none() {
        return unmeasured_response::<CallChainMeasurementV1>(
            &state,
            StatusCode::NOT_FOUND,
            "node_not_found",
            &format!("source node not found: {from}"),
        );
    }
    if to_node.is_none() {
        return unmeasured_response::<CallChainMeasurementV1>(
            &state,
            StatusCode::NOT_FOUND,
            "node_not_found",
            &format!("target node not found: {to}"),
        );
    }

    let path = match graph.get_call_chain(from, to, max_depth).await {
        Ok(path) => path,
        Err(error) => {
            return failed_response::<CallChainMeasurementV1>(
                &state,
                "call_chain_endpoint_read_failed",
                error.to_string(),
                true,
            );
        }
    };
    let steps = path
        .as_ref()
        .map(|path| {
            path.iter()
                .map(|(node, edge)| CallChainStepV1 {
                    node: NodeRefV1::from(node),
                    incoming_edge: edge.as_ref().map(IncomingCallEdgeV1::from),
                })
                .collect()
        })
        .unwrap_or_default();
    measured_response(
        &state,
        CallChainMeasurementV1 {
            from_node_id: from.to_string(),
            to_node_id: to.to_string(),
            max_depth,
            directed: true,
            edge_kind: "calls",
            selection: "single_shortest_path",
            found: path.is_some(),
            hop_count: path.as_ref().map(|path| path.len().saturating_sub(1)),
            steps,
        },
        2,
        "endpoint nodes",
        None,
    )
}

/// `GET /api/plugins/graph/strata`
async fn strata(State(state): State<DashboardState>) -> Response {
    let Some(graph) = state.project_graph.as_deref() else {
        return unmeasured_response::<StrataMeasurementV1>(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_authority_unavailable",
            "the retained project graph is unavailable",
        );
    };
    let stats = match graph.get_stats().await {
        Ok(stats) => stats,
        Err(error) => {
            return failed_response::<StrataMeasurementV1>(
                &state,
                "strata_generation_read_failed",
                error.to_string(),
                true,
            );
        }
    };
    let graph_generation = format!(
        "{}:{}:{}:{}:{}",
        stats.last_sync_at,
        stats.last_updated,
        stats.node_count,
        stats.edge_count,
        stats.file_count
    );
    let cache = STRATA_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    let (snapshot, cache_state) = if let Some(existing) = guard.get(&state.graph_db_path)
        && existing.graph_generation == graph_generation
    {
        (existing.clone(), "hit")
    } else {
        let scan = match tokio::time::timeout(
            STRATA_SCAN_BUDGET,
            GraphQueryManager::new(graph.db())
                .build_file_adjacency_bounded(STRATA_MAX_FILES, STRATA_MAX_DEPENDENCY_EDGES),
        )
        .await
        {
            Ok(Ok(scan)) => scan,
            Ok(Err(error)) => {
                return failed_response::<StrataMeasurementV1>(
                    &state,
                    "strata_scan_failed",
                    error.to_string(),
                    false,
                );
            }
            Err(_) => {
                return failed_response::<StrataMeasurementV1>(
                    &state,
                    "strata_scan_timed_out",
                    format!(
                        "file adjacency scan exceeded the {}ms budget",
                        STRATA_SCAN_BUDGET.as_millis()
                    ),
                    true,
                );
            }
        };
        let depth = dependency_depth(&scan.adjacency, scan.adjacency.len());
        let mut files = Vec::with_capacity(scan.adjacency.len());
        for chain in &depth.chains {
            for path in &chain.scc_files {
                files.push(StrataFileV1 {
                    path: path.clone(),
                    depth: chain.depth,
                    scc_size: chain.scc_files.len(),
                    chain: chain.chain.clone(),
                });
            }
        }
        files.sort_by(|left, right| {
            right
                .depth
                .cmp(&left.depth)
                .then_with(|| left.path.cmp(&right.path))
        });
        let clusters = dsm_clusters(&scan.adjacency)
            .into_iter()
            .enumerate()
            .map(|(index, cluster)| {
                let boundary_edges = cluster.boundary_edges();
                StrataClusterV1 {
                    order: index,
                    directory: cluster.directory,
                    file_count: cluster.file_count,
                    internal_edges: cluster.internal_edges,
                    outgoing_edges: cluster.outgoing_edges,
                    incoming_edges: cluster.incoming_edges,
                    boundary_edges,
                }
            })
            .collect();
        let computed = Arc::new(CachedStrataV1 {
            graph_generation: graph_generation.clone(),
            max_depth: depth.max_depth,
            ideal_depth: depth.ideal_depth,
            files,
            clusters,
            files_examined: scan.files_examined,
            dependency_edges_examined: scan.dependency_edges_examined,
        });
        guard.insert(state.graph_db_path.clone(), computed.clone());
        (computed, "miss")
    };
    drop(guard);

    measured_response(
        &state,
        StrataMeasurementV1 {
            graph_generation: snapshot.graph_generation.clone(),
            granularity: "file",
            dependency_edge_kinds: ["calls", "uses"],
            algorithm: "tarjan_scc_then_longest_path",
            cluster_ordering: "dsm_boundary_edges_desc_then_file_count_desc",
            max_depth: snapshot.max_depth,
            ideal_depth: snapshot.ideal_depth,
            files: snapshot.files.clone(),
            clusters: snapshot.clusters.clone(),
            scan: StrataScanV1 {
                cache_scope: "graph_generation",
                cache_state,
                budget_ms: STRATA_SCAN_BUDGET.as_millis() as u64,
                max_files: STRATA_MAX_FILES,
                max_dependency_edges: STRATA_MAX_DEPENDENCY_EDGES,
                files_examined: snapshot.files_examined,
                dependency_edges_examined: snapshot.dependency_edges_examined,
            },
        },
        snapshot.files_examined as u64,
        "files",
        Some(snapshot.graph_generation.clone()),
    )
}

/// `GET /api/plugins/graph/node/{node_id}/facts`
async fn node_facts(
    State(state): State<DashboardState>,
    JsonPath(node_id): JsonPath<String>,
) -> Response {
    let Some(graph) = state.project_graph.as_deref() else {
        return unmeasured_response::<FactMatchesMeasurementV1>(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_authority_unavailable",
            "the retained project graph is unavailable",
        );
    };
    let node = match graph.get_node(&node_id).await {
        Ok(Some(node)) => node,
        Ok(None) => {
            return unmeasured_response::<FactMatchesMeasurementV1>(
                &state,
                StatusCode::NOT_FOUND,
                "node_not_found",
                &format!("node not found: {node_id}"),
            );
        }
        Err(error) => {
            return failed_response::<FactMatchesMeasurementV1>(
                &state,
                "fact_node_read_failed",
                error.to_string(),
                true,
            );
        }
    };
    let normalized_name = normalize_entity(&node.name).to_ascii_lowercase();
    let row_limit = i64::try_from(FACT_MATCH_LIMIT + 1).unwrap_or(i64::MAX);
    let connection = state.mem_db.engine_conn();
    let entity_sql = "
        SELECT CAST(f.fact_id AS TEXT) AS fact_id, f.content, f.category,
               f.trust_score, f.updated_at, f.source
        FROM memory_entities entity
        JOIN memory_fact_entities relation ON relation.entity_id = entity.entity_id
        JOIN memory_facts f ON f.fact_id = relation.fact_id
        WHERE entity.normalized_name = ?1
        ORDER BY f.updated_at DESC, f.fact_id DESC
        LIMIT ?2";
    let mut entity_rows = match query_rows(
        &connection,
        entity_sql,
        params![normalized_name.as_str(), row_limit],
    )
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            return failed_response::<FactMatchesMeasurementV1>(
                &state,
                "fact_entity_read_failed",
                error,
                true,
            );
        }
    };
    let entity_truncated = entity_rows.len() > FACT_MATCH_LIMIT;
    entity_rows.truncate(FACT_MATCH_LIMIT);

    let fts_query = format!("\"{}\"", node.name.replace('"', "\"\""));
    let fts_sql = "
        SELECT current.fact_id, payload.content, current.trust_score,
               current.updated_at, payload.payload_json
        FROM memory_v2_assertion_payloads_fts fts
        JOIN memory_v2_assertion_payloads payload ON payload.rowid = fts.rowid
        JOIN memory_v2_current_facts current
          ON current.fact_id = payload.fact_id
         AND current.owner_kind = payload.owner_kind
         AND current.project_id = payload.project_id
         AND current.active_assertion_id = payload.assertion_id
        WHERE memory_v2_assertion_payloads_fts MATCH ?1
          AND current.payload_access = 'eligible'
        ORDER BY bm25(memory_v2_assertion_payloads_fts), current.updated_at DESC
        LIMIT ?2";
    let mut fts_rows = match query_rows(&connection, fts_sql, params![fts_query, row_limit]).await {
        Ok(rows) => rows,
        Err(error) => {
            return failed_response::<FactMatchesMeasurementV1>(
                &state,
                "fact_payload_fts_read_failed",
                error,
                true,
            );
        }
    };
    let fts_truncated = fts_rows.len() > FACT_MATCH_LIMIT;
    fts_rows.truncate(FACT_MATCH_LIMIT);
    let entity_coverage = fact_arm_coverage(entity_rows.len(), entity_truncated);
    let fts_coverage = fact_arm_coverage(fts_rows.len(), fts_truncated);
    let measurement = FactMatchesMeasurementV1 {
        node: NodeRefV1::from(&node),
        name: node.name.clone(),
        normalized_name,
        granularity: "name_match",
        identity_semantics: "not_symbol_identity",
        caption: "citing this name",
        same_name_collision_possible: true,
        entity_matches: entity_rows.clone(),
        payload_fts_matches: fts_rows.clone(),
        arms: vec![
            FactMatchArmV1 {
                match_basis: "memory_entities.normalized_name",
                strength: "exact_normalized_name",
                collision_warning: "same-name symbols can share this match",
                coverage: entity_coverage,
                facts: entity_rows,
            },
            FactMatchArmV1 {
                match_basis: "memory_v2_assertion_payloads_fts",
                strength: "free_text_phrase",
                collision_warning: "text mentions are not symbol identity",
                coverage: fts_coverage,
                facts: fts_rows,
            },
        ],
    };
    measured_response(&state, measurement, 2, "name-match arms", None)
}

/// `GET /api/plugins/graph/node/{node_id}/tests`
async fn node_tests(
    State(state): State<DashboardState>,
    JsonPath(node_id): JsonPath<String>,
) -> Response {
    let Some(graph) = state.project_graph.as_deref() else {
        return unmeasured_response::<TestMapMeasurementV1>(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_authority_unavailable",
            "the retained project graph is unavailable",
        );
    };
    let node = match graph.get_node(&node_id).await {
        Ok(Some(node)) => node,
        Ok(None) => {
            return unmeasured_response::<TestMapMeasurementV1>(
                &state,
                StatusCode::NOT_FOUND,
                "node_not_found",
                &format!("node not found: {node_id}"),
            );
        }
        Err(error) => {
            return failed_response::<TestMapMeasurementV1>(
                &state,
                "test_map_node_read_failed",
                error.to_string(),
                true,
            );
        }
    };
    if !node.kind.is_callable_kind() {
        return measured_response(
            &state,
            TestMapMeasurementV1 {
                node: NodeRefV1::from(&node),
                granularity: "symbol",
                algorithm: "callers_depth_3_intersect_test_files_or_test_annotations",
                caller_depth: TEST_CALLER_DEPTH,
                applicable: false,
                reason: Some("source node is not callable"),
                tests: Vec::new(),
                test_files: Vec::new(),
            },
            1,
            "source symbols",
            None,
        );
    }

    let callers = match graph.get_callers(&node.id, TEST_CALLER_DEPTH).await {
        Ok(callers) => callers,
        Err(error) => {
            return failed_response::<TestMapMeasurementV1>(
                &state,
                "test_map_callers_read_failed",
                error.to_string(),
                true,
            );
        }
    };
    let caller_ids: Vec<String> = callers
        .iter()
        .map(|(caller, _)| caller.id.clone())
        .collect();
    let annotated = match graph.get_test_annotated_node_ids(&caller_ids).await {
        Ok(annotated) => annotated,
        Err(error) => {
            return failed_response::<TestMapMeasurementV1>(
                &state,
                "test_map_annotations_read_failed",
                error.to_string(),
                true,
            );
        }
    };
    let mut tests: Vec<CoveringTestV1> = callers
        .iter()
        .filter_map(|(caller, _)| {
            let qualification = if crate::tracedecay::is_test_file(&caller.file_path) {
                "test_file"
            } else if annotated.contains(&caller.id) {
                "test_annotation"
            } else {
                return None;
            };
            Some(CoveringTestV1 {
                id: caller.id.clone(),
                name: caller.name.clone(),
                file_path: caller.file_path.clone(),
                start_line: caller.start_line,
                qualification,
            })
        })
        .collect();
    tests.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.start_line.cmp(&right.start_line))
            .then_with(|| left.id.cmp(&right.id))
    });
    let test_files = tests
        .iter()
        .map(|test| test.file_path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    measured_response(
        &state,
        TestMapMeasurementV1 {
            node: NodeRefV1::from(&node),
            granularity: "symbol",
            algorithm: "callers_depth_3_intersect_test_files_or_test_annotations",
            caller_depth: TEST_CALLER_DEPTH,
            applicable: true,
            reason: None,
            tests,
            test_files,
        },
        1,
        "source symbols",
        None,
    )
}

/// `GET /api/plugins/graph/node/{node_id}/sessions`
async fn node_sessions(
    State(state): State<DashboardState>,
    JsonPath(node_id): JsonPath<String>,
) -> Response {
    let Some(graph) = state.project_graph.as_deref() else {
        return unmeasured_response::<NodeSessionsMeasurementV1>(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_authority_unavailable",
            "the retained project graph is unavailable",
        );
    };
    let node = match graph.get_node(&node_id).await {
        Ok(Some(node)) => node,
        Ok(None) => {
            return unmeasured_response::<NodeSessionsMeasurementV1>(
                &state,
                StatusCode::NOT_FOUND,
                "node_not_found",
                &format!("node not found: {node_id}"),
            );
        }
        Err(error) => {
            return failed_response::<NodeSessionsMeasurementV1>(
                &state,
                "session_node_read_failed",
                error.to_string(),
                true,
            );
        }
    };
    let Some(database) = state.lcm_db.as_deref() else {
        return unmeasured_response::<NodeSessionsMeasurementV1>(
            &state,
            StatusCode::SERVICE_UNAVAILABLE,
            "session_authority_unavailable",
            "the resolved project session authority is unavailable",
        );
    };
    let snapshot = match database.read_snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return failed_response::<NodeSessionsMeasurementV1>(
                &state,
                "session_snapshot_read_failed",
                error.to_string(),
                true,
            );
        }
    };
    let linkage = match super::loom_api::sessions_for_edited_file(&snapshot, &node.file_path).await
    {
        Ok(linkage) => linkage,
        Err(error) => {
            return failed_response::<NodeSessionsMeasurementV1>(
                &state,
                "session_linkage_read_failed",
                error,
                true,
            );
        }
    };
    let eligible = linkage.eligible_sessions;
    measured_response(
        &state,
        NodeSessionsMeasurementV1 {
            node: NodeRefV1::from(&node),
            linkage,
            available_granularities: vec!["file"],
            symbol_granularity_available: false,
            symbol_granularity_reason: "exact-source-occurrence anchors exist, but no indexed graph-node-to-session join is mounted",
        },
        eligible,
        "sessions with provider-native edited-file metadata",
        None,
    )
}

fn fact_arm_coverage(returned: usize, truncated: bool) -> FactArmCoverageV1 {
    FactArmCoverageV1 {
        completeness: if truncated { "partial" } else { "complete" },
        returned,
        truncated,
        limit: FACT_MATCH_LIMIT,
    }
}

fn measured_response<T: Serialize>(
    state: &DashboardState,
    measurement: T,
    eligible: u64,
    unit: &'static str,
    graph_version: Option<String>,
) -> Response {
    let mut envelope = DashboardEnvelopeV1::ready(
        scope_from_state(state),
        DashboardCoverageV1::complete(eligible, unit),
        StructureReadV1::Measured { measurement },
    );
    if let Some(graph_version) = graph_version {
        envelope = envelope.with_version(DashboardVersionV1 {
            entity_version: None,
            graph_version: Some(graph_version),
        });
    }
    Json(envelope).into_response()
}

fn unmeasured_response<T: Serialize>(
    state: &DashboardState,
    status: StatusCode,
    reason: &'static str,
    detail: &str,
) -> Response {
    (
        status,
        Json(DashboardEnvelopeV1::new(
            scope_from_state(state),
            DashboardDomainStateV1::Unknown,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            StructureReadV1::<T>::Unmeasured {
                reason,
                detail: detail.to_string(),
            },
        )),
    )
        .into_response()
}

fn failed_response<T: Serialize>(
    state: &DashboardState,
    code: &'static str,
    detail: String,
    retryable: bool,
) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(DashboardEnvelopeV1::new(
            scope_from_state(state),
            DashboardDomainStateV1::Error,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            StructureReadV1::<T>::Failed {
                code,
                detail,
                retryable,
            },
        )),
    )
        .into_response()
}

//! Typed read models for the structure-visualization graph surfaces.
//!
//! These routes preserve the distinction between a measured empty result, an
//! unavailable authority, and a failed read. The existing graph Explorer JSON
//! remains unchanged; new structure consumers receive `DashboardEnvelopeV1`.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardVersionV1, scope_from_state,
};
use super::util::{JsonPath, JsonQuery, query_rows};
use super::{DashboardHttpRequestControlV1, DashboardState};
use crate::graph::health::{dependency_depth, dsm_clusters};
use crate::graph::queries::GraphQueryManager;
use tracedecay_application::{CallableCodeOperationKind, callable_code_operation};
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSemanticEdgeV1, CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::memory::entities::normalize_entity;

const MAX_CALL_CHAIN_DEPTH: usize = 20;
const TEST_CALLER_DEPTH: usize = 3;
const FACT_MATCH_LIMIT: usize = 100;
const STRATA_MAX_FILES: usize = 50_000;
const STRATA_MAX_DEPENDENCY_EDGES: usize = 250_000;
const STRATA_SCAN_BUDGET_MS: u64 = 5_000;
const STRATA_SCAN_BUDGET: Duration = Duration::from_millis(STRATA_SCAN_BUDGET_MS);

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

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct IncomingCallEdgeV1 {
    source: String,
    target: String,
    kind: &'static str,
    line: Option<u32>,
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
    complete: bool,
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
    control: Option<Extension<DashboardHttpRequestControlV1>>,
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
    let control = match graph_control::<CallChainMeasurementV1>(&state, control) {
        Ok(control) => control,
        Err(response) => return *response,
    };
    let graph = match admitted_graph::<CallChainMeasurementV1>(
        &state,
        &control,
        CallableCodeOperationKind::Callees,
    )
    .await
    {
        Ok(graph) => graph,
        Err(response) => return response,
    };
    let from_occurrence = match SymbolOccurrenceId::new(from.to_owned()) {
        Ok(occurrence) => occurrence,
        Err(error) => {
            return unmeasured_response::<CallChainMeasurementV1>(
                &state,
                StatusCode::BAD_REQUEST,
                "invalid_node_identity",
                &error.to_string(),
            );
        }
    };
    let to_occurrence = match SymbolOccurrenceId::new(to.to_owned()) {
        Ok(occurrence) => occurrence,
        Err(error) => {
            return unmeasured_response::<CallChainMeasurementV1>(
                &state,
                StatusCode::BAD_REQUEST,
                "invalid_node_identity",
                &error.to_string(),
            );
        }
    };
    let from_node = match symbol_summary(&graph, &from_occurrence) {
        Ok(Some(node)) => node,
        Ok(None) => {
            return unmeasured_response::<CallChainMeasurementV1>(
                &state,
                StatusCode::NOT_FOUND,
                "node_not_found",
                &format!("source node not found: {from}"),
            );
        }
        Err(error) => return graph_error_response::<CallChainMeasurementV1>(&state, error),
    };
    match symbol_summary(&graph, &to_occurrence) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return unmeasured_response::<CallChainMeasurementV1>(
                &state,
                StatusCode::NOT_FOUND,
                "node_not_found",
                &format!("target node not found: {to}"),
            );
        }
        Err(error) => return graph_error_response::<CallChainMeasurementV1>(&state, error),
    }
    let path = match graph.reader.shortest_path(
        &from_occurrence,
        &to_occurrence,
        &[RelationEdgeKindV1::Calls],
        max_depth as u32,
        STRATA_MAX_DEPENDENCY_EDGES,
        Arc::clone(&graph.cancellation),
    ) {
        Ok(path) => path,
        Err(error) => {
            return graph_error_response::<CallChainMeasurementV1>(
                &state,
                crate::graph::map_projection_error(error),
            );
        }
    };
    let steps = match call_chain_steps(&state, &graph, from_node, path.path.as_deref()) {
        Ok(steps) => steps,
        Err(error) => return graph_error_response::<CallChainMeasurementV1>(&state, error),
    };
    let found = path.path.is_some();
    let hop_count = path.path.as_ref().map(Vec::len);
    measured_response(
        &state,
        CallChainMeasurementV1 {
            from_node_id: from.to_string(),
            to_node_id: to.to_string(),
            max_depth,
            directed: true,
            edge_kind: "calls",
            selection: "single_shortest_path",
            complete: path.complete,
            found,
            hop_count,
            steps,
        },
        2,
        "endpoint nodes",
        Some(graph.reader.generation().as_str().to_owned()),
    )
}

/// `GET /api/plugins/graph/strata`
async fn strata(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
) -> Response {
    let control = match graph_control::<StrataMeasurementV1>(&state, control) {
        Ok(control) => control,
        Err(response) => return *response,
    };
    let graph = match admitted_graph::<StrataMeasurementV1>(
        &state,
        &control,
        CallableCodeOperationKind::Facets,
    )
    .await
    {
        Ok(graph) => graph,
        Err(response) => return response,
    };
    let graph_generation = graph.reader.generation().as_str().to_owned();
    let cache_key = graph_generation.clone();
    let cache = STRATA_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    let (snapshot, cache_state) = if let Some(existing) = guard.get(&cache_key)
        && existing.graph_generation == graph_generation
    {
        (existing.clone(), "hit")
    } else {
        let scan = match tokio::time::timeout(
            STRATA_SCAN_BUDGET,
            GraphQueryManager::new(&graph.reader, Arc::clone(&graph.cancellation))
                .build_file_adjacency_bounded(STRATA_MAX_FILES, STRATA_MAX_DEPENDENCY_EDGES),
        )
        .await
        {
            Ok(Ok(scan)) => scan,
            Ok(Err(error)) => {
                return graph_runtime_error_response::<StrataMeasurementV1>(&state, error);
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
        guard.insert(cache_key, computed.clone());
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
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonPath(node_id): JsonPath<String>,
) -> Response {
    let control = match graph_control::<FactMatchesMeasurementV1>(&state, control) {
        Ok(control) => control,
        Err(response) => return *response,
    };
    let graph = match admitted_graph::<FactMatchesMeasurementV1>(
        &state,
        &control,
        CallableCodeOperationKind::ExactOccurrence,
    )
    .await
    {
        Ok(graph) => graph,
        Err(response) => return response,
    };
    let occurrence = match parse_occurrence::<FactMatchesMeasurementV1>(&state, &node_id) {
        Ok(occurrence) => occurrence,
        Err(response) => return *response,
    };
    let symbol = match symbol_summary(&graph, &occurrence) {
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
            return graph_error_response::<FactMatchesMeasurementV1>(&state, error);
        }
    };
    let node = match node_ref(&state, &symbol) {
        Ok(node) => node,
        Err(error) => return graph_error_response::<FactMatchesMeasurementV1>(&state, error),
    };
    let normalized_name = normalize_entity(&node.name).to_ascii_lowercase();
    let row_limit = i64::try_from(FACT_MATCH_LIMIT + 1).unwrap_or(i64::MAX);
    let connection = state.mem_db.read_connection();
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
    let fts_coverage = fact_arm_coverage(fts_rows.len(), fts_truncated);
    let node_name = node.name.clone();
    let measurement = FactMatchesMeasurementV1 {
        node,
        name: node_name,
        normalized_name,
        granularity: "name_match",
        identity_semantics: "not_symbol_identity",
        caption: "citing this name",
        same_name_collision_possible: true,
        entity_matches: Vec::new(),
        payload_fts_matches: fts_rows.clone(),
        arms: vec![FactMatchArmV1 {
            match_basis: "memory_v2_assertion_payloads_fts",
            strength: "free_text_phrase",
            collision_warning: "text mentions are not symbol identity",
            coverage: fts_coverage,
            facts: fts_rows,
        }],
    };
    measured_response(
        &state,
        measurement,
        1,
        "canonical fact-match arms",
        Some(graph.reader.generation().as_str().to_owned()),
    )
}

/// `GET /api/plugins/graph/node/{node_id}/tests`
async fn node_tests(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonPath(node_id): JsonPath<String>,
) -> Response {
    let control = match graph_control::<TestMapMeasurementV1>(&state, control) {
        Ok(control) => control,
        Err(response) => return *response,
    };
    let graph = match admitted_graph::<TestMapMeasurementV1>(
        &state,
        &control,
        CallableCodeOperationKind::Callers,
    )
    .await
    {
        Ok(graph) => graph,
        Err(response) => return response,
    };
    let occurrence = match parse_occurrence::<TestMapMeasurementV1>(&state, &node_id) {
        Ok(occurrence) => occurrence,
        Err(response) => return *response,
    };
    let symbol = match symbol_summary(&graph, &occurrence) {
        Ok(Some(node)) => node,
        Ok(None) => {
            return unmeasured_response::<TestMapMeasurementV1>(
                &state,
                StatusCode::NOT_FOUND,
                "node_not_found",
                &format!("node not found: {node_id}"),
            );
        }
        Err(error) => return graph_error_response::<TestMapMeasurementV1>(&state, error),
    };
    let node = match node_ref(&state, &symbol) {
        Ok(node) => node,
        Err(error) => return graph_error_response::<TestMapMeasurementV1>(&state, error),
    };
    let graph_version = Some(graph.reader.generation().as_str().to_owned());
    if !is_callable_kind(&node.kind) {
        return measured_response(
            &state,
            TestMapMeasurementV1 {
                node,
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
            graph_version,
        );
    }

    let callers = match graph.reader.impact(
        std::slice::from_ref(&occurrence),
        &[RelationEdgeKindV1::Calls],
        TEST_CALLER_DEPTH as u32,
        STRATA_MAX_FILES,
        STRATA_MAX_DEPENDENCY_EDGES,
        Arc::clone(&graph.cancellation),
    ) {
        Ok(callers) => callers,
        Err(error) => {
            return graph_error_response::<TestMapMeasurementV1>(
                &state,
                crate::graph::map_projection_error(error),
            );
        }
    };
    if !callers.complete && callers.impacted.len() == STRATA_MAX_FILES {
        return graph_error_response::<TestMapMeasurementV1>(
            &state,
            crate::graph::CodeGraphReadError::BudgetExhausted {
                detail: "test-map caller expansion exceeded its complete-symbol budget".to_owned(),
            },
        );
    }
    let caller_occurrences = callers
        .impacted
        .iter()
        .map(|caller| caller.summary.occurrence.clone())
        .collect::<Vec<_>>();
    let annotations = match graph.reader.callers(
        &caller_occurrences,
        &[RelationEdgeKindV1::Annotates],
        STRATA_MAX_DEPENDENCY_EDGES,
        Arc::clone(&graph.cancellation),
    ) {
        Ok(annotations) => annotations,
        Err(error) => {
            return graph_error_response::<TestMapMeasurementV1>(
                &state,
                crate::graph::map_projection_error(error),
            );
        }
    };
    if annotations.len() != callers.impacted.len() {
        return graph_error_response::<TestMapMeasurementV1>(
            &state,
            graph_reset_required("test-map annotation batches did not match their caller seeds"),
        );
    }
    let mut tests = Vec::new();
    for (caller, annotations) in callers.impacted.iter().zip(annotations) {
        let caller = match node_ref(&state, &caller.summary) {
            Ok(caller) => caller,
            Err(error) => return graph_error_response::<TestMapMeasurementV1>(&state, error),
        };
        let qualification = if crate::tracedecay::is_test_file(&caller.file_path) {
            "test_file"
        } else if match has_test_annotation(&annotations) {
            Ok(has_test_annotation) => has_test_annotation,
            Err(error) => return graph_error_response::<TestMapMeasurementV1>(&state, error),
        } {
            "test_annotation"
        } else {
            continue;
        };
        tests.push(CoveringTestV1 {
            id: caller.id,
            name: caller.name,
            file_path: caller.file_path,
            start_line: caller.start_line,
            qualification,
        });
    }
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
            node,
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
        graph_version,
    )
}

/// `GET /api/plugins/graph/node/{node_id}/sessions`
async fn node_sessions(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    JsonPath(node_id): JsonPath<String>,
) -> Response {
    let control = match graph_control::<NodeSessionsMeasurementV1>(&state, control) {
        Ok(control) => control,
        Err(response) => return *response,
    };
    let graph = match admitted_graph::<NodeSessionsMeasurementV1>(
        &state,
        &control,
        CallableCodeOperationKind::ExactOccurrence,
    )
    .await
    {
        Ok(graph) => graph,
        Err(response) => return response,
    };
    let occurrence = match parse_occurrence::<NodeSessionsMeasurementV1>(&state, &node_id) {
        Ok(occurrence) => occurrence,
        Err(response) => return *response,
    };
    let symbol = match symbol_summary(&graph, &occurrence) {
        Ok(Some(node)) => node,
        Ok(None) => {
            return unmeasured_response::<NodeSessionsMeasurementV1>(
                &state,
                StatusCode::NOT_FOUND,
                "node_not_found",
                &format!("node not found: {node_id}"),
            );
        }
        Err(error) => return graph_error_response::<NodeSessionsMeasurementV1>(&state, error),
    };
    let node = match node_ref(&state, &symbol) {
        Ok(node) => node,
        Err(error) => return graph_error_response::<NodeSessionsMeasurementV1>(&state, error),
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
            node,
            linkage,
            available_granularities: vec!["file"],
            symbol_granularity_available: false,
            symbol_granularity_reason: "exact-source-occurrence anchors exist, but no indexed graph-node-to-session join is mounted",
        },
        eligible,
        "sessions with provider-native edited-file metadata",
        Some(graph.reader.generation().as_str().to_owned()),
    )
}

struct AdmittedGraphReadV1 {
    reader: CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
}

fn graph_control<T: Serialize>(
    state: &DashboardState,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
) -> std::result::Result<DashboardHttpRequestControlV1, Box<Response>> {
    control.map(|Extension(control)| control).ok_or_else(|| {
        Box::new(unmeasured_response::<T>(
            state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_request_admission_unavailable",
            "dashboard HTTP request admission is unavailable",
        ))
    })
}

async fn admitted_graph<T: Serialize>(
    state: &DashboardState,
    control: &DashboardHttpRequestControlV1,
    operation_kind: CallableCodeOperationKind,
) -> std::result::Result<AdmittedGraphReadV1, Response> {
    let (Some(admission), Some(projection)) = (
        state.code_graph_read_admission.as_ref(),
        state.code_graph_projection_read_port.as_ref(),
    ) else {
        return Err(unmeasured_response::<T>(
            state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_authority_unavailable",
            "the exact-project verified code graph authority is unavailable",
        ));
    };
    let operation = callable_code_operation(operation_kind).map_err(|error| {
        graph_error_response::<T>(
            state,
            crate::graph::CodeGraphReadError::InvalidRequest {
                detail: error.to_string(),
            },
        )
    })?;
    let context = admission
        .admit(crate::graph::CodeGraphReadAdmissionRequest::new(
            &operation,
            control.request_id(),
            control.deadline(),
            control.cancellation(),
            control.observed_at(),
        ))
        .await
        .map_err(|error| graph_error_response::<T>(state, error))?;
    let cancellation = crate::graph::application_graph_cancellation(control.cancellation());
    let verified = projection
        .open(crate::graph::CodeGraphReadRequest::new(
            &context,
            control.observed_at(),
            Arc::clone(&cancellation),
        ))
        .await
        .map_err(|error| graph_error_response::<T>(state, error))?;
    let reader = verified
        .reader_with_cancellation(&context, control.observed_at(), Arc::clone(&cancellation))
        .map_err(|error| graph_error_response::<T>(state, error))?;
    Ok(AdmittedGraphReadV1 {
        reader,
        cancellation,
    })
}

fn parse_occurrence<T: Serialize>(
    state: &DashboardState,
    node_id: &str,
) -> std::result::Result<SymbolOccurrenceId, Box<Response>> {
    SymbolOccurrenceId::new(node_id.to_owned()).map_err(|error| {
        Box::new(unmeasured_response::<T>(
            state,
            StatusCode::BAD_REQUEST,
            "invalid_node_identity",
            &error.to_string(),
        ))
    })
}

fn symbol_summary(
    graph: &AdmittedGraphReadV1,
    occurrence: &SymbolOccurrenceId,
) -> std::result::Result<Option<CodeGraphSymbolSummaryV1>, crate::graph::CodeGraphReadError> {
    graph
        .reader
        .symbol_summary(occurrence, Arc::clone(&graph.cancellation))
        .map_err(crate::graph::map_projection_error)
}

fn node_ref(
    state: &DashboardState,
    symbol: &CodeGraphSymbolSummaryV1,
) -> std::result::Result<NodeRefV1, crate::graph::CodeGraphReadError> {
    let metadata = symbol.metadata.as_ref().ok_or_else(|| {
        graph_reset_required("verified graph symbol is missing its lineage metadata")
    })?;
    let binding = symbol.binding.as_ref().ok_or_else(|| {
        graph_reset_required("verified graph symbol is missing its source binding")
    })?;
    let file_path = binding.logical_path.clone().ok_or_else(|| {
        graph_reset_required("verified graph symbol binding is missing its logical path")
    })?;
    let source_span = binding.source_span.as_ref().ok_or_else(|| {
        graph_reset_required("verified graph symbol binding is missing its source span")
    })?;
    let source = std::fs::read(state.project_root.join(&file_path)).map_err(|error| {
        crate::graph::CodeGraphReadError::Stale {
            detail: format!(
                "verified graph source {file_path:?} cannot be read for line mapping: {error}"
            ),
        }
    })?;
    let start_line = line_for_byte_offset(&source, source_span.start_byte).ok_or_else(|| {
        graph_reset_required(
            "verified graph symbol source span exceeds the admitted source file bytes",
        )
    })?;
    Ok(NodeRefV1 {
        id: symbol.occurrence.as_str().to_owned(),
        name: simple_symbol_name(&metadata.qualified_name).to_owned(),
        qualified_name: metadata.qualified_name.clone(),
        kind: metadata.kind.clone(),
        file_path,
        start_line,
    })
}

fn call_chain_steps(
    state: &DashboardState,
    graph: &AdmittedGraphReadV1,
    from: CodeGraphSymbolSummaryV1,
    path: Option<&[tracedecay_domain::CanonicalRelationEdgeV1]>,
) -> std::result::Result<Vec<CallChainStepV1>, crate::graph::CodeGraphReadError> {
    let mut previous = node_ref(state, &from)?;
    let mut steps = vec![CallChainStepV1 {
        node: previous.clone(),
        incoming_edge: None,
    }];
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let mut previous_occurrence = from.occurrence;
    for edge in path {
        if edge.kind != RelationEdgeKindV1::Calls || edge.from_occurrence != previous_occurrence {
            return Err(graph_reset_required(
                "verified call path is not a contiguous directed calls path",
            ));
        }
        let target = symbol_summary(graph, &edge.to_occurrence)?.ok_or_else(|| {
            graph_reset_required("verified call path target symbol is missing from its generation")
        })?;
        let target_ref = node_ref(state, &target)?;
        let line = line_for_project_file(state, &previous.file_path, edge.evidence_span.start_byte);
        steps.push(CallChainStepV1 {
            node: target_ref.clone(),
            incoming_edge: Some(IncomingCallEdgeV1 {
                source: edge.from_occurrence.as_str().to_owned(),
                target: edge.to_occurrence.as_str().to_owned(),
                kind: "calls",
                line,
            }),
        });
        previous = target_ref;
        previous_occurrence = edge.to_occurrence.clone();
    }
    Ok(steps)
}

fn line_for_project_file(state: &DashboardState, file_path: &str, byte_offset: u64) -> Option<u32> {
    let source = std::fs::read(state.project_root.join(file_path)).ok()?;
    line_for_byte_offset(&source, byte_offset)
}

fn line_for_byte_offset(source: &[u8], byte_offset: u64) -> Option<u32> {
    let offset = usize::try_from(byte_offset).ok()?;
    let prefix = source.get(..offset)?;
    u32::try_from(prefix.iter().filter(|byte| **byte == b'\n').count()).ok()
}

fn simple_symbol_name(qualified_name: &str) -> &str {
    match qualified_name
        .rsplit("::")
        .next()
        .and_then(|name| name.rsplit('.').next())
    {
        Some(name) => name,
        None => qualified_name,
    }
}

fn is_callable_kind(kind: &str) -> bool {
    matches!(
        kind.to_ascii_lowercase().as_str(),
        "function" | "method" | "constructor" | "lambda" | "closure"
    )
}

fn has_test_annotation(
    annotations: &[CodeGraphSemanticEdgeV1],
) -> std::result::Result<bool, crate::graph::CodeGraphReadError> {
    for annotation in annotations {
        let metadata = annotation.neighbor.metadata.as_ref().ok_or_else(|| {
            graph_reset_required("verified annotation edge is missing source-symbol metadata")
        })?;
        if simple_symbol_name(&metadata.qualified_name).eq_ignore_ascii_case("test") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn graph_reset_required(detail: &str) -> crate::graph::CodeGraphReadError {
    crate::graph::CodeGraphReadError::ResetRequired {
        detail: detail.to_owned(),
    }
}

fn fact_arm_coverage(returned: usize, truncated: bool) -> FactArmCoverageV1 {
    FactArmCoverageV1 {
        completeness: if truncated { "partial" } else { "complete" },
        returned,
        truncated,
        limit: FACT_MATCH_LIMIT,
    }
}

fn graph_error_response<T: Serialize>(
    state: &DashboardState,
    error: crate::graph::CodeGraphReadError,
) -> Response {
    use crate::graph::CodeGraphReadError;

    match error {
        CodeGraphReadError::MissingRegistry => unmeasured_response::<T>(
            state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_registry_missing",
            "the exact-project code graph registry is unavailable",
        ),
        CodeGraphReadError::Unavailable { detail } => unmeasured_response::<T>(
            state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_authority_unavailable",
            &detail,
        ),
        CodeGraphReadError::Stale { detail } => unmeasured_response::<T>(
            state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_generation_stale",
            &detail,
        ),
        CodeGraphReadError::Cancelled => unmeasured_response::<T>(
            state,
            StatusCode::REQUEST_TIMEOUT,
            "graph_request_cancelled",
            "the verified code graph request was cancelled",
        ),
        CodeGraphReadError::TimedOut => unmeasured_response::<T>(
            state,
            StatusCode::GATEWAY_TIMEOUT,
            "graph_request_timed_out",
            "the verified code graph request exceeded its admitted deadline",
        ),
        CodeGraphReadError::BudgetExhausted { detail } => unmeasured_response::<T>(
            state,
            StatusCode::SERVICE_UNAVAILABLE,
            "graph_read_budget_exhausted",
            &detail,
        ),
        CodeGraphReadError::Denied => unmeasured_response::<T>(
            state,
            StatusCode::FORBIDDEN,
            "graph_read_denied",
            "the request is not authorized for this exact project graph",
        ),
        CodeGraphReadError::InvalidRequest { detail } => unmeasured_response::<T>(
            state,
            StatusCode::BAD_REQUEST,
            "invalid_graph_request",
            &detail,
        ),
        CodeGraphReadError::ResetRequired { detail } => {
            failed_response::<T>(state, "graph_reset_required", detail, false)
        }
        CodeGraphReadError::Corrupt { detail } => {
            failed_response::<T>(state, "graph_projection_corrupt", detail, false)
        }
    }
}

fn graph_runtime_error_response<T: Serialize>(
    state: &DashboardState,
    error: tracedecay_runtime_core::errors::TraceDecayError,
) -> Response {
    if let Some((_authority, reason)) = error.reset_required_context() {
        return graph_error_response::<T>(
            state,
            crate::graph::CodeGraphReadError::ResetRequired {
                detail: reason.to_owned(),
            },
        );
    }
    if let Some((reason_code, _retryable, detail)) = error.project_route_context() {
        let graph_error = match reason_code {
            "code-graph-registry-missing" => crate::graph::CodeGraphReadError::MissingRegistry,
            "code-graph-stale" => crate::graph::CodeGraphReadError::Stale {
                detail: detail.to_owned(),
            },
            "code-graph-cancelled" => crate::graph::CodeGraphReadError::Cancelled,
            "code-graph-timed-out" => crate::graph::CodeGraphReadError::TimedOut,
            "code-graph-budget-exhausted" => crate::graph::CodeGraphReadError::BudgetExhausted {
                detail: detail.to_owned(),
            },
            "code-graph-denied" => crate::graph::CodeGraphReadError::Denied,
            "code-graph-invalid-request" => crate::graph::CodeGraphReadError::InvalidRequest {
                detail: detail.to_owned(),
            },
            "code-graph-corrupt" => crate::graph::CodeGraphReadError::Corrupt {
                detail: detail.to_owned(),
            },
            "code-graph-reset-required" => crate::graph::CodeGraphReadError::ResetRequired {
                detail: detail.to_owned(),
            },
            _ => crate::graph::CodeGraphReadError::Unavailable {
                detail: detail.to_owned(),
            },
        };
        return graph_error_response::<T>(state, graph_error);
    }
    failed_response::<T>(state, "strata_scan_failed", error.to_string(), false)
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

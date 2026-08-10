use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tracedecay_application::{CallableCodeOperationKind, callable_code_operation};
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSemanticEdgeV1, CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_usecases::graph::{
    CodeGraphReadAdmissionRequest, CodeGraphReadError, CodeGraphReadRequest,
    application_graph_cancellation, map_projection_error,
};

use super::{DashboardHttpRequestControlV1, DashboardState};

const MAX_GRAPH_SYMBOLS: usize = 50_000;
const MAX_GRAPH_FILES: usize = 50_000;
const MAX_GRAPH_RELATIONS: usize = tracedecay_graph_db::MAX_VERIFIED_GENERATION_RELATIONS;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct GraphSpanV1 {
    start_line: i64,
    end_line: i64,
    start_column: Option<i64>,
    end_column: Option<i64>,
    attrs_start_line: Option<i64>,
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
    pub(super) generation: String,
}

struct AdmittedGraphReadV1 {
    reader: CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
}

async fn admitted_graph(
    state: &DashboardState,
    control: &DashboardHttpRequestControlV1,
    operation_kind: CallableCodeOperationKind,
) -> Result<AdmittedGraphReadV1, CodeGraphReadError> {
    let (Some(admission), Some(projection)) = (
        state.code_graph_read_admission.as_ref(),
        state.code_graph_projection_read_port.as_ref(),
    ) else {
        return Err(CodeGraphReadError::MissingRegistry);
    };
    let operation = callable_code_operation(operation_kind).map_err(|error| {
        CodeGraphReadError::InvalidRequest {
            detail: error.to_string(),
        }
    })?;
    let context = admission
        .admit(CodeGraphReadAdmissionRequest::new(
            &operation,
            control.request_id(),
            control.deadline(),
            control.cancellation(),
            control.observed_at(),
        ))
        .await?;
    let cancellation = application_graph_cancellation(control.cancellation());
    let verified = projection
        .open(CodeGraphReadRequest::new(
            &context,
            control.observed_at(),
            Arc::clone(&cancellation),
        ))
        .await?;
    let reader = verified.reader_with_cancellation(
        &context,
        control.observed_at(),
        Arc::clone(&cancellation),
    )?;
    Ok(AdmittedGraphReadV1 {
        reader,
        cancellation,
    })
}

pub async fn overview_payload(
    state: &DashboardState,
    control: &DashboardHttpRequestControlV1,
) -> Result<GraphServiceReadV1<GraphOverviewPayloadV1>, CodeGraphReadError> {
    let graph = admitted_graph(state, control, CallableCodeOperationKind::Facets).await?;
    let symbols = all_symbols(&graph)?;
    let occurrences: Vec<_> = symbols
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect();
    let edges = graph
        .reader
        .edges_among(
            &occurrences,
            &[],
            MAX_GRAPH_RELATIONS,
            Arc::clone(&graph.cancellation),
        )
        .map_err(map_projection_error)?;
    let files = graph
        .reader
        .files(MAX_GRAPH_FILES, Arc::clone(&graph.cancellation))
        .map_err(map_projection_error)?;

    let mut nodes_by_kind = BTreeMap::<String, i64>::new();
    let mut nodes_by_file = BTreeMap::<String, i64>::new();
    for symbol in &symbols {
        if let Some(metadata) = &symbol.metadata {
            *nodes_by_kind.entry(metadata.kind.clone()).or_default() += 1;
        }
        if let Some(path) = symbol
            .binding
            .as_ref()
            .and_then(|binding| binding.logical_path.clone())
        {
            *nodes_by_file.entry(path).or_default() += 1;
        }
    }
    let mut edges_by_kind = BTreeMap::<String, i64>::new();
    for edge in &edges {
        *edges_by_kind
            .entry(relation_kind_str(edge.edge.kind).to_owned())
            .or_default() += 1;
    }
    let mut files_by_language = BTreeMap::<String, i64>::new();
    for file in &files {
        if let Some(language) = &file.language {
            *files_by_language
                .entry(language.as_str().to_owned())
                .or_default() += 1;
        }
    }
    let ranking = graph
        .reader
        .degree_ranking(12, MAX_GRAPH_SYMBOLS, Arc::clone(&graph.cancellation))
        .map_err(map_projection_error)?;
    if !ranking.complete {
        return Err(CodeGraphReadError::BudgetExhausted {
            detail: format!(
                "dashboard overview examined {} symbols without completing the generation",
                ranking.symbols_examined
            ),
        });
    }
    let mut top_connected = Vec::with_capacity(ranking.ranked.len());
    for degree in ranking.ranked {
        let Some(summary) = graph
            .reader
            .symbol_summary(&degree.occurrence, Arc::clone(&graph.cancellation))
            .map_err(map_projection_error)?
        else {
            return Err(CodeGraphReadError::Corrupt {
                detail: format!("ranked symbol {} is absent", degree.occurrence),
            });
        };
        top_connected.push(node_from_summary(
            &summary,
            Some(degree.outgoing.saturating_add(degree.incoming)),
        )?);
    }
    let mut largest_files: Vec<_> = nodes_by_file
        .into_iter()
        .map(|(path, node_count)| GraphLargestFileV1 { path, node_count })
        .collect();
    largest_files.sort_by(|left, right| {
        right
            .node_count
            .cmp(&left.node_count)
            .then_with(|| left.path.cmp(&right.path))
    });
    largest_files.truncate(20);
    let generation = graph.reader.generation().as_str().to_owned();
    Ok(GraphServiceReadV1 {
        payload: GraphOverviewPayloadV1 {
            totals: GraphTotalsV1 {
                nodes: symbols.len() as u64,
                edges: edges.len() as u64,
                files: files.len() as u64,
            },
            nodes_by_kind: nodes_by_kind
                .into_iter()
                .map(|(kind, count)| GraphKindCountV1 { kind, count })
                .collect(),
            edges_by_kind: edges_by_kind
                .into_iter()
                .map(|(kind, count)| GraphKindCountV1 { kind, count })
                .collect(),
            files_by_language: files_by_language
                .into_iter()
                .map(|(language, count)| GraphLanguageCountV1 { language, count })
                .collect(),
            largest_files,
            path: generation.clone(),
            top_connected,
        },
        generation,
    })
}

pub async fn search_payload(
    state: &DashboardState,
    control: &DashboardHttpRequestControlV1,
    query: &str,
    limit: i64,
    offset: i64,
) -> Result<GraphServiceReadV1<GraphSearchPayloadV1>, CodeGraphReadError> {
    let graph = admitted_graph(state, control, CallableCodeOperationKind::SymbolSearch).await?;
    let needle = query.to_lowercase();
    let matched: Vec<_> = all_symbols(&graph)?
        .into_iter()
        .filter(|symbol| {
            needle.is_empty()
                || symbol.metadata.as_ref().is_some_and(|metadata| {
                    metadata.qualified_name.to_lowercase().contains(&needle)
                        || metadata.simple_name.to_lowercase().contains(&needle)
                })
        })
        .collect();
    let total = matched.len() as i64;
    let start = non_negative_usize(offset, "graph search offset")?.min(matched.len());
    let end = start
        .saturating_add(non_negative_usize(limit, "graph search limit")?)
        .min(matched.len());
    let selected = &matched[start..end];
    let occurrences: Vec<_> = selected
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect();
    let degree_by_id = degree_map(&graph, &occurrences)?;
    let results = selected
        .iter()
        .map(|symbol| node_from_summary(symbol, degree_by_id.get(&symbol.occurrence).copied()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(GraphServiceReadV1 {
        payload: GraphSearchPayloadV1 {
            query: query.to_owned(),
            limit,
            offset,
            total,
            count: results.len(),
            results,
        },
        generation: graph.reader.generation().as_str().to_owned(),
    })
}

pub async fn node_payload(
    state: &DashboardState,
    control: &DashboardHttpRequestControlV1,
    node_id: &str,
) -> Result<GraphServiceReadV1<Option<GraphNodePayloadV1>>, CodeGraphReadError> {
    let graph = admitted_graph(state, control, CallableCodeOperationKind::ExactOccurrence).await?;
    let occurrence = parse_occurrence(node_id)?;
    let summary = graph
        .reader
        .symbol_summary(&occurrence, Arc::clone(&graph.cancellation))
        .map_err(map_projection_error)?;
    let payload = if let Some(summary) = summary {
        let degrees = degree_map(&graph, std::slice::from_ref(&occurrence))?;
        Some(GraphNodePayloadV1 {
            node: node_from_summary(&summary, degrees.get(&occurrence).copied())?,
        })
    } else {
        None
    };
    Ok(GraphServiceReadV1 {
        payload,
        generation: graph.reader.generation().as_str().to_owned(),
    })
}

pub async fn neighbors_payload(
    state: &DashboardState,
    control: &DashboardHttpRequestControlV1,
    node_id: &str,
    limit: i64,
) -> Result<GraphServiceReadV1<Option<GraphNeighborsPayloadV1>>, CodeGraphReadError> {
    let graph = admitted_graph(state, control, CallableCodeOperationKind::Callers).await?;
    let occurrence = parse_occurrence(node_id)?;
    if graph
        .reader
        .symbol_summary(&occurrence, Arc::clone(&graph.cancellation))
        .map_err(map_projection_error)?
        .is_none()
    {
        return Ok(GraphServiceReadV1 {
            payload: None,
            generation: graph.reader.generation().as_str().to_owned(),
        });
    }
    let seeds = [occurrence.clone()];
    let callers = single_seed(
        graph
            .reader
            .callers(
                &seeds,
                &[],
                MAX_GRAPH_RELATIONS,
                Arc::clone(&graph.cancellation),
            )
            .map_err(map_projection_error)?,
    )?;
    let callees = single_seed(
        graph
            .reader
            .callees(
                &seeds,
                &[],
                MAX_GRAPH_RELATIONS,
                Arc::clone(&graph.cancellation),
            )
            .map_err(map_projection_error)?,
    )?;
    let mut neighbor_occurrences = BTreeSet::new();
    for edge in callers.iter().chain(&callees) {
        neighbor_occurrences.insert(edge.neighbor.occurrence.clone());
    }
    let neighbor_occurrences: Vec<_> = neighbor_occurrences.into_iter().collect();
    let degrees = degree_map(&graph, &neighbor_occurrences)?;
    let row_limit = non_negative_usize(limit, "graph neighbor limit")?;
    let hydrate =
        |edges: &[CodeGraphSemanticEdgeV1]| -> Result<Vec<GraphNodeV1>, CodeGraphReadError> {
            let mut rows: Vec<_> = edges
                .iter()
                .filter(|edge| edge.edge.kind == RelationEdgeKindV1::Calls)
                .map(|edge| {
                    let mut node = node_from_summary(
                        &edge.neighbor,
                        degrees.get(&edge.neighbor.occurrence).copied(),
                    )?;
                    node.edge_kind = Some(relation_kind_str(edge.edge.kind).to_owned());
                    Ok(node)
                })
                .collect::<Result<Vec<_>, CodeGraphReadError>>()?;
            rows.sort_by(|left, right| {
                left.qualified_name
                    .cmp(&right.qualified_name)
                    .then_with(|| left.id.cmp(&right.id))
            });
            rows.truncate(row_limit);
            Ok(rows)
        };
    let counts = graph
        .reader
        .edge_kind_counts(&occurrence, Arc::clone(&graph.cancellation))
        .map_err(map_projection_error)?;
    let mut merged = counts.outgoing;
    for (kind, count) in counts.incoming {
        *merged.entry(kind).or_default() += count;
    }
    let mut edges = Vec::with_capacity(callers.len() + callees.len());
    edges.extend(callers.iter().map(edge_from_semantic));
    edges.extend(callees.iter().map(edge_from_semantic));
    Ok(GraphServiceReadV1 {
        payload: Some(GraphNeighborsPayloadV1 {
            node_id: node_id.to_owned(),
            depth: 1,
            limit,
            callers: hydrate(&callers)?,
            callees: hydrate(&callees)?,
            edges,
            edges_by_kind: merged
                .into_iter()
                .map(|(kind, count)| GraphKindCountV1 {
                    kind: relation_kind_str(kind).to_owned(),
                    count: count as i64,
                })
                .collect(),
        }),
        generation: graph.reader.generation().as_str().to_owned(),
    })
}

pub async fn subgraph_payload(
    state: &DashboardState,
    control: &DashboardHttpRequestControlV1,
    node_id: Option<String>,
    query: &str,
    node_limit: i64,
    edge_limit: i64,
) -> Result<GraphServiceReadV1<GraphSubgraphPayloadV1>, CodeGraphReadError> {
    let graph = admitted_graph(state, control, CallableCodeOperationKind::Facets).await?;
    let node_budget = non_negative_usize(node_limit, "graph subgraph node limit")?;
    let edge_budget = non_negative_usize(edge_limit, "graph subgraph edge limit")?;
    let explicit_seed = node_id.filter(|value| !value.trim().is_empty());
    let seed = if let Some(seed) = explicit_seed {
        Some(parse_occurrence(&seed)?)
    } else if query.is_empty() {
        None
    } else {
        all_symbols(&graph)?
            .into_iter()
            .find(|symbol| symbol_matches(symbol, query))
            .map(|symbol| symbol.occurrence)
    };
    let (mode, seed_id, selected, nodes_capped) = if let Some(seed) = seed {
        if graph
            .reader
            .symbol_summary(&seed, Arc::clone(&graph.cancellation))
            .map_err(map_projection_error)?
            .is_none()
        {
            ("seeded", Some(seed.as_str().to_owned()), Vec::new(), false)
        } else {
            let seeds = [seed.clone()];
            let incoming = single_seed(
                graph
                    .reader
                    .callers(
                        &seeds,
                        &[],
                        MAX_GRAPH_RELATIONS,
                        Arc::clone(&graph.cancellation),
                    )
                    .map_err(map_projection_error)?,
            )?;
            let outgoing = single_seed(
                graph
                    .reader
                    .callees(
                        &seeds,
                        &[],
                        MAX_GRAPH_RELATIONS,
                        Arc::clone(&graph.cancellation),
                    )
                    .map_err(map_projection_error)?,
            )?;
            let mut candidates = vec![seed.clone()];
            for edge in incoming.iter().chain(&outgoing) {
                if !candidates.contains(&edge.neighbor.occurrence) {
                    candidates.push(edge.neighbor.occurrence.clone());
                }
            }
            let capped = candidates.len() > node_budget;
            candidates.truncate(node_budget);
            ("seeded", Some(seed.as_str().to_owned()), candidates, capped)
        }
    } else if !query.is_empty() {
        ("seeded", None, Vec::new(), false)
    } else {
        let ranking = graph
            .reader
            .degree_ranking(
                node_budget.max(1),
                MAX_GRAPH_SYMBOLS,
                Arc::clone(&graph.cancellation),
            )
            .map_err(map_projection_error)?;
        if !ranking.complete {
            return Err(CodeGraphReadError::BudgetExhausted {
                detail: "dashboard subgraph could not rank the complete generation".to_owned(),
            });
        }
        let selected = ranking
            .ranked
            .into_iter()
            .take(node_budget)
            .map(|degree| degree.occurrence)
            .collect::<Vec<_>>();
        let all = all_symbols(&graph)?;
        ("default", None, selected, all.len() > node_budget)
    };
    let summaries = summaries_for(&graph, &selected)?;
    let degrees = degree_map(&graph, &selected)?;
    let mut edges = if selected.is_empty() {
        Vec::new()
    } else {
        graph
            .reader
            .edges_among(
                &selected,
                &[],
                MAX_GRAPH_RELATIONS,
                Arc::clone(&graph.cancellation),
            )
            .map_err(map_projection_error)?
    };
    let edges_capped = edges.len() > edge_budget;
    edges.truncate(edge_budget);
    Ok(GraphServiceReadV1 {
        payload: GraphSubgraphPayloadV1 {
            seed_id,
            mode: mode.to_owned(),
            nodes: summaries
                .iter()
                .map(|summary| {
                    node_from_summary(summary, degrees.get(&summary.occurrence).copied())
                })
                .collect::<Result<Vec<_>, _>>()?,
            edges: edges.iter().map(edge_from_semantic).collect(),
            capped: GraphCappedV1 {
                nodes: nodes_capped,
                edges: edges_capped,
            },
            limits: GraphLimitsV1 {
                nodes: node_limit,
                edges: edge_limit,
            },
        },
        generation: graph.reader.generation().as_str().to_owned(),
    })
}

pub async fn path_payload(
    state: &DashboardState,
    control: &DashboardHttpRequestControlV1,
    from: &str,
    to: &str,
    max_depth: i64,
) -> Result<GraphServiceReadV1<GraphPathPayloadV1>, CodeGraphReadError> {
    let graph = admitted_graph(state, control, CallableCodeOperationKind::Callees).await?;
    let from_occurrence = parse_occurrence(from)?;
    let to_occurrence = parse_occurrence(to)?;
    let path = graph
        .reader
        .shortest_path(
            &from_occurrence,
            &to_occurrence,
            &[],
            u32::try_from(max_depth).map_err(|error| CodeGraphReadError::InvalidRequest {
                detail: format!("graph path depth is invalid: {error}"),
            })?,
            MAX_GRAPH_RELATIONS,
            Arc::clone(&graph.cancellation),
        )
        .map_err(map_projection_error)?;
    let (ids, summaries, edges) = if let Some(edges) = path.path {
        let mut ids = vec![from_occurrence.clone()];
        ids.extend(edges.iter().map(|edge| edge.to_occurrence.clone()));
        let summaries = summaries_for(&graph, &ids)?;
        (ids, summaries, edges)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    let degrees = degree_map(&graph, &ids)?;
    Ok(GraphServiceReadV1 {
        payload: GraphPathPayloadV1 {
            from: from.to_owned(),
            to: to.to_owned(),
            found: !ids.is_empty(),
            path: ids.iter().map(|id| id.as_str().to_owned()).collect(),
            nodes: summaries
                .iter()
                .map(|summary| {
                    node_from_summary(summary, degrees.get(&summary.occurrence).copied())
                })
                .collect::<Result<Vec<_>, _>>()?,
            edges: edges
                .iter()
                .map(|edge| GraphEdgeV1 {
                    source: edge.from_occurrence.as_str().to_owned(),
                    target: edge.to_occurrence.as_str().to_owned(),
                    kind: relation_kind_str(edge.kind).to_owned(),
                    line: None,
                    source_name: None,
                    target_name: None,
                })
                .collect(),
            max_depth,
        },
        generation: graph.reader.generation().as_str().to_owned(),
    })
}

fn all_symbols(
    graph: &AdmittedGraphReadV1,
) -> Result<Vec<CodeGraphSymbolSummaryV1>, CodeGraphReadError> {
    let page = graph
        .reader
        .symbols_page(None, MAX_GRAPH_SYMBOLS, Arc::clone(&graph.cancellation))
        .map_err(map_projection_error)?;
    if page.has_more {
        return Err(CodeGraphReadError::BudgetExhausted {
            detail: format!("dashboard graph exceeds the {MAX_GRAPH_SYMBOLS}-symbol census budget"),
        });
    }
    Ok(page.symbols)
}

fn parse_occurrence(value: &str) -> Result<SymbolOccurrenceId, CodeGraphReadError> {
    SymbolOccurrenceId::new(value.to_owned()).map_err(|error| CodeGraphReadError::InvalidRequest {
        detail: error.to_string(),
    })
}

fn single_seed(
    mut batches: Vec<Vec<CodeGraphSemanticEdgeV1>>,
) -> Result<Vec<CodeGraphSemanticEdgeV1>, CodeGraphReadError> {
    if batches.len() != 1 {
        return Err(CodeGraphReadError::Corrupt {
            detail: format!(
                "graph adjacency returned {} batches for one seed",
                batches.len()
            ),
        });
    }
    Ok(batches.remove(0))
}

fn summaries_for(
    graph: &AdmittedGraphReadV1,
    occurrences: &[SymbolOccurrenceId],
) -> Result<Vec<CodeGraphSymbolSummaryV1>, CodeGraphReadError> {
    let mut summaries = Vec::with_capacity(occurrences.len());
    for occurrence in occurrences {
        let Some(summary) = graph
            .reader
            .symbol_summary(occurrence, Arc::clone(&graph.cancellation))
            .map_err(map_projection_error)?
        else {
            return Err(CodeGraphReadError::Corrupt {
                detail: format!("selected symbol {occurrence} is absent"),
            });
        };
        summaries.push(summary);
    }
    Ok(summaries)
}

fn degree_map(
    graph: &AdmittedGraphReadV1,
    occurrences: &[SymbolOccurrenceId],
) -> Result<BTreeMap<SymbolOccurrenceId, u64>, CodeGraphReadError> {
    if occurrences.is_empty() {
        return Ok(BTreeMap::new());
    }
    Ok(graph
        .reader
        .degrees(occurrences, Arc::clone(&graph.cancellation))
        .map_err(map_projection_error)?
        .into_iter()
        .map(|degree| {
            (
                degree.occurrence,
                degree.outgoing.saturating_add(degree.incoming),
            )
        })
        .collect())
}

fn symbol_matches(symbol: &CodeGraphSymbolSummaryV1, query: &str) -> bool {
    let needle = query.to_lowercase();
    symbol.metadata.as_ref().is_some_and(|metadata| {
        metadata.qualified_name.to_lowercase().contains(&needle)
            || metadata.simple_name.to_lowercase().contains(&needle)
    })
}

fn node_from_summary(
    summary: &CodeGraphSymbolSummaryV1,
    degree: Option<u64>,
) -> Result<GraphNodeV1, CodeGraphReadError> {
    let metadata = summary
        .metadata
        .as_ref()
        .ok_or_else(|| CodeGraphReadError::Corrupt {
            detail: format!(
                "dashboard graph symbol {} has no published metadata",
                summary.occurrence
            ),
        })?;
    let start_line = Some(i64::from(metadata.start_line));
    let end_line = Some({
        i64::from(
            metadata
                .start_line
                .saturating_add(metadata.line_span.saturating_sub(1)),
        )
    });
    Ok(GraphNodeV1 {
        id: summary.occurrence.as_str().to_owned(),
        kind: metadata.kind.clone(),
        name: Some(metadata.simple_name.clone()),
        qualified_name: Some(metadata.qualified_name.clone()),
        file_path: summary
            .binding
            .as_ref()
            .and_then(|binding| binding.logical_path.clone()),
        start_line,
        end_line,
        start_column: None,
        end_column: None,
        attrs_start_line: start_line,
        doc: None,
        signature: metadata.signature.clone(),
        visibility: Some(metadata.visibility.clone()),
        is_async: None,
        branches: Some(i64::from(metadata.branches)),
        loops: Some(i64::from(metadata.loops)),
        returns: None,
        max_nesting: Some(i64::from(metadata.max_nesting)),
        unsafe_blocks: None,
        unchecked_calls: None,
        assertions: None,
        updated_at: None,
        parent_id: None,
        degree: degree.map(|value| value as i64),
        span: start_line
            .zip(end_line)
            .map(|(start_line, end_line)| GraphSpanV1 {
                start_line,
                end_line,
                start_column: None,
                end_column: None,
                attrs_start_line: None,
            }),
        edge_kind: None,
        edge_line: None,
    })
}

fn non_negative_usize(value: i64, field: &'static str) -> Result<usize, CodeGraphReadError> {
    usize::try_from(value).map_err(|error| CodeGraphReadError::InvalidRequest {
        detail: format!("{field} is invalid: {error}"),
    })
}

fn edge_from_semantic(edge: &CodeGraphSemanticEdgeV1) -> GraphEdgeV1 {
    let source = &edge.edge.from_occurrence;
    let target = &edge.edge.to_occurrence;
    GraphEdgeV1 {
        source: source.as_str().to_owned(),
        target: target.as_str().to_owned(),
        kind: relation_kind_str(edge.edge.kind).to_owned(),
        line: None,
        source_name: None,
        target_name: None,
    }
}

fn relation_kind_str(kind: RelationEdgeKindV1) -> &'static str {
    match kind {
        RelationEdgeKindV1::Calls => "calls",
        RelationEdgeKindV1::Uses => "uses",
        RelationEdgeKindV1::TypeOf => "type_of",
        RelationEdgeKindV1::Contains => "contains",
        RelationEdgeKindV1::Implements => "implements",
        RelationEdgeKindV1::Extends => "extends",
        RelationEdgeKindV1::Annotates => "annotates",
        RelationEdgeKindV1::Returns => "returns",
        RelationEdgeKindV1::Receives => "receives",
    }
}

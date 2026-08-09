use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::types::NodeKind;

use super::map_projection_error;

const MAX_ANALYTICAL_SYMBOLS: usize = 500_000;
const MAX_ANALYTICAL_RELATIONS: usize = 2_000_000;

#[derive(Debug, Clone)]
pub struct NodeMetrics {
    pub incoming_edge_count: usize,
    pub outgoing_edge_count: usize,
    pub call_count: usize,
    pub caller_count: usize,
    pub child_count: usize,
    pub depth: usize,
}

#[derive(Debug)]
pub struct FileAdjacencyScan {
    pub adjacency: HashMap<String, HashSet<String>>,
    pub files_examined: usize,
    pub dependency_edges_examined: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VerifiedHealthFileAggregateV1 {
    pub file_path: String,
    pub complexity: f64,
    pub function_methods: usize,
    pub skipped_function_methods: usize,
    pub dead_function_methods: usize,
}

/// Generation-pinned analytical queries over the verified Grafeo projection.
pub struct GraphQueryManager<'a> {
    reader: &'a CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
}

impl<'a> GraphQueryManager<'a> {
    pub fn new(
        reader: &'a CodeGraphInteractiveReader,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Self {
        Self {
            reader,
            cancellation,
        }
    }

    pub async fn find_dead_code(
        &self,
        kinds: &[NodeKind],
        include_public: bool,
        limit: Option<usize>,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        let mut after = None;
        let mut symbols = Vec::new();
        loop {
            let page = self
                .reader
                .symbols_page(
                    after.as_ref(),
                    MAX_ANALYTICAL_SYMBOLS,
                    Arc::clone(&self.cancellation),
                )
                .map_err(|error| {
                    super::map_code_graph_read_runtime_error(map_projection_error(error))
                })?;
            if symbols.len().saturating_add(page.symbols.len()) > MAX_ANALYTICAL_SYMBOLS {
                return Err(unavailable(
                    "verified dead-code census exceeded its analytical budget",
                ));
            }
            after = page.symbols.last().map(|symbol| symbol.occurrence.clone());
            symbols.extend(page.symbols);
            if !page.has_more {
                break;
            }
        }
        let occurrences = symbols
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        if symbols.iter().any(|symbol| {
            symbol.metadata.is_none()
                || symbol
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.logical_path.as_ref())
                    .is_none()
        }) {
            return Err(unavailable(
                "verified dead-code evidence is incomplete for one or more symbols",
            ));
        }
        let edges = self
            .reader
            .edges_among(
                &occurrences,
                &[
                    RelationEdgeKindV1::Calls,
                    RelationEdgeKindV1::Uses,
                    RelationEdgeKindV1::TypeOf,
                    RelationEdgeKindV1::Implements,
                    RelationEdgeKindV1::Extends,
                    RelationEdgeKindV1::Returns,
                    RelationEdgeKindV1::Receives,
                    RelationEdgeKindV1::Annotates,
                ],
                MAX_ANALYTICAL_RELATIONS,
                Arc::clone(&self.cancellation),
            )
            .map_err(|error| {
                super::map_code_graph_read_runtime_error(map_projection_error(error))
            })?;
        let test_markers = symbols
            .iter()
            .filter(|symbol| symbol.metadata.as_ref().is_some_and(is_test_marker))
            .map(|symbol| symbol.occurrence.clone())
            .collect::<HashSet<_>>();
        let test_annotated = edges
            .iter()
            .filter(|edge| {
                edge.edge.kind == RelationEdgeKindV1::Annotates
                    && test_markers.contains(&edge.edge.from_occurrence)
            })
            .map(|edge| edge.edge.to_occurrence.clone())
            .collect::<HashSet<_>>();
        let live_targets = edges
            .iter()
            .filter(|edge| edge.edge.kind != RelationEdgeKindV1::Annotates)
            .map(|edge| edge.edge.to_occurrence.clone())
            .collect::<HashSet<_>>();
        let kind_filter = kinds.iter().map(NodeKind::as_str).collect::<HashSet<_>>();
        let mut dead = symbols
            .into_iter()
            .filter(|symbol| {
                let Some(metadata) = symbol.metadata.as_ref() else {
                    return false;
                };
                (kind_filter.is_empty() || kind_filter.contains(metadata.kind.as_str()))
                    && (include_public || metadata.visibility != "public")
                    && metadata.simple_name != "main"
                    && !metadata.simple_name.starts_with("test")
                    && !test_annotated.contains(&symbol.occurrence)
                    && !live_targets.contains(&symbol.occurrence)
            })
            .collect::<Vec<_>>();
        dead.sort_by(|left, right| {
            let left_binding = left.binding.as_ref();
            let right_binding = right.binding.as_ref();
            left_binding
                .and_then(|binding| binding.logical_path.as_deref())
                .cmp(&right_binding.and_then(|binding| binding.logical_path.as_deref()))
                .then_with(|| {
                    left.metadata
                        .as_ref()
                        .map(|metadata| metadata.start_line)
                        .cmp(&right.metadata.as_ref().map(|metadata| metadata.start_line))
                })
                .then(left.occurrence.cmp(&right.occurrence))
        });
        if let Some(limit) = limit {
            dead.truncate(limit);
        }
        Ok(dead)
    }

    pub async fn get_node_metrics(&self, node_id: &str) -> Result<NodeMetrics> {
        let occurrence = SymbolOccurrenceId::new(node_id.to_owned()).map_err(|error| {
            TraceDecayError::Config {
                message: error.to_string(),
            }
        })?;
        let counts = self
            .reader
            .edge_kind_counts(&occurrence, Arc::clone(&self.cancellation))
            .map_err(|error| {
                super::map_code_graph_read_runtime_error(map_projection_error(error))
            })?;
        let incoming_edge_count =
            usize::try_from(counts.incoming.values().sum::<u64>()).unwrap_or(usize::MAX);
        let outgoing_edge_count =
            usize::try_from(counts.outgoing.values().sum::<u64>()).unwrap_or(usize::MAX);
        Ok(NodeMetrics {
            incoming_edge_count,
            outgoing_edge_count,
            call_count: usize::try_from(
                counts
                    .outgoing
                    .get(&RelationEdgeKindV1::Calls)
                    .copied()
                    .unwrap_or(0),
            )
            .unwrap_or(usize::MAX),
            caller_count: usize::try_from(
                counts
                    .incoming
                    .get(&RelationEdgeKindV1::Calls)
                    .copied()
                    .unwrap_or(0),
            )
            .unwrap_or(usize::MAX),
            child_count: usize::try_from(
                counts
                    .outgoing
                    .get(&RelationEdgeKindV1::Contains)
                    .copied()
                    .unwrap_or(0),
            )
            .unwrap_or(usize::MAX),
            depth: 0,
        })
    }

    pub async fn get_file_dependencies(&self, file_path: &str) -> Result<Vec<String>> {
        self.file_neighbors(file_path, false)
    }

    pub async fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>> {
        self.file_neighbors(file_path, true)
    }

    fn file_neighbors(&self, file_path: &str, incoming: bool) -> Result<Vec<String>> {
        let symbols = self
            .reader
            .symbols_in_logical_file(
                file_path,
                MAX_ANALYTICAL_SYMBOLS,
                Arc::clone(&self.cancellation),
            )
            .map_err(|error| {
                super::map_code_graph_read_runtime_error(map_projection_error(error))
            })?;
        let seeds = symbols
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        let edges = if incoming {
            self.reader.callers(
                &seeds,
                &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
                MAX_ANALYTICAL_RELATIONS,
                Arc::clone(&self.cancellation),
            )
        } else {
            self.reader.callees(
                &seeds,
                &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
                MAX_ANALYTICAL_RELATIONS,
                Arc::clone(&self.cancellation),
            )
        }
        .map_err(|error| super::map_code_graph_read_runtime_error(map_projection_error(error)))?;
        let mut paths = edges
            .into_iter()
            .flatten()
            .filter_map(|edge| edge.neighbor.binding?.logical_path)
            .filter(|path| path != file_path)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    }

    pub async fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>> {
        let adjacency = self.build_file_adjacency(None).await?;
        let mut cycles = super::scc::tarjan_scc(&adjacency)
            .into_iter()
            .filter(|component| super::scc::is_cyclic_scc(component, &adjacency))
            .collect::<Vec<_>>();
        for cycle in &mut cycles {
            cycle.sort_unstable();
        }
        Ok(cycles)
    }

    pub async fn build_file_adjacency(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<HashMap<String, HashSet<String>>> {
        Ok(self
            .build_file_adjacency_bounded(MAX_ANALYTICAL_SYMBOLS, MAX_ANALYTICAL_RELATIONS)
            .await?
            .adjacency
            .into_iter()
            .filter(|(source, _)| path_prefix.is_none_or(|prefix| source.starts_with(prefix)))
            .map(|(source, targets)| {
                let targets = targets
                    .into_iter()
                    .filter(|target| path_prefix.is_none_or(|prefix| target.starts_with(prefix)))
                    .collect();
                (source, targets)
            })
            .collect())
    }

    pub async fn build_file_adjacency_bounded(
        &self,
        max_files: usize,
        max_dependency_edges: usize,
    ) -> Result<FileAdjacencyScan> {
        let files = self
            .reader
            .files(max_files, Arc::clone(&self.cancellation))
            .map_err(|error| {
                super::map_code_graph_read_runtime_error(map_projection_error(error))
            })?;
        let mut adjacency = files
            .iter()
            .map(|file| (file.logical_path.clone(), HashSet::new()))
            .collect::<HashMap<_, _>>();
        let mut after = None;
        let mut symbols = Vec::new();
        loop {
            let page = self
                .reader
                .symbols_page(
                    after.as_ref(),
                    max_dependency_edges.max(1),
                    Arc::clone(&self.cancellation),
                )
                .map_err(|error| {
                    super::map_code_graph_read_runtime_error(map_projection_error(error))
                })?;
            if symbols.len().saturating_add(page.symbols.len()) > MAX_ANALYTICAL_SYMBOLS {
                return Err(unavailable(
                    "verified graph symbol census exceeded its analytical budget",
                ));
            }
            after = page.symbols.last().map(|symbol| symbol.occurrence.clone());
            symbols.extend(page.symbols);
            if !page.has_more {
                break;
            }
        }
        let occurrences = symbols
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        let edges = self
            .reader
            .edges_among(
                &occurrences,
                &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
                max_dependency_edges,
                Arc::clone(&self.cancellation),
            )
            .map_err(|error| {
                super::map_code_graph_read_runtime_error(map_projection_error(error))
            })?;
        let paths = symbols
            .into_iter()
            .filter_map(|symbol| Some((symbol.occurrence, symbol.binding?.logical_path?)))
            .collect::<HashMap<_, _>>();
        for edge in &edges {
            let (Some(source), Some(target)) = (
                paths.get(&edge.edge.from_occurrence),
                paths.get(&edge.edge.to_occurrence),
            ) else {
                continue;
            };
            if source != target {
                adjacency
                    .entry(source.clone())
                    .or_default()
                    .insert(target.clone());
            }
        }
        Ok(FileAdjacencyScan {
            files_examined: adjacency.len(),
            dependency_edges_examined: edges.len(),
            adjacency,
        })
    }

    /// Folds every health input from one immutable graph generation. Symbol
    /// metrics are parser-attested metadata; liveness and test annotations are
    /// derived from the same generation's canonical relation set.
    pub async fn health_file_aggregates(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<VerifiedHealthFileAggregateV1>> {
        let mut after = None;
        let mut symbols = Vec::new();
        loop {
            let page = self
                .reader
                .symbols_page(
                    after.as_ref(),
                    MAX_ANALYTICAL_SYMBOLS,
                    Arc::clone(&self.cancellation),
                )
                .map_err(|error| {
                    super::map_code_graph_read_runtime_error(map_projection_error(error))
                })?;
            if symbols.len().saturating_add(page.symbols.len()) > MAX_ANALYTICAL_SYMBOLS {
                return Err(unavailable(
                    "verified health symbol census exceeded its analytical budget",
                ));
            }
            after = page.symbols.last().map(|symbol| symbol.occurrence.clone());
            symbols.extend(page.symbols);
            if !page.has_more {
                break;
            }
        }
        let occurrences = symbols
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        let edges = self
            .reader
            .edges_among(
                &occurrences,
                &[
                    RelationEdgeKindV1::Calls,
                    RelationEdgeKindV1::Uses,
                    RelationEdgeKindV1::TypeOf,
                    RelationEdgeKindV1::Implements,
                    RelationEdgeKindV1::Extends,
                    RelationEdgeKindV1::Returns,
                    RelationEdgeKindV1::Receives,
                    RelationEdgeKindV1::Annotates,
                ],
                MAX_ANALYTICAL_RELATIONS,
                Arc::clone(&self.cancellation),
            )
            .map_err(|error| {
                super::map_code_graph_read_runtime_error(map_projection_error(error))
            })?;
        let metadata = symbols
            .iter()
            .filter_map(|symbol| {
                Some((
                    symbol.occurrence.clone(),
                    (
                        symbol.binding.as_ref()?.logical_path.clone()?,
                        symbol.metadata.as_ref()?,
                    ),
                ))
            })
            .collect::<HashMap<_, _>>();
        if metadata.len() != symbols.len() {
            return Err(unavailable(
                "verified health evidence is incomplete for one or more symbols",
            ));
        }
        let live_targets = edges
            .iter()
            .filter(|edge| edge.edge.kind != RelationEdgeKindV1::Annotates)
            .map(|edge| edge.edge.to_occurrence.clone())
            .collect::<HashSet<_>>();
        let test_markers = metadata
            .iter()
            .filter(|(_, (_, record))| is_test_marker(record))
            .map(|(occurrence, _)| occurrence.clone())
            .collect::<HashSet<_>>();
        let test_annotated = edges
            .iter()
            .filter(|edge| {
                edge.edge.kind == RelationEdgeKindV1::Annotates
                    && test_markers.contains(&edge.edge.from_occurrence)
            })
            .map(|edge| edge.edge.to_occurrence.clone())
            .collect::<HashSet<_>>();
        let mut by_file = HashMap::<String, VerifiedHealthFileAggregateV1>::new();
        for (occurrence, (file_path, record)) in metadata {
            if path_prefix.is_some_and(|prefix| !file_path.starts_with(prefix)) {
                continue;
            }
            let aggregate =
                by_file
                    .entry(file_path.clone())
                    .or_insert_with(|| VerifiedHealthFileAggregateV1 {
                        file_path,
                        ..VerifiedHealthFileAggregateV1::default()
                    });
            aggregate.complexity += f64::from(record.branches) * 2.0
                + f64::from(record.loops) * 2.0
                + f64::from(record.max_nesting) * 3.0
                + f64::from(record.line_span);
            if !matches!(record.kind.as_str(), "function" | "method") {
                continue;
            }
            aggregate.function_methods += 1;
            aggregate.skipped_function_methods += usize::from(record.skip_test_coverage);
            let entrypoint = record.simple_name == "main"
                || record.simple_name.starts_with("test")
                || record.visibility == "public"
                || test_annotated.contains(&occurrence);
            if !entrypoint && !live_targets.contains(&occurrence) {
                aggregate.dead_function_methods += 1;
            }
        }
        let mut aggregates = by_file.into_values().collect::<Vec<_>>();
        aggregates.sort_by(|left, right| left.file_path.cmp(&right.file_path));
        Ok(aggregates)
    }
}

fn is_test_marker(record: &tracedecay_code_index::lineage::LineageSymbolRecordV1) -> bool {
    record.kind == "annotation_usage"
        && matches!(
            record.simple_name.as_str(),
            "test" | "wasm_bindgen_test" | "rstest" | "parameterized"
        )
}

fn unavailable(detail: &str) -> TraceDecayError {
    TraceDecayError::ProjectRoute {
        reason_code: "verified-code-graph-evidence-unavailable".to_owned(),
        retryable: false,
        detail: detail.to_owned(),
    }
}

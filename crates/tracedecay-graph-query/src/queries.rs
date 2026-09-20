use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSemanticEdgeV1, CodeGraphSymbolPageV1,
    CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, RelationEdgeKindV1, SymbolOccurrenceId,
};
use tracedecay_graph_db::GraphCancellation;

use super::map_projection_error;

const MAX_ANALYTICAL_SYMBOLS: usize = 500_000;
const MAX_ANALYTICAL_RELATIONS: usize = 2_000_000;
const HEALTH_EDGE_KINDS: [RelationEdgeKindV1; 8] = [
    RelationEdgeKindV1::Calls,
    RelationEdgeKindV1::Uses,
    RelationEdgeKindV1::TypeOf,
    RelationEdgeKindV1::Implements,
    RelationEdgeKindV1::Extends,
    RelationEdgeKindV1::Returns,
    RelationEdgeKindV1::Receives,
    RelationEdgeKindV1::Annotates,
];

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
    /// Summed over symbols whose bounded complexity walk covered their body;
    /// the others are counted in `incomplete_complexity_symbols` instead of
    /// contributing lower-bound counters as if exact.
    pub complexity: f64,
    pub incomplete_complexity_symbols: usize,
    pub function_methods: usize,
    pub skipped_function_methods: usize,
    pub dead_function_methods: usize,
}

pub struct VerifiedHealthInputsV1 {
    pub adjacency: HashMap<String, HashSet<String>>,
    pub aggregates: Vec<VerifiedHealthFileAggregateV1>,
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

    #[hotpath::skip]
    pub fn generation(&self) -> &CodeGenerationId {
        self.reader.generation()
    }

    #[hotpath::measure(label = "usecases.graph.query.page")]
    pub fn page_all_symbols(
        &self,
        page_size: usize,
        overflow_detail: &str,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        let mut after = None;
        let mut symbols = Vec::new();
        loop {
            let page = self
                .reader
                .symbols_page(
                    after.as_ref(),
                    page_size.max(1),
                    Arc::clone(&self.cancellation),
                )
                .map_err(|error| {
                    super::map_code_graph_read_runtime_error(map_projection_error(error))
                })?;
            if symbols.len().saturating_add(page.symbols.len()) > MAX_ANALYTICAL_SYMBOLS {
                return Err(unavailable(overflow_detail));
            }
            after = page.symbols.last().map(|symbol| symbol.occurrence.clone());
            symbols.extend(page.symbols);
            if !page.has_more {
                return Ok(symbols);
            }
        }
    }

    pub fn symbols_in_logical_files_page(
        &self,
        logical_paths: &HashSet<String>,
        after: Option<&SymbolOccurrenceId>,
        limit: usize,
        max_symbols_examined: usize,
    ) -> Result<CodeGraphSymbolPageV1> {
        if limit == 0 || max_symbols_examined == 0 {
            return Err(invalid_request(
                "verified graph file-symbol paging requires positive limits",
            ));
        }
        let mut matched = Vec::new();
        for path in logical_paths {
            let budget = max_symbols_examined
                .checked_sub(matched.len())
                .filter(|remaining| *remaining > 0)
                .ok_or_else(|| {
                    budget_exhausted("verified graph file-symbol paging exceeded its scan budget")
                })?;
            let mut in_file = self
                .reader
                .symbols_in_logical_file(
                    path,
                    budget.saturating_add(1),
                    Arc::clone(&self.cancellation),
                )
                .map_err(|error| {
                    super::map_code_graph_read_runtime_error(map_projection_error(error))
                })?;
            if in_file.len() > budget {
                return Err(budget_exhausted(
                    "verified graph file-symbol paging exceeded its scan budget",
                ));
            }
            matched.append(&mut in_file);
        }
        matched.sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
        let mut symbols = Vec::with_capacity(limit.min(matched.len()));
        let mut has_more = false;
        for symbol in matched {
            if after.is_some_and(|after| symbol.occurrence <= *after) {
                continue;
            }
            if symbols.len() == limit {
                has_more = true;
                break;
            }
            symbols.push(symbol);
        }
        Ok(CodeGraphSymbolPageV1 { symbols, has_more })
    }

    /// Symbols no indexed relation reaches, narrowed to `path_prefix` before
    /// `limit` truncates the page.
    ///
    /// `path_prefix` scopes what is reported, never what counts as a reference:
    /// liveness is decided over every indexed symbol, so a call from outside
    /// the prefix still keeps a symbol inside it alive. Filtering the census
    /// before the relation scan would drop those incoming edges and report
    /// live symbols as dead.
    #[hotpath::measure(label = "usecases.graph.dead_code", future = true)]
    pub async fn find_dead_code(
        &self,
        kinds: &[NodeKind],
        include_public: bool,
        path_prefix: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        let symbols = hotpath::measure_block!("usecases.graph.dead_code.symbols", {
            self.page_all_symbols(
                MAX_ANALYTICAL_SYMBOLS,
                "verified dead-code census exceeded its analytical budget",
            )
        })?;
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
        let edges = hotpath::measure_block!("usecases.graph.dead_code.edges", {
            self.reader
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
                })
        })?;
        let test_markers = symbols
            .iter()
            .filter(|symbol| symbol.metadata.as_ref().is_some_and(is_test_marker))
            .map(|symbol| symbol.occurrence.clone())
            .collect::<HashSet<_>>();
        let test_annotated = edges
            .iter()
            .filter(|edge| {
                edge.kind == RelationEdgeKindV1::Annotates
                    && test_markers.contains(&edge.from_occurrence)
            })
            .map(|edge| edge.to_occurrence.clone())
            .collect::<HashSet<_>>();
        let live_targets = edges
            .iter()
            .filter(|edge| edge.kind != RelationEdgeKindV1::Annotates)
            .map(|edge| edge.to_occurrence.clone())
            .collect::<HashSet<_>>();
        let kind_filter = kinds.iter().map(NodeKind::as_str).collect::<HashSet<_>>();
        let mut dead = symbols
            .into_iter()
            .filter(|symbol| {
                let Some(metadata) = symbol.metadata.as_ref() else {
                    return false;
                };
                symbol
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.logical_path.as_deref())
                    .is_some_and(|path| {
                        tracedecay_runtime_core::path_scope::path_matches_scope(path, path_prefix)
                    })
                    && (kind_filter.is_empty() || kind_filter.contains(metadata.kind.as_str()))
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

    #[hotpath::measure(label = "usecases.graph.node_metrics", future = true)]
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

    #[hotpath::measure(label = "usecases.graph.file_dependencies", future = true)]
    pub async fn get_file_dependencies(&self, file_path: &str) -> Result<Vec<String>> {
        self.file_neighbors(file_path, false)
    }

    #[hotpath::measure(label = "usecases.graph.file_dependents", future = true)]
    pub async fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>> {
        self.file_neighbors(file_path, true)
    }

    fn file_neighbors(&self, file_path: &str, incoming: bool) -> Result<Vec<String>> {
        let symbols = hotpath::measure_block!("usecases.graph.file_neighbors.symbols", {
            self.reader
                .symbols_in_logical_file(
                    file_path,
                    MAX_ANALYTICAL_SYMBOLS,
                    Arc::clone(&self.cancellation),
                )
                .map_err(|error| {
                    super::map_code_graph_read_runtime_error(map_projection_error(error))
                })
        })?;
        let seeds = symbols
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        // `symbols_in_logical_file` answers a path this generation never
        // published with an empty vector by contract, and adjacency refuses an
        // empty seed list by contract. Without this the seam between the two
        // turned "no such file here" into a non-retryable invalid request.
        // `incoming_edges` and `edges_among` below already carry this guard;
        // this was the one adjacency call site missing it.
        if seeds.is_empty() {
            return Ok(Vec::new());
        }
        let edges = hotpath::measure_block!("usecases.graph.file_neighbors.edges", {
            if incoming {
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
            .map_err(|error| super::map_code_graph_read_runtime_error(map_projection_error(error)))
        })?;
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

    #[hotpath::measure(label = "usecases.graph.circular_dependencies", future = true)]
    pub async fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>> {
        let adjacency = hotpath::future!(
            self.build_file_adjacency(None),
            label = "usecases.graph.circular.adjacency"
        )
        .await?;
        let mut cycles = hotpath::measure_block!("usecases.graph.circular.scc", {
            super::scc::tarjan_scc_cancellable(&adjacency, self.cancellation.as_ref())
                .map_err(|super::scc::SccCancelled| {
                    super::map_code_graph_read_runtime_error(super::CodeGraphReadError::Cancelled)
                })?
                .into_iter()
                .filter(|component| super::scc::is_cyclic_scc(component, &adjacency))
                .collect::<Vec<_>>()
        });
        for cycle in &mut cycles {
            cycle.sort_unstable();
        }
        Ok(cycles)
    }

    #[hotpath::measure(label = "usecases.graph.file_adjacency", future = true)]
    pub async fn build_file_adjacency(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<HashMap<String, HashSet<String>>> {
        if let Some(path_prefix) = path_prefix {
            let files = hotpath::measure_block!("usecases.graph.adjacency.files", {
                self.reader
                    .files(MAX_ANALYTICAL_SYMBOLS, Arc::clone(&self.cancellation))
                    .map_err(|error| {
                        super::map_code_graph_read_runtime_error(map_projection_error(error))
                    })
            })?;
            let logical_paths = files
                .into_iter()
                .map(|file| file.logical_path)
                .filter(|path| path_is_within(path, path_prefix))
                .collect::<HashSet<_>>();
            let symbols = hotpath::measure_block!("usecases.graph.adjacency.symbols", {
                self.symbols_in_logical_files_page(
                    &logical_paths,
                    None,
                    MAX_ANALYTICAL_SYMBOLS,
                    MAX_ANALYTICAL_SYMBOLS,
                )
                .map(|page| page.symbols)
            })?;
            let edges = hotpath::measure_block!("usecases.graph.adjacency.edges", {
                self.incoming_edges(
                    &symbols,
                    &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
                )
            })?
            .into_iter()
            .map(|edge| edge.edge)
            .collect::<Vec<_>>();
            return Ok(file_adjacency(logical_paths, &symbols, &edges));
        }
        Ok(self
            .build_file_adjacency_bounded(MAX_ANALYTICAL_SYMBOLS, MAX_ANALYTICAL_RELATIONS)
            .await?
            .adjacency)
    }

    #[hotpath::skip]
    pub async fn build_file_adjacency_bounded(
        &self,
        max_files: usize,
        max_dependency_edges: usize,
    ) -> Result<FileAdjacencyScan> {
        let files = hotpath::measure_block!("usecases.graph.adjacency.files", {
            self.reader
                .files(max_files, Arc::clone(&self.cancellation))
                .map_err(|error| {
                    super::map_code_graph_read_runtime_error(map_projection_error(error))
                })
        })?;
        let symbols = hotpath::measure_block!("usecases.graph.adjacency.symbols", {
            self.page_all_symbols(
                max_dependency_edges.max(1),
                "verified graph symbol census exceeded its analytical budget",
            )
        })?;
        let occurrences = symbols
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        let edges = hotpath::measure_block!("usecases.graph.adjacency.edges", {
            self.reader
                .edges_among(
                    &occurrences,
                    &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
                    max_dependency_edges,
                    Arc::clone(&self.cancellation),
                )
                .map_err(|error| {
                    super::map_code_graph_read_runtime_error(map_projection_error(error))
                })
        })?;
        let dependency_edges_examined = edges.len();
        let adjacency = file_adjacency(
            files.into_iter().map(|file| file.logical_path).collect(),
            &symbols,
            &edges,
        );
        Ok(FileAdjacencyScan {
            files_examined: adjacency.len(),
            dependency_edges_examined,
            adjacency,
        })
    }

    /// Folds every health input from one immutable graph generation. Symbol
    /// metrics are parser-attested metadata; liveness and test annotations are
    /// derived from the same generation's canonical relation set.
    #[hotpath::measure(label = "usecases.graph.health_inputs", future = true)]
    pub async fn health_inputs(&self, path_prefix: Option<&str>) -> Result<VerifiedHealthInputsV1> {
        let files = hotpath::measure_block!("usecases.graph.health.files", {
            self.reader
                .files(MAX_ANALYTICAL_SYMBOLS, Arc::clone(&self.cancellation))
                .map_err(|error| {
                    super::map_code_graph_read_runtime_error(map_projection_error(error))
                })
        })?;
        let logical_paths = path_prefix.map(|prefix| {
            files
                .iter()
                .map(|file| file.logical_path.clone())
                .filter(|path| path_is_within(path, prefix))
                .collect::<HashSet<_>>()
        });
        let (symbols, edges, external_test_markers) =
            self.health_evidence(logical_paths.as_ref())?;
        let metadata = health_symbol_metadata(&symbols)?;
        let mut adjacency = files
            .into_iter()
            .map(|file| (file.logical_path, HashSet::new()))
            .collect::<HashMap<_, _>>();
        for edge in edges.iter().filter(|edge| {
            matches!(
                edge.kind,
                RelationEdgeKindV1::Calls | RelationEdgeKindV1::Uses
            )
        }) {
            let (Some((source, _)), Some((target, _))) = (
                metadata.get(&edge.from_occurrence),
                metadata.get(&edge.to_occurrence),
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
        adjacency.retain(|source, targets| {
            if path_prefix.is_some_and(|prefix| !path_is_within(source, prefix)) {
                return false;
            }
            targets
                .retain(|target| path_prefix.is_none_or(|prefix| path_is_within(target, prefix)));
            true
        });
        Ok(VerifiedHealthInputsV1 {
            adjacency,
            aggregates: fold_health_aggregates(
                metadata,
                &edges,
                external_test_markers,
                path_prefix,
            ),
        })
    }

    #[hotpath::measure(label = "usecases.graph.health_file_aggregates", future = true)]
    pub async fn health_file_aggregates(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<VerifiedHealthFileAggregateV1>> {
        let logical_paths = match path_prefix {
            Some(prefix) => Some(
                self.reader
                    .files(MAX_ANALYTICAL_SYMBOLS, Arc::clone(&self.cancellation))
                    .map_err(|error| {
                        super::map_code_graph_read_runtime_error(map_projection_error(error))
                    })?
                    .into_iter()
                    .map(|file| file.logical_path)
                    .filter(|path| path_is_within(path, prefix))
                    .collect::<HashSet<_>>(),
            ),
            None => None,
        };
        let (symbols, edges, external_test_markers) =
            self.health_evidence(logical_paths.as_ref())?;
        let metadata = health_symbol_metadata(&symbols)?;
        Ok(fold_health_aggregates(
            metadata,
            &edges,
            external_test_markers,
            path_prefix,
        ))
    }

    /// Health symbols, the induced edge set, and the test markers only the
    /// scoped `callers` walk can see: its far endpoints legitimately sit
    /// outside the scoped symbol census, so their marker metadata cannot be
    /// recovered from `symbols` the way the whole-generation branch's can.
    fn health_evidence(
        &self,
        logical_paths: Option<&HashSet<String>>,
    ) -> Result<(
        Vec<CodeGraphSymbolSummaryV1>,
        Vec<CanonicalRelationEdgeV1>,
        HashSet<SymbolOccurrenceId>,
    )> {
        let symbols = hotpath::measure_block!("usecases.graph.health.symbols", {
            match logical_paths {
                Some(paths) => self
                    .symbols_in_logical_files_page(
                        paths,
                        None,
                        MAX_ANALYTICAL_SYMBOLS,
                        MAX_ANALYTICAL_SYMBOLS,
                    )
                    .map(|page| page.symbols),
                None => self.page_all_symbols(
                    MAX_ANALYTICAL_SYMBOLS,
                    "verified health symbol census exceeded its analytical budget",
                ),
            }
        })?;
        let occurrences = symbols
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        let (edges, external_test_markers) =
            hotpath::measure_block!("usecases.graph.health.edges", {
                if logical_paths.is_some() {
                    self.incoming_edges(&symbols, &HEALTH_EDGE_KINDS)
                        .map(|edges| {
                            let markers = edges
                                .iter()
                                .filter(|edge| {
                                    edge.neighbor.occurrence == edge.edge.from_occurrence
                                        && edge
                                            .neighbor
                                            .metadata
                                            .as_ref()
                                            .is_some_and(is_test_marker)
                                })
                                .map(|edge| edge.edge.from_occurrence.clone())
                                .collect();
                            (edges.into_iter().map(|edge| edge.edge).collect(), markers)
                        })
                } else {
                    self.edges_among(&occurrences, &HEALTH_EDGE_KINDS)
                        .map(|edges| (edges, HashSet::new()))
                }
            })?;
        Ok((symbols, edges, external_test_markers))
    }

    fn incoming_edges(
        &self,
        symbols: &[CodeGraphSymbolSummaryV1],
        kinds: &[RelationEdgeKindV1],
    ) -> Result<Vec<CodeGraphSemanticEdgeV1>> {
        if symbols.is_empty() {
            return Ok(Vec::new());
        }
        let occurrences = symbols
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        self.reader
            .callers(
                &occurrences,
                kinds,
                MAX_ANALYTICAL_RELATIONS,
                Arc::clone(&self.cancellation),
            )
            .map(|edges| edges.into_iter().flatten().collect())
            .map_err(|error| super::map_code_graph_read_runtime_error(map_projection_error(error)))
    }

    fn edges_among(
        &self,
        occurrences: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
    ) -> Result<Vec<CanonicalRelationEdgeV1>> {
        if occurrences.is_empty() {
            return Ok(Vec::new());
        }
        self.reader
            .edges_among(
                occurrences,
                kinds,
                MAX_ANALYTICAL_RELATIONS,
                Arc::clone(&self.cancellation),
            )
            .map_err(|error| super::map_code_graph_read_runtime_error(map_projection_error(error)))
    }
}

fn file_adjacency(
    logical_paths: HashSet<String>,
    symbols: &[CodeGraphSymbolSummaryV1],
    edges: &[CanonicalRelationEdgeV1],
) -> HashMap<String, HashSet<String>> {
    let paths = symbols
        .iter()
        .filter_map(|symbol| {
            Some((
                symbol.occurrence.clone(),
                symbol.binding.as_ref()?.logical_path.clone()?,
            ))
        })
        .collect::<HashMap<_, _>>();
    let mut adjacency = logical_paths
        .into_iter()
        .map(|path| (path, HashSet::new()))
        .collect::<HashMap<_, _>>();
    for edge in edges {
        let (Some(source), Some(target)) = (
            paths.get(&edge.from_occurrence),
            paths.get(&edge.to_occurrence),
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
    adjacency
}

fn health_symbol_metadata(
    symbols: &[CodeGraphSymbolSummaryV1],
) -> Result<
    HashMap<
        SymbolOccurrenceId,
        (
            String,
            &tracedecay_code_index::lineage::LineageSymbolRecordV1,
        ),
    >,
> {
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
    Ok(metadata)
}

fn fold_health_aggregates(
    metadata: HashMap<
        SymbolOccurrenceId,
        (
            String,
            &tracedecay_code_index::lineage::LineageSymbolRecordV1,
        ),
    >,
    edges: &[CanonicalRelationEdgeV1],
    external_test_markers: HashSet<SymbolOccurrenceId>,
    path_prefix: Option<&str>,
) -> Vec<VerifiedHealthFileAggregateV1> {
    let live_targets = edges
        .iter()
        .filter(|edge| edge.kind != RelationEdgeKindV1::Annotates)
        .map(|edge| edge.to_occurrence.clone())
        .collect::<HashSet<_>>();
    let test_markers = metadata
        .iter()
        .filter(|(_, (_, record))| is_test_marker(record))
        .map(|(occurrence, _)| occurrence.clone())
        .chain(external_test_markers)
        .collect::<HashSet<_>>();
    let test_annotated = edges
        .iter()
        .filter(|edge| {
            edge.kind == RelationEdgeKindV1::Annotates
                && test_markers.contains(&edge.from_occurrence)
        })
        .map(|edge| edge.to_occurrence.clone())
        .collect::<HashSet<_>>();
    let mut by_file = HashMap::<String, VerifiedHealthFileAggregateV1>::new();
    for (occurrence, (file_path, record)) in metadata {
        if path_prefix.is_some_and(|prefix| !path_is_within(&file_path, prefix)) {
            continue;
        }
        let aggregate =
            by_file
                .entry(file_path.clone())
                .or_insert_with(|| VerifiedHealthFileAggregateV1 {
                    file_path,
                    ..VerifiedHealthFileAggregateV1::default()
                });
        match record.exact_complexity() {
            Some(complexity) => {
                aggregate.complexity += f64::from(complexity.branches) * 2.0
                    + f64::from(complexity.loops) * 2.0
                    + f64::from(complexity.max_nesting) * 3.0
                    + f64::from(record.line_span);
            }
            None => aggregate.incomplete_complexity_symbols += 1,
        }
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
    aggregates
}

pub fn is_test_marker(record: &tracedecay_code_index::lineage::LineageSymbolRecordV1) -> bool {
    tracedecay_code_index::is_test_marker(record)
}

fn unavailable(detail: &str) -> TraceDecayError {
    TraceDecayError::ProjectRoute {
        reason_code: "verified-code-graph-evidence-unavailable".to_owned(),
        retryable: false,
        detail: detail.to_owned(),
    }
}

fn invalid_request(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("code-graph-invalid-request", false, detail)
}

fn budget_exhausted(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("code-graph-budget-exhausted", false, detail)
}

fn path_is_within(path: &str, directory: &str) -> bool {
    Path::new(path).starts_with(directory)
}

#[cfg(test)]
mod path_scope_tests {
    use std::collections::{HashMap, HashSet};
    use std::fmt::Debug;

    use tracedecay_code_index::graph_projection::{
        CodeGraphSymbolBindingV1, CodeGraphSymbolSummaryV1,
    };
    use tracedecay_code_index::lineage::LineageSymbolRecordV1;
    use tracedecay_domain::{
        CanonicalRelationEdgeV1, ComplexityAnalysisV1, EdgeAuthorityV1, FileOccurrenceId,
        LanguageDescriptorRevision, RelationEdgeKindV1, SourceSpan, SymbolOccurrenceId,
    };

    use super::{file_adjacency, fold_health_aggregates, path_is_within};

    fn digest<T>(byte: char) -> T
    where
        T: TryFrom<String>,
        T::Error: Debug,
    {
        T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    fn metadata(occurrence: &str, name: &str, kind: &str) -> LineageSymbolRecordV1 {
        LineageSymbolRecordV1 {
            occurrence: SymbolOccurrenceId::new(occurrence).expect("occurrence"),
            identity: digest('1'),
            qualified_name: name.to_owned(),
            simple_name: name.to_owned(),
            kind: kind.to_owned(),
            visibility: "private".to_owned(),
            branches: 0,
            loops: 0,
            max_nesting: 0,
            complexity_analysis: ComplexityAnalysisV1::Complete,
            line_span: 1,
            start_line: 1,
            signature: None,
            docstring: None,
            is_async: false,
            derives: Vec::new(),
            skip_test_coverage: false,
            file_identity: digest('2'),
            content_digest: digest('3'),
        }
    }

    fn symbol(occurrence: &str, file: &str) -> CodeGraphSymbolSummaryV1 {
        CodeGraphSymbolSummaryV1 {
            occurrence: SymbolOccurrenceId::new(occurrence).expect("occurrence"),
            binding: Some(CodeGraphSymbolBindingV1 {
                file: FileOccurrenceId::new(format!("file.{occurrence}")).expect("file occurrence"),
                logical_path: Some(file.to_owned()),
                source_span: None,
                chunk: None,
                language_descriptor_revision: LanguageDescriptorRevision::new("language.rust.v1")
                    .expect("language revision"),
            }),
            metadata: None,
        }
    }

    fn edge(
        from: &CodeGraphSymbolSummaryV1,
        to: &CodeGraphSymbolSummaryV1,
    ) -> CanonicalRelationEdgeV1 {
        CanonicalRelationEdgeV1 {
            from_occurrence: from.occurrence.clone(),
            to_occurrence: to.occurrence.clone(),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        }
    }

    #[test]
    fn directory_scope_excludes_sibling_names_with_the_same_prefix() {
        assert!(path_is_within(
            "crates/tracedecay/src/lib.rs",
            "crates/tracedecay"
        ));
        assert!(path_is_within("crates/tracedecay", "crates/tracedecay"));
        assert!(!path_is_within(
            "crates/tracedecay-code-index/src/lib.rs",
            "crates/tracedecay"
        ));
    }

    #[test]
    fn scoped_adjacency_matches_whole_graph_induced_result_at_the_boundary() {
        let inside = symbol("symbol.inside", "src/scoped/inside.rs");
        let next = symbol("symbol.next", "src/scoped/next.rs");
        let outside = symbol("symbol.outside", "src/outside.rs");
        let edges = vec![edge(&inside, &next), edge(&outside, &inside)];
        let whole = file_adjacency(
            HashSet::from([
                "src/scoped/inside.rs".to_owned(),
                "src/scoped/next.rs".to_owned(),
                "src/outside.rs".to_owned(),
            ]),
            &[inside.clone(), next.clone(), outside],
            &edges,
        );
        let scoped = file_adjacency(
            HashSet::from([
                "src/scoped/inside.rs".to_owned(),
                "src/scoped/next.rs".to_owned(),
            ]),
            &[inside, next],
            &edges,
        );

        assert_eq!(
            scoped["src/scoped/inside.rs"],
            HashSet::from(["src/scoped/next.rs".to_owned()])
        );
        assert_eq!(
            scoped,
            whole
                .into_iter()
                .filter(|(source, _)| source.starts_with("src/scoped/"))
                .map(|(source, targets)| {
                    (
                        source,
                        targets
                            .into_iter()
                            .filter(|target| target.starts_with("src/scoped/"))
                            .collect(),
                    )
                })
                .collect()
        );
    }

    #[test]
    fn scoped_health_recognizes_an_external_test_marker_from_the_incoming_edge() {
        let inside_record = metadata("symbol.inside_test", "inside_test", "function");
        let marker_record = metadata("symbol.test_marker", "test", "annotation_usage");
        let mut inside = symbol("symbol.inside_test", "src/scoped/inside.rs");
        inside.metadata = Some(inside_record.clone());
        let mut marker = symbol("symbol.test_marker", "src/outside.rs");
        marker.metadata = Some(marker_record);
        let mut annotation = edge(&marker, &inside);
        annotation.kind = RelationEdgeKindV1::Annotates;
        let aggregates = fold_health_aggregates(
            HashMap::from([(
                inside.occurrence.clone(),
                ("src/scoped/inside.rs".to_owned(), &inside_record),
            )]),
            &[annotation],
            HashSet::from([marker.occurrence.clone()]),
            Some("src/scoped"),
        );

        assert_eq!(aggregates.len(), 1);
        assert_eq!(aggregates[0].function_methods, 1);
        assert_eq!(aggregates[0].dead_function_methods, 0);
    }
}

#[cfg(test)]
mod dead_code_scope_tests {
    use std::collections::HashMap;
    use std::fmt::Debug;
    use std::sync::Arc;

    use tracedecay_code_index::graph_projection::HermeticCodeGraphProjectionStore;
    use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
    use tracedecay_contracts::CancellationSignal;
    use tracedecay_domain::{
        BoundedSanitizedText, CanonicalRelationEdgeV1, CodeGenerationId, CodeSearchChunkAnchorV1,
        CodeSearchChunkGrainV1, CodeSearchChunkV1, ComplexityAnalysisV1, EdgeAuthorityV1,
        FileOccurrenceId, LanguageId, RelationEdgeKindV1, SanitizedCodeFileV1, SensitivityDecision,
        SensitivityLevelV1, SnapshotFileDispositionV1, SourceSpan, SymbolOccurrenceId,
    };
    use tracedecay_graph_db::NeverCancelled;

    use super::{CodeGraphInteractiveReader, GraphQueryManager, NodeKind};

    /// One unreferenced function the census should consider dead.
    struct FixtureSymbol {
        path: &'static str,
        name: &'static str,
    }

    fn fixture_id<T>(value: impl Into<String>) -> T
    where
        T: TryFrom<String>,
        T::Error: Debug,
    {
        T::try_from(value.into()).expect("valid fixture identity")
    }

    fn fixture_digest<T>(ordinal: usize) -> T
    where
        T: TryFrom<String>,
        T::Error: Debug,
    {
        fixture_id(format!("sha256:{ordinal:064x}"))
    }

    /// Publishes `symbols` into an in-memory generation, drawing a `Calls` edge
    /// for every `(caller, callee)` index pair, and returns a reader over it.
    fn fixture_reader(
        symbols: &[FixtureSymbol],
        calls: &[(usize, usize)],
    ) -> CodeGraphInteractiveReader {
        let generation: CodeGenerationId = fixture_id("generation.dead-code-scope.1");
        let mut files = Vec::new();
        let mut file_occurrences = HashMap::<&str, FileOccurrenceId>::new();
        let mut records = Vec::new();
        let mut chunks = Vec::new();
        let mut occurrences = Vec::new();

        for (ordinal, fixture) in symbols.iter().enumerate() {
            let file = file_occurrences
                .entry(fixture.path)
                .or_insert_with(|| {
                    let occurrence: FileOccurrenceId = fixture_id(format!("file.{}", files.len()));
                    files.push(SanitizedCodeFileV1 {
                        file_occurrence_id: occurrence.clone(),
                        logical_path: fixture.path.to_owned(),
                        language: Some(LanguageId::new("rust").expect("fixture language")),
                        content_digest: fixture_digest(5_000 + files.len()),
                        disposition: SnapshotFileDispositionV1::Present,
                    });
                    occurrence
                })
                .clone();
            let occurrence: SymbolOccurrenceId = fixture_id(format!("symbol.{ordinal}"));
            records.push(Arc::new(LineageSymbolRecordV1 {
                occurrence: occurrence.clone(),
                identity: fixture_digest(1_000 + ordinal),
                qualified_name: fixture.name.to_owned(),
                simple_name: fixture.name.to_owned(),
                kind: "function".to_owned(),
                visibility: "private".to_owned(),
                branches: 0,
                loops: 0,
                max_nesting: 0,
                complexity_analysis: ComplexityAnalysisV1::Complete,
                line_span: 1,
                start_line: u32::try_from(ordinal).expect("fixture line") + 1,
                signature: None,
                docstring: None,
                is_async: false,
                derives: Vec::new(),
                skip_test_coverage: false,
                file_identity: fixture_digest(3_000 + ordinal),
                content_digest: fixture_digest(2_000 + ordinal),
            }));
            chunks.push(Arc::new(CodeSearchChunkV1 {
                id: fixture_id(format!("chunk.{ordinal}")),
                anchor: CodeSearchChunkAnchorV1 {
                    generation_id: generation.clone(),
                    file_occurrence_id: file,
                    symbol_occurrence_id: Some(occurrence.clone()),
                    parent_chunk_id: None,
                    source_span: SourceSpan {
                        start_byte: ordinal as u64,
                        end_byte: ordinal as u64 + 1,
                    },
                    grain: CodeSearchChunkGrainV1::SymbolBody,
                    ordinal: u32::try_from(ordinal).expect("fixture ordinal"),
                },
                content_digest: fixture_digest(4_000 + ordinal),
                language_descriptor_revision: fixture_id("language.rust.fixture.v1"),
                chunker_revision: fixture_id("chunker.fixture.v1"),
                sanitizer_revision: fixture_id("sanitizer.fixture.v1"),
                sensitivity: SensitivityDecision {
                    level: SensitivityLevelV1::Public,
                    policy_revision: fixture_id("policy.fixture.v1"),
                },
                exact_terms: Vec::new(),
                subtokens: Vec::new(),
                sanitized_text: BoundedSanitizedText::new("fixture symbol")
                    .expect("bounded fixture text"),
            }));
            occurrences.push(occurrence);
        }

        let edges = calls
            .iter()
            .map(|(caller, callee)| CanonicalRelationEdgeV1 {
                from_occurrence: occurrences[*caller].clone(),
                to_occurrence: occurrences[*callee].clone(),
                kind: RelationEdgeKindV1::Calls,
                authority: EdgeAuthorityV1::SyntaxExact,
                evidence_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 1,
                },
            })
            .collect::<Vec<_>>();
        let index = GenerationSymbolIndexV1::new(generation.clone(), records)
            .expect("fixture symbol index");
        let cancellation =
            CancellationSignal::active("cancel.dead-code-scope").expect("fixture cancellation");
        let store =
            HermeticCodeGraphProjectionStore::memory(&cancellation).expect("fixture projection");
        store
            .publish_indexed_with_cancellation(
                &generation,
                &edges,
                &chunks,
                &files,
                &index,
                Arc::new(NeverCancelled),
            )
            .expect("publish fixture generation");
        store
            .verified_store(&generation)
            .expect("open fixture generation")
            .interactive_reader_with_cancellation(&generation, Arc::new(NeverCancelled))
            .expect("fixture reader")
    }

    fn reported_paths(symbols: &[super::CodeGraphSymbolSummaryV1]) -> Vec<&str> {
        symbols
            .iter()
            .map(|symbol| {
                symbol
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.logical_path.as_deref())
                    .expect("reported symbol carries its logical path")
            })
            .collect()
    }

    /// A fixture corpus that sorts ahead of product source used to consume the
    /// whole page: the limit was applied to the unfiltered census, so no
    /// `crates/` symbol was ever reported.
    #[tokio::test]
    async fn path_prefix_bounds_the_limit_to_the_filtered_symbols() {
        let reader = fixture_reader(
            &[
                FixtureSymbol {
                    path: "benchmark_data/index-bench/a.rs",
                    name: "bench_alpha",
                },
                FixtureSymbol {
                    path: "benchmark_data/index-bench/b.rs",
                    name: "bench_beta",
                },
                FixtureSymbol {
                    path: "crates/product/src/lib.rs",
                    name: "orphan_gamma",
                },
            ],
            &[],
        );
        let manager = GraphQueryManager::new(&reader, Arc::new(NeverCancelled));

        let unscoped = manager
            .find_dead_code(&[NodeKind::Function], false, None, Some(2))
            .await
            .expect("unscoped dead-code census");
        assert_eq!(
            reported_paths(&unscoped),
            vec![
                "benchmark_data/index-bench/a.rs",
                "benchmark_data/index-bench/b.rs"
            ],
            "an unscoped census keeps reporting the whole graph in path order"
        );

        let scoped = manager
            .find_dead_code(&[NodeKind::Function], false, Some("crates"), Some(2))
            .await
            .expect("scoped dead-code census");
        assert_eq!(
            reported_paths(&scoped),
            vec!["crates/product/src/lib.rs"],
            "the prefix must be applied before the limit truncates the page"
        );
    }

    /// Scoping the report must not turn an outside caller into no caller.
    #[tokio::test]
    async fn path_prefix_does_not_discard_callers_outside_the_prefix() {
        let reader = fixture_reader(
            &[
                FixtureSymbol {
                    path: "benchmark_data/index-bench/a.rs",
                    name: "bench_caller",
                },
                FixtureSymbol {
                    path: "crates/product/src/lib.rs",
                    name: "called_from_the_corpus",
                },
                FixtureSymbol {
                    path: "crates/product/src/orphan.rs",
                    name: "orphan_delta",
                },
            ],
            &[(0, 1)],
        );
        let manager = GraphQueryManager::new(&reader, Arc::new(NeverCancelled));

        let scoped = manager
            .find_dead_code(&[NodeKind::Function], false, Some("crates"), Some(10))
            .await
            .expect("scoped dead-code census");

        assert_eq!(
            reported_paths(&scoped),
            vec!["crates/product/src/orphan.rs"],
            "a symbol called only from outside the prefix is still alive"
        );
    }
}

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::tracedecay::TraceDecay;
use tracedecay_application::RequestContext;
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_code_index::graph_projection::{
    CodeGraphImpactBatchV1, CodeGraphInteractiveReader, CodeGraphSemanticEdgeV1,
    CodeGraphSymbolPageV1, CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::{CodeGenerationId, RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_domain::errors::Result;
use tracedecay_usecases::graph::queries::GraphQueryManager;
use tracedecay_usecases::graph::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionPort, CodeGraphReadAdmissionRequest,
    CodeGraphReadRequest, application_graph_cancellation, map_code_graph_read_runtime_error,
    map_projection_error,
};

pub(crate) struct VerifiedGraphQuery {
    reader: CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    request_context: RequestContext,
}

impl VerifiedGraphQuery {
    pub(crate) fn from_reader(
        reader: CodeGraphInteractiveReader,
        cancellation: Arc<dyn GraphCancellation>,
        request_context: RequestContext,
    ) -> Self {
        Self {
            reader,
            cancellation,
            request_context,
        }
    }

    pub(crate) fn request_context(&self) -> &RequestContext {
        &self.request_context
    }

    pub(crate) fn manager(&self) -> GraphQueryManager<'_> {
        GraphQueryManager::new(&self.reader, Arc::clone(&self.cancellation))
    }

    pub(crate) fn reader(&self) -> &CodeGraphInteractiveReader {
        &self.reader
    }

    pub(crate) fn cancellation(&self) -> Arc<dyn GraphCancellation> {
        Arc::clone(&self.cancellation)
    }

    #[hotpath::measure(label = "graph.query.dead_code", future = true)]
    pub(crate) async fn find_dead_code(
        &self,
        kinds: &[NodeKind],
        include_public: bool,
        limit: usize,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        self.manager()
            .find_dead_code(kinds, include_public, Some(limit))
            .await
    }

    #[hotpath::measure(label = "graph.query.circular", future = true)]
    pub(crate) async fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>> {
        self.manager().find_circular_dependencies().await
    }

    #[hotpath::measure(label = "graph.query.file_adjacency", future = true)]
    pub(crate) async fn build_file_adjacency(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<HashMap<String, HashSet<String>>> {
        self.manager().build_file_adjacency(path_prefix).await
    }

    pub(crate) fn generation(&self) -> &CodeGenerationId {
        self.reader.generation()
    }

    pub(crate) fn symbol_summary(
        &self,
        occurrence: &SymbolOccurrenceId,
    ) -> Result<Option<CodeGraphSymbolSummaryV1>> {
        self.reader
            .symbol_summary(occurrence, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    pub(crate) fn symbols_page(
        &self,
        after: Option<&SymbolOccurrenceId>,
        max_symbols: usize,
    ) -> Result<CodeGraphSymbolPageV1> {
        self.reader
            .symbols_page(after, max_symbols, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    /// Returns one stable page restricted to the requested logical files.
    ///
    /// Resolution goes through the per-file catalog index rather than the
    /// whole-corpus occurrence stream — a page for a handful of changed
    /// files must never hydrate every symbol in the generation. The canonical
    /// stream orders by occurrence, so sorting the per-file union preserves
    /// the exact page identity the stream scan produced.
    /// `max_symbols_examined` bounds the requested files' combined symbol
    /// count; exhausting it is a typed budget refusal rather than a false
    /// end-of-page result.
    #[hotpath::measure(label = "graph.query.file_symbols_page")]
    pub(crate) fn symbols_in_logical_files_page(
        &self,
        logical_paths: &HashSet<String>,
        after: Option<&SymbolOccurrenceId>,
        limit: usize,
        max_symbols_examined: usize,
    ) -> Result<CodeGraphSymbolPageV1> {
        if limit == 0 || max_symbols_examined == 0 {
            return Err(graph_invalid_request(
                "verified graph file-symbol paging requires positive limits",
            ));
        }
        if logical_paths.is_empty() {
            return Ok(CodeGraphSymbolPageV1 {
                symbols: Vec::new(),
                has_more: false,
            });
        }
        let mut matched = Vec::new();
        for path in logical_paths {
            let budget = max_symbols_examined
                .checked_sub(matched.len())
                .filter(|remaining| *remaining > 0)
                .ok_or_else(|| {
                    graph_budget_exhausted(
                        "verified graph file-symbol paging exceeded its scan budget",
                    )
                })?;
            let mut in_file = self.symbols_in_logical_file(path, budget.saturating_add(1))?;
            if in_file.len() > budget {
                return Err(graph_budget_exhausted(
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

    pub(crate) fn symbols_in_logical_file(
        &self,
        logical_path: &str,
        limit: usize,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        self.reader
            .symbols_in_logical_file(logical_path, limit, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    pub(crate) fn resolve_simple_name(
        &self,
        name: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        self.reader
            .resolve_simple_name(name, kind, limit, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    pub(crate) fn external_type_import_candidates(
        &self,
        query: &str,
        scope_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CodeIndexImportEvidenceV1>> {
        self.reader
            .external_type_import_candidates(
                query,
                scope_prefix,
                limit,
                Arc::clone(&self.cancellation),
            )
            .map_err(graph_projection_error)
    }

    pub(crate) fn resolve_qualified_name(
        &self,
        qualified_name: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        self.reader
            .resolve_qualified_name(qualified_name, kind, limit, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    #[hotpath::measure(label = "graph.query.callers")]
    pub(crate) fn callers(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
    ) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>> {
        self.reader
            .callers(seeds, kinds, max_relations, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    #[hotpath::measure(label = "graph.query.callees")]
    pub(crate) fn callees(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
    ) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>> {
        self.reader
            .callees(seeds, kinds, max_relations, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    pub(crate) fn edges_among(
        &self,
        occurrences: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
    ) -> Result<Vec<CodeGraphSemanticEdgeV1>> {
        self.reader
            .edges_among(
                occurrences,
                kinds,
                max_relations,
                Arc::clone(&self.cancellation),
            )
            .map_err(graph_projection_error)
    }

    #[hotpath::measure(label = "graph.query.impact")]
    pub(crate) fn impact(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_depth: u32,
        max_symbols: usize,
        max_relations_per_hop: usize,
    ) -> Result<CodeGraphImpactBatchV1> {
        self.reader
            .impact(
                seeds,
                kinds,
                max_depth,
                max_symbols,
                max_relations_per_hop,
                Arc::clone(&self.cancellation),
            )
            .map_err(graph_projection_error)
    }

    /// Finds files containing functions targeted by canonical annotation
    /// edges whose source is a recognized test annotation marker.
    ///
    /// A request scoped to specific files resolves through the per-file
    /// catalog index instead of the whole-corpus symbol stream: the four
    /// recognized markers are lexically attached attributes, so a marker
    /// always occupies the same file as the function it annotates, and only
    /// the requested files can contribute either endpoint. The unscoped
    /// census keeps the corpus sweep — that is its job.
    #[hotpath::measure(label = "graph.query.test_annotated_files")]
    pub(crate) fn test_annotated_logical_files(
        &self,
        logical_paths: Option<&HashSet<String>>,
        max_symbols: usize,
        max_relations: usize,
    ) -> Result<HashSet<String>> {
        let (symbols, scoped) = match logical_paths {
            Some(requested) => {
                let mut symbols = Vec::new();
                for path in requested {
                    let budget = max_symbols
                        .checked_sub(symbols.len())
                        .filter(|remaining| *remaining > 0)
                        .ok_or_else(|| {
                            graph_budget_exhausted(
                                "verified test-attribution census exceeded its symbol budget",
                            )
                        })?;
                    let mut in_file = self.symbols_in_logical_file(path, budget.saturating_add(1))?;
                    if in_file.len() > budget {
                        return Err(graph_budget_exhausted(
                            "verified test-attribution census exceeded its symbol budget",
                        ));
                    }
                    symbols.append(&mut in_file);
                }
                (symbols, true)
            }
            None => {
                let page = self.symbols_page(None, max_symbols)?;
                if page.has_more {
                    return Err(graph_budget_exhausted(
                        "verified test-attribution census exceeded its symbol budget",
                    ));
                }
                (page.symbols, false)
            }
        };
        let mut paths = HashMap::new();
        let mut test_markers = HashSet::new();
        for symbol in &symbols {
            let binding = symbol.binding.as_ref().ok_or_else(|| {
                graph_corrupt("verified graph symbol is missing its file binding")
            })?;
            let path = binding.logical_path.as_ref().ok_or_else(|| {
                graph_corrupt("verified graph symbol is missing its logical file path")
            })?;
            let metadata = symbol.metadata.as_ref().ok_or_else(|| {
                graph_corrupt("verified graph symbol is missing lineage metadata")
            })?;
            paths.insert(symbol.occurrence.clone(), path.clone());
            if metadata.kind == "annotation_usage"
                && matches!(
                    metadata.simple_name.as_str(),
                    "test" | "wasm_bindgen_test" | "rstest" | "parameterized"
                )
            {
                test_markers.insert(symbol.occurrence.clone());
            }
        }
        if scoped {
            if test_markers.is_empty() {
                return Ok(HashSet::new());
            }
            // Outgoing annotation edges from the scoped markers alone: the
            // corpus-seeded `edges_among` variant below needs every endpoint
            // in its seed set, which is exactly the full-corpus hydration a
            // file-scoped request must not pay.
            let markers = test_markers.iter().cloned().collect::<Vec<_>>();
            return Ok(self
                .callees(&markers, &[RelationEdgeKindV1::Annotates], max_relations)?
                .into_iter()
                .flatten()
                .filter_map(|edge| paths.get(&edge.edge.to_occurrence).cloned())
                .filter(|path| logical_paths.is_none_or(|requested| requested.contains(path)))
                .collect());
        }
        let occurrences = symbols
            .iter()
            .map(|symbol| symbol.occurrence.clone())
            .collect::<Vec<_>>();
        Ok(self
            .edges_among(
                &occurrences,
                &[RelationEdgeKindV1::Annotates],
                max_relations,
            )?
            .into_iter()
            .filter(|edge| test_markers.contains(&edge.edge.from_occurrence))
            .filter_map(|edge| paths.get(&edge.edge.to_occurrence).cloned())
            .filter(|path| logical_paths.is_none_or(|requested| requested.contains(path)))
            .collect())
    }
}

fn graph_projection_error(
    error: tracedecay_code_index::graph_projection::CodeGraphProjectionError,
) -> tracedecay_domain::errors::TraceDecayError {
    map_code_graph_read_runtime_error(map_projection_error(error))
}

fn graph_invalid_request(detail: &str) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::project_route(
        "code-graph-invalid-request",
        false,
        detail,
    )
}

fn graph_budget_exhausted(detail: &str) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::project_route(
        "code-graph-budget-exhausted",
        false,
        detail,
    )
}

fn graph_corrupt(detail: &str) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::project_route(
        "code-graph-corrupt",
        false,
        detail,
    )
}

impl TraceDecay {
    #[hotpath::measure(label = "graph.query.open_verified", future = true)]
    pub(crate) async fn open_verified_graph_query(
        &self,
        projection: &dyn CodeGraphProjectionReadPort,
        admission: &dyn CodeGraphReadAdmissionPort,
        operation: &tracedecay_application::ApplicationOperation,
        request_id: tracedecay_application::RequestId,
        deadline: tracedecay_application::Deadline,
        cancellation: &tracedecay_application::CancellationSignal,
    ) -> Result<VerifiedGraphQuery> {
        let observed_at = tracedecay_application::now_micros();
        let context = admission
            .admit(CodeGraphReadAdmissionRequest::new(
                operation,
                request_id,
                deadline,
                cancellation,
                observed_at,
            ))
            .await
            .map_err(map_code_graph_read_runtime_error)?;
        let graph_cancellation = application_graph_cancellation(cancellation);
        let verified = projection
            .open(CodeGraphReadRequest::new(
                &context,
                observed_at,
                Arc::clone(&graph_cancellation),
            ))
            .await
            .map_err(map_code_graph_read_runtime_error)?;
        let reader = verified
            .reader_with_cancellation(&context, observed_at, Arc::clone(&graph_cancellation))
            .map_err(map_code_graph_read_runtime_error)?;
        Ok(VerifiedGraphQuery::from_reader(
            reader,
            graph_cancellation,
            context,
        ))
    }
}

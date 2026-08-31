//! Generation-pinned verified graph query and the open-only admission port.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use tracedecay_application::{
    ApplicationOperation, CancellationSignal, Deadline, RequestContext, RequestId,
};
use tracedecay_code_index::chunks::CodeIndexImportEvidenceV1;
use tracedecay_code_index::graph_projection::{
    CodeGraphImpactBatchV1, CodeGraphInteractiveReader, CodeGraphSemanticEdgeV1,
    CodeGraphSymbolPageV1, CodeGraphSymbolSummaryV1,
};
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{CodeGenerationId, RelationEdgeKindV1, SymbolOccurrenceId};
use tracedecay_graph_db::GraphCancellation;

use super::queries::{GraphQueryManager, NodeMetrics, VerifiedHealthFileAggregateV1};
use super::source_authority::{
    AdmittedSourceAuthority, CodeGraphSourceAuthorityPort, CodeGraphSourceBindRequest,
    graph_source_scope_mismatch, graph_source_unbound,
};
use super::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionPort, CodeGraphReadAdmissionRequest,
    CodeGraphReadRequest, application_graph_cancellation, map_code_graph_read_runtime_error,
    map_projection_error,
};
#[cfg(any(test, feature = "test-helpers"))]
use crate::SourceReadRuntimePort;
use crate::context::read_modes;
use crate::context::source_read::{self, SourceReadOutput, SourceReadRequest};
use tracedecay_session_memory::context::{RequestInterruption, run_deadline_signal_interruptible};

/// Inputs required to admit and open one verified graph query.
#[derive(Clone)]
pub struct VerifiedGraphQueryRequest<'a> {
    pub operation: &'a ApplicationOperation,
    pub request_id: RequestId,
    pub deadline: Deadline,
    pub cancellation: &'a CancellationSignal,
}

impl<'a> VerifiedGraphQueryRequest<'a> {
    pub fn new(
        operation: &'a ApplicationOperation,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: &'a CancellationSignal,
    ) -> Self {
        Self {
            operation,
            request_id,
            deadline,
            cancellation,
        }
    }
}

pub type VerifiedGraphQueryFuture<'a> =
    Pin<Box<dyn Future<Output = Result<VerifiedGraphQuery>> + Send + 'a>>;

/// Open-only boundary for an admitted verified graph query.
///
/// Implementations close over projection and admission authorities. `open`
/// never names a composition-root type.
pub trait VerifiedGraphQueryPort: Send + Sync {
    fn open<'a>(&'a self, request: VerifiedGraphQueryRequest<'a>) -> VerifiedGraphQueryFuture<'a>;
}

impl<T> VerifiedGraphQueryPort for Arc<T>
where
    T: VerifiedGraphQueryPort + ?Sized,
{
    fn open<'a>(&'a self, request: VerifiedGraphQueryRequest<'a>) -> VerifiedGraphQueryFuture<'a> {
        (**self).open(request)
    }
}

/// Generation-pinned analytical queries over the verified Grafeo projection.
pub struct VerifiedGraphQuery {
    reader: CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    request_context: RequestContext,
    source: Option<AdmittedSourceAuthority>,
    live_cancellation: CancellationSignal,
    freshness: super::CodeGraphReadFreshnessV1,
}

impl VerifiedGraphQuery {
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn from_fixture_reader(
        reader: CodeGraphInteractiveReader,
        cancellation: Arc<dyn GraphCancellation>,
        request_context: RequestContext,
    ) -> Self {
        Self::from_admitted_parts(
            reader,
            cancellation,
            request_context,
            None,
            CancellationSignal::active("cancel.verified-graph-query.fixture")
                .expect("fixture cancellation"),
            super::CodeGraphReadFreshnessV1::Current,
        )
    }

    /// Fixture-only source binding. It runs the same freeze as the admitted
    /// open path, so even fixtures cannot retain a live runtime facade.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn with_source(mut self, source: Arc<dyn SourceReadRuntimePort>) -> Self {
        self.source = Some(
            AdmittedSourceAuthority::capture(&self.request_context, source.as_ref())
                .expect("fixture source authority matches the fixture scope"),
        );
        self
    }

    fn from_admitted_parts(
        reader: CodeGraphInteractiveReader,
        cancellation: Arc<dyn GraphCancellation>,
        request_context: RequestContext,
        source: Option<AdmittedSourceAuthority>,
        live_cancellation: CancellationSignal,
        freshness: super::CodeGraphReadFreshnessV1,
    ) -> Self {
        Self {
            reader,
            cancellation,
            request_context,
            source,
            live_cancellation,
            freshness,
        }
    }

    pub fn request_context(&self) -> &RequestContext {
        &self.request_context
    }

    /// Freshness of the generation this query answers from, as proven by the
    /// projection open: current, or the last complete seated generation
    /// served while the scheduler rebuilds.
    pub fn freshness(&self) -> super::CodeGraphReadFreshnessV1 {
        self.freshness
    }

    pub(crate) fn manager(&self) -> GraphQueryManager<'_> {
        GraphQueryManager::new(&self.reader, Arc::clone(&self.cancellation))
    }

    fn bound_source(&self) -> Result<&AdmittedSourceAuthority> {
        self.refuse_if_bound_closed()?;
        self.source.as_ref().ok_or_else(graph_source_unbound)
    }

    fn refuse_if_bound_closed(&self) -> Result<()> {
        refuse_if_query_closed(
            &self.request_context,
            self.request_context.deadline(),
            &self.live_cancellation,
        )
    }

    async fn await_bound<T>(&self, future: impl Future<Output = Result<T>>) -> Result<T> {
        self.refuse_if_bound_closed()?;
        match run_deadline_signal_interruptible(
            self.request_context.deadline(),
            &self.live_cancellation,
            future,
        )
        .await
        {
            Ok(result) => {
                self.refuse_if_bound_closed()?;
                result
            }
            Err(RequestInterruption::Cancelled) => Err(map_code_graph_read_runtime_error(
                super::CodeGraphReadError::Cancelled,
            )),
            Err(RequestInterruption::DeadlineExceeded) => Err(map_code_graph_read_runtime_error(
                super::CodeGraphReadError::TimedOut,
            )),
        }
    }

    pub fn project_root(&self) -> Result<&Path> {
        Ok(self.bound_source()?.project_root())
    }

    pub fn project_id(&self) -> Result<&str> {
        Ok(self.bound_source()?.project_id())
    }

    pub fn read_indexed_source_file(&self, file: &str) -> Result<String> {
        let (absolute, _) = self.resolve_indexed_source_file(file)?;
        tracedecay_runtime_core::sync::read_source_file(&absolute).map_err(|error| {
            TraceDecayError::Config {
                message: format!("cannot read indexed source file '{file}': {error}"),
            }
        })
    }

    #[hotpath::measure(label = "usecases.graph.verified.dead_code", future = true)]
    pub async fn find_dead_code(
        &self,
        kinds: &[NodeKind],
        include_public: bool,
        limit: usize,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        self.await_bound(
            self.manager()
                .find_dead_code(kinds, include_public, Some(limit)),
        )
        .await
    }

    #[hotpath::measure(label = "usecases.graph.verified.circular", future = true)]
    pub async fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>> {
        self.await_bound(self.manager().find_circular_dependencies())
            .await
    }

    #[hotpath::measure(label = "usecases.graph.verified.file_adjacency", future = true)]
    pub async fn build_file_adjacency(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<HashMap<String, HashSet<String>>> {
        self.await_bound(self.manager().build_file_adjacency(path_prefix))
            .await
    }

    #[hotpath::measure(label = "usecases.graph.verified.file_dependents", future = true)]
    pub async fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>> {
        self.await_bound(self.manager().get_file_dependents(file_path))
            .await
    }

    #[hotpath::measure(label = "usecases.graph.verified.file_dependencies", future = true)]
    pub async fn get_file_dependencies(&self, file_path: &str) -> Result<Vec<String>> {
        self.await_bound(self.manager().get_file_dependencies(file_path))
            .await
    }

    #[hotpath::measure(label = "usecases.graph.verified.node_metrics", future = true)]
    pub async fn get_node_metrics(&self, node_id: &str) -> Result<NodeMetrics> {
        self.await_bound(self.manager().get_node_metrics(node_id))
            .await
    }

    #[hotpath::measure(label = "usecases.graph.verified.health_aggregates", future = true)]
    pub async fn health_file_aggregates(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<VerifiedHealthFileAggregateV1>> {
        self.await_bound(self.manager().health_file_aggregates(path_prefix))
            .await
    }

    #[hotpath::measure(label = "usecases.graph.verified.health_snapshot", future = true)]
    pub async fn verified_health_snapshot(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<super::health::VerifiedHealthSnapshotV1> {
        self.await_bound(super::health::compute_verified_health_snapshot(
            &self.manager(),
            path_prefix,
        ))
        .await
    }

    pub async fn read_source(&self, request: SourceReadRequest<'_>) -> Result<SourceReadOutput> {
        let source = self.bound_source()?;
        if request.project_id != source.project_id() {
            return Err(graph_source_scope_mismatch());
        }
        self.await_bound(source_read::read_source(
            source.project_root(),
            source.db(),
            source.read_only(),
            &self.reader,
            Arc::clone(&self.cancellation),
            request,
        ))
        .await
    }

    pub fn resolve_indexed_source_file(&self, file: &str) -> Result<(PathBuf, String)> {
        let source = self.bound_source()?;
        let (absolute, display) = source_read::resolve_indexed_source_file(
            source.project_root(),
            &self.reader,
            Arc::clone(&self.cancellation),
            file,
        )?;
        if !absolute.starts_with(source.project_root()) {
            return Err(graph_source_scope_mismatch());
        }
        Ok((absolute, display))
    }

    pub fn render_map(&self, file_path: &str, kinds: Option<&[String]>) -> Result<Value> {
        self.refuse_if_bound_closed()?;
        read_modes::render_map(
            &self.reader,
            Arc::clone(&self.cancellation),
            file_path,
            kinds,
        )
    }

    pub fn render_signatures(&self, file_path: &str) -> Result<Value> {
        self.refuse_if_bound_closed()?;
        read_modes::render_signatures(&self.reader, Arc::clone(&self.cancellation), file_path)
    }

    pub fn generation(&self) -> &CodeGenerationId {
        self.reader.generation()
    }

    pub fn symbol_summary(
        &self,
        occurrence: &SymbolOccurrenceId,
    ) -> Result<Option<CodeGraphSymbolSummaryV1>> {
        self.refuse_if_bound_closed()?;
        self.reader
            .symbol_summary(occurrence, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    pub fn symbols_page(
        &self,
        after: Option<&SymbolOccurrenceId>,
        max_symbols: usize,
    ) -> Result<CodeGraphSymbolPageV1> {
        self.refuse_if_bound_closed()?;
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
    #[hotpath::measure(label = "usecases.graph.verified.file_symbols_page")]
    pub fn symbols_in_logical_files_page(
        &self,
        logical_paths: &HashSet<String>,
        after: Option<&SymbolOccurrenceId>,
        limit: usize,
        max_symbols_examined: usize,
    ) -> Result<CodeGraphSymbolPageV1> {
        self.refuse_if_bound_closed()?;
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

    pub fn symbols_in_logical_file(
        &self,
        logical_path: &str,
        limit: usize,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        self.refuse_if_bound_closed()?;
        self.reader
            .symbols_in_logical_file(logical_path, limit, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    pub fn resolve_simple_name(
        &self,
        name: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        self.refuse_if_bound_closed()?;
        self.reader
            .resolve_simple_name(name, kind, limit, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    pub fn external_type_import_candidates(
        &self,
        query: &str,
        scope_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CodeIndexImportEvidenceV1>> {
        self.refuse_if_bound_closed()?;
        self.reader
            .external_type_import_candidates(
                query,
                scope_prefix,
                limit,
                Arc::clone(&self.cancellation),
            )
            .map_err(graph_projection_error)
    }

    pub fn resolve_qualified_name(
        &self,
        qualified_name: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>> {
        self.refuse_if_bound_closed()?;
        self.reader
            .resolve_qualified_name(qualified_name, kind, limit, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    #[hotpath::measure(label = "usecases.graph.verified.callers")]
    pub fn callers(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
    ) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>> {
        self.refuse_if_bound_closed()?;
        self.reader
            .callers(seeds, kinds, max_relations, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    #[hotpath::measure(label = "usecases.graph.verified.callees")]
    pub fn callees(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
    ) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>> {
        self.refuse_if_bound_closed()?;
        self.reader
            .callees(seeds, kinds, max_relations, Arc::clone(&self.cancellation))
            .map_err(graph_projection_error)
    }

    pub fn edges_among(
        &self,
        occurrences: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
    ) -> Result<Vec<CodeGraphSemanticEdgeV1>> {
        self.refuse_if_bound_closed()?;
        self.reader
            .edges_among(
                occurrences,
                kinds,
                max_relations,
                Arc::clone(&self.cancellation),
            )
            .map_err(graph_projection_error)
    }

    #[hotpath::measure(label = "usecases.graph.verified.impact")]
    pub fn impact(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_depth: u32,
        max_symbols: usize,
        max_relations_per_hop: usize,
    ) -> Result<CodeGraphImpactBatchV1> {
        self.refuse_if_bound_closed()?;
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
    #[hotpath::measure(label = "usecases.graph.verified.test_annotated_files")]
    pub fn test_annotated_logical_files(
        &self,
        logical_paths: Option<&HashSet<String>>,
        max_symbols: usize,
        max_relations: usize,
    ) -> Result<HashSet<String>> {
        self.refuse_if_bound_closed()?;
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
                    let mut in_file =
                        self.symbols_in_logical_file(path, budget.saturating_add(1))?;
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

/// Admits the request and opens a verified query through the lower graph
/// ports. Every port wait — admission, source bind, and projection open — is
/// raced against the canonical deadline/cancellation pair, and fresh time is
/// rechecked after each acquisition.
///
/// The composition-root adapter closes over those ports; this function never
/// names a root type, and the source authority is frozen from the bind
/// result before the query exists — no later surface accepts a runtime,
/// root, or database.
#[hotpath::measure(label = "usecases.graph.open_verified", future = true)]
pub async fn open_verified_graph_query(
    admission: &dyn CodeGraphReadAdmissionPort,
    projection: &dyn CodeGraphProjectionReadPort,
    request: VerifiedGraphQueryRequest<'_>,
    source: Option<&dyn CodeGraphSourceAuthorityPort>,
) -> Result<VerifiedGraphQuery> {
    let observed_at = tracedecay_application::now_micros();
    let context = await_graph_port_wait(
        &request.deadline,
        request.cancellation,
        admission.admit(CodeGraphReadAdmissionRequest::new(
            request.operation,
            request.request_id.clone(),
            request.deadline.clone(),
            request.cancellation,
            observed_at,
        )),
    )
    .await?
    .map_err(map_code_graph_read_runtime_error)?;
    refuse_if_query_closed(&context, &request.deadline, request.cancellation)?;
    let source = match source {
        None => None,
        Some(port) => {
            let observed_at = tracedecay_application::now_micros();
            let runtime = await_graph_port_wait(
                &request.deadline,
                request.cancellation,
                port.bind(CodeGraphSourceBindRequest {
                    context: &context,
                    observed_at,
                }),
            )
            .await?
            .map_err(map_code_graph_read_runtime_error)?;
            refuse_if_query_closed(&context, &request.deadline, request.cancellation)?;
            Some(AdmittedSourceAuthority::capture(
                &context,
                runtime.as_ref(),
            )?)
        }
    };
    let graph_cancellation = application_graph_cancellation(request.cancellation);
    let observed_at = tracedecay_application::now_micros();
    refuse_if_query_closed(&context, &request.deadline, request.cancellation)?;
    let verified = await_graph_port_wait(
        &request.deadline,
        request.cancellation,
        projection.open(
            CodeGraphReadRequest::new(&context, observed_at, Arc::clone(&graph_cancellation))
                .with_deadline(request.deadline.clone())
                .with_live_cancellation(request.cancellation),
        ),
    )
    .await?
    .map_err(map_code_graph_read_runtime_error)?;
    refuse_if_query_closed(&context, &request.deadline, request.cancellation)?;
    let observed_at = tracedecay_application::now_micros();
    let reader = verified
        .reader_with_cancellation(&context, observed_at, Arc::clone(&graph_cancellation))
        .map_err(map_code_graph_read_runtime_error)?;
    refuse_if_query_closed(&context, &request.deadline, request.cancellation)?;
    Ok(VerifiedGraphQuery::from_admitted_parts(
        reader,
        graph_cancellation,
        context,
        source,
        request.cancellation.clone(),
        verified.freshness(),
    ))
}

async fn await_graph_port_wait<T>(
    deadline: &Deadline,
    cancellation: &CancellationSignal,
    future: impl Future<Output = T>,
) -> Result<T> {
    match run_deadline_signal_interruptible(deadline, cancellation, future).await {
        Ok(result) => Ok(result),
        Err(RequestInterruption::Cancelled) => Err(map_code_graph_read_runtime_error(
            super::CodeGraphReadError::Cancelled,
        )),
        Err(RequestInterruption::DeadlineExceeded) => Err(map_code_graph_read_runtime_error(
            super::CodeGraphReadError::TimedOut,
        )),
    }
}

fn refuse_if_query_closed(
    context: &RequestContext,
    deadline: &Deadline,
    cancellation: &CancellationSignal,
) -> Result<()> {
    if cancellation.is_cancelled() {
        return Err(map_code_graph_read_runtime_error(
            super::CodeGraphReadError::Cancelled,
        ));
    }
    let observed_at = tracedecay_application::now_micros();
    if deadline.is_elapsed_at(observed_at) {
        return Err(map_code_graph_read_runtime_error(
            super::CodeGraphReadError::TimedOut,
        ));
    }
    match context.admission_at(observed_at) {
        tracedecay_application::RequestAdmission::Admitted => Ok(()),
        tracedecay_application::RequestAdmission::Cancelled => Err(
            map_code_graph_read_runtime_error(super::CodeGraphReadError::Cancelled),
        ),
        tracedecay_application::RequestAdmission::TimedOut => Err(
            map_code_graph_read_runtime_error(super::CodeGraphReadError::TimedOut),
        ),
    }
}

fn graph_projection_error(
    error: tracedecay_code_index::graph_projection::CodeGraphProjectionError,
) -> TraceDecayError {
    map_code_graph_read_runtime_error(map_projection_error(error))
}

fn graph_invalid_request(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("code-graph-invalid-request", false, detail)
}

fn graph_budget_exhausted(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("code-graph-budget-exhausted", false, detail)
}

fn graph_corrupt(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("code-graph-corrupt", false, detail)
}

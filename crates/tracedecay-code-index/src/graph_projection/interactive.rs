//! Name/kind-keyed interactive reads over the code-graph projection.
//!
//! The retrieval-shaped [`CodeGraphEvidenceReader`] is occurrence-seeded: it
//! can only expand outward from occurrences a retrieval lane already found.
//! Interactive consumers (graph tools, dashboard, impact analysis) instead
//! start from a qualified name, a kind, or a file, and need adjacency in both
//! directions. This module serves those reads from the same verified
//! snapshot, pinned to the same generation, with the same typed refusal
//! doctrine: generation mismatches, cancellation, budget exhaustion, and
//! payload corruption are all explicit errors, never silent truncation.
//!
//! Name, file, and import keys are served from an [`InteractiveCatalog`] built
//! lazily by one bounded, cancellable scan of the projection and cached on the
//! owning [`CodeGraphProjectionStore`]. The catalog is derived from the
//! verified snapshot and shares its lifetime, so it is a cache of the
//! projection authority, not a second authority. Per-seed adjacency reads go
//! straight to the snapshot's kind-filtered relation fan-outs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, RwLock, TryLockError};
use std::time::Instant;

use tracedecay_contracts::{RequestCostReceiptV1, StorePointReadsV1};
use tracedecay_domain::process_heap::OwnerHeapV1;
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, FileOccurrenceId, RelationEdgeKindV1,
    SanitizedCodeFileV1, SymbolOccurrenceId, repository_path_matches_scope,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntityId, GraphProjectionIdentity, GraphReadMeter, GraphRelation,
    GraphRelationKind, MAX_VERIFIED_GENERATION_RELATIONS, NeverCancelled, RelationFanoutOverflow,
    VerifiedGraphSnapshot,
};

use super::{
    CodeGraphProjectionError, CodeGraphProjectionStore, CodeGraphReadCancellation,
    CodeGraphServingWarmthV1, CodeGraphSymbolBindingV1, RELATION_EDGE_KINDS, SymbolRecordV1,
    code_edge_kind, code_edge_kind_edge, compare_edges, edge_record, load_symbol_entity_record,
    load_symbol_record, symbol_entity_id,
};
use crate::lineage::LineageSymbolRecordV1;

mod catalog;
mod imports;
mod models;

use self::models::CatalogSymbol;
pub(super) use self::models::InteractiveCatalog;
pub use self::models::{
    CodeGraphCensusV1, CodeGraphDegreeRankingV1, CodeGraphEdgeKindCountsV1,
    CodeGraphFileDependenciesV1, CodeGraphFileSymbolCountV1, CodeGraphImpactBatchV1,
    CodeGraphImpactedSymbolV1, CodeGraphPathSearchV1, CodeGraphRankedNeighborsV1,
    CodeGraphRankedSymbolV1, CodeGraphRelationKeyV1, CodeGraphRelationKeysV1,
    CodeGraphSemanticEdgeV1, CodeGraphSymbolDegreesV1, CodeGraphSymbolPageV1, CodeGraphSymbolRefV1,
    CodeGraphSymbolSearchPageV1, CodeGraphSymbolSummaryV1, UnresolvedCallerGapsV1,
};

pub type CodeGraphSymbolPredicate<'a> = dyn Fn(
        &SymbolOccurrenceId,
        Option<&CodeGraphSymbolBindingV1>,
        Option<&LineageSymbolRecordV1>,
    ) -> bool
    + 'a;

enum InteractiveCatalogState {
    Cold,
    Warming {
        owner: Option<Arc<InteractiveCatalogBuildLease>>,
    },
    Ready(Arc<InteractiveCatalog>),
    /// Given back for memory. The next catalog read starts a background
    /// rebuild; no request thread pays for the scan.
    Released,
    Failed(CodeGraphProjectionError),
}

const CATALOG_WARMING: &str = "code graph interactive catalog is warming in the background";
const CATALOG_RELEASED: &str =
    "code graph interactive catalog was released and is re-warming in the background";

struct InteractiveCatalogBuildLease;

/// The state a catalog build took over, restored when the build is cancelled.
#[derive(Clone, Copy)]
enum TakenOverCatalog {
    Cold,
    Released,
    Background,
}

pub(super) struct InteractiveCatalogCache {
    state: RwLock<InteractiveCatalogState>,
    build: Mutex<()>,
    /// Count of full projection warm scans run against this store, so tests
    /// can prove concurrent readers share one scan. The scan may run on a
    /// background thread, hence an atomic.
    scan_builds: std::sync::atomic::AtomicUsize,
    /// [`InteractiveCatalog::retained_bytes`] of the ready catalog, measured
    /// once when it is built.
    ready_bytes: std::sync::atomic::AtomicU64,
}

/// Outcome of asking a store to give back its interactive catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeGraphCatalogReleaseV1 {
    /// The ready catalog was dropped; the next catalog read rebuilds it.
    Released { bytes: u64 },
    /// A build or a reader holds the catalog state; ask again later.
    Busy,
    /// No ready catalog was held.
    NotReady,
}

/// Seeds per batch traversal, under the store's `MAX_BATCH_TRAVERSAL_STARTS`
/// (100k) with headroom. A whole-repo census chunks its seeds across several
/// traversals rather than being refused for having too many.
const SEMANTIC_NEIGHBOR_SEED_CHUNK: usize = 50_000;

impl InteractiveCatalogCache {
    pub(super) fn new() -> Self {
        Self {
            state: RwLock::new(InteractiveCatalogState::Cold),
            build: Mutex::new(()),
            scan_builds: std::sync::atomic::AtomicUsize::new(0),
            ready_bytes: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Bytes the ready catalog holds, or `None` when none is ready.
    pub(super) fn ready_bytes(&self) -> Option<u64> {
        match self.state.try_read() {
            Ok(state) if matches!(&*state, InteractiveCatalogState::Ready(_)) => {
                Some(self.ready_bytes.load(std::sync::atomic::Ordering::Acquire))
            }
            Ok(_) | Err(TryLockError::WouldBlock) | Err(TryLockError::Poisoned(_)) => None,
        }
    }

    /// Return a ready catalog to cold. Never waits on a build or a reader.
    pub(super) fn release(&self) -> CodeGraphCatalogReleaseV1 {
        let Ok(_build) = self.build.try_lock() else {
            return CodeGraphCatalogReleaseV1::Busy;
        };
        let Ok(mut state) = self.state.try_write() else {
            return CodeGraphCatalogReleaseV1::Busy;
        };
        if !matches!(&*state, InteractiveCatalogState::Ready(_)) {
            return CodeGraphCatalogReleaseV1::NotReady;
        }
        *state = InteractiveCatalogState::Released;
        CodeGraphCatalogReleaseV1::Released {
            bytes: self
                .ready_bytes
                .swap(0, std::sync::atomic::Ordering::AcqRel),
        }
    }

    pub(super) fn warmth(&self) -> Result<CodeGraphServingWarmthV1, CodeGraphProjectionError> {
        let state = self.state.read().map_err(|_| catalog_lock_poisoned())?;
        Ok(match &*state {
            InteractiveCatalogState::Ready(_) => CodeGraphServingWarmthV1::Warm,
            InteractiveCatalogState::Cold | InteractiveCatalogState::Warming { .. } => {
                CodeGraphServingWarmthV1::Warming(CATALOG_WARMING.to_owned())
            }
            InteractiveCatalogState::Released => CodeGraphServingWarmthV1::Warming(
                "code graph interactive catalog was released for memory; the next graph read \
                 re-warms it"
                    .to_owned(),
            ),
            InteractiveCatalogState::Failed(error) => {
                CodeGraphServingWarmthV1::Failed(error.to_string())
            }
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AdjacencyDirection {
    Outgoing,
    Incoming,
}

/// Interactive, generation-pinned reader over one published code graph.
#[derive(Clone)]
pub struct CodeGraphInteractiveReader {
    generation: CodeGenerationId,
    projection: GraphProjectionIdentity,
    snapshot: Arc<VerifiedGraphSnapshot>,
    projection_node_count: usize,
    cancellation: Arc<dyn GraphCancellation>,
    catalog: Arc<InteractiveCatalogCache>,
    meter: Option<Arc<GraphReadMeter>>,
}

impl fmt::Debug for CodeGraphInteractiveReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeGraphInteractiveReader")
            .field("generation", &self.generation)
            .field("projection_node_count", &self.projection_node_count)
            .finish_non_exhaustive()
    }
}

/// One request's store accounting: the lease meter its reader counts on, and
/// when the request opened it.
#[derive(Clone, Debug)]
pub struct CodeGraphReadCostMeter {
    meter: Arc<GraphReadMeter>,
    started: Instant,
}

impl CodeGraphReadCostMeter {
    #[must_use]
    pub fn start() -> Self {
        Self {
            meter: Arc::new(GraphReadMeter::default()),
            started: Instant::now(),
        }
    }

    /// What the request has cost its stores so far.
    #[must_use]
    pub fn receipt(&self) -> RequestCostReceiptV1 {
        let cost = self.meter.cost();
        RequestCostReceiptV1 {
            wall_micros: u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX),
            point_reads: StorePointReadsV1 {
                graph_sealed: cost.sealed_point_reads,
                graph_staging: cost.staging_point_reads,
            },
            adjacency_queries: cost.adjacency_queries,
            adjacency_rows: cost.adjacency_rows,
            bytes_hydrated: cost.bytes_hydrated,
            catalog_symbols: cost.catalog_symbols,
        }
    }
}

impl CodeGraphProjectionStore {
    /// Builds and validates the generation-pinned interactive catalog before
    /// serving latency-bounded reads. Only a fully built immutable catalog is
    /// published into the store's shared slot.
    pub fn warm_interactive_catalog_with_cancellation(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), CodeGraphProjectionError> {
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        let reader =
            self.interactive_reader_with_cancellation(&self.generation, Arc::clone(&cancellation))?;
        reader.warm_catalog(cancellation)
    }

    /// Marks the catalog as background warming before graph serving is
    /// installed. Catalog-dependent reads then refuse promptly instead of
    /// winning a race to perform the full scan on a request thread.
    pub fn mark_interactive_catalog_warming(&self) -> Result<(), CodeGraphProjectionError> {
        let mut state = self
            .interactive_catalog
            .state
            .write()
            .map_err(|_| catalog_lock_poisoned())?;
        match &*state {
            InteractiveCatalogState::Cold | InteractiveCatalogState::Released => {
                *state = InteractiveCatalogState::Warming { owner: None };
                Ok(())
            }
            InteractiveCatalogState::Warming { .. } | InteractiveCatalogState::Ready(_) => Ok(()),
            InteractiveCatalogState::Failed(error) => Err(error.clone()),
        }
    }

    /// Number of full projection warm scans this store has run.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn interactive_catalog_scan_builds(&self) -> usize {
        self.interactive_catalog
            .scan_builds
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Reports whether this store's generation-pinned interactive catalog has
    /// been fully built without triggering a build or hiding lock failure.
    pub fn interactive_catalog_is_warm(&self) -> Result<bool, CodeGraphProjectionError> {
        match self.interactive_catalog.state.try_read() {
            Ok(state) => Ok(matches!(*state, InteractiveCatalogState::Ready(_))),
            Err(TryLockError::WouldBlock) => Err(CodeGraphProjectionError::Unavailable(
                "code graph interactive catalog warm state is contended".to_owned(),
            )),
            Err(TryLockError::Poisoned(_)) => Err(catalog_lock_poisoned()),
        }
    }
}

impl CodeGraphInteractiveReader {
    #[hotpath::skip]
    pub(super) fn assemble(
        generation: CodeGenerationId,
        projection: GraphProjectionIdentity,
        snapshot: Arc<VerifiedGraphSnapshot>,
        projection_node_count: usize,
        cancellation: Arc<dyn GraphCancellation>,
        catalog: Arc<InteractiveCatalogCache>,
    ) -> Self {
        Self {
            generation,
            projection,
            snapshot,
            projection_node_count,
            cancellation,
            catalog,
            meter: None,
        }
    }

    #[hotpath::skip]
    pub fn generation(&self) -> &CodeGenerationId {
        &self.generation
    }

    /// This reader with every store read it serves counted on `cost`.
    #[must_use]
    pub fn metered(&self, cost: &CodeGraphReadCostMeter) -> Self {
        Self {
            snapshot: Arc::new(self.snapshot.metered(Arc::clone(&cost.meter))),
            meter: Some(Arc::clone(&cost.meter)),
            ..self.clone()
        }
    }

    fn served_from_catalog<T>(&self, served: Vec<T>) -> Vec<T> {
        if let Some(meter) = &self.meter {
            meter.record_catalog_symbols(served.len() as u64);
        }
        served
    }

    /// Resolves symbols by exact qualified name, optionally narrowed to one
    /// kind. Resolution is scoped to the pinned generation by construction.
    pub fn resolve_qualified_name(
        &self,
        qualified_name: &str,
        kind: Option<&str>,
        limit: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(limit, "code graph name resolution limit")?;
        let catalog = self.catalog(cancellation)?;
        Ok(self.served_from_catalog(resolve_from_index(
            &catalog,
            catalog
                .by_qualified_name
                .get(qualified_name)
                .map(|ids| &ids[..]),
            kind,
            limit,
        )))
    }

    /// Resolves symbols by case-insensitive simple name (the trailing
    /// segment of the qualified name), optionally narrowed to one kind.
    pub fn resolve_simple_name(
        &self,
        name: &str,
        kind: Option<&str>,
        limit: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(limit, "code graph name resolution limit")?;
        let catalog = self.catalog(cancellation)?;
        Ok(self.served_from_catalog(resolve_from_index(
            &catalog,
            catalog
                .by_simple_name
                .get(&name.to_lowercase())
                .map(|ids| &ids[..]),
            kind,
            limit,
        )))
    }

    /// Whether unresolved call sites can name one of the queried methods.
    /// Matching a member name establishes uncertainty only, never a target edge.
    pub fn has_unresolved_callers(
        &self,
        targets: &[SymbolOccurrenceId],
        scope_prefix: Option<&str>,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, CodeGraphProjectionError> {
        self.unresolved_caller_gaps(targets, scope_prefix, request_cancellation)
            .map(|gaps| !gaps.is_empty())
    }

    /// The kinds of unresolved call site that can name one of the queried
    /// methods: receiver or import calls without exact target evidence, and
    /// calls under a `use` shape the extractor could not model.
    pub fn unresolved_caller_gaps(
        &self,
        targets: &[SymbolOccurrenceId],
        scope_prefix: Option<&str>,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<UnresolvedCallerGapsV1, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        let catalog = self.catalog(Arc::clone(&cancellation))?;
        let mut methods = BTreeSet::new();
        let mut gaps = UnresolvedCallerGapsV1::default();
        for target in targets {
            catalog::check_cancelled(cancellation.as_ref())?;
            let metadata = catalog
                .symbols
                .get(target)
                .and_then(|symbol| symbol.metadata.as_ref())
                .ok_or_else(|| {
                    CodeGraphProjectionError::Unavailable(
                        "caller target has no admitted symbol metadata".to_owned(),
                    )
                })?;
            if !methods.insert(&metadata.simple_name) {
                continue;
            }
            for source in catalog
                .unresolved_call_sources
                .get(&metadata.simple_name)
                .into_iter()
                .flatten()
            {
                catalog::check_cancelled(cancellation.as_ref())?;
                let symbol = catalog.symbols.get(source);
                let path = symbol
                    .and_then(|symbol| symbol.binding.as_ref())
                    .and_then(|binding| binding.logical_path.as_deref())
                    .ok_or_else(|| {
                        CodeGraphProjectionError::Corrupt(
                            "unresolved caller source has no bound logical path".to_owned(),
                        )
                    })?;
                if !repository_path_matches_scope(path, scope_prefix) {
                    continue;
                }
                for call in symbol
                    .into_iter()
                    .flat_map(|symbol| &symbol.unresolved_calls)
                {
                    if models::unresolved_callee_name(&call.reference_name) != metadata.simple_name
                    {
                        continue;
                    }
                    match call.unmodeled_import {
                        Some(shape) => {
                            gaps.unmodeled_imports.insert(shape);
                        }
                        None => gaps.exact_target_unavailable = true,
                    }
                }
            }
        }
        Ok(gaps)
    }

    /// The kinds of unresolved call site the queried symbols themselves make:
    /// calls whose target the seal could not bind are callees the graph
    /// cannot list.
    pub fn unresolved_callee_gaps(
        &self,
        sources: &[SymbolOccurrenceId],
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<UnresolvedCallerGapsV1, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        let catalog = self.catalog(Arc::clone(&cancellation))?;
        let mut gaps = UnresolvedCallerGapsV1::default();
        for source in sources {
            catalog::check_cancelled(cancellation.as_ref())?;
            for call in catalog
                .symbols
                .get(source)
                .into_iter()
                .flat_map(|symbol| &symbol.unresolved_calls)
            {
                match call.unmodeled_import {
                    Some(shape) => {
                        gaps.unmodeled_imports.insert(shape);
                    }
                    None => gaps.exact_target_unavailable = true,
                }
            }
        }
        Ok(gaps)
    }

    /// Lists the symbols bound to one file occurrence.
    pub fn symbols_in_file(
        &self,
        file: &FileOccurrenceId,
        limit: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(limit, "code graph file listing limit")?;
        file.validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        let catalog = self.catalog(cancellation)?;
        Ok(self.served_from_catalog(resolve_from_index(
            &catalog,
            catalog.by_file.get(file).map(|ids| &ids[..]),
            None,
            limit,
        )))
    }

    /// Lists the symbols bound to the file published under logical path
    /// `path`, resolving the path through the catalog rather than requiring
    /// the caller to already hold a [`FileOccurrenceId`].
    ///
    /// Unlike [`Self::symbols_in_file`], a path this generation never
    /// published is not an error: it truthfully reports "no such file in
    /// this generation" as an empty vector, because the caller had no
    /// occurrence identity to assert existence against in the first place.
    pub fn symbols_in_logical_file(
        &self,
        path: &str,
        limit: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(limit, "code graph logical file listing limit")?;
        let catalog = self.catalog(cancellation)?;
        let Some(file) = catalog.by_logical_path.get(path) else {
            return Ok(Vec::new());
        };
        Ok(self.served_from_catalog(resolve_from_index(
            &catalog,
            catalog.by_file.get(file).map(|ids| &ids[..]),
            None,
            limit,
        )))
    }

    pub fn file_by_logical_path(
        &self,
        path: &str,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<SanitizedCodeFileV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        let catalog = self.catalog(cancellation)?;
        Ok(catalog
            .by_logical_path
            .get(path)
            .and_then(|file| catalog.files.get(file))
            .cloned())
    }

    pub fn files(
        &self,
        max_files: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<SanitizedCodeFileV1>, CodeGraphProjectionError> {
        require_positive(max_files, "code graph file census limit")?;
        let cancellation = self.read_cancellation(request_cancellation)?;
        let catalog = self.catalog(cancellation)?;
        if catalog.files.len() > max_files {
            return Err(CodeGraphProjectionError::BudgetExhausted {
                budget: "file census".to_owned(),
                limit: u64::try_from(max_files).unwrap_or(u64::MAX),
            });
        }
        Ok(catalog.files.values().cloned().collect())
    }

    /// Hydrates the summary of the symbol one relation key names; `Ok(None)`
    /// means no symbol entity carries that identity in this generation.
    pub fn symbol_summary_for(
        &self,
        symbol: &CodeGraphSymbolRefV1,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<CodeGraphSymbolSummaryV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        Ok(
            load_symbol_entity_record(&self.snapshot, &self.projection, &symbol.0, cancellation)?
                .map(summary_from_record),
        )
    }

    /// Per-seed relation keys over the admitted edge kinds (every kind when
    /// none is named), outgoing or `reverse`, from two batched fan-outs: the
    /// seeds' edge relations, then each edge's far endpoint. Only relation
    /// rows are read, never an edge or symbol entity, so enumerating a
    /// neighborhood costs its adjacency rows and a page hydrates just the
    /// keys it returns. The edge fan-out reads at most `max_relations` rows
    /// across all seeds and reports `truncated` when it reached that many.
    pub fn relation_keys(
        &self,
        seeds: &[CodeGraphSymbolRefV1],
        kinds: &[RelationEdgeKindV1],
        reverse: bool,
        max_relations: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphRelationKeysV1, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(max_relations, "code graph relation key limit")?;
        let starts = seeds.iter().map(|seed| seed.0.clone()).collect::<Vec<_>>();
        // A reverse walk admits every kind at the store and filters after,
        // so its truncation point is the one the unfiltered fan-out defines.
        let admitted: BTreeSet<RelationEdgeKindV1> = kinds.iter().copied().collect();
        let edge_kinds = code_relation_kinds(if reverse { &[] } else { kinds })?;
        let edge_rows = if reverse {
            self.snapshot.incoming_relations_truncated(
                &starts,
                &edge_kinds,
                max_relations,
                cancellation,
            )?
        } else {
            self.snapshot.outgoing_relations_truncated(
                &starts,
                &edge_kinds,
                max_relations,
                cancellation,
            )?
        };
        if edge_rows.len() != seeds.len() {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph relation key batch shape does not match its seeds".to_owned(),
            ));
        }
        let truncated = edge_rows.iter().map(Vec::len).sum::<usize>() == max_relations;
        let per_seed = edge_rows
            .into_iter()
            .map(|relations| {
                let mut keys = Vec::with_capacity(relations.len());
                for relation in &relations {
                    let kind = relation_edge_kind(relation)?;
                    if reverse && !admitted.is_empty() && !admitted.contains(&kind) {
                        continue;
                    }
                    keys.push(CodeGraphRelationKeyV1 {
                        neighbor: CodeGraphSymbolRefV1(if reverse {
                            relation.from.clone()
                        } else {
                            relation.to.clone()
                        }),
                        kind,
                    });
                }
                Ok(keys)
            })
            .collect::<Result<Vec<_>, CodeGraphProjectionError>>()?;
        Ok(CodeGraphRelationKeysV1 {
            per_seed,
            truncated,
        })
    }

    /// Hydrates one symbol summary; `Ok(None)` means the occurrence has no
    /// symbol entity in this generation.
    pub fn symbol_summary(
        &self,
        occurrence: &SymbolOccurrenceId,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<CodeGraphSymbolSummaryV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        occurrence
            .validate()
            .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
        Ok(
            load_symbol_record(&self.snapshot, &self.projection, occurrence, cancellation)?
                .map(summary_from_record),
        )
    }

    /// One page of the generation's symbols in canonical occurrence order.
    /// `after` is an exclusive cursor.
    pub fn symbols_page(
        &self,
        after: Option<&SymbolOccurrenceId>,
        max_symbols: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphSymbolPageV1, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(max_symbols, "code graph symbol page limit")?;
        let catalog = self.catalog(cancellation)?;
        let range: Box<dyn Iterator<Item = (&SymbolOccurrenceId, &CatalogSymbol)>> = match after {
            Some(after) => Box::new(catalog.symbols.after(after)),
            None => Box::new(catalog.symbols.iter()),
        };
        let mut symbols = Vec::new();
        let mut has_more = false;
        for (occurrence, record) in range {
            if symbols.len() == max_symbols {
                has_more = true;
                break;
            }
            symbols.push(CodeGraphSymbolSummaryV1 {
                occurrence: occurrence.clone(),
                binding: record.binding.clone(),
                metadata: record.metadata.clone(),
            });
        }
        Ok(CodeGraphSymbolPageV1 {
            symbols: self.served_from_catalog(symbols),
            has_more,
        })
    }

    /// Finds symbols in canonical occurrence order without hydrating
    /// non-matching catalog records.
    pub fn find_symbols(
        &self,
        predicate: &CodeGraphSymbolPredicate<'_>,
        limit: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<CodeGraphSymbolSummaryV1>, CodeGraphProjectionError> {
        const CANCELLATION_INTERVAL: usize = 4_096;

        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(limit, "code graph symbol find limit")?;
        let catalog = self.catalog(Arc::clone(&cancellation))?;
        let mut symbols = Vec::new();
        for (index, (occurrence, record)) in catalog.symbols.iter().enumerate() {
            if index.is_multiple_of(CANCELLATION_INTERVAL) && cancellation.is_cancelled() {
                return Err(CodeGraphProjectionError::Cancelled);
            }
            if predicate(
                occurrence,
                record.binding.as_ref(),
                record.metadata.as_ref(),
            ) {
                symbols.push(CodeGraphSymbolSummaryV1 {
                    occurrence: occurrence.clone(),
                    binding: record.binding.clone(),
                    metadata: record.metadata.clone(),
                });
                if symbols.len() == limit {
                    break;
                }
            }
        }
        Ok(self.served_from_catalog(symbols))
    }

    /// Per-seed outgoing semantic edges (callees when filtered to call
    /// kinds). `max_relations` bounds the fan-out examined across the whole
    /// batch; exceeding it is a typed [`CodeGraphProjectionError::BudgetExhausted`].
    pub fn callees(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        self.semantic_neighbors(
            seeds,
            kinds,
            AdjacencyDirection::Outgoing,
            max_relations,
            cancellation,
            RelationFanoutOverflow::Refuse,
        )
    }

    /// Page-shaped outgoing fan-out: stops at `max_relations` instead of
    /// refusing the batch. Context assembly uses this so a popular symbol
    /// cannot force a 50k-edge hydrate on every call.
    pub fn callees_truncated(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        self.semantic_neighbors(
            seeds,
            kinds,
            AdjacencyDirection::Outgoing,
            max_relations,
            cancellation,
            RelationFanoutOverflow::Truncate,
        )
    }

    /// Per-seed incoming semantic edges (callers when filtered to call
    /// kinds), with the same batch-wide budget semantics as [`Self::callees`].
    pub fn callers(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        self.semantic_neighbors(
            seeds,
            kinds,
            AdjacencyDirection::Incoming,
            max_relations,
            cancellation,
            RelationFanoutOverflow::Refuse,
        )
    }

    /// Page-shaped incoming fan-out: stops at `max_relations` instead of
    /// refusing the batch.
    pub fn callers_truncated(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        self.semantic_neighbors(
            seeds,
            kinds,
            AdjacencyDirection::Incoming,
            max_relations,
            cancellation,
            RelationFanoutOverflow::Truncate,
        )
    }

    /// True per-kind totals of one symbol's semantic edges, both directions.
    pub fn edge_kind_counts(
        &self,
        occurrence: &SymbolOccurrenceId,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphEdgeKindCountsV1, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        let seeds = std::slice::from_ref(occurrence);
        let outgoing = self.semantic_neighbors(
            seeds,
            &[],
            AdjacencyDirection::Outgoing,
            MAX_VERIFIED_GENERATION_RELATIONS,
            Arc::clone(&cancellation),
            RelationFanoutOverflow::Refuse,
        )?;
        let incoming = self.semantic_neighbors(
            seeds,
            &[],
            AdjacencyDirection::Incoming,
            MAX_VERIFIED_GENERATION_RELATIONS,
            cancellation,
            RelationFanoutOverflow::Refuse,
        )?;
        let mut counts = CodeGraphEdgeKindCountsV1::default();
        for edge in outgoing.into_iter().flatten() {
            *counts.outgoing.entry(edge.edge.kind).or_default() += 1;
        }
        for edge in incoming.into_iter().flatten() {
            *counts.incoming.entry(edge.edge.kind).or_default() += 1;
        }
        Ok(counts)
    }

    /// True semantic in/out degrees for a batch of symbols, without edge
    /// payload hydration.
    pub fn degrees(
        &self,
        occurrences: &[SymbolOccurrenceId],
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<CodeGraphSymbolDegreesV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        let starts = entity_ids(occurrences)?;
        let edge_kinds = code_relation_kinds(&[])?;
        let outgoing = self.snapshot.outgoing_relation_ids(
            &starts,
            &edge_kinds,
            MAX_VERIFIED_GENERATION_RELATIONS,
            Arc::clone(&cancellation),
        )?;
        let incoming = self.snapshot.incoming_relation_ids(
            &starts,
            &edge_kinds,
            MAX_VERIFIED_GENERATION_RELATIONS,
            cancellation,
        )?;
        if outgoing.len() != occurrences.len() || incoming.len() != occurrences.len() {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph degree batch shape does not match its seeds".to_owned(),
            ));
        }
        Ok(occurrences
            .iter()
            .zip(outgoing)
            .zip(incoming)
            .map(
                |((occurrence, outgoing), incoming)| CodeGraphSymbolDegreesV1 {
                    occurrence: occurrence.clone(),
                    outgoing: outgoing.len() as u64,
                    incoming: incoming.len() as u64,
                },
            )
            .collect())
    }

    /// The `top` most-connected symbols of the generation, ranked by total
    /// semantic degree over every symbol of the generation.
    ///
    /// Degrees come from the generation-pinned catalog, which tallied them
    /// once from the relation rows, so a ranking costs one in-memory pass
    /// over the catalog and no adjacency reads. Ordering is total and
    /// deterministic, total degree descending, then qualified name, then
    /// occurrence, so equal-degree symbols do not reshuffle between reads.
    pub fn degree_ranking(
        &self,
        top: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphDegreeRankingV1, CodeGraphProjectionError> {
        require_positive(top, "code graph degree ranking size")?;
        let cancellation = self.read_cancellation(request_cancellation)?;
        let catalog = self.catalog(Arc::clone(&cancellation))?;
        let mut ranked: Vec<(u64, &str, &SymbolOccurrenceId, &CatalogSymbol)> = catalog
            .symbols
            .iter()
            .map(|(occurrence, record)| {
                let name = record
                    .metadata
                    .as_ref()
                    .map_or(occurrence.as_str(), |metadata| {
                        metadata.qualified_name.as_str()
                    });
                (
                    record.outgoing.saturating_add(record.incoming),
                    name,
                    occurrence,
                    record,
                )
            })
            .collect();
        catalog::check_cancelled(cancellation.as_ref())?;
        let order = |left: &(u64, &str, &SymbolOccurrenceId, &CatalogSymbol),
                     right: &(u64, &str, &SymbolOccurrenceId, &CatalogSymbol)| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.cmp(right.1))
                .then_with(|| left.2.cmp(right.2))
        };
        if ranked.len() > top {
            ranked.select_nth_unstable_by(top - 1, order);
            ranked.truncate(top);
        }
        ranked.sort_unstable_by(order);
        Ok(CodeGraphDegreeRankingV1 {
            ranked: self.served_from_catalog(
                ranked
                    .into_iter()
                    .map(|(_, _, occurrence, record)| CodeGraphRankedSymbolV1 {
                        summary: InteractiveCatalog::symbol_summary(occurrence, record),
                        outgoing: record.outgoing,
                        incoming: record.incoming,
                    })
                    .collect(),
            ),
            symbol_count: catalog.symbols.len(),
        })
    }

    /// The seeds' neighbors over every edge kind in both directions, ranked
    /// before the cut to `limit`: the lowest `kind_rank` of any edge joining
    /// the neighbor to a seed, then total catalog degree descending, then
    /// qualified name, then occurrence. Seeds are not their own neighbors.
    ///
    /// Each direction reads at most `max_relations` edge rows across all
    /// seeds and takes the neighbor from the row's edge record; degrees,
    /// names, and the kept summaries come from the catalog, so no symbol
    /// entity is read.
    pub fn ranked_neighbors(
        &self,
        seeds: &[SymbolOccurrenceId],
        kind_rank: fn(RelationEdgeKindV1) -> u8,
        max_relations: usize,
        limit: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphRankedNeighborsV1, CodeGraphProjectionError> {
        require_positive(max_relations, "code graph neighbor walk limit")?;
        require_positive(limit, "code graph neighbor limit")?;
        let cancellation = self.read_cancellation(request_cancellation)?;
        let starts = entity_ids(seeds)?;
        let every_kind = code_relation_kinds(&[])?;
        let outgoing = self.snapshot.outgoing_relations_truncated(
            &starts,
            &every_kind,
            max_relations,
            Arc::clone(&cancellation),
        )?;
        let incoming = self.snapshot.incoming_relations_truncated(
            &starts,
            &every_kind,
            max_relations,
            Arc::clone(&cancellation),
        )?;
        if outgoing.len() != seeds.len() || incoming.len() != seeds.len() {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph neighbor batch shape does not match its seeds".to_owned(),
            ));
        }
        let walk_truncated = [&outgoing, &incoming]
            .iter()
            .any(|rows| rows.iter().map(Vec::len).sum::<usize>() == max_relations);
        let seed_set = seeds.iter().collect::<BTreeSet<_>>();
        let mut best_rank = BTreeMap::<SymbolOccurrenceId, u8>::new();
        for (direction, batches) in [
            (AdjacencyDirection::Outgoing, outgoing),
            (AdjacencyDirection::Incoming, incoming),
        ] {
            for (seed, relations) in seeds.iter().zip(batches) {
                catalog::check_cancelled(cancellation.as_ref())?;
                for relation in &relations {
                    let edge = seed_edge_record(seed, relation, direction)?;
                    let far = match direction {
                        AdjacencyDirection::Outgoing => edge.to_occurrence,
                        AdjacencyDirection::Incoming => edge.from_occurrence,
                    };
                    if seed_set.contains(&far) {
                        continue;
                    }
                    let rank = kind_rank(edge.kind);
                    best_rank
                        .entry(far)
                        .and_modify(|best| *best = (*best).min(rank))
                        .or_insert(rank);
                }
            }
        }
        let catalog = self.catalog(Arc::clone(&cancellation))?;
        let mut ranked = best_rank
            .iter()
            .map(|(occurrence, rank)| {
                let record = catalog.symbols.get(occurrence).ok_or_else(|| {
                    CodeGraphProjectionError::Corrupt(
                        "code graph edge endpoint has no symbol entity".to_owned(),
                    )
                })?;
                let name = record
                    .metadata
                    .as_ref()
                    .map_or(occurrence.as_str(), |metadata| {
                        metadata.qualified_name.as_str()
                    });
                Ok((
                    *rank,
                    record.outgoing.saturating_add(record.incoming),
                    name,
                    occurrence,
                    record,
                ))
            })
            .collect::<Result<Vec<_>, CodeGraphProjectionError>>()?;
        catalog::check_cancelled(cancellation.as_ref())?;
        let total = ranked.len();
        type Ranked<'a> = (u8, u64, &'a str, &'a SymbolOccurrenceId, &'a CatalogSymbol);
        let order = |left: &Ranked<'_>, right: &Ranked<'_>| {
            left.0
                .cmp(&right.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.cmp(right.2))
                .then_with(|| left.3.cmp(right.3))
        };
        if ranked.len() > limit {
            ranked.select_nth_unstable_by(limit - 1, order);
            ranked.truncate(limit);
        }
        ranked.sort_unstable_by(order);
        Ok(CodeGraphRankedNeighborsV1 {
            neighbors: self.served_from_catalog(
                ranked
                    .into_iter()
                    .map(|(_, _, _, occurrence, record)| {
                        InteractiveCatalog::symbol_summary(occurrence, record)
                    })
                    .collect(),
            ),
            total,
            walk_truncated,
        })
    }

    /// Generation-wide counts with the `largest_files` most symbol-dense
    /// files, read from aggregates the catalog derived when it was built.
    pub fn census(
        &self,
        largest_files: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphCensusV1, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        let catalog = self.catalog(cancellation)?;
        Ok(CodeGraphCensusV1 {
            symbols: catalog.symbols.len() as u64,
            semantic_edges: catalog.semantic_edges,
            files: catalog.files.len() as u64,
            symbols_by_kind: catalog
                .symbols_by_kind
                .iter()
                .map(|(kind, count)| (kind.clone(), *count))
                .collect(),
            files_by_language: catalog
                .files_by_language
                .iter()
                .map(|(language, count)| (language.clone(), *count))
                .collect(),
            largest_files: catalog
                .largest_files
                .iter()
                .take(largest_files)
                .cloned()
                .collect(),
        })
    }

    /// File-level `calls`/`uses` dependencies the catalog folded when it
    /// was built.
    pub fn file_dependencies(
        &self,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphFileDependenciesV1, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        Ok(self.catalog(cancellation)?.file_dependencies.clone())
    }

    /// Canonical symbol name search: exact simple-name hits from the
    /// simple-name index first, then every other symbol whose simple or
    /// qualified name contains `query` (ASCII case-insensitive), each group
    /// in source order (logical path, then start line). Returns the
    /// `[offset, offset + limit)` window of the symbols `admit` accepts (all
    /// when `None`).
    ///
    /// ponytail: every query scans every catalog name (~150 ms on a
    /// 200k-symbol generation); a name n-gram index built with the catalog is
    /// the upgrade when that bites.
    pub fn search_symbols(
        &self,
        query: &str,
        admit: Option<&CodeGraphSymbolPredicate<'_>>,
        offset: usize,
        limit: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphSymbolSearchPageV1, CodeGraphProjectionError> {
        const CANCELLATION_INTERVAL: usize = 4_096;

        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(limit, "code graph symbol search limit")?;
        let catalog = self.catalog(Arc::clone(&cancellation))?;
        let admitted = |occurrence: &SymbolOccurrenceId, symbol: &CatalogSymbol| {
            admit.is_none_or(|admit| {
                admit(
                    occurrence,
                    symbol.binding.as_ref(),
                    symbol.metadata.as_ref(),
                )
            })
        };
        // Occurrence ids digest the repository identity, so their order differs
        // between two clones of one checkout; hits are served in source order.
        let source_position = |occurrence: &SymbolOccurrenceId| {
            let symbol = catalog.symbols.get(occurrence);
            (
                symbol
                    .and_then(|symbol| symbol.binding.as_ref())
                    .and_then(|binding| binding.logical_path.as_deref()),
                symbol
                    .and_then(|symbol| symbol.metadata.as_ref())
                    .map(|metadata| metadata.start_line),
            )
        };
        let source_order = |left: &&SymbolOccurrenceId, right: &&SymbolOccurrenceId| {
            source_position(left)
                .cmp(&source_position(right))
                .then_with(|| left.cmp(right))
        };
        let exact: BTreeSet<&SymbolOccurrenceId> = catalog
            .by_simple_name
            .get(&query.to_lowercase())
            .into_iter()
            .flatten()
            .filter(|occurrence| {
                catalog
                    .symbols
                    .get(*occurrence)
                    .is_some_and(|symbol| admitted(occurrence, symbol))
            })
            .collect();
        let mut exact_hits: Vec<&SymbolOccurrenceId> = exact.iter().copied().collect();
        exact_hits.sort_by(source_order);
        // ponytail: every named hit is collected and sorted before the window
        // is cut, so a broad query costs O(hits log hits) per page; a
        // source-ordered name index built with the catalog is the upgrade.
        let mut named_hits = Vec::new();
        for (index, (occurrence, symbol)) in catalog.symbols.iter().enumerate() {
            if index.is_multiple_of(CANCELLATION_INTERVAL) && cancellation.is_cancelled() {
                return Err(CodeGraphProjectionError::Cancelled);
            }
            let named = symbol.metadata.as_ref().is_some_and(|metadata| {
                contains_ignore_ascii_case(&metadata.simple_name, query)
                    || contains_ignore_ascii_case(&metadata.qualified_name, query)
            });
            if named && !exact.contains(occurrence) && admitted(occurrence, symbol) {
                named_hits.push(occurrence);
            }
        }
        named_hits.sort_by(source_order);

        let mut symbols = Vec::new();
        let mut matched = 0_usize;
        let mut has_more = false;
        let mut accept = |occurrence: &SymbolOccurrenceId| -> bool {
            if matched >= offset {
                if symbols.len() == limit {
                    has_more = true;
                    return true;
                }
                if let Some(summary) = catalog.summary(occurrence) {
                    symbols.push(summary);
                }
            }
            matched += 1;
            false
        };
        for occurrence in exact_hits.into_iter().chain(named_hits) {
            if accept(occurrence) {
                break;
            }
        }
        let total = if query.is_empty() && admit.is_none() {
            Some(catalog.symbols.len() as u64)
        } else {
            (!has_more).then_some(matched as u64)
        };
        Ok(CodeGraphSymbolSearchPageV1 {
            symbols: self.served_from_catalog(symbols),
            has_more,
            total,
        })
    }

    /// Semantic edges induced among a symbol set: edges whose endpoints are
    /// both members. `max_relations` bounds the batch-wide fan-out examined.
    ///
    /// Every caller reads the edge alone, so the walk stops at the edge
    /// payload instead of hydrating a summary for each far endpoint the way
    /// `semantic_neighbors` does for callers, callees and impact. The edge
    /// entities come back decoded with the traversal that found them, so a
    /// whole-repo census pays no per-edge point read either.
    pub fn edges_among(
        &self,
        occurrences: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<CanonicalRelationEdgeV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        let members: BTreeSet<&SymbolOccurrenceId> = occurrences.iter().collect();
        let admitted: BTreeSet<RelationEdgeKindV1> = kinds.iter().copied().collect();
        // Seeds are chunked because the store bounds one batch traversal's
        // starts (`MAX_BATCH_TRAVERSAL_STARTS`, 100k). A whole-repo census -
        // dead code, unused symbols - legitimately has more seeds than that,
        // and refusing it turned a complete answer into a typed budget error.
        // The bound exists to cap one call's working set, which chunking
        // preserves: each traversal still costs at most one chunk, and the
        // per-seed relation budget is unchanged.
        let mut edges: Vec<CanonicalRelationEdgeV1> = Vec::new();
        for chunk in occurrences.chunks(SEMANTIC_NEIGHBOR_SEED_CHUNK) {
            let starts = entity_ids(chunk)?;
            let per_seed = self.snapshot.outgoing_relations(
                &starts,
                &code_relation_kinds(kinds)?,
                max_relations,
                Arc::clone(&cancellation),
            )?;
            if per_seed.len() != chunk.len() {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph adjacency batch shape does not match its seeds".to_owned(),
                ));
            }
            for (seed, relations) in chunk.iter().zip(per_seed) {
                for relation in relations {
                    if cancellation.is_cancelled() {
                        return Err(CodeGraphProjectionError::Cancelled);
                    }
                    let edge = edge_record(&relation)?;
                    if edge.from_occurrence != *seed {
                        return Err(CodeGraphProjectionError::Corrupt(
                            "code graph edge endpoint does not match its adjacency seed".to_owned(),
                        ));
                    }
                    if !admitted.is_empty() && !admitted.contains(&edge.kind) {
                        continue;
                    }
                    if members.contains(&edge.to_occurrence) {
                        edges.push(edge);
                    }
                }
            }
        }
        edges.sort_by(compare_edges);
        edges.dedup();
        Ok(edges)
    }

    /// Bounded reverse-reachability closure from the seeds over the admitted
    /// edge kinds. Every expansion hop charges `max_relations_per_hop`;
    /// exceeding it is a typed budget refusal, while reaching `max_symbols`
    /// truthfully returns a truncated batch with `complete: false`.
    pub fn impact(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_depth: u32,
        max_symbols: usize,
        max_relations_per_hop: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphImpactBatchV1, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(max_depth as usize, "code graph impact depth")?;
        require_positive(max_symbols, "code graph impact symbol ceiling")?;
        let mut seen: BTreeSet<SymbolOccurrenceId> = seeds.iter().cloned().collect();
        let mut frontier: Vec<SymbolOccurrenceId> = seeds.to_vec();
        let mut impacted = Vec::new();
        let mut complete = true;
        let mut depth = 0_u32;
        'expansion: while !frontier.is_empty() && depth < max_depth {
            depth += 1;
            let per_seed = self.semantic_neighbors(
                &frontier,
                kinds,
                AdjacencyDirection::Incoming,
                max_relations_per_hop,
                Arc::clone(&cancellation),
                RelationFanoutOverflow::Refuse,
            )?;
            let mut next = Vec::new();
            for edge in per_seed.into_iter().flatten() {
                let neighbor = edge.neighbor;
                if !seen.insert(neighbor.occurrence.clone()) {
                    continue;
                }
                if impacted.len() == max_symbols {
                    complete = false;
                    break 'expansion;
                }
                next.push(neighbor.occurrence.clone());
                impacted.push(CodeGraphImpactedSymbolV1 {
                    summary: neighbor,
                    depth,
                });
            }
            frontier = next;
        }
        if complete && depth == max_depth && !frontier.is_empty() {
            // The depth ceiling stopped the expansion while callers of the
            // last level were still unexplored.
            let remaining = self.semantic_neighbors(
                &frontier,
                kinds,
                AdjacencyDirection::Incoming,
                max_relations_per_hop,
                Arc::clone(&cancellation),
                RelationFanoutOverflow::Refuse,
            )?;
            if remaining
                .into_iter()
                .flatten()
                .any(|edge| !seen.contains(&edge.neighbor.occurrence))
            {
                complete = false;
            }
        }
        Ok(CodeGraphImpactBatchV1 { impacted, complete })
    }

    /// Breadth-first shortest path from `from` to `to` over the admitted
    /// edge kinds, ties broken by canonical edge order.
    pub fn shortest_path(
        &self,
        from: &SymbolOccurrenceId,
        to: &SymbolOccurrenceId,
        kinds: &[RelationEdgeKindV1],
        max_depth: u32,
        max_relations_per_hop: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphPathSearchV1, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        require_positive(max_depth as usize, "code graph path depth")?;
        if from == to {
            return Ok(CodeGraphPathSearchV1 {
                path: Some(Vec::new()),
                complete: true,
            });
        }
        let mut parents: BTreeMap<SymbolOccurrenceId, CanonicalRelationEdgeV1> = BTreeMap::new();
        let mut frontier = VecDeque::from([from.clone()]);
        let mut depth = 0_u32;
        while !frontier.is_empty() && depth < max_depth {
            depth += 1;
            let level: Vec<_> = frontier.drain(..).collect();
            let per_seed = self.semantic_neighbors(
                &level,
                kinds,
                AdjacencyDirection::Outgoing,
                max_relations_per_hop,
                Arc::clone(&cancellation),
                RelationFanoutOverflow::Refuse,
            )?;
            for edge in per_seed.into_iter().flatten() {
                let target = edge.edge.to_occurrence.clone();
                if target == *from || parents.contains_key(&target) {
                    continue;
                }
                parents.insert(target.clone(), edge.edge.clone());
                if target == *to {
                    return Ok(CodeGraphPathSearchV1 {
                        path: Some(reconstruct_path(&parents, from, to)?),
                        complete: true,
                    });
                }
                frontier.push_back(target);
            }
        }
        Ok(CodeGraphPathSearchV1 {
            path: None,
            complete: frontier.is_empty(),
        })
    }

    #[hotpath::skip]
    fn read_cancellation(
        &self,
        request: Arc<dyn GraphCancellation>,
    ) -> Result<Arc<dyn GraphCancellation>, CodeGraphProjectionError> {
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(CodeGraphReadCancellation {
            lifecycle: Arc::clone(&self.cancellation),
            request,
        });
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        Ok(cancellation)
    }

    #[hotpath::skip]
    fn catalog(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Arc<InteractiveCatalog>, CodeGraphProjectionError> {
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        {
            let state = self
                .catalog
                .state
                .read()
                .map_err(|_| catalog_lock_poisoned())?;
            match &*state {
                InteractiveCatalogState::Ready(catalog) => {
                    if cancellation.is_cancelled() {
                        return Err(CodeGraphProjectionError::Cancelled);
                    }
                    return Ok(Arc::clone(catalog));
                }
                InteractiveCatalogState::Warming { .. } => {
                    return Err(CodeGraphProjectionError::Unavailable(
                        CATALOG_WARMING.to_owned(),
                    ));
                }
                InteractiveCatalogState::Failed(error) => return Err(error.clone()),
                InteractiveCatalogState::Released => {}
                InteractiveCatalogState::Cold => {
                    drop(state);
                    return self.build_cold_catalog(cancellation);
                }
            }
        }
        Err(self.rewarm_released_catalog())
    }

    fn build_cold_catalog(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Arc<InteractiveCatalog>, CodeGraphProjectionError> {
        self.warm_catalog(cancellation)?;
        let state = self
            .catalog
            .state
            .read()
            .map_err(|_| catalog_lock_poisoned())?;
        match &*state {
            InteractiveCatalogState::Ready(catalog) => Ok(Arc::clone(catalog)),
            InteractiveCatalogState::Failed(error) => Err(error.clone()),
            InteractiveCatalogState::Cold
            | InteractiveCatalogState::Released
            | InteractiveCatalogState::Warming { .. } => Err(
                CodeGraphProjectionError::Unavailable(CATALOG_WARMING.to_owned()),
            ),
        }
    }

    /// Start rebuilding a released catalog on a thread of its own and answer
    /// the typed warming state. The rebuild answers to no request's
    /// cancellation, so a short read cannot abandon it half-scanned.
    pub(super) fn rewarm_released_catalog(&self) -> CodeGraphProjectionError {
        let released = CodeGraphProjectionError::Unavailable(CATALOG_RELEASED.to_owned());
        {
            let Ok(mut state) = self.catalog.state.write() else {
                return catalog_lock_poisoned();
            };
            if !matches!(&*state, InteractiveCatalogState::Released) {
                return CodeGraphProjectionError::Unavailable(CATALOG_WARMING.to_owned());
            }
            *state = InteractiveCatalogState::Warming { owner: None };
        }
        let background = Self {
            cancellation: Arc::new(NeverCancelled),
            ..self.clone()
        };
        let spawned = std::thread::Builder::new()
            .name("code-graph-catalog-rewarm".to_owned())
            .spawn(move || {
                // A failed build is recorded as the catalog's `Failed` state,
                // which every later read and the serving status answer.
                let _ = background.warm_catalog(Arc::new(NeverCancelled));
            });
        match spawned {
            Ok(_) => released,
            Err(error) => {
                if let Ok(mut state) = self.catalog.state.write() {
                    *state = InteractiveCatalogState::Released;
                }
                CodeGraphProjectionError::Unavailable(format!(
                    "code graph interactive catalog re-warm could not start: {error}"
                ))
            }
        }
    }

    fn warm_catalog(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), CodeGraphProjectionError> {
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        let _build = self
            .catalog
            .build
            .lock()
            .map_err(|_| catalog_lock_poisoned())?;
        if cancellation.is_cancelled() {
            return Err(CodeGraphProjectionError::Cancelled);
        }
        let build_lease = Arc::new(InteractiveCatalogBuildLease);
        let taken_over = {
            let mut state = self
                .catalog
                .state
                .write()
                .map_err(|_| catalog_lock_poisoned())?;
            let taken_over = match &*state {
                InteractiveCatalogState::Ready(_) => {
                    if cancellation.is_cancelled() {
                        return Err(CodeGraphProjectionError::Cancelled);
                    }
                    return Ok(());
                }
                InteractiveCatalogState::Failed(error) => return Err(error.clone()),
                InteractiveCatalogState::Cold => TakenOverCatalog::Cold,
                InteractiveCatalogState::Released => TakenOverCatalog::Released,
                InteractiveCatalogState::Warming { owner: None } => TakenOverCatalog::Background,
                InteractiveCatalogState::Warming { owner: Some(_) } => {
                    return Err(CodeGraphProjectionError::Unavailable(
                        "code graph interactive catalog warm already has an owner".to_owned(),
                    ));
                }
            };
            *state = InteractiveCatalogState::Warming {
                owner: Some(Arc::clone(&build_lease)),
            };
            taken_over
        };
        self.catalog
            .scan_builds
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        // Built in a heap of its own, the catalog's pages hold nothing else,
        // are charged as the catalog's, and return whole when it is dropped.
        let (built, heap) = hotpath::measure_block!("code_graph.catalog.build", {
            OwnerHeapV1::build(|| {
                catalog::build_interactive_catalog(
                    &self.snapshot,
                    &self.projection,
                    self.projection_node_count,
                    Arc::clone(&cancellation),
                )
            })
        });
        let result = built.and_then(|mut catalog| {
            if cancellation.is_cancelled() {
                Err(CodeGraphProjectionError::Cancelled)
            } else {
                catalog.heap = heap;
                Ok(Arc::new(catalog))
            }
        });
        let mut state = self
            .catalog
            .state
            .write()
            .map_err(|_| catalog_lock_poisoned())?;
        let owns_warm = matches!(
            &*state,
            InteractiveCatalogState::Warming { owner: Some(owner) }
                if Arc::ptr_eq(owner, &build_lease)
        );
        if !owns_warm {
            return Err(CodeGraphProjectionError::Unavailable(
                "code graph interactive catalog warm ownership changed".to_owned(),
            ));
        }
        match result {
            Ok(catalog) => {
                self.catalog.ready_bytes.store(
                    catalog.resident_bytes(),
                    std::sync::atomic::Ordering::Release,
                );
                *state = InteractiveCatalogState::Ready(catalog);
                Ok(())
            }
            Err(CodeGraphProjectionError::Cancelled) => {
                // A background-marked build remains background-owned after
                // cancellation, so a request cannot take over its full scan.
                *state = match taken_over {
                    TakenOverCatalog::Cold => InteractiveCatalogState::Cold,
                    TakenOverCatalog::Released => InteractiveCatalogState::Released,
                    TakenOverCatalog::Background => {
                        InteractiveCatalogState::Warming { owner: None }
                    }
                };
                Err(CodeGraphProjectionError::Cancelled)
            }
            Err(error) => {
                *state = InteractiveCatalogState::Failed(error.clone());
                Err(error)
            }
        }
    }

    /// Hydration is staged so excluded work is never paid: each adjacency row
    /// loads its relation and edge payload first, edges outside the admitted
    /// kinds stop there without touching their far endpoint, and each unique
    /// far endpoint that survives the filter is hydrated once per batch,
    /// impact frontiers and shared callees converge on the same neighbors, so
    /// per-edge endpoint reads repeated the same snapshot lookups.
    fn semantic_neighbors(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        direction: AdjacencyDirection,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
        overflow: RelationFanoutOverflow,
    ) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>, CodeGraphProjectionError> {
        let starts = entity_ids(seeds)?;
        let admitted: BTreeSet<RelationEdgeKindV1> = kinds.iter().copied().collect();
        let per_seed_relations = match (direction, overflow) {
            (AdjacencyDirection::Outgoing, RelationFanoutOverflow::Refuse) => {
                self.snapshot.outgoing_relations(
                    &starts,
                    &code_relation_kinds(kinds)?,
                    max_relations,
                    Arc::clone(&cancellation),
                )?
            }
            (AdjacencyDirection::Outgoing, RelationFanoutOverflow::Truncate) => {
                self.snapshot.outgoing_relations_truncated(
                    &starts,
                    &code_relation_kinds(kinds)?,
                    max_relations,
                    Arc::clone(&cancellation),
                )?
            }
            (AdjacencyDirection::Incoming, RelationFanoutOverflow::Refuse) => {
                self.snapshot.incoming_relations(
                    &starts,
                    &code_relation_kinds(&[])?,
                    max_relations,
                    Arc::clone(&cancellation),
                )?
            }
            (AdjacencyDirection::Incoming, RelationFanoutOverflow::Truncate) => {
                self.snapshot.incoming_relations_truncated(
                    &starts,
                    &code_relation_kinds(&[])?,
                    max_relations,
                    Arc::clone(&cancellation),
                )?
            }
        };
        if per_seed_relations.len() != seeds.len() {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph adjacency batch shape does not match its seeds".to_owned(),
            ));
        }
        let mut neighbors = BTreeMap::<SymbolOccurrenceId, CodeGraphSymbolSummaryV1>::new();
        let mut batches = Vec::with_capacity(seeds.len());
        for (seed, relations) in seeds.iter().zip(per_seed_relations) {
            let mut edges = Vec::new();
            for relation in relations {
                if cancellation.is_cancelled() {
                    return Err(CodeGraphProjectionError::Cancelled);
                }
                let edge = seed_edge_record(seed, &relation, direction)?;
                if !admitted.is_empty() && !admitted.contains(&edge.kind) {
                    continue;
                }
                let far = match direction {
                    AdjacencyDirection::Outgoing => &edge.to_occurrence,
                    AdjacencyDirection::Incoming => &edge.from_occurrence,
                };
                let neighbor = match neighbors.get(far) {
                    Some(summary) => summary.clone(),
                    None => {
                        let record = load_symbol_record(
                            &self.snapshot,
                            &self.projection,
                            far,
                            Arc::clone(&cancellation),
                        )?
                        .ok_or_else(|| {
                            CodeGraphProjectionError::Corrupt(
                                "code graph edge endpoint has no symbol entity".to_owned(),
                            )
                        })?;
                        let summary = summary_from_record(record);
                        neighbors.insert(far.clone(), summary.clone());
                        summary
                    }
                };
                edges.push(CodeGraphSemanticEdgeV1 { edge, neighbor });
            }
            edges.sort_by(|left, right| compare_edges(&left.edge, &right.edge));
            edges.dedup();
            batches.push(edges);
        }
        Ok(batches)
    }
}

fn resolve_from_index(
    catalog: &InteractiveCatalog,
    occurrences: Option<&[SymbolOccurrenceId]>,
    kind: Option<&str>,
    limit: usize,
) -> Vec<CodeGraphSymbolSummaryV1> {
    occurrences
        .into_iter()
        .flatten()
        .filter_map(|occurrence| catalog.summary(occurrence))
        .filter(|summary| match kind {
            Some(kind) => summary
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.kind == kind),
            None => true,
        })
        .take(limit)
        .collect()
}

fn summary_from_record(record: SymbolRecordV1) -> CodeGraphSymbolSummaryV1 {
    CodeGraphSymbolSummaryV1 {
        occurrence: record.occurrence,
        binding: record.binding,
        metadata: record.metadata,
    }
}

/// One adjacency row's validated edge payload, checked against the seed it
/// was read from.
fn seed_edge_record(
    seed: &SymbolOccurrenceId,
    relation: &GraphRelation,
    direction: AdjacencyDirection,
) -> Result<CanonicalRelationEdgeV1, CodeGraphProjectionError> {
    let edge = edge_record(relation)?;
    let near = match direction {
        AdjacencyDirection::Outgoing => &edge.from_occurrence,
        AdjacencyDirection::Incoming => &edge.to_occurrence,
    };
    if near != seed {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph edge endpoint does not match its adjacency seed".to_owned(),
        ));
    }
    Ok(edge)
}

/// The edge kind a code edge row names.
fn relation_edge_kind(
    relation: &GraphRelation,
) -> Result<RelationEdgeKindV1, CodeGraphProjectionError> {
    code_edge_kind_edge(relation.kind.as_str()).ok_or_else(|| {
        CodeGraphProjectionError::Corrupt("code graph edge row names no edge kind".to_owned())
    })
}

fn entity_ids(
    occurrences: &[SymbolOccurrenceId],
) -> Result<Vec<GraphEntityId>, CodeGraphProjectionError> {
    if occurrences.is_empty() {
        return Err(CodeGraphProjectionError::Contract(
            "code graph adjacency requires at least one seed".to_owned(),
        ));
    }
    occurrences
        .iter()
        .map(|occurrence| {
            occurrence
                .validate()
                .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
            symbol_entity_id(occurrence)
        })
        .collect()
}

/// Code edge relation kinds for the admitted edge kinds; every kind when
/// none is named.
fn code_relation_kinds(
    kinds: &[RelationEdgeKindV1],
) -> Result<BTreeSet<GraphRelationKind>, CodeGraphProjectionError> {
    let admitted = if kinds.is_empty() {
        &RELATION_EDGE_KINDS[..]
    } else {
        kinds
    };
    admitted
        .iter()
        .map(|kind| GraphRelationKind::new(code_edge_kind(*kind)).map_err(Into::into))
        .collect()
}

fn contains_ignore_ascii_case(value: &str, query: &str) -> bool {
    query.is_empty()
        || value
            .as_bytes()
            .windows(query.len())
            .any(|window| window.eq_ignore_ascii_case(query.as_bytes()))
}

fn require_positive(value: usize, what: &str) -> Result<(), CodeGraphProjectionError> {
    if value == 0 {
        return Err(CodeGraphProjectionError::Contract(format!(
            "{what} must be positive"
        )));
    }
    Ok(())
}

fn reconstruct_path(
    parents: &BTreeMap<SymbolOccurrenceId, CanonicalRelationEdgeV1>,
    from: &SymbolOccurrenceId,
    to: &SymbolOccurrenceId,
) -> Result<Vec<CanonicalRelationEdgeV1>, CodeGraphProjectionError> {
    let mut path = Vec::new();
    let mut cursor = to.clone();
    while cursor != *from {
        let edge = parents.get(&cursor).ok_or_else(|| {
            CodeGraphProjectionError::Corrupt(
                "code graph path reconstruction lost its parent chain".to_owned(),
            )
        })?;
        cursor = edge.from_occurrence.clone();
        path.push(edge.clone());
        if path.len() > parents.len() {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph path reconstruction cycled".to_owned(),
            ));
        }
    }
    path.reverse();
    Ok(path)
}

fn catalog_lock_poisoned() -> CodeGraphProjectionError {
    CodeGraphProjectionError::Unavailable(
        "code graph interactive catalog lock is poisoned".to_owned(),
    )
}

#[cfg(test)]
mod tests;

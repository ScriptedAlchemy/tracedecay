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
//! Name and file keys are served from a [`SymbolCatalog`] built lazily by one
//! bounded, cancellable scan of the projection's symbol entities and cached
//! on the owning [`CodeGraphProjectionStore`]. The catalog is derived from
//! the verified snapshot and shares its lifetime, so it is a cache of the
//! projection authority — not a second authority. Per-seed adjacency reads go
//! straight to the snapshot's kind-filtered relation fan-outs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, RwLock};

use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, FileOccurrenceId, RelationEdgeKindV1,
    SanitizedCodeFileV1, SymbolOccurrenceId,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphProjectionIdentity,
    GraphProjectionReadRequest, GraphRelationId, GraphRelationKind, GraphRelationRef,
    MAX_VERIFIED_GENERATION_RELATIONS, VerifiedGraphSnapshot,
};

use super::{
    CodeGraphProjectionError, CodeGraphReadCancellation, EDGE_LABEL, EDGE_RECORD_PROPERTY,
    FILE_LABEL, FILE_RECORD_PROPERTY, SOURCE_EDGE_KIND, SYMBOL_LABEL, SYMBOL_RECORD_PROPERTY,
    SymbolRecordV1, TARGET_EDGE_KIND, compare_edges, deserialize_property, edge_entity_id,
    file_entity_id, has_label, load_symbol_record, symbol_entity_id, validate_edge,
    validate_symbol_record,
};

mod models;

use self::models::CatalogSymbol;
pub(super) use self::models::SymbolCatalog;
pub use self::models::{
    CodeGraphDegreeRankingV1, CodeGraphEdgeKindCountsV1, CodeGraphImpactBatchV1,
    CodeGraphImpactedSymbolV1, CodeGraphPathSearchV1, CodeGraphSemanticEdgeV1,
    CodeGraphSymbolDegreesV1, CodeGraphSymbolPageV1, CodeGraphSymbolSummaryV1,
};

/// Entities examined per projection page while building the symbol catalog.
const CATALOG_SCAN_PAGE_ENTITIES: usize = 1_024;

/// Symbols measured per bulk degree read while ranking a generation. Bounds
/// the batch-wide relation budget each measurement charges.
const DEGREE_RANKING_BATCH_SYMBOLS: usize = 256;

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
    catalog: Arc<RwLock<Option<Arc<SymbolCatalog>>>>,
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

impl CodeGraphInteractiveReader {
    pub(super) fn assemble(
        generation: CodeGenerationId,
        projection: GraphProjectionIdentity,
        snapshot: Arc<VerifiedGraphSnapshot>,
        projection_node_count: usize,
        cancellation: Arc<dyn GraphCancellation>,
        catalog: Arc<RwLock<Option<Arc<SymbolCatalog>>>>,
    ) -> Self {
        Self {
            generation,
            projection,
            snapshot,
            projection_node_count,
            cancellation,
            catalog,
        }
    }

    pub fn generation(&self) -> &CodeGenerationId {
        &self.generation
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
        Ok(resolve_from_index(
            &catalog,
            catalog.by_qualified_name.get(qualified_name),
            kind,
            limit,
        ))
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
        Ok(resolve_from_index(
            &catalog,
            catalog.by_simple_name.get(&name.to_lowercase()),
            kind,
            limit,
        ))
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
        Ok(resolve_from_index(
            &catalog,
            catalog.by_file.get(file),
            None,
            limit,
        ))
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
        Ok(resolve_from_index(
            &catalog,
            catalog.by_file.get(file),
            None,
            limit,
        ))
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
            return Err(CodeGraphProjectionError::BudgetExhausted);
        }
        Ok(catalog.files.values().cloned().collect())
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
            Some(after) => Box::new(catalog.symbols.range::<SymbolOccurrenceId, _>((
                std::ops::Bound::Excluded(after),
                std::ops::Bound::Unbounded,
            ))),
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
        Ok(CodeGraphSymbolPageV1 { symbols, has_more })
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
        )?;
        let incoming = self.semantic_neighbors(
            seeds,
            &[],
            AdjacencyDirection::Incoming,
            MAX_VERIFIED_GENERATION_RELATIONS,
            cancellation,
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
        let outgoing = self.snapshot.outgoing_relation_ids(
            &starts,
            &source_relation_kinds()?,
            MAX_VERIFIED_GENERATION_RELATIONS,
            Arc::clone(&cancellation),
        )?;
        let incoming = self.snapshot.incoming_relation_ids(
            &starts,
            &target_relation_kinds()?,
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
    /// semantic degree.
    ///
    /// This is the bounded replacement for the dashboard's degree pool and
    /// top-connected panels, both of which aggregated the whole `edges` table
    /// twice per read. `max_symbols_examined` bounds the scan itself, not just
    /// the output: reaching it returns `complete: false` rather than silently
    /// ranking a prefix as if it were the graph. Ordering is total and
    /// deterministic — total degree descending, then qualified name, then
    /// occurrence — so equal-degree symbols do not reshuffle between reads.
    pub fn degree_ranking(
        &self,
        top: usize,
        max_symbols_examined: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphDegreeRankingV1, CodeGraphProjectionError> {
        require_positive(top, "code graph degree ranking size")?;
        require_positive(
            max_symbols_examined,
            "code graph degree ranking examination budget",
        )?;
        let cancellation = self.read_cancellation(Arc::clone(&request_cancellation))?;
        let catalog = self.catalog(cancellation)?;

        let mut measured: Vec<(CodeGraphSymbolDegreesV1, String)> = Vec::new();
        let mut complete = true;
        let mut batch: Vec<SymbolOccurrenceId> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for (occurrence, record) in &catalog.symbols {
            if measured.len() + batch.len() == max_symbols_examined {
                complete = false;
                break;
            }
            batch.push(occurrence.clone());
            names.push(record.metadata.as_ref().map_or_else(
                || occurrence.as_str().to_owned(),
                |metadata| metadata.qualified_name.clone(),
            ));
            if batch.len() == DEGREE_RANKING_BATCH_SYMBOLS {
                self.measure_degree_batch(
                    &batch,
                    &names,
                    &mut measured,
                    Arc::clone(&request_cancellation),
                )?;
                batch.clear();
                names.clear();
            }
        }
        if !batch.is_empty() {
            self.measure_degree_batch(&batch, &names, &mut measured, request_cancellation)?;
        }

        let symbols_examined = measured.len();
        measured.sort_by(|left, right| {
            let left_total = left.0.outgoing.saturating_add(left.0.incoming);
            let right_total = right.0.outgoing.saturating_add(right.0.incoming);
            right_total
                .cmp(&left_total)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.0.occurrence.cmp(&right.0.occurrence))
        });
        measured.truncate(top);
        Ok(CodeGraphDegreeRankingV1 {
            ranked: measured.into_iter().map(|(degrees, _)| degrees).collect(),
            symbols_examined,
            complete,
        })
    }

    /// Measures one batch of the degree ranking scan, pairing each measurement
    /// with the sort name captured for it.
    fn measure_degree_batch(
        &self,
        batch: &[SymbolOccurrenceId],
        names: &[String],
        measured: &mut Vec<(CodeGraphSymbolDegreesV1, String)>,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), CodeGraphProjectionError> {
        let degrees = self.degrees(batch, request_cancellation)?;
        if degrees.len() != names.len() {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph degree ranking batch shape does not match its seeds".to_owned(),
            ));
        }
        measured.extend(degrees.into_iter().zip(names.iter().cloned()));
        Ok(())
    }

    /// Semantic edges induced among a symbol set: edges whose endpoints are
    /// both members. `max_relations` bounds the batch-wide fan-out examined.
    pub fn edges_among(
        &self,
        occurrences: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        max_relations: usize,
        request_cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<CodeGraphSemanticEdgeV1>, CodeGraphProjectionError> {
        let cancellation = self.read_cancellation(request_cancellation)?;
        let members: BTreeSet<_> = occurrences.iter().cloned().collect();
        let per_seed = self.semantic_neighbors(
            occurrences,
            kinds,
            AdjacencyDirection::Outgoing,
            max_relations,
            cancellation,
        )?;
        let mut edges: Vec<_> = per_seed
            .into_iter()
            .flatten()
            .filter(|edge| members.contains(&edge.edge.to_occurrence))
            .collect();
        edges.sort_by(|left, right| compare_edges(&left.edge, &right.edge));
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

    fn catalog(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Arc<SymbolCatalog>, CodeGraphProjectionError> {
        if let Some(catalog) = self
            .catalog
            .read()
            .map_err(|_| catalog_lock_poisoned())?
            .as_ref()
        {
            return Ok(Arc::clone(catalog));
        }
        let built = Arc::new(self.build_catalog(cancellation)?);
        let mut slot = self.catalog.write().map_err(|_| catalog_lock_poisoned())?;
        if let Some(existing) = slot.as_ref() {
            // A concurrent reader finished first; both builds derive from the
            // same immutable snapshot, so either value is authoritative.
            return Ok(Arc::clone(existing));
        }
        *slot = Some(Arc::clone(&built));
        Ok(built)
    }

    fn build_catalog(
        &self,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<SymbolCatalog, CodeGraphProjectionError> {
        let mut catalog = SymbolCatalog {
            symbols: BTreeMap::new(),
            by_qualified_name: BTreeMap::new(),
            by_simple_name: BTreeMap::new(),
            by_file: BTreeMap::new(),
            by_logical_path: BTreeMap::new(),
            files: BTreeMap::new(),
        };
        let mut after: Option<GraphEntityId> = None;
        let mut scanned = 0_usize;
        loop {
            if cancellation.is_cancelled() {
                return Err(CodeGraphProjectionError::Cancelled);
            }
            let page = self.snapshot.read_projection(GraphProjectionReadRequest {
                namespace: self.projection.namespace.clone(),
                projection: self.projection.projection.clone(),
                after_entity: after.clone(),
                after_relation: None,
                max_entities: CATALOG_SCAN_PAGE_ENTITIES,
                max_relations: 0,
                cancellation: Arc::clone(&cancellation),
            })?;
            scanned = scanned.saturating_add(page.entities.len());
            if scanned > self.projection_node_count {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph symbol scan exceeded the declared projection node count".to_owned(),
                ));
            }
            for entity in &page.entities {
                if has_label(entity, FILE_LABEL) {
                    let record: SanitizedCodeFileV1 =
                        deserialize_property(entity, FILE_RECORD_PROPERTY)?;
                    record
                        .validate()
                        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
                    if file_entity_id(&record.file_occurrence_id)? != entity.identity {
                        return Err(CodeGraphProjectionError::Corrupt(
                            "code graph file identity does not match its payload".to_owned(),
                        ));
                    }
                    let previous = catalog.by_logical_path.insert(
                        record.logical_path.clone(),
                        record.file_occurrence_id.clone(),
                    );
                    if let Some(existing) = previous {
                        if existing != record.file_occurrence_id {
                            return Err(CodeGraphProjectionError::Corrupt(format!(
                                "code graph logical path `{}` is claimed by more than one file occurrence",
                                record.logical_path
                            )));
                        }
                    }
                    catalog
                        .files
                        .insert(record.file_occurrence_id.clone(), record);
                    continue;
                }
                if !has_label(entity, SYMBOL_LABEL) {
                    continue;
                }
                let record: SymbolRecordV1 = deserialize_property(entity, SYMBOL_RECORD_PROPERTY)?;
                validate_symbol_record(&record)?;
                if symbol_entity_id(&record.occurrence)? != entity.identity {
                    return Err(CodeGraphProjectionError::Corrupt(
                        "code graph symbol identity does not match its payload".to_owned(),
                    ));
                }
                catalog.insert(
                    record.occurrence.clone(),
                    CatalogSymbol {
                        binding: record.binding,
                        metadata: record.metadata,
                    },
                );
            }
            match page.next_entity {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        Ok(catalog)
    }

    fn semantic_neighbors(
        &self,
        seeds: &[SymbolOccurrenceId],
        kinds: &[RelationEdgeKindV1],
        direction: AdjacencyDirection,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<CodeGraphSemanticEdgeV1>>, CodeGraphProjectionError> {
        let starts = entity_ids(seeds)?;
        let admitted: BTreeSet<RelationEdgeKindV1> = kinds.iter().copied().collect();
        let per_seed_relations = match direction {
            AdjacencyDirection::Outgoing => self.snapshot.outgoing_relation_ids(
                &starts,
                &source_relation_kinds()?,
                max_relations,
                Arc::clone(&cancellation),
            )?,
            AdjacencyDirection::Incoming => self.snapshot.incoming_relation_ids(
                &starts,
                &target_relation_kinds()?,
                max_relations,
                Arc::clone(&cancellation),
            )?,
        };
        if per_seed_relations.len() != seeds.len() {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph adjacency batch shape does not match its seeds".to_owned(),
            ));
        }
        let mut batches = Vec::with_capacity(seeds.len());
        for (seed, relations) in seeds.iter().zip(per_seed_relations) {
            let mut edges = Vec::new();
            for relation in relations {
                if cancellation.is_cancelled() {
                    return Err(CodeGraphProjectionError::Cancelled);
                }
                let edge = self.hydrate_semantic_edge(
                    seed,
                    &relation,
                    direction,
                    Arc::clone(&cancellation),
                )?;
                if admitted.is_empty() || admitted.contains(&edge.edge.kind) {
                    edges.push(edge);
                }
            }
            edges.sort_by(|left, right| compare_edges(&left.edge, &right.edge));
            edges.dedup();
            batches.push(edges);
        }
        Ok(batches)
    }

    fn hydrate_semantic_edge(
        &self,
        seed: &SymbolOccurrenceId,
        relation: &GraphRelationId,
        direction: AdjacencyDirection,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<CodeGraphSemanticEdgeV1, CodeGraphProjectionError> {
        let relation = self
            .snapshot
            .relation(
                &GraphRelationRef::new(self.projection.clone(), relation.clone()),
                Arc::clone(&cancellation),
            )?
            .ok_or_else(|| {
                CodeGraphProjectionError::Corrupt(
                    "code graph adjacency listed a missing relation".to_owned(),
                )
            })?;
        let edge_reference = match direction {
            AdjacencyDirection::Outgoing => &relation.to,
            AdjacencyDirection::Incoming => &relation.from,
        };
        let entity = self
            .snapshot
            .entity(edge_reference, Arc::clone(&cancellation))?
            .ok_or_else(|| {
                CodeGraphProjectionError::Corrupt(
                    "code graph adjacency referenced a missing edge entity".to_owned(),
                )
            })?;
        let edge = load_edge_record(&entity)?;
        let (near, far) = match direction {
            AdjacencyDirection::Outgoing => (&edge.from_occurrence, &edge.to_occurrence),
            AdjacencyDirection::Incoming => (&edge.to_occurrence, &edge.from_occurrence),
        };
        if near != seed {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph edge endpoint does not match its adjacency seed".to_owned(),
            ));
        }
        let neighbor = load_symbol_record(&self.snapshot, &self.projection, far, cancellation)?
            .ok_or_else(|| {
                CodeGraphProjectionError::Corrupt(
                    "code graph edge endpoint has no symbol entity".to_owned(),
                )
            })?;
        Ok(CodeGraphSemanticEdgeV1 {
            edge,
            neighbor: summary_from_record(neighbor),
        })
    }
}

fn resolve_from_index(
    catalog: &SymbolCatalog,
    occurrences: Option<&Vec<SymbolOccurrenceId>>,
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

fn load_edge_record(
    entity: &GraphEntity,
) -> Result<CanonicalRelationEdgeV1, CodeGraphProjectionError> {
    if !has_label(entity, EDGE_LABEL) {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph adjacency contains a non-edge entity".to_owned(),
        ));
    }
    let edge: CanonicalRelationEdgeV1 = deserialize_property(entity, EDGE_RECORD_PROPERTY)?;
    validate_edge(&edge)?;
    if edge_entity_id(&edge)? != entity.identity {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph edge identity does not match its payload".to_owned(),
        ));
    }
    Ok(edge)
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

fn source_relation_kinds() -> Result<BTreeSet<GraphRelationKind>, CodeGraphProjectionError> {
    Ok(BTreeSet::from([GraphRelationKind::new(SOURCE_EDGE_KIND)?]))
}

fn target_relation_kinds() -> Result<BTreeSet<GraphRelationKind>, CodeGraphProjectionError> {
    Ok(BTreeSet::from([GraphRelationKind::new(TARGET_EDGE_KIND)?]))
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
    CodeGraphProjectionError::Unavailable("code graph symbol catalog lock is poisoned".to_owned())
}

#[cfg(test)]
mod tests;

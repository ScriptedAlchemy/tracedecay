//! Production graph evidence adapter over one frozen Plan 25 generation.
//!
//! Traversal runs over canonical relation edges only. Tree-sitter object
//! identity never becomes product identity; each emitted path re-binds to
//! generation-local symbol/file/chunk occurrences from admitted chunks.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use grafeo_adapters::plugins::algorithms::{Control, TraversalEvent, bfs_with_visitor};
use grafeo_common::types::{EdgeId, NodeId};
use grafeo_core::graph::{
    Direction, GraphProjection, GraphStoreSearch, ProjectionSpec, lpg::LpgStore,
};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, CodeSearchChunkId, CodeSearchChunkV1,
    CompactCandidate, ComponentRevision, EdgeAuthorityV1, EvidenceRole, FileOccurrenceId,
    FixedPointScore, FreshnessCompatibilityV1, LanguageDescriptorRevision, LogicalEvidenceId,
    RelationEdgeKindV1, RepositoryId, RetrievalAnchorId, RetrieverBatch, RetrieverCoverage,
    RetrieverKind, RetrieverOutcome, ScoreDomainId, SourceFreshness, SourceOccurrenceId,
    SymbolOccurrenceId, UtcMicros,
};

use super::{GraphLaneEvidence, GraphLaneRequest, GraphPathSegmentV1};
use crate::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, GraphEvidenceReadPort, RetrievalPortError,
    contract_error,
};

// Domain-scoped labels and edge types let a future Work DAG use the same
// embedded Grafeo substrate through a filtered projection without overlap.
const CODE_SYMBOL_LABEL: &str = "TraceDecayCodeSymbol";

#[derive(Clone, Debug)]
struct SymbolBindingV1 {
    file: FileOccurrenceId,
    chunk: Option<CodeSearchChunkId>,
    language_descriptor_revision: LanguageDescriptorRevision,
}

/// Immutable read port over one published generation's relation evidence.
#[derive(Clone)]
pub struct CodeGraphEvidenceAdapterV1 {
    generation: CodeGenerationId,
    repository_id: Option<RepositoryId>,
    freshness: SourceFreshness,
    retriever_revision: ComponentRevision,
    score_domain: ScoreDomainId,
    graph: Arc<LpgStore>,
    nodes: Arc<BTreeMap<SymbolOccurrenceId, NodeId>>,
    edges: Arc<BTreeMap<EdgeId, GraphPathSegmentV1>>,
    symbols: Arc<BTreeMap<SymbolOccurrenceId, SymbolBindingV1>>,
}

impl fmt::Debug for CodeGraphEvidenceAdapterV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeGraphEvidenceAdapterV1")
            .field("generation", &self.generation)
            .field("repository_id", &self.repository_id)
            .field("freshness", &self.freshness)
            .field("symbols", &self.symbols.len())
            .field("edges", &self.edges.len())
            .finish_non_exhaustive()
    }
}

impl CodeGraphEvidenceAdapterV1 {
    /// Build a generation-bound graph evidence port from published edges and
    /// the chunks that carry occurrence/file/chunk binding facts.
    pub fn new(
        generation: CodeGenerationId,
        repository_id: Option<RepositoryId>,
        freshness: SourceFreshness,
        edges: &[CanonicalRelationEdgeV1],
        chunks: &[CodeSearchChunkV1],
    ) -> Result<Self, RetrievalPortError> {
        generation.validate().map_err(contract_error)?;
        if let Some(repository_id) = &repository_id {
            repository_id.validate().map_err(contract_error)?;
        }
        freshness
            .source_namespace
            .validate()
            .map_err(contract_error)?;
        freshness
            .source_instance
            .validate()
            .map_err(contract_error)?;
        freshness
            .policy_revision
            .validate()
            .map_err(contract_error)?;

        let mut symbols = BTreeMap::new();
        for chunk in chunks {
            if chunk.anchor.generation_id != generation {
                return Err(RetrievalPortError::GenerationMismatch);
            }
            let Some(symbol) = chunk.anchor.symbol_occurrence_id.clone() else {
                continue;
            };
            let candidate = SymbolBindingV1 {
                file: chunk.anchor.file_occurrence_id.clone(),
                chunk: Some(chunk.id.clone()),
                language_descriptor_revision: chunk.language_descriptor_revision.clone(),
            };
            match symbols.entry(symbol) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(candidate);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let current = entry.get_mut();
                    if current.file != candidate.file
                        || current.language_descriptor_revision
                            != candidate.language_descriptor_revision
                    {
                        return Err(RetrievalPortError::Contract(
                            "one symbol occurrence has conflicting graph candidate bindings"
                                .to_owned(),
                        ));
                    }
                    if candidate.chunk < current.chunk {
                        current.chunk = candidate.chunk;
                    }
                }
            }
        }

        let mut retained_edges: Vec<_> = edges
            .iter()
            .filter(|edge| symbols.contains_key(&edge.from_occurrence))
            .cloned()
            .collect();
        retained_edges.sort_by(|left, right| {
            (
                &left.from_occurrence,
                &left.to_occurrence,
                left.kind,
                left.authority,
                left.evidence_span.start_byte,
                left.evidence_span.end_byte,
            )
                .cmp(&(
                    &right.from_occurrence,
                    &right.to_occurrence,
                    right.kind,
                    right.authority,
                    right.evidence_span.start_byte,
                    right.evidence_span.end_byte,
                ))
        });
        retained_edges.dedup();

        let mut occurrences: BTreeSet<_> = symbols.keys().cloned().collect();
        for edge in &retained_edges {
            occurrences.insert(edge.to_occurrence.clone());
        }
        let graph = Arc::new(LpgStore::new().map_err(graph_unavailable)?);
        let nodes: BTreeMap<_, _> = occurrences
            .into_iter()
            .map(|occurrence| {
                // Grafeo IDs are disposable projection-local handles. Stable
                // authority stays in typed occurrence bindings and edges.
                let node = graph.create_node(&[CODE_SYMBOL_LABEL]);
                (occurrence, node)
            })
            .collect();
        let mut projected_edges = BTreeMap::new();
        for edge in retained_edges {
            let from = nodes.get(&edge.from_occurrence).copied().ok_or_else(|| {
                RetrievalPortError::Contract(
                    "graph projection lost a retained source binding".to_owned(),
                )
            })?;
            let to = nodes.get(&edge.to_occurrence).copied().ok_or_else(|| {
                RetrievalPortError::Contract(
                    "graph projection lost a retained target binding".to_owned(),
                )
            })?;
            let edge_id = graph.create_edge(from, to, edge_type(edge.kind));
            projected_edges.insert(
                edge_id,
                GraphPathSegmentV1 {
                    from: edge.from_occurrence,
                    to: edge.to_occurrence,
                    edge_kind: edge.kind,
                    authority: edge.authority,
                    evidence_span: edge.evidence_span,
                },
            );
        }

        Ok(Self {
            generation,
            repository_id,
            freshness,
            retriever_revision: ComponentRevision::new(
                crate::retrieval::QUERY_GRAPH_RETRIEVER_REVISION_V1,
            )
            .map_err(contract_error)?,
            score_domain: ScoreDomainId::new(crate::retrieval::QUERY_GRAPH_SCORE_DOMAIN_V1)
                .map_err(contract_error)?,
            graph,
            nodes: Arc::new(nodes),
            edges: Arc::new(projected_edges),
            symbols: Arc::new(symbols),
        })
    }

    fn validate_generation(&self, request: &GraphLaneRequest) -> Result<(), RetrievalPortError> {
        if request.generation != self.generation {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        Ok(())
    }

    fn traverse(
        &self,
        request: &GraphLaneRequest,
    ) -> Result<RetrieverBatch<GraphLaneEvidence>, RetrievalPortError> {
        let edge_kinds: BTreeSet<RelationEdgeKindV1> = request.edge_kinds.iter().copied().collect();
        let mut best_by_occurrence = BTreeMap::new();
        let mut examined = 0u64;
        let mut excluded = 0u64;
        let mut unknown = 0u64;

        let graph_store: Arc<dyn GraphStoreSearch> = self.graph.clone();
        let projection = GraphProjection::new(
            graph_store,
            ProjectionSpec::new()
                .with_node_labels([CODE_SYMBOL_LABEL])
                .with_edge_types(edge_kinds.iter().copied().map(edge_type)),
        );
        for seed in &request.seed_anchors {
            let seed_symbol = seed.occurrence.symbol.as_ref().ok_or_else(|| {
                RetrievalPortError::Contract(
                    "graph seed anchors require a symbol occurrence".to_owned(),
                )
            })?;
            let Some(seed_node) = self.nodes.get(seed_symbol).copied() else {
                continue;
            };
            if !self.symbols.contains_key(seed_symbol) {
                return Err(RetrievalPortError::Contract(
                    "graph projection authorized an unbound seed".to_owned(),
                ));
            }

            let mut frontiers = BTreeMap::from([(seed_node, vec![FrontierPath::seed()])]);
            let traversal_error =
                bfs_with_visitor::<RetrievalPortError, _>(&projection, seed_node, |event| {
                    let result = (|| match event {
                        TraversalEvent::Discover(node) => {
                            let paths = projected_frontier(&frontiers, node)?;
                            let path = paths.first().ok_or_else(|| {
                                RetrievalPortError::Contract(
                                    "Grafeo returned an empty path frontier".to_owned(),
                                )
                            })?;
                            let bound = path
                                .segments
                                .last()
                                .is_none_or(|segment| self.symbols.contains_key(&segment.to));
                            if path.segments.len() >= request.max_depth as usize || !bound {
                                Ok(Control::Prune)
                            } else {
                                Ok(Control::Continue)
                            }
                        }
                        TraversalEvent::TreeEdge {
                            source,
                            target,
                            edge,
                        }
                        | TraversalEvent::NonTreeEdge {
                            source,
                            target,
                            edge,
                        } => {
                            let prefixes = projected_frontier(&frontiers, source)?.to_vec();
                            let segment = self.edges.get(&edge).ok_or_else(|| {
                                RetrievalPortError::Contract(
                                    "Grafeo traversal referenced an unknown projected edge"
                                        .to_owned(),
                                )
                            })?;
                            for prefix in prefixes {
                                let path = prefix.extended(segment);
                                admit_frontier_path(frontiers.entry(target).or_default(), path);
                            }
                            Ok(Control::Continue)
                        }
                        TraversalEvent::Finish(node) => {
                            let paths = projected_frontier(&frontiers, node)?;
                            let path = paths.first().ok_or_else(|| {
                                RetrievalPortError::Contract(
                                    "Grafeo returned an empty path frontier".to_owned(),
                                )
                            })?;
                            if path.segments.len() < request.max_depth as usize {
                                let expansions = paths.len() as u64;
                                for (_, edge_id) in self.graph.edges_from(node, Direction::Outgoing)
                                {
                                    let segment = self.edges.get(&edge_id).ok_or_else(|| {
                                        RetrievalPortError::Contract(
                                            "Grafeo adjacency referenced an unknown \
                                                     projected edge"
                                                .to_owned(),
                                        )
                                    })?;
                                    examined = examined.saturating_add(expansions);
                                    if !edge_kinds.contains(&segment.edge_kind) {
                                        excluded = excluded.saturating_add(expansions);
                                    }
                                }
                            }
                            Ok(Control::Continue)
                        }
                        TraversalEvent::BackEdge { .. } => Ok(Control::Continue),
                    })();
                    match result {
                        Ok(control) => control,
                        Err(error) => Control::Break(error),
                    }
                });
            if let Some(error) = traversal_error {
                return Err(error);
            }

            for paths in frontiers.into_values() {
                let first = paths.first().ok_or_else(|| {
                    RetrievalPortError::Contract(
                        "Grafeo returned an empty path frontier".to_owned(),
                    )
                })?;
                let Some(target) = first.segments.last().map(|segment| segment.to.clone()) else {
                    continue;
                };
                let Some(binding_meta) = self.symbols.get(&target) else {
                    unknown = unknown.saturating_add(paths.len() as u64);
                    continue;
                };
                let best = best_frontier_path(paths)?;
                let weakest_authority = best.weakest.ok_or_else(|| {
                    RetrievalPortError::Contract("Grafeo returned an empty graph path".to_owned())
                })?;
                let score_micros = best.score;
                let path = best.segments;
                let occurrence = format!("code-graph:{}", target.as_str());
                let evidence_id = format!("code-symbol:{}", target.as_str());
                let anchor_id = retrieval_anchor(evidence_id.clone())?;
                let logical_evidence_id =
                    LogicalEvidenceId::new(evidence_id).map_err(contract_error)?;
                let candidate = CompactCandidate {
                    anchor_id,
                    logical_evidence_id,
                    source_occurrence_id: SourceOccurrenceId::new(occurrence.clone())
                        .map_err(contract_error)?,
                    file_occurrence_id: Some(binding_meta.file.clone()),
                    source_namespace: self.freshness.source_namespace.clone(),
                    repository_id: self.repository_id.clone(),
                    session_or_thread_id: None,
                    logical_copy_cluster_id: None,
                    logical_copy_evidence_anchor: None,
                    evidence_role: EvidenceRole::Primary,
                    retriever: RetrieverKind::Graph,
                    retriever_revision: self.retriever_revision.clone(),
                    score_domain: self.score_domain.clone(),
                    raw_score: FixedPointScore(score_micros),
                    ordinal_rank: 0,
                    exact_admission_proof: None,
                    retriever_evidence_anchor: retrieval_anchor(format!("evidence.{occurrence}"))?,
                    freshness: self.freshness.clone(),
                };
                let evidence = GraphLaneEvidence {
                    binding: CodeCandidateBindingV1 {
                        candidate_anchor: candidate.anchor_id.clone(),
                        occurrence: CodeOccurrenceRefV1 {
                            generation: self.generation.clone(),
                            file: binding_meta.file.clone(),
                            symbol: Some(target),
                            chunk: binding_meta.chunk.clone(),
                        },
                        language_descriptor_revision: binding_meta
                            .language_descriptor_revision
                            .clone(),
                        matched_term_kinds: Vec::new(),
                        source_occurrence: candidate.source_occurrence_id.clone(),
                    },
                    path,
                    weakest_authority,
                };
                match best_by_occurrence.entry(candidate.source_occurrence_id.clone()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert((candidate, evidence));
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let (current_candidate, current_evidence) = entry.get();
                        if candidate.raw_score > current_candidate.raw_score
                            || (candidate.raw_score == current_candidate.raw_score
                                && compare_paths(&evidence.path, &current_evidence.path).is_lt())
                        {
                            entry.insert((candidate, evidence));
                        }
                    }
                }
            }
        }

        let mut pairs: Vec<_> = best_by_occurrence.into_values().collect();
        pairs.sort_by(|left, right| {
            right.0.raw_score.cmp(&left.0.raw_score).then_with(|| {
                left.0
                    .source_occurrence_id
                    .cmp(&right.0.source_occurrence_id)
            })
        });

        let mut candidates = Vec::with_capacity(pairs.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, (mut candidate, evidence)) in pairs.into_iter().enumerate() {
            candidate.ordinal_rank = ordinal as u32;
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        let eligible = candidates.len() as u64;
        Ok(RetrieverBatch {
            candidates,
            evidence_by_occurrence,
            coverage: RetrieverCoverage {
                examined,
                eligible,
                excluded,
                capped: 0,
                unknown,
            },
            continuation: None,
        })
    }
}

#[derive(Clone)]
struct FrontierPath {
    segments: Vec<GraphPathSegmentV1>,
    weakest: Option<EdgeAuthorityV1>,
    score: u64,
}

impl FrontierPath {
    fn seed() -> Self {
        Self {
            segments: Vec::new(),
            weakest: None,
            score: u64::MAX,
        }
    }

    fn extended(&self, segment: &GraphPathSegmentV1) -> Self {
        let weakest = self.weakest.map_or(segment.authority, |current| {
            current.weakest(segment.authority)
        });
        let mut segments = self.segments.clone();
        segments.push(segment.clone());
        Self {
            score: graph_score_micros(segments.len(), weakest),
            segments,
            weakest: Some(weakest),
        }
    }
}

type PathFrontiers = BTreeMap<NodeId, Vec<FrontierPath>>;

fn projected_frontier(
    frontiers: &PathFrontiers,
    node: NodeId,
) -> Result<&[FrontierPath], RetrievalPortError> {
    frontiers
        .get(&node)
        .filter(|paths| !paths.is_empty())
        .map(Vec::as_slice)
        .ok_or_else(|| RetrievalPortError::Contract("Grafeo node has no path frontier".to_owned()))
}

fn admit_frontier_path(frontier: &mut Vec<FrontierPath>, candidate: FrontierPath) {
    if let Some(depth) = frontier.first().map(|path| path.segments.len()) {
        if depth < candidate.segments.len() {
            return;
        }
        if depth > candidate.segments.len() {
            frontier.clear();
        }
    }

    for current in frontier.iter() {
        if current.score >= candidate.score
            && !compare_paths(&current.segments, &candidate.segments).is_gt()
        {
            return;
        }
    }

    let mut retained = Vec::with_capacity(frontier.len() + 1);
    for current in frontier.drain(..) {
        if candidate.score < current.score
            || compare_paths(&candidate.segments, &current.segments).is_gt()
        {
            retained.push(current);
        }
    }
    retained.push(candidate);
    retained.sort_by(|left, right| compare_paths(&left.segments, &right.segments));
    *frontier = retained;
}

fn best_frontier_path(paths: Vec<FrontierPath>) -> Result<FrontierPath, RetrievalPortError> {
    let mut best = None::<FrontierPath>;
    for path in paths {
        let improves = best.as_ref().is_none_or(|current| {
            path.score > current.score
                || (path.score == current.score
                    && compare_paths(&path.segments, &current.segments).is_lt())
        });
        if improves {
            best = Some(path);
        }
    }
    best.ok_or_else(|| RetrievalPortError::Contract("Grafeo path frontier is empty".to_owned()))
}

fn edge_type(kind: RelationEdgeKindV1) -> &'static str {
    match kind {
        RelationEdgeKindV1::Calls => "CodeCalls",
        RelationEdgeKindV1::Uses => "CodeUses",
        RelationEdgeKindV1::TypeOf => "CodeTypeOf",
        RelationEdgeKindV1::Contains => "CodeContains",
        RelationEdgeKindV1::Implements => "CodeImplements",
        RelationEdgeKindV1::Extends => "CodeExtends",
        RelationEdgeKindV1::Annotates => "CodeAnnotates",
    }
}

fn graph_unavailable(error: impl fmt::Display) -> RetrievalPortError {
    RetrievalPortError::AuthorityUnavailable(format!(
        "embedded Grafeo projection is unavailable: {error}"
    ))
}

fn compare_paths(left: &[GraphPathSegmentV1], right: &[GraphPathSegmentV1]) -> Ordering {
    left.iter()
        .map(|segment| {
            (
                &segment.from,
                &segment.to,
                segment.edge_kind,
                segment.authority,
                segment.evidence_span,
            )
        })
        .cmp(right.iter().map(|segment| {
            (
                &segment.from,
                &segment.to,
                segment.edge_kind,
                segment.authority,
                segment.evidence_span,
            )
        }))
}

impl GraphEvidenceReadPort for CodeGraphEvidenceAdapterV1 {
    fn read_graph_evidence(
        &self,
        request: &GraphLaneRequest,
    ) -> Result<RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>, RetrievalPortError> {
        request.validate()?;
        self.validate_generation(request)?;
        if self.freshness.compatibility != FreshnessCompatibilityV1::Current {
            return Ok(RetrieverOutcome::Stale(self.freshness.clone()));
        }
        Ok(RetrieverOutcome::Complete(self.traverse(request)?))
    }
}

fn graph_score_micros(path_len: usize, authority: EdgeAuthorityV1) -> u64 {
    let depth_bonus = 1_000_000u64.saturating_sub((path_len as u64).saturating_mul(50_000));
    let authority_bonus = match authority {
        EdgeAuthorityV1::SyntaxExact => 40_000,
        EdgeAuthorityV1::NameResolved => 30_000,
        EdgeAuthorityV1::CompilerOrLspResolved => 20_000,
        EdgeAuthorityV1::DynamicObserved => 10_000,
        EdgeAuthorityV1::HeuristicCandidate => 5_000,
        EdgeAuthorityV1::UnknownUnsupported => 1_000,
    };
    depth_bonus.saturating_add(authority_bonus)
}

fn retrieval_anchor(value: String) -> Result<RetrievalAnchorId, RetrievalPortError> {
    RetrievalAnchorId::new(value).map_err(contract_error)
}

/// Shared freshness envelope for daemon-owned production graph/exact/lexical
/// owners reading one complete published generation.
pub fn production_code_index_freshness(
    observed_at: UtcMicros,
    policy_revision: ComponentRevision,
) -> Result<SourceFreshness, RetrievalPortError> {
    Ok(SourceFreshness {
        source_namespace: tracedecay_domain::SourceNamespace::new("ns.code.daemon")
            .map_err(contract_error)?,
        source_instance: tracedecay_domain::SourceInstanceKey::new("instance.code-index.daemon")
            .map_err(contract_error)?,
        source_watermark: Some(1),
        projection_watermark: Some(1),
        observed_at,
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility: FreshnessCompatibilityV1::Current,
        policy_revision,
    })
}

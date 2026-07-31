//! Production graph evidence adapter over one frozen Plan 25 generation.
//!
//! Traversal runs over canonical relation edges only. Tree-sitter object
//! identity never becomes product identity; each emitted path re-binds to
//! generation-local symbol/file/chunk occurrences from admitted chunks.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

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

#[derive(Clone, Debug)]
struct SymbolBindingV1 {
    file: FileOccurrenceId,
    chunk: Option<CodeSearchChunkId>,
    language_descriptor_revision: LanguageDescriptorRevision,
}

/// Immutable read port over one published generation's relation evidence.
#[derive(Clone, Debug)]
pub struct CodeGraphEvidenceAdapterV1 {
    generation: CodeGenerationId,
    repository_id: Option<RepositoryId>,
    freshness: SourceFreshness,
    retriever_revision: ComponentRevision,
    score_domain: ScoreDomainId,
    adjacency: Arc<BTreeMap<SymbolOccurrenceId, Vec<CanonicalRelationEdgeV1>>>,
    symbols: Arc<BTreeMap<SymbolOccurrenceId, SymbolBindingV1>>,
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
        generation
            .validate()
            .map_err(contract_error)?;
        if let Some(repository_id) = &repository_id {
            repository_id
                .validate()
                .map_err(contract_error)?;
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

        let mut adjacency: BTreeMap<SymbolOccurrenceId, Vec<CanonicalRelationEdgeV1>> =
            BTreeMap::new();
        for edge in edges {
            if !symbols.contains_key(&edge.from_occurrence) {
                // An unbound source cannot be reached from an authorized
                // seed. A bound source with an unbound target is retained so
                // traversal can report the missing target as unknown.
                continue;
            }
            adjacency
                .entry(edge.from_occurrence.clone())
                .or_default()
                .push(edge.clone());
        }
        for neighbors in adjacency.values_mut() {
            neighbors.sort_by(|left, right| {
                (
                    &left.to_occurrence,
                    left.kind,
                    left.authority,
                    left.evidence_span.start_byte,
                    left.evidence_span.end_byte,
                )
                    .cmp(&(
                        &right.to_occurrence,
                        right.kind,
                        right.authority,
                        right.evidence_span.start_byte,
                        right.evidence_span.end_byte,
                    ))
            });
            neighbors.dedup_by(|left, right| {
                left.to_occurrence == right.to_occurrence
                    && left.kind == right.kind
                    && left.authority == right.authority
                    && left.evidence_span == right.evidence_span
            });
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
            adjacency: Arc::new(adjacency),
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
        for seed in &request.seed_anchors {
            let seed_symbol = seed.occurrence.symbol.as_ref().ok_or_else(|| {
                RetrievalPortError::Contract(
                    "graph seed anchors require a symbol occurrence".to_owned(),
                )
            })?;
            if !self.symbols.contains_key(seed_symbol) {
                continue;
            }
            let mut queue = VecDeque::new();
            queue.push_back((seed_symbol.clone(), Vec::<GraphPathSegmentV1>::new()));
            let mut best_seed_paths = BTreeMap::new();
            best_seed_paths.insert(seed_symbol.clone(), (u64::MAX, Vec::new()));
            while let Some((current, path)) = queue.pop_front() {
                if path.len() as u32 >= request.max_depth {
                    continue;
                }
                let Some(neighbors) = self.adjacency.get(&current) else {
                    continue;
                };
                for edge in neighbors {
                    examined = examined.saturating_add(1);
                    if !edge_kinds.contains(&edge.kind) {
                        excluded = excluded.saturating_add(1);
                        continue;
                    }
                    let mut next_path = path.clone();
                    next_path.push(GraphPathSegmentV1 {
                        from: edge.from_occurrence.clone(),
                        to: edge.to_occurrence.clone(),
                        edge_kind: edge.kind,
                        authority: edge.authority,
                        evidence_span: edge.evidence_span,
                    });
                    let weakest_authority = next_path
                        .iter()
                        .map(|segment| segment.authority)
                        .reduce(EdgeAuthorityV1::weakest)
                        .unwrap_or_else(|| panic!("path has at least one edge"));
                    let score_micros = graph_score_micros(next_path.len(), weakest_authority);
                    let improves_seed_path = match best_seed_paths.get(&edge.to_occurrence) {
                        None => true,
                        Some((current_score, current_path)) => {
                            score_micros > *current_score
                                || (score_micros == *current_score
                                    && canonical_path_key_from_segments(&next_path)
                                        < canonical_path_key_from_segments(current_path))
                        }
                    };
                    if !improves_seed_path {
                        continue;
                    }
                    best_seed_paths.insert(
                        edge.to_occurrence.clone(),
                        (score_micros, next_path.clone()),
                    );
                    let Some(binding_meta) = self.symbols.get(&edge.to_occurrence) else {
                        unknown = unknown.saturating_add(1);
                        continue;
                    };
                    let occurrence = format!("code-graph:{}", edge.to_occurrence.as_str());
                    let evidence_id = format!("code-symbol:{}", edge.to_occurrence.as_str());
                    let anchor_id = retrieval_anchor(evidence_id.clone())?;
                    let logical_evidence_id = LogicalEvidenceId::new(evidence_id)
                        .map_err(contract_error)?;
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
                        retriever_evidence_anchor: retrieval_anchor(format!(
                            "evidence.{occurrence}"
                        ))?,
                        freshness: self.freshness.clone(),
                    };
                    let evidence = GraphLaneEvidence {
                        binding: CodeCandidateBindingV1 {
                            candidate_anchor: candidate.anchor_id.clone(),
                            occurrence: CodeOccurrenceRefV1 {
                                generation: self.generation.clone(),
                                file: binding_meta.file.clone(),
                                symbol: Some(edge.to_occurrence.clone()),
                                chunk: binding_meta.chunk.clone(),
                            },
                            language_descriptor_revision: binding_meta
                                .language_descriptor_revision
                                .clone(),
                            matched_term_kinds: Vec::new(),
                            source_occurrence: candidate.source_occurrence_id.clone(),
                        },
                        path: next_path.clone(),
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
                                    && canonical_path_key(&evidence)
                                        < canonical_path_key(current_evidence))
                            {
                                entry.insert((candidate, evidence));
                            }
                        }
                    }
                    queue.push_back((edge.to_occurrence.clone(), next_path));
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

fn canonical_path_key(
    evidence: &GraphLaneEvidence,
) -> Vec<(
    &SymbolOccurrenceId,
    &SymbolOccurrenceId,
    RelationEdgeKindV1,
    EdgeAuthorityV1,
    tracedecay_domain::SourceSpan,
)> {
    canonical_path_key_from_segments(&evidence.path)
}

fn canonical_path_key_from_segments(
    path: &[GraphPathSegmentV1],
) -> Vec<(
    &SymbolOccurrenceId,
    &SymbolOccurrenceId,
    RelationEdgeKindV1,
    EdgeAuthorityV1,
    tracedecay_domain::SourceSpan,
)> {
    path.iter()
        .map(|segment| {
            (
                &segment.from,
                &segment.to,
                segment.edge_kind,
                segment.authority,
                segment.evidence_span,
            )
        })
        .collect()
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

//! Vector read ports over published and isolated evaluation generations.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkV1, CompactCandidate, ComponentRevision, EvidenceRole,
    FixedPointScore, LogicalEvidenceId, ManifestDigest, RetrievalAnchorId, RetrieverKind,
    ScoreDomainId, SemanticSearchIndexKeyV1, SemanticSearchIndexKindV1, SourceOccurrenceId,
    VectorGenerationIdV1,
};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_query::retrieval::graph::production_code_index_freshness;
use tracedecay_query::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, RetrievalPortError,
};
use tracedecay_query::retrieval::semantic::{
    SemanticAnnCandidateWindowV1, SemanticAnnCandidatesV1, SemanticAnnIndexStateV1,
    SemanticSearchKindV1, SemanticVectorReadPort, SemanticVectorReadRequestV1,
    SemanticVectorRecordV1, SemanticVectorScanSummaryV1,
};
use tracedecay_semantic::projector::PreparedVectorGenerationV1;

use super::source_coherence::{
    SemanticSourceCoherenceOutcomeV1, SemanticSourceCoherenceV1, semantic_source_coherence,
};
use crate::store::vector_generations::{
    GraphVectorGenerationStoreV1, PublishedVectorGenerationV1, SemanticAnnServingIndexV1,
    VectorGenerationStoreErrorV1,
};

pub(super) struct PublishedSemanticVectorReadPortV1 {
    pub(super) generation: VectorGenerationIdV1,
    pub(super) projection_key: tracedecay_domain::ProjectionKeyV1,
    pub(super) search_index_key: SemanticSearchIndexKeyV1,
    pub(super) source_generation: CodeGenerationId,
    pub(super) capability_manifest_digest: ManifestDigest,
    pub(super) source_coherence: SemanticSourceCoherenceV1,
    pub(super) rows: Vec<SemanticVectorRecordV1>,
    pub(super) ann: PublishedSemanticAnnBindingV1,
}

/// The port's generation-bound ANN candidate authority, decided once at
/// construction against the complete resident row set.
pub(super) enum PublishedSemanticAnnBindingV1 {
    /// The typed reason ANN candidates cannot serve; the lane observes it
    /// and falls back to the exact-flat scan.
    Unavailable(SemanticAnnIndexStateV1),
    /// A persisted index whose census equals the resident row count, so
    /// every index hit maps to exactly one resident row.
    Serving {
        index: SemanticAnnServingIndexV1,
        rows_by_chunk: BTreeMap<tracedecay_domain::CodeSearchChunkId, usize>,
    },
}

impl PublishedSemanticAnnBindingV1 {
    /// Binds an acquired index to the resident rows, or records the typed
    /// reason none serves. `index` must be `None` exactly when the store was
    /// consulted and held no populated index for the generation.
    pub(super) fn bind(
        search_index_key: &SemanticSearchIndexKeyV1,
        index: Option<SemanticAnnServingIndexV1>,
        rows: &[SemanticVectorRecordV1],
    ) -> Result<Self, RetrievalPortError> {
        match (search_index_key.kind, index) {
            (SemanticSearchIndexKindV1::ExactFlat, None) => {
                Ok(Self::Unavailable(SemanticAnnIndexStateV1::Unsupported))
            }
            (SemanticSearchIndexKindV1::ExactFlat, Some(_)) => Err(RetrievalPortError::Contract(
                "an exact-flat semantic port must not bind an ANN index".to_owned(),
            )),
            (SemanticSearchIndexKindV1::AnnHnswExactRescore, None) => {
                Ok(Self::Unavailable(SemanticAnnIndexStateV1::Missing))
            }
            (SemanticSearchIndexKindV1::AnnHnswExactRescore, Some(index)) => {
                let resident = rows.len() as u64;
                if index.indexed() == resident {
                    let rows_by_chunk = rows
                        .iter()
                        .enumerate()
                        .map(|(ordinal, row)| (row.chunk_id.clone(), ordinal))
                        .collect();
                    Ok(Self::Serving {
                        index,
                        rows_by_chunk,
                    })
                } else {
                    // The index covers only this generation's own staged
                    // vectors; rows hydrated from base-generation reuse are
                    // resident but not indexed, so candidate generation
                    // would silently drop them.
                    Ok(Self::Unavailable(
                        SemanticAnnIndexStateV1::IncompleteCoverage {
                            indexed: index.indexed(),
                            resident,
                        },
                    ))
                }
            }
        }
    }
}

pub(super) fn semantic_candidate_identity(
    chunk: &CodeSearchChunkV1,
) -> Result<(RetrievalAnchorId, LogicalEvidenceId, SourceOccurrenceId), RetrievalPortError> {
    let chunk_id = chunk.id.as_str();
    let evidence_id = chunk.anchor.symbol_occurrence_id.as_ref().map_or_else(
        || format!("code-chunk:{chunk_id}"),
        |symbol| format!("code-symbol:{}", symbol.as_str()),
    );
    Ok((
        RetrievalAnchorId::new(evidence_id.clone())
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        LogicalEvidenceId::new(evidence_id)
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        SourceOccurrenceId::new(format!("code-chunk:{chunk_id}"))
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
    ))
}

pub(super) struct ScopedSemanticEvaluationVectorReadPortV1<'a> {
    pub(super) inner: &'a PublishedSemanticVectorReadPortV1,
    pub(super) allowed_chunks: &'a BTreeSet<tracedecay_domain::CodeSearchChunkId>,
}

impl SemanticVectorReadPort for ScopedSemanticEvaluationVectorReadPortV1<'_> {
    fn scan_exact_flat(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        examine: &mut dyn FnMut() -> Result<(), RetrievalPortError>,
        visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
    ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
        let mut eligible = 0_u64;
        let mut scoped_visit = |row: &SemanticVectorRecordV1| {
            if self.allowed_chunks.contains(&row.chunk_id) {
                eligible = eligible.saturating_add(1);
                visit(row)?;
            }
            Ok(())
        };
        let summary = self
            .inner
            .scan_exact_flat(request, examine, &mut scoped_visit)?;
        Ok(SemanticVectorScanSummaryV1 {
            examined: summary.examined,
            eligible,
            excluded: summary.examined.saturating_sub(eligible),
            unknown: summary.unknown,
        })
    }
}

pub(super) fn published_semantic_candidate_score_domain()
-> Result<ScoreDomainId, RetrievalPortError> {
    ScoreDomainId::new(tracedecay_query::retrieval::QUERY_SEMANTIC_SCORE_DOMAIN_V1)
        .map_err(|error| RetrievalPortError::Contract(error.to_string()))
}

impl PublishedSemanticVectorReadPortV1 {
    pub(super) fn from_prepared(
        prepared: &PreparedVectorGenerationV1,
        generation: VectorGenerationIdV1,
        search_index_key: SemanticSearchIndexKeyV1,
        code: &CodeIndexPublishedGenerationV1,
    ) -> Result<Self, RetrievalPortError> {
        if prepared.request.changes.to_generation != code.manifest().generation_id {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        let freshness = production_code_index_freshness(
            code.manifest().seal.sealed_at,
            ComponentRevision::new("policy.semantic.evaluation.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )?;
        let chunks = code
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| (&chunk.id, chunk))
            .collect::<BTreeMap<_, _>>();
        let mut rows = Vec::with_capacity(prepared.vectors.len());
        for (ordinal, vector) in prepared.vectors.iter().enumerate() {
            let chunk = chunks
                .get(&vector.chunk_id)
                .ok_or(RetrievalPortError::GenerationMismatch)?;
            let chunk_id = &vector.chunk_id;
            let (anchor_id, logical_evidence_id, source_occurrence) =
                semantic_candidate_identity(chunk)?;
            let candidate = CompactCandidate {
                anchor_id: anchor_id.clone(),
                logical_evidence_id,
                source_occurrence_id: source_occurrence.clone(),
                file_occurrence_id: Some(chunk.anchor.file_occurrence_id.clone()),
                source_namespace: freshness.source_namespace.clone(),
                repository_id: Some(code.snapshot().repository.clone()),
                session_or_thread_id: None,
                logical_copy_cluster_id: None,
                logical_copy_evidence_anchor: None,
                evidence_role: EvidenceRole::Primary,
                retriever: RetrieverKind::Semantic,
                retriever_revision: ComponentRevision::new("retriever.semantic-flat.evaluation.v1")
                    .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                score_domain: published_semantic_candidate_score_domain()?,
                raw_score: FixedPointScore::ZERO,
                ordinal_rank: ordinal as u32,
                exact_admission_proof: None,
                retriever_evidence_anchor: RetrievalAnchorId::new(format!(
                    "code-semantic:{}",
                    chunk_id.as_str()
                ))
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                freshness: freshness.clone(),
            };
            rows.push(SemanticVectorRecordV1 {
                vector_generation: generation.clone(),
                projection_key: prepared.request.target_projection_key.clone(),
                source_generation: prepared.request.changes.to_generation.clone(),
                chunk_id: chunk_id.clone(),
                candidate,
                binding: CodeCandidateBindingV1 {
                    candidate_anchor: anchor_id,
                    occurrence: CodeOccurrenceRefV1 {
                        generation: chunk.anchor.generation_id.clone(),
                        file: chunk.anchor.file_occurrence_id.clone(),
                        symbol: chunk.anchor.symbol_occurrence_id.clone(),
                        chunk: Some(chunk_id.clone()),
                    },
                    language_descriptor_revision: chunk.language_descriptor_revision.clone(),
                    matched_term_kinds: Vec::new(),
                    source_occurrence,
                },
                values: vector.values.clone(),
            });
        }
        Ok(Self {
            generation,
            projection_key: prepared.request.target_projection_key.clone(),
            search_index_key,
            source_generation: prepared.request.changes.to_generation.clone(),
            capability_manifest_digest: code.capability().manifest_digest.clone(),
            source_coherence: SemanticSourceCoherenceV1::ExactGeneration,
            rows,
            // Evaluation ports read a prepared in-memory projection that was
            // never staged into the graph store, so no persisted index can
            // exist for it.
            ann: PublishedSemanticAnnBindingV1::Unavailable(SemanticAnnIndexStateV1::Unsupported),
        })
    }

    /// Serve `vectors` for the exact code generation they were projected from.
    ///
    /// Naming that generation is not proof of it, so admission still runs the
    /// one coherence verdict and refuses any other arm.
    pub(super) fn new(
        vectors: PublishedVectorGenerationV1,
        search_index_key: SemanticSearchIndexKeyV1,
        code: &CodeIndexPublishedGenerationV1,
        ann: Option<SemanticAnnServingIndexV1>,
    ) -> Result<Self, RetrievalPortError> {
        let SemanticSourceCoherenceOutcomeV1::Coherent(
            coherence @ SemanticSourceCoherenceV1::ExactGeneration,
        ) = semantic_source_coherence(&vectors, code.manifest())
        else {
            return Err(RetrievalPortError::GenerationMismatch);
        };
        Self::bind(vectors, search_index_key, code, ann, coherence)
    }

    /// Serve `vectors` for `code` when it is either their exact source
    /// generation or a publication whose sealed chunk corpus is proven
    /// byte-identical to the one the vectors were projected from
    /// ([`semantic_source_content_coherent`]). The port and its rows then bind
    /// the served (current) generation identity, so every downstream
    /// exact-generation check compares against the publication queries
    /// actually pin.
    pub(super) fn new_source_coherent(
        vectors: PublishedVectorGenerationV1,
        search_index_key: SemanticSearchIndexKeyV1,
        code: &CodeIndexPublishedGenerationV1,
        ann: Option<SemanticAnnServingIndexV1>,
    ) -> Result<Self, RetrievalPortError> {
        let SemanticSourceCoherenceOutcomeV1::Coherent(coherence) =
            semantic_source_coherence(&vectors, code.manifest())
        else {
            return Err(RetrievalPortError::GenerationMismatch);
        };
        Self::bind(vectors, search_index_key, code, ann, coherence)
    }

    pub(super) fn bind(
        vectors: PublishedVectorGenerationV1,
        search_index_key: SemanticSearchIndexKeyV1,
        code: &CodeIndexPublishedGenerationV1,
        ann: Option<SemanticAnnServingIndexV1>,
        source_coherence: SemanticSourceCoherenceV1,
    ) -> Result<Self, RetrievalPortError> {
        let source_generation = code.manifest().generation_id.clone();
        let freshness = production_code_index_freshness(
            code.manifest().seal.sealed_at,
            ComponentRevision::new("policy.semantic.daemon.v1")
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
        )?;
        let chunks = code
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| (&chunk.id, chunk))
            .collect::<BTreeMap<_, _>>();
        let mut rows = Vec::with_capacity(vectors.vectors().len());
        for (ordinal, (chunk_id, vector)) in vectors.vectors().iter().enumerate() {
            let chunk = chunks
                .get(chunk_id)
                .ok_or(RetrievalPortError::GenerationMismatch)?;
            let (anchor_id, logical_evidence_id, source_occurrence) =
                semantic_candidate_identity(chunk)?;
            let candidate = CompactCandidate {
                anchor_id: anchor_id.clone(),
                logical_evidence_id,
                source_occurrence_id: source_occurrence.clone(),
                file_occurrence_id: Some(chunk.anchor.file_occurrence_id.clone()),
                source_namespace: freshness.source_namespace.clone(),
                repository_id: Some(code.snapshot().repository.clone()),
                session_or_thread_id: None,
                logical_copy_cluster_id: None,
                logical_copy_evidence_anchor: None,
                evidence_role: EvidenceRole::Primary,
                retriever: RetrieverKind::Semantic,
                retriever_revision: ComponentRevision::new("retriever.semantic-flat.daemon.v1")
                    .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                score_domain: published_semantic_candidate_score_domain()?,
                raw_score: FixedPointScore::ZERO,
                ordinal_rank: ordinal as u32,
                exact_admission_proof: None,
                retriever_evidence_anchor: RetrievalAnchorId::new(format!(
                    "code-semantic:{}",
                    chunk_id.as_str()
                ))
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?,
                freshness: freshness.clone(),
            };
            rows.push(SemanticVectorRecordV1 {
                vector_generation: vectors.generation_id().clone(),
                projection_key: vectors.projection_key().clone(),
                source_generation: source_generation.clone(),
                chunk_id: chunk_id.clone(),
                candidate,
                binding: CodeCandidateBindingV1 {
                    candidate_anchor: anchor_id,
                    occurrence: CodeOccurrenceRefV1 {
                        generation: chunk.anchor.generation_id.clone(),
                        file: chunk.anchor.file_occurrence_id.clone(),
                        symbol: chunk.anchor.symbol_occurrence_id.clone(),
                        chunk: Some(chunk_id.clone()),
                    },
                    language_descriptor_revision: chunk.language_descriptor_revision.clone(),
                    matched_term_kinds: Vec::new(),
                    source_occurrence,
                },
                values: vector.values.clone(),
            });
        }
        let ann = PublishedSemanticAnnBindingV1::bind(&search_index_key, ann, &rows)?;
        Ok(Self {
            generation: vectors.generation_id().clone(),
            projection_key: vectors.projection_key().clone(),
            search_index_key,
            source_generation,
            capability_manifest_digest: code.capability().manifest_digest.clone(),
            source_coherence,
            rows,
            ann,
        })
    }
}

impl SemanticVectorReadPort for PublishedSemanticVectorReadPortV1 {
    fn scan_exact_flat(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        examine: &mut dyn FnMut() -> Result<(), RetrievalPortError>,
        visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
    ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError> {
        if request.search_kind != SemanticSearchKindV1::ExactFlat
            || request.vector_generation != &self.generation
            || request.projection_key != &self.projection_key
            || request.search_index_key != &self.search_index_key
            || request.source_generation != &self.source_generation
            || request.capability_manifest_digest != &self.capability_manifest_digest
        {
            return Err(RetrievalPortError::IncompatibleProjection);
        }
        hotpath::gauge!("semantic_exact_flat_scan_rows").set(self.rows.len());
        hotpath::gauge!("semantic_exact_flat_scan_dimensions")
            .set(self.rows.first().map_or(0, |row| row.values.len()));
        hotpath::measure_block!("semantic.vector.scan_exact_flat", {
            for row in &self.rows {
                examine()?;
                visit(row)?;
            }
            Ok::<(), RetrievalPortError>(())
        })?;
        Ok(SemanticVectorScanSummaryV1 {
            examined: self.rows.len() as u64,
            eligible: self.rows.len() as u64,
            excluded: 0,
            unknown: 0,
        })
    }

    /// Serves `window` as the ranks past `window.skip` of one index search
    /// to `window.depth`. HNSW has no rank offset: the deeper search is the
    /// only way to reach those ranks, and its prefix may reorder relative to
    /// the shallower pass, which is why the lane tolerates re-served rows.
    fn ann_candidates(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        query: &[f32],
        window: SemanticAnnCandidateWindowV1,
    ) -> Result<SemanticAnnCandidatesV1<'_>, RetrievalPortError> {
        if request.search_kind != SemanticSearchKindV1::AnnHnswExactRescore
            || request.vector_generation != &self.generation
            || request.projection_key != &self.projection_key
            || request.search_index_key != &self.search_index_key
            || request.source_generation != &self.source_generation
            || request.capability_manifest_digest != &self.capability_manifest_digest
        {
            return Err(RetrievalPortError::IncompatibleProjection);
        }
        let (index, rows_by_chunk) = match &self.ann {
            PublishedSemanticAnnBindingV1::Unavailable(state) => {
                return Ok(SemanticAnnCandidatesV1::Unavailable(*state));
            }
            PublishedSemanticAnnBindingV1::Serving {
                index,
                rows_by_chunk,
            } => (index, rows_by_chunk),
        };
        let chunks = hotpath::measure_block!(
            "semantic.vector.ann_candidates",
            index
                .search(query, window.depth)
                .map_err(ann_search_port_error)
        )?;
        hotpath::gauge!("semantic_ann_candidates").set(chunks.len());
        chunks
            .into_iter()
            .skip(window.skip)
            .map(|chunk_id| {
                rows_by_chunk
                    .get(&chunk_id)
                    .map(|ordinal| &self.rows[*ordinal])
                    .ok_or_else(|| {
                        RetrievalPortError::Contract(
                            "semantic ANN index answered with a non-resident row".to_owned(),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(SemanticAnnCandidatesV1::Candidates)
    }
}

/// Consults the store for the generation's persisted ANN index exactly when
/// the pinned search profile is ANN-kinded. Exact-flat profiles never bind
/// one, and `Ok(None)` under an ANN profile is the typed "no populated
/// index" state the port reports as `Missing`.
pub(super) async fn semantic_ann_serving_index(
    store: &GraphVectorGenerationStoreV1,
    active: &PublishedVectorGenerationV1,
    search_index_key: &SemanticSearchIndexKeyV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Option<SemanticAnnServingIndexV1>, VectorGenerationStoreErrorV1> {
    match search_index_key.kind {
        SemanticSearchIndexKindV1::ExactFlat => Ok(None),
        SemanticSearchIndexKindV1::AnnHnswExactRescore => {
            store
                .ann_serving_index(
                    active.generation_id(),
                    active.embedding_key(),
                    active.vectors().keys(),
                    cancellation,
                )
                .await
        }
    }
}

/// The ANN index search runs under the same request control as the scan, so
/// its store failures map onto the lane's typed port errors.
pub(super) fn ann_search_port_error(error: VectorGenerationStoreErrorV1) -> RetrievalPortError {
    match error {
        VectorGenerationStoreErrorV1::Cancelled => RetrievalPortError::Cancelled,
        VectorGenerationStoreErrorV1::DeadlineExceeded => RetrievalPortError::BudgetExceeded,
        other => RetrievalPortError::AuthorityUnavailable(other.to_string()),
    }
}

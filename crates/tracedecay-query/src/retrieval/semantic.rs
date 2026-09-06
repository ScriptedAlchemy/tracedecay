//! Semantic retrieval lane: exact-flat scan and ANN candidates with exact
//! rescoring.
//!
//! The lane consumes only an admitted embedding projection, a request-local
//! query-embedding port, and an immutable vector-generation read port. The
//! request's search-index key selects the path: `exact_flat` scans and
//! exactly scores every published row, while `ann_hnsw_exact_rescore` gathers
//! index-bounded candidates under the adaptive recall policy committed in the
//! profile digest and exactly rescores them with the same canonical distance,
//! falling back to the exact-flat scan (typed, never silent) when the port
//! reports its index missing or incomplete.
//! The lane performs no artifact admission, vector mutation, fusion,
//! reranking, hydration, activation, or calls into another retrieval lane.

#[cfg(test)]
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    AdaptiveRecallDepthPolicyV1, AdaptiveRecallStepV1, AdaptiveRecallStopV1,
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, CompactCandidate,
    CursorPayloadDigest, EmbeddingMetricV1, EmbeddingProjectionKeyV1,
    EphemeralSanitizedQueryViewV1, FixedPointScore, FreshnessCompatibilityV1, ManifestDigest,
    ProjectionKeyV1, QueryDigest, RetrievalBudget, RetrievalBudgetUsage, RetrievalFailure,
    RetrievalRequest, RetrieverBatch, RetrieverContinuation, RetrieverCoverage, RetrieverKind,
    RetrieverOutcome, SEMANTIC_ANN_RECALL_POLICY_V1, SemanticSearchIndexKeyV1,
    SemanticSearchIndexKindV1, SemanticSearchIndexProfileV1, SourceOccurrenceId,
    VectorGenerationIdV1,
};
use tracedecay_graph_db::MAX_VECTOR_SEARCH_LIMIT;

use super::ports::{
    CodeCandidateBindingV1, RetrievalPortError, candidate_checkpoint_prefix, checkpoint_digest,
    contract_error, lane_candidate_cap,
};

/// Fallback exact-flat scan deadline when both request budgets omit
/// `deadline_micros`. Retention is heap-capped; the visit is still a full
/// generation scan, so a missing request deadline must not run unbounded.
pub const SEMANTIC_EXACT_FLAT_DEFAULT_DEADLINE_MICROS_V1: u64 = 5_000_000;

mod execution_authority;
mod service;
pub use execution_authority::{
    ExecutedSemanticCompositionV1, SemanticCompositionAuthorityErrorV1,
    SemanticCompositionExecutionAuthorityV1, SemanticCompositionExecutionOutcomeV1,
    SemanticRerankExecutionPortV1, SemanticRerankReadinessV1, restore_frozen_semantic_order,
};
pub use service::{
    CalibratedSemanticQueryService, CompleteSemanticGenerationV1, SemanticAbstentionDispositionV1,
    SemanticAbstentionV1, SemanticCalibrationEvidenceV1, SemanticCalibrationProfileV1,
    SemanticIndexStateV1, SemanticLaneReadinessV1, SemanticQueryDecisionV1, SemanticQueryModeV1,
    SemanticQueryServiceError, SemanticQueryServiceOutcomeV1,
};

const SEMANTIC_DISTANCE_SCALE: f64 = 1_000_000_000.0;
const SEMANTIC_CHECKPOINT_DOMAIN: &str = "tracedecay.semantic-flat-checkpoint.v1";

/// The search implementation a read request asks the port to serve. It is
/// distinct from the request's search-index kind because an ANN-profiled
/// request that fell back reads the port as `ExactFlat`; the executed path
/// and its facts are published through `SemanticSearchExecutionV1`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SemanticSearchKindV1 {
    #[serde(rename = "exact_flat")]
    ExactFlat,
    #[serde(rename = "ann_hnsw_exact_rescore")]
    AnnHnswExactRescore,
}

// The lane executes `SEMANTIC_ANN_RECALL_POLICY_V1`, the value the canonical
// ANN profile commits into its parameters digest, and refuses an ANN key
// minted from any other policy, so the depth sequence a request observes is
// always the one its index identity promised. The ceiling must stay within
// the vector store's hard search bound; otherwise the port would clamp a pass
// and the loop would misread the clamp as an unsaturated index.
const _: () = assert!(SEMANTIC_ANN_RECALL_POLICY_V1.max_depth as usize <= MAX_VECTOR_SEARCH_LIMIT);

/// One executed adaptive recall loop: how deep the ANN path actually searched
/// before rescoring, and why it stopped there.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticAdaptiveRecallExecutionV1 {
    /// Passes issued to the index, each for the ranks past the previous depth.
    pub passes: u32,
    /// Deepest rank the last pass searched.
    pub final_depth: u32,
    /// Pool size the policy aimed for under this request's retained cap.
    pub target: u32,
    pub stop: AdaptiveRecallStopV1,
}

/// The search path that actually executed for one batch, with the execution
/// facts that path produces. An ANN-profiled request that fell back to the
/// exact-flat scan is visibly `ExactFlat` here.
///
/// The tag is spelled `search_kind` and the enum is flattened into
/// `CodeSemanticEvidenceV1`, so exact-flat evidence keeps the byte-exact wire
/// shape of the SHA-pinned packaged native qualification (`search_kind` as
/// its last key, no recall facts) while ANN evidence carries `recall` beside
/// the tag.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "search_kind", rename_all = "snake_case")]
pub enum SemanticSearchExecutionV1 {
    ExactFlat,
    AnnHnswExactRescore {
        recall: SemanticAdaptiveRecallExecutionV1,
    },
}

impl SemanticSearchExecutionV1 {
    #[hotpath::skip]
    pub const fn kind(self) -> SemanticSearchKindV1 {
        match self {
            Self::ExactFlat => SemanticSearchKindV1::ExactFlat,
            Self::AnnHnswExactRescore { .. } => SemanticSearchKindV1::AnnHnswExactRescore,
        }
    }
}

/// Canonical fixed-point semantic distance. Smaller values rank first.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalSemanticDistanceV1(i64);

impl CanonicalSemanticDistanceV1 {
    #[hotpath::skip]
    pub const fn micros(self) -> i64 {
        self.0
    }

    fn as_descending_score(self) -> FixedPointScore {
        let order_preserving = (self.0 as u64) ^ (1_u64 << 63);
        FixedPointScore(u64::MAX - order_preserving)
    }
}

/// Per-candidate semantic evidence. Unknown fields cannot be denied here
/// because `search` is flattened (serde offers no `deny_unknown_fields` with
/// `flatten`); the packaged qualification loader still rejects any byte drift
/// through its SHA pin and canonical re-serialization check.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeSemanticEvidenceV1 {
    pub projection_key: EmbeddingProjectionKeyV1,
    pub search_index_key: SemanticSearchIndexKeyV1,
    pub vector_generation: VectorGenerationIdV1,
    pub chunk_id: CodeSearchChunkId,
    pub distance: CanonicalSemanticDistanceV1,
    #[serde(flatten)]
    pub search: SemanticSearchExecutionV1,
}

/// Frozen semantic-lane request. The query view remains borrowed and
/// non-serializable; only its privacy-bound digest may outlive the call.
#[derive(Debug)]
pub struct SemanticRetrievalRequestV1<'a> {
    pub base: RetrievalRequest,
    pub query_digest: QueryDigest,
    pub query_view: &'a EphemeralSanitizedQueryViewV1,
    pub projection: &'a AdmittedEmbeddingProjectionKeyV1,
    pub search_index_key: &'a SemanticSearchIndexKeyV1,
    pub capability_manifest_digest: ManifestDigest,
    pub vector_generation: VectorGenerationIdV1,
    pub code_generation: CodeGenerationId,
    pub budget: RetrievalBudget,
}

impl SemanticRetrievalRequestV1<'_> {
    pub fn validate(&self) -> Result<(), RetrievalPortError> {
        self.base.budget.validate().map_err(contract_error)?;
        self.budget.validate().map_err(contract_error)?;
        self.query_digest.validate().map_err(contract_error)?;
        self.code_generation.validate().map_err(contract_error)?;
        self.capability_manifest_digest
            .validate()
            .map_err(contract_error)?;
        self.vector_generation
            .as_digest()
            .validate()
            .map_err(contract_error)?;
        self.projection
            .embedding_key()
            .validate()
            .map_err(contract_error)?;
        self.search_index_key.validate().map_err(contract_error)?;

        if self.base.scope.privacy_domain != *self.projection.privacy_domain()
            || self.query_digest.privacy_domain != *self.projection.privacy_domain()
            || self.query_digest.key_epoch != self.projection.privacy_key_epoch()
        {
            return Err(RetrievalPortError::Contract(
                "semantic request scope, query digest, and admitted projection must share one privacy domain and key epoch"
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

/// Request presented to the admitted projection/artifact query runtime.
#[derive(Clone, Copy, Debug)]
pub struct SemanticQueryEmbeddingRequestV1<'a> {
    pub query_digest: &'a QueryDigest,
    pub query_view: &'a EphemeralSanitizedQueryViewV1,
    pub projection: &'a AdmittedEmbeddingProjectionKeyV1,
}

/// Request-local query vector. It deliberately implements neither
/// serialization nor cloning and is dropped after the exact-flat scan.
pub struct EphemeralQueryEmbeddingV1 {
    query_digest: QueryDigest,
    projection: AdmittedEmbeddingProjectionKeyV1,
    values: Vec<f32>,
}

impl EphemeralQueryEmbeddingV1 {
    pub fn new(
        query_digest: QueryDigest,
        projection: AdmittedEmbeddingProjectionKeyV1,
        values: Vec<f32>,
    ) -> Result<Self, RetrievalPortError> {
        query_digest.validate().map_err(contract_error)?;
        if query_digest.privacy_domain != *projection.privacy_domain()
            || query_digest.key_epoch != projection.privacy_key_epoch()
        {
            return Err(RetrievalPortError::Contract(
                "query embedding digest does not match the admitted projection privacy identity"
                    .to_owned(),
            ));
        }
        validate_vector(
            &values,
            projection.embedding_key().dimensions,
            "query embedding",
        )?;
        Ok(Self {
            query_digest,
            projection,
            values,
        })
    }
}

/// Root-private adapter over the already-admitted projection/artifact
/// authority. Implementations may infer one query vector and nothing else.
pub trait SemanticQueryEmbeddingPort {
    fn embed_query(
        &self,
        request: SemanticQueryEmbeddingRequestV1<'_>,
    ) -> Result<EphemeralQueryEmbeddingV1, RetrievalPortError>;
}

/// Frozen identity passed to the immutable vector read port.
#[derive(Clone, Copy, Debug)]
pub struct SemanticVectorReadRequestV1<'a> {
    pub vector_generation: &'a VectorGenerationIdV1,
    pub projection_key: &'a ProjectionKeyV1,
    pub search_index_key: &'a SemanticSearchIndexKeyV1,
    pub source_generation: &'a CodeGenerationId,
    pub capability_manifest_digest: &'a ManifestDigest,
    pub search_kind: SemanticSearchKindV1,
}

/// One immutable vector row and its generic candidate binding.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticVectorRecordV1 {
    pub vector_generation: VectorGenerationIdV1,
    pub projection_key: ProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub chunk_id: CodeSearchChunkId,
    pub candidate: CompactCandidate,
    pub binding: CodeCandidateBindingV1,
    pub values: Vec<f32>,
}

/// Store-owned coverage for one complete exact-flat generation scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SemanticVectorScanSummaryV1 {
    pub examined: u64,
    pub eligible: u64,
    pub excluded: u64,
    pub unknown: u64,
}

/// Typed reason one generation-bound ANN candidate index cannot serve.
/// A servable index is expressed by returning candidates, so no
/// contradictory "unavailable but ready" state is representable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticAnnIndexStateV1 {
    /// No persisted index exists for the serving generation.
    Missing,
    /// The index exists but does not cover the complete serving row set —
    /// for example rows reused from base-generation lineage are hydrated in
    /// memory but were never native entities of this generation's namespace.
    IncompleteCoverage { indexed: u64, resident: u64 },
    /// The port does not implement ANN candidate generation.
    Unsupported,
}

/// One slice of the index's nearest-neighbour ranking: the ranks in
/// `skip..depth`. The first pass of the adaptive recall loop starts at rank
/// zero; every later pass asks only for the ranks past what earlier passes
/// already served, so no row crosses the port twice under an exact ranking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticAnnCandidateWindowV1 {
    /// Ranks earlier passes already served; the port skips this many.
    pub skip: usize,
    /// Total ranking depth this pass searches.
    pub depth: usize,
}

impl SemanticAnnCandidateWindowV1 {
    /// Most rows the port may answer with for this window.
    #[hotpath::skip]
    pub const fn requested(self) -> usize {
        self.depth.saturating_sub(self.skip)
    }
}

/// Outcome of one bounded ANN candidate request.
pub enum SemanticAnnCandidatesV1<'a> {
    /// Index-nearest rows for the requested window in ascending
    /// index-distance order, at most `window.requested()` of them, each a
    /// serving row of the requested generation. An approximate index may
    /// rank a row differently at a deeper search, so a later window can
    /// re-serve a row an earlier one already answered; the lane never rescores
    /// or republishes such a row and accounts for it as excluded coverage.
    Candidates(Vec<&'a SemanticVectorRecordV1>),
    /// The typed reason the index cannot serve this request; the lane falls
    /// back to the exact-flat scan and records the fallback.
    Unavailable(SemanticAnnIndexStateV1),
}

/// Read-only port over one immutable, fully published vector generation.
/// The callback shape lets the lane scan without retaining or copying the
/// complete vector set. Implementations must invoke `examine` before every
/// row they inspect, including rows they exclude before invoking `visit`.
pub trait SemanticVectorReadPort {
    fn scan_exact_flat(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        examine: &mut dyn FnMut() -> Result<(), RetrievalPortError>,
        visit: &mut dyn FnMut(&SemanticVectorRecordV1) -> Result<(), RetrievalPortError>,
    ) -> Result<SemanticVectorScanSummaryV1, RetrievalPortError>;

    /// Index-bounded nearest candidates for one request-local query vector,
    /// restricted to the ranks in `window`.
    ///
    /// `query` is the ephemeral query embedding: implementations may use it
    /// for the one transient index search and must not retain, copy beyond
    /// the search, or serialize it. Ports without a generation-bound ANN
    /// index report `Unavailable` with a typed state; they must never
    /// approximate this surface with a partial scan. Availability is a
    /// property of the immutable generation, so it must not change between
    /// windows of one request.
    fn ann_candidates(
        &self,
        request: SemanticVectorReadRequestV1<'_>,
        query: &[f32],
        window: SemanticAnnCandidateWindowV1,
    ) -> Result<SemanticAnnCandidatesV1<'_>, RetrievalPortError> {
        let _ = (request, query, window);
        Ok(SemanticAnnCandidatesV1::Unavailable(
            SemanticAnnIndexStateV1::Unsupported,
        ))
    }
}

/// Request-scoped cancellation and monotonic deadline authority.
pub trait SemanticExecutionControl {
    fn is_cancelled(&self) -> bool;
    /// Monotonic elapsed time in the same request-relative domain as
    /// `RetrievalBudget::deadline_micros`.
    fn elapsed_micros(&self) -> u64;
}

/// Independently inspectable semantic-lane port.
pub trait SemanticLaneRetriever {
    fn retrieve_semantic(
        &self,
        request: &SemanticRetrievalRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError>;
}

/// Exact-flat semantic retriever. Its only dependencies are the admitted
/// query embedder, immutable vectors, and request execution control.
pub struct SemanticCodeRetriever<'a, E, V, C> {
    embedder: &'a E,
    vectors: &'a V,
    control: &'a C,
}

impl<'a, E, V, C> SemanticCodeRetriever<'a, E, V, C> {
    #[hotpath::skip]
    pub const fn new(embedder: &'a E, vectors: &'a V, control: &'a C) -> Self {
        Self {
            embedder,
            vectors,
            control,
        }
    }
}

impl<E, V, C> SemanticCodeRetriever<'_, E, V, C>
where
    E: SemanticQueryEmbeddingPort,
    V: SemanticVectorReadPort,
    C: SemanticExecutionControl,
{
    fn score_record(
        request: &SemanticRetrievalRequestV1<'_>,
        record: &SemanticVectorRecordV1,
        query: &EphemeralQueryEmbeddingV1,
    ) -> Result<CanonicalSemanticDistanceV1, RetrievalPortError> {
        #[cfg(test)]
        SEMANTIC_SCORED_ROWS.with(|count| count.set(count.get() + 1));
        crate::hotpath_metrics::measure_frequent("query.lane.semantic.score_row", || {
            Self::score_record_inner(request, record, query)
        })
    }

    fn score_record_inner(
        request: &SemanticRetrievalRequestV1<'_>,
        record: &SemanticVectorRecordV1,
        query: &EphemeralQueryEmbeddingV1,
    ) -> Result<CanonicalSemanticDistanceV1, RetrievalPortError> {
        if record.vector_generation != request.vector_generation
            || record.projection_key != *request.projection.projection_key()
        {
            observe_semantic_lane_failure("score_generation_projection", "incompatible_projection");
            return Err(RetrievalPortError::IncompatibleProjection);
        }
        if record.source_generation != request.code_generation
            || record.binding.occurrence.generation != request.code_generation
        {
            observe_semantic_lane_failure("score_source_generation", "generation_mismatch");
            return Err(RetrievalPortError::GenerationMismatch);
        }
        if record.binding.occurrence.chunk.as_ref() != Some(&record.chunk_id)
            || record.binding.candidate_anchor != record.candidate.anchor_id
            || record.binding.source_occurrence != record.candidate.source_occurrence_id
        {
            observe_semantic_lane_failure("score_candidate_binding", "contract");
            return Err(RetrievalPortError::Contract(
                "semantic vector row does not bind its chunk and candidate occurrence".to_owned(),
            ));
        }
        if record.candidate.retriever != RetrieverKind::Semantic
            || record.candidate.exact_admission_proof.is_some()
        {
            observe_semantic_lane_failure("score_candidate_kind", "contract");
            return Err(RetrievalPortError::Contract(
                "semantic vectors may emit only non-exact semantic candidates".to_owned(),
            ));
        }
        if record.candidate.repository_id.as_ref() != Some(&request.base.scope.root.repository)
            || record.candidate.freshness.compatibility != FreshnessCompatibilityV1::Current
        {
            observe_semantic_lane_failure("score_scope_freshness", "contract");
            return Err(RetrievalPortError::Contract(
                "semantic candidate is outside the frozen repository or freshness scope".to_owned(),
            ));
        }
        validate_vector(
            &record.values,
            request.projection.embedding_key().dimensions,
            "stored semantic vector",
        )
        .inspect_err(|error| {
            observe_semantic_lane_failure("score_vector_validation", port_error_class(error));
        })?;
        canonical_distance(
            request.projection.embedding_key().metric,
            &query.values,
            &record.values,
        )
        .inspect_err(|error| {
            observe_semantic_lane_failure("score_distance", port_error_class(error));
        })
    }

    fn materialize_record(
        record: &SemanticVectorRecordV1,
        distance: CanonicalSemanticDistanceV1,
    ) -> SemanticRankedEntryV1 {
        #[cfg(test)]
        SEMANTIC_RETAINED_MATERIALIZATIONS.with(|count| count.set(count.get() + 1));
        let mut candidate = record.candidate.clone();
        candidate.raw_score = distance.as_descending_score();
        SemanticRankedEntryV1 {
            candidate,
            chunk_id: record.chunk_id.clone(),
            distance,
        }
    }

    fn retain_scored_record(
        record: &SemanticVectorRecordV1,
        distance: CanonicalSemanticDistanceV1,
        cap: usize,
        ranked: &mut BinaryHeap<SemanticRankedEntryV1>,
    ) {
        if cap == 0 {
            return;
        }
        // Compare against the heap worst using the same key as SemanticRankedEntryV1
        // without cloning a losing candidate.
        let retain = if ranked.len() < cap {
            true
        } else if let Some(worst) = ranked.peek() {
            rank_key(
                distance,
                &record.candidate.source_occurrence_id,
                &record.candidate.retriever_evidence_anchor,
                &record.chunk_id,
            )
            .cmp(&rank_key(
                worst.distance,
                &worst.candidate.source_occurrence_id,
                &worst.candidate.retriever_evidence_anchor,
                &worst.chunk_id,
            )) == Ordering::Less
        } else {
            false
        };
        if !retain {
            return;
        }
        let entry = Self::materialize_record(record, distance);
        if ranked.len() == cap {
            ranked.pop();
        }
        ranked.push(entry);
    }

    fn retrieve_complete(
        &self,
        request: &SemanticRetrievalRequestV1<'_>,
        query: &EphemeralQueryEmbeddingV1,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        if query.projection != *request.projection || query.query_digest != request.query_digest {
            observe_semantic_lane_failure("query_embedding_identity", "incompatible_projection");
            return Err(RetrievalPortError::IncompatibleProjection);
        }
        match request.search_index_key.kind {
            SemanticSearchIndexKindV1::ExactFlat => self.retrieve_exact_flat(request, query),
            SemanticSearchIndexKindV1::AnnHnswExactRescore => {
                self.retrieve_ann_exact_rescore(request, query)
            }
        }
    }

    /// ANN candidate generation with exact rescoring. Candidates come from
    /// the port's generation-bound index, gathered by the adaptive recall
    /// loop `SEMANTIC_ANN_RECALL_POLICY_V1` defines: each pass asks for the
    /// ranks past the previous depth, and the depth grows only while the pass
    /// was saturated and the pool is under target. Each candidate is rescored
    /// once with the same canonical distance the exact-flat scan uses, so
    /// published distances are bit-identical to a flat scan's for every
    /// returned row. Coverage is candidate-bounded and the continuation never
    /// claims exhaustion: an index-bounded candidate set cannot prove no
    /// better row exists.
    ///
    /// The loop is a pure function of the policy, the retained cap, and the
    /// row counts the port answers; cancellation and the deadline are observed
    /// before every pass and every row, and budget usage counts every pass.
    fn retrieve_ann_exact_rescore(
        &self,
        request: &SemanticRetrievalRequestV1<'_>,
        query: &EphemeralQueryEmbeddingV1,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        let policy = SEMANTIC_ANN_RECALL_POLICY_V1;
        verify_ann_search_index_key(request.search_index_key, &policy)?;
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let retained_cap = u32::try_from(cap).map_err(|_| {
            observe_semantic_lane_failure("ann_retained_cap", "contract");
            RetrievalPortError::Contract(
                "semantic lane candidate cap exceeds the recall policy domain".to_owned(),
            )
        })?;
        let read_request = SemanticVectorReadRequestV1 {
            vector_generation: &request.vector_generation,
            projection_key: request.projection.projection_key(),
            search_index_key: request.search_index_key,
            source_generation: &request.code_generation,
            capability_manifest_digest: &request.capability_manifest_digest,
            search_kind: SemanticSearchKindV1::AnnHnswExactRescore,
        };

        let mut ranked: BinaryHeap<SemanticRankedEntryV1> = BinaryHeap::new();
        // Distinct rows rescored so far; `reserved_count` is rows an
        // approximate index re-served in a later window (never rescored).
        let mut eligible_count: usize = 0;
        let mut reserved_count: usize = 0;
        // Occurrence -> the pass that scored it, so a repeat inside one pass
        // (a corrupt answer) is told apart from a repeat across passes.
        let mut scored_passes: BTreeMap<SourceOccurrenceId, u32> = BTreeMap::new();
        let mut passes: u32 = 0;
        let mut searched_depth: u32 = 0;
        let mut next_depth = policy.first_depth();
        let stop = loop {
            if self.control.is_cancelled() {
                hotpath::gauge!("query.cancel.count").inc(1u32);
                return Ok(RetrieverOutcome::Cancelled);
            }
            if deadline_exhausted(request, self.control) {
                return Ok(RetrieverOutcome::BudgetExceeded(budget_usage(
                    request,
                    (eligible_count + reserved_count) as u64,
                    0,
                    self.control,
                )));
            }
            let window = SemanticAnnCandidateWindowV1 {
                skip: searched_depth as usize,
                depth: next_depth as usize,
            };
            let candidates = match self
                .vectors
                .ann_candidates(read_request, &query.values, window)
            {
                Ok(candidates) => candidates,
                Err(error) => {
                    observe_semantic_lane_failure("ann_candidates", port_error_class(&error));
                    return port_error_outcome(
                        error,
                        budget_usage(
                            request,
                            (eligible_count + reserved_count) as u64,
                            0,
                            self.control,
                        ),
                    );
                }
            };
            let records = match candidates {
                SemanticAnnCandidatesV1::Candidates(records) => records,
                SemanticAnnCandidatesV1::Unavailable(state) if passes == 0 => {
                    observe_semantic_ann_fallback(state);
                    return self.retrieve_exact_flat(request, query);
                }
                SemanticAnnCandidatesV1::Unavailable(_) => {
                    // The port reads one immutable generation: an index that
                    // served the first window cannot truthfully vanish before
                    // the next, and falling back now would discard scored
                    // rows behind a flat-scan label.
                    observe_semantic_lane_failure("ann_unavailable_between_passes", "contract");
                    return Err(RetrievalPortError::Contract(
                        "semantic ANN index became unavailable between recall passes".to_owned(),
                    ));
                }
            };
            if records.len() > window.requested() {
                observe_semantic_lane_failure("ann_candidate_limit", "contract");
                return Err(RetrievalPortError::Contract(
                    "semantic ANN port returned more candidates than requested".to_owned(),
                ));
            }
            passes += 1;
            let returned = records.len();
            for record in records {
                if self.control.is_cancelled() {
                    hotpath::gauge!("query.cancel.count").inc(1u32);
                    return Ok(RetrieverOutcome::Cancelled);
                }
                if deadline_exhausted(request, self.control) {
                    return Ok(RetrieverOutcome::BudgetExceeded(budget_usage(
                        request,
                        (eligible_count + reserved_count) as u64,
                        0,
                        self.control,
                    )));
                }
                match scored_passes.get(&record.candidate.source_occurrence_id) {
                    Some(scored_in) if *scored_in == passes => {
                        observe_semantic_lane_failure("ann_duplicate_occurrence", "contract");
                        return Err(RetrievalPortError::Contract(
                            "semantic ANN candidates contain duplicate source occurrences"
                                .to_owned(),
                        ));
                    }
                    Some(_) => {
                        reserved_count += 1;
                        continue;
                    }
                    None => {}
                }
                let distance = Self::score_record(request, record, query)?;
                scored_passes.insert(record.candidate.source_occurrence_id.clone(), passes);
                eligible_count += 1;
                Self::retain_scored_record(record, distance, cap, &mut ranked);
            }
            // `returned <= requested <= next_depth` and the pool never exceeds
            // the total depth searched, so these conversions only fail if the
            // window check above was bypassed.
            let requested = next_depth - searched_depth;
            let returned = u32::try_from(returned).map_err(|_| {
                RetrievalPortError::Contract(
                    "semantic ANN pass answered beyond the recall policy domain".to_owned(),
                )
            })?;
            let pool = u32::try_from(eligible_count).map_err(|_| {
                RetrievalPortError::Contract(
                    "semantic ANN candidate pool exceeds the recall policy domain".to_owned(),
                )
            })?;
            searched_depth = next_depth;
            match policy.step(retained_cap, searched_depth, requested, returned, pool) {
                AdaptiveRecallStepV1::Grow { next_depth: depth } => next_depth = depth,
                AdaptiveRecallStepV1::Stop(stop) => break stop,
            }
        };
        let recall = SemanticAdaptiveRecallExecutionV1 {
            passes,
            final_depth: searched_depth,
            target: policy.target(retained_cap),
            stop,
        };
        hotpath::gauge!("query.lane.semantic.ann.passes").set(passes);
        hotpath::gauge!("query.lane.semantic.ann.depth").set(searched_depth);
        hotpath::gauge!("query.lane.semantic.ann.candidates").set(eligible_count + reserved_count);
        let coverage = RetrieverCoverage {
            examined: (eligible_count + reserved_count) as u64,
            eligible: eligible_count as u64,
            excluded: reserved_count as u64,
            capped: eligible_count.saturating_sub(ranked.len()) as u64,
            unknown: 0,
        };
        hotpath::gauge!("query.lane.semantic.examined").set(coverage.examined);
        hotpath::gauge!("query.lane.semantic.candidates").set(eligible_count);
        self.assemble_ranked_batch(
            request,
            ranked,
            coverage,
            false,
            SemanticSearchExecutionV1::AnnHnswExactRescore { recall },
        )
    }

    /// Exact-flat scan: visit and exactly score every published row.
    fn retrieve_exact_flat(
        &self,
        request: &SemanticRetrievalRequestV1<'_>,
        query: &EphemeralQueryEmbeddingV1,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        // Bound retention to the lane cap during the scan with a max-heap
        // keyed by the final ranking order, instead of collecting every
        // eligible row into an unbounded vec and sorting the whole set. The cap
        // depends only on the request budgets, so it is known up front. Every
        // eligible row is still visited (duplicate detection and coverage
        // accounting run over all of them via `eligible_count`); only the
        // retained set is bounded. Because `source_occurrence_id` is unique
        // across retained rows, the ranking order is a strict total order with
        // no ties, so the cap smallest rows and their order are identical to a
        // full sort followed by truncation.
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let mut ranked: BinaryHeap<SemanticRankedEntryV1> = BinaryHeap::new();
        let mut eligible_count: usize = 0;
        let mut seen_occurrences = BTreeSet::new();
        let scan_request = SemanticVectorReadRequestV1 {
            vector_generation: &request.vector_generation,
            projection_key: request.projection.projection_key(),
            search_index_key: request.search_index_key,
            source_generation: &request.code_generation,
            capability_manifest_digest: &request.capability_manifest_digest,
            search_kind: SemanticSearchKindV1::ExactFlat,
        };
        let mut examine = || {
            if self.control.is_cancelled() {
                return Err(RetrievalPortError::Cancelled);
            }
            if deadline_exhausted(request, self.control) {
                return Err(RetrievalPortError::BudgetExceeded);
            }
            Ok(())
        };
        let scan = self
            .vectors
            .scan_exact_flat(scan_request, &mut examine, &mut |record| {
                let distance = Self::score_record(request, record, query)?;
                if !seen_occurrences.insert(record.candidate.source_occurrence_id.clone()) {
                    observe_semantic_lane_failure("scan_duplicate_occurrence", "contract");
                    return Err(RetrievalPortError::Contract(
                        "semantic vector generation contains duplicate source occurrences"
                            .to_owned(),
                    ));
                }
                eligible_count += 1;
                Self::retain_scored_record(record, distance, cap, &mut ranked);
                Ok(())
            });
        let summary = match scan {
            Ok(summary) => summary,
            Err(error) => {
                return port_error_outcome(
                    error,
                    budget_usage(request, eligible_count as u64, 0, self.control),
                );
            }
        };

        hotpath::gauge!("query.lane.semantic.examined").set(summary.examined);
        hotpath::gauge!("query.lane.semantic.candidates").set(eligible_count);
        if self.control.is_cancelled() {
            hotpath::gauge!("query.cancel.count").inc(1u32);
            return Ok(RetrieverOutcome::Cancelled);
        }
        if deadline_exhausted(request, self.control) {
            return Ok(RetrieverOutcome::BudgetExceeded(budget_usage(
                request,
                eligible_count as u64,
                0,
                self.control,
            )));
        }
        if summary.eligible != eligible_count as u64 {
            observe_semantic_lane_failure("scan_eligible_coverage", "contract");
            return Err(RetrievalPortError::Contract(
                "semantic vector scan coverage does not match the visited eligible rows".to_owned(),
            ));
        }
        let accounted = summary
            .eligible
            .checked_add(summary.excluded)
            .and_then(|count| count.checked_add(summary.unknown))
            .ok_or_else(|| {
                observe_semantic_lane_failure("scan_coverage_overflow", "contract");
                RetrievalPortError::Contract("semantic scan coverage overflowed".to_owned())
            })?;
        if summary.examined != accounted {
            observe_semantic_lane_failure("scan_coverage_accounting", "contract");
            return Err(RetrievalPortError::Contract(
                "semantic vector scan coverage is incomplete".to_owned(),
            ));
        }
        if summary.unknown != 0 {
            return Ok(RetrieverOutcome::Unavailable(
                RetrievalFailure::AuthorityUnavailable {
                    detail: "semantic vector generation has unknown coverage".to_owned(),
                },
            ));
        }

        let truncated = eligible_count.saturating_sub(cap);
        let coverage = RetrieverCoverage {
            examined: summary.examined,
            eligible: summary.eligible,
            excluded: summary.excluded,
            capped: truncated as u64,
            unknown: summary.unknown,
        };
        self.assemble_ranked_batch(
            request,
            ranked,
            coverage,
            truncated == 0,
            SemanticSearchExecutionV1::ExactFlat,
        )
    }

    /// Drain the retained heap in ascending ranking order (identical to a
    /// full sort followed by truncation), bind evidence, and validate the
    /// batch. Shared by the exact-flat and ANN-rescore paths; only their
    /// coverage accounting, exhaustion claims, and executed search differ.
    /// Evidence is bound here rather than at retention because the ANN
    /// execution facts are known only once the recall loop has stopped.
    fn assemble_ranked_batch(
        &self,
        request: &SemanticRetrievalRequestV1<'_>,
        ranked: BinaryHeap<SemanticRankedEntryV1>,
        coverage: RetrieverCoverage,
        exhausted: bool,
        search: SemanticSearchExecutionV1,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        let ranked = ranked.into_sorted_vec();
        let mut candidates = Vec::with_capacity(ranked.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, entry) in ranked.into_iter().enumerate() {
            let SemanticRankedEntryV1 {
                mut candidate,
                chunk_id,
                distance,
            } = entry;
            candidate.ordinal_rank = ordinal as u32;
            evidence_by_occurrence.insert(
                candidate.source_occurrence_id.clone(),
                CodeSemanticEvidenceV1 {
                    projection_key: request.projection.embedding_key().clone(),
                    search_index_key: request.search_index_key.clone(),
                    vector_generation: request.vector_generation.clone(),
                    chunk_id,
                    distance,
                    search,
                },
            );
            candidates.push(candidate);
        }
        let checkpoint_digest =
            semantic_checkpoint_digest(request, &candidates).inspect_err(|error| {
                observe_semantic_lane_failure("checkpoint_digest", port_error_class(error));
            })?;
        let batch = RetrieverBatch {
            candidates,
            evidence_by_occurrence,
            coverage,
            continuation: Some(RetrieverContinuation {
                lane: RetrieverKind::Semantic,
                checkpoint_digest,
                exhausted,
            }),
        };
        batch
            .validate()
            .map_err(contract_error)
            .inspect_err(|error| {
                observe_semantic_lane_failure("batch_validation", port_error_class(error));
            })?;
        if self.control.is_cancelled() {
            return Ok(RetrieverOutcome::Cancelled);
        }
        if deadline_exhausted(request, self.control) {
            return Ok(RetrieverOutcome::BudgetExceeded(budget_usage(
                request,
                coverage.examined,
                batch.candidates.len() as u64,
                self.control,
            )));
        }
        Ok(RetrieverOutcome::Complete(batch))
    }
}

impl<E, V, C> SemanticLaneRetriever for SemanticCodeRetriever<'_, E, V, C>
where
    E: SemanticQueryEmbeddingPort,
    V: SemanticVectorReadPort,
    C: SemanticExecutionControl,
{
    #[hotpath::measure(label = "query.lane.semantic")]
    fn retrieve_semantic(
        &self,
        request: &SemanticRetrievalRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        request.validate().inspect_err(|error| {
            observe_semantic_lane_failure("request_validation", port_error_class(error));
        })?;
        if self.control.is_cancelled() {
            hotpath::gauge!("query.cancel.count").inc(1u32);
            return Ok(RetrieverOutcome::Cancelled);
        }
        if deadline_exhausted(request, self.control) {
            return Ok(RetrieverOutcome::BudgetExceeded(budget_usage(
                request,
                0,
                0,
                self.control,
            )));
        }
        let query = match self.embedder.embed_query(SemanticQueryEmbeddingRequestV1 {
            query_digest: &request.query_digest,
            query_view: request.query_view,
            projection: request.projection,
        }) {
            Ok(query) => query,
            Err(error) => {
                observe_semantic_lane_failure("query_embedding", port_error_class(&error));
                return port_error_outcome(error, budget_usage(request, 0, 0, self.control));
            }
        };
        if self.control.is_cancelled() {
            hotpath::gauge!("query.cancel.count").inc(1u32);
            return Ok(RetrieverOutcome::Cancelled);
        }
        if deadline_exhausted(request, self.control) {
            return Ok(RetrieverOutcome::BudgetExceeded(budget_usage(
                request,
                0,
                0,
                self.control,
            )));
        }
        let outcome = self.retrieve_complete(request, &query)?;
        crate::hotpath_metrics::record_lane(
            "query.lane.semantic.candidates",
            "query.lane.semantic.examined",
            "query.lane.semantic.results",
            "query.lane.semantic.residency",
            &outcome,
        );
        Ok(outcome)
    }
}

pub(super) fn port_error_class(error: &RetrievalPortError) -> &'static str {
    match error {
        RetrievalPortError::CapabilityManifestRejected => "capability_manifest_rejected",
        RetrievalPortError::GenerationMismatch => "generation_mismatch",
        RetrievalPortError::AuthorityUnavailable(_) => "authority_unavailable",
        RetrievalPortError::IncompatibleProjection => "incompatible_projection",
        RetrievalPortError::StaleEvidence => "stale_evidence",
        RetrievalPortError::Cancelled => "cancelled",
        RetrievalPortError::BudgetExceeded => "budget_exceeded",
        RetrievalPortError::Contract(_) => "contract",
    }
}

/// An ANN-profiled request fell back to the exact-flat scan. Fallbacks are
/// typed and observable, never silent: the evidence records the executed
/// `ExactFlat` path and these gauges record why the index could not serve.
fn observe_semantic_ann_fallback(state: SemanticAnnIndexStateV1) {
    match state {
        SemanticAnnIndexStateV1::Missing => {
            hotpath::gauge!("query.lane.semantic.ann.fallback.missing").inc(1_u64);
        }
        SemanticAnnIndexStateV1::IncompleteCoverage { .. } => {
            hotpath::gauge!("query.lane.semantic.ann.fallback.incomplete_coverage").inc(1_u64);
        }
        SemanticAnnIndexStateV1::Unsupported => {
            hotpath::gauge!("query.lane.semantic.ann.fallback.unsupported").inc(1_u64);
        }
    }
}

pub(super) fn observe_semantic_lane_failure(stage: &'static str, error_class: &'static str) {
    match stage {
        "request_validation" => {
            hotpath::gauge!("query.lane.semantic.failure.request_validation").inc(1_u64);
        }
        "query_embedding" => {
            hotpath::gauge!("query.lane.semantic.failure.query_embedding").inc(1_u64);
        }
        "query_embedding_identity" => {
            hotpath::gauge!("query.lane.semantic.failure.query_embedding_identity").inc(1_u64);
        }
        "score_generation_projection" => {
            hotpath::gauge!("query.lane.semantic.failure.score_generation_projection").inc(1_u64);
        }
        "score_source_generation" => {
            hotpath::gauge!("query.lane.semantic.failure.score_source_generation").inc(1_u64);
        }
        "score_candidate_binding" => {
            hotpath::gauge!("query.lane.semantic.failure.score_candidate_binding").inc(1_u64);
        }
        "score_candidate_kind" => {
            hotpath::gauge!("query.lane.semantic.failure.score_candidate_kind").inc(1_u64);
        }
        "score_scope_freshness" => {
            hotpath::gauge!("query.lane.semantic.failure.score_scope_freshness").inc(1_u64);
        }
        "score_vector_validation" => {
            hotpath::gauge!("query.lane.semantic.failure.score_vector_validation").inc(1_u64);
        }
        "score_distance" => {
            hotpath::gauge!("query.lane.semantic.failure.score_distance").inc(1_u64);
        }
        "scan_duplicate_occurrence" => {
            hotpath::gauge!("query.lane.semantic.failure.scan_duplicate_occurrence").inc(1_u64);
        }
        "ann_candidates" => {
            hotpath::gauge!("query.lane.semantic.failure.ann_candidates").inc(1_u64);
        }
        "ann_duplicate_occurrence" => {
            hotpath::gauge!("query.lane.semantic.failure.ann_duplicate_occurrence").inc(1_u64);
        }
        "ann_candidate_limit" => {
            hotpath::gauge!("query.lane.semantic.failure.ann_candidate_limit").inc(1_u64);
        }
        "ann_unavailable_between_passes" => {
            hotpath::gauge!("query.lane.semantic.failure.ann_unavailable_between_passes")
                .inc(1_u64);
        }
        "ann_recall_policy" => {
            hotpath::gauge!("query.lane.semantic.failure.ann_recall_policy").inc(1_u64);
        }
        "ann_retained_cap" => {
            hotpath::gauge!("query.lane.semantic.failure.ann_retained_cap").inc(1_u64);
        }
        "scan_eligible_coverage" => {
            hotpath::gauge!("query.lane.semantic.failure.scan_eligible_coverage").inc(1_u64);
        }
        "scan_coverage_overflow" => {
            hotpath::gauge!("query.lane.semantic.failure.scan_coverage_overflow").inc(1_u64);
        }
        "scan_coverage_accounting" => {
            hotpath::gauge!("query.lane.semantic.failure.scan_coverage_accounting").inc(1_u64);
        }
        "checkpoint_digest" => {
            hotpath::gauge!("query.lane.semantic.failure.checkpoint_digest").inc(1_u64);
        }
        "batch_validation" => {
            hotpath::gauge!("query.lane.semantic.failure.batch_validation").inc(1_u64);
        }
        "service_retrieval" => {
            hotpath::gauge!("query.lane.semantic.failure.service_retrieval").inc(1_u64);
        }
        _ => {
            hotpath::gauge!("query.lane.semantic.failure.unknown_stage").inc(1_u64);
        }
    }
    match error_class {
        "capability_manifest_rejected" => {
            hotpath::gauge!("query.lane.semantic.failure.class.capability_manifest_rejected")
                .inc(1_u64);
        }
        "generation_mismatch" => {
            hotpath::gauge!("query.lane.semantic.failure.class.generation_mismatch").inc(1_u64);
        }
        "authority_unavailable" => {
            hotpath::gauge!("query.lane.semantic.failure.class.authority_unavailable").inc(1_u64);
        }
        "incompatible_projection" => {
            hotpath::gauge!("query.lane.semantic.failure.class.incompatible_projection").inc(1_u64);
        }
        "stale_evidence" => {
            hotpath::gauge!("query.lane.semantic.failure.class.stale_evidence").inc(1_u64);
        }
        "cancelled" => {
            hotpath::gauge!("query.lane.semantic.failure.class.cancelled").inc(1_u64);
        }
        "budget_exceeded" => {
            hotpath::gauge!("query.lane.semantic.failure.class.budget_exceeded").inc(1_u64);
        }
        "contract" => {
            hotpath::gauge!("query.lane.semantic.failure.class.contract").inc(1_u64);
        }
        _ => {
            hotpath::gauge!("query.lane.semantic.failure.class.unknown").inc(1_u64);
        }
    }
}

fn validate_vector(values: &[f32], dimensions: u32, label: &str) -> Result<(), RetrievalPortError> {
    if values.len() != dimensions as usize {
        return Err(RetrievalPortError::IncompatibleProjection);
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(RetrievalPortError::Contract(format!(
            "{label} contains a non-finite value"
        )));
    }
    Ok(())
}

fn canonical_distance(
    metric: EmbeddingMetricV1,
    query: &[f32],
    document: &[f32],
) -> Result<CanonicalSemanticDistanceV1, RetrievalPortError> {
    if query.len() != document.len() || query.is_empty() {
        return Err(RetrievalPortError::IncompatibleProjection);
    }
    let distance = match metric {
        EmbeddingMetricV1::Cosine => {
            let mut dot = 0.0_f64;
            let mut query_norm = 0.0_f64;
            let mut document_norm = 0.0_f64;
            for (&query_value, &document_value) in query.iter().zip(document) {
                let query_value = f64::from(query_value);
                let document_value = f64::from(document_value);
                dot += query_value * document_value;
                query_norm += query_value * query_value;
                document_norm += document_value * document_value;
            }
            if query_norm == 0.0 || document_norm == 0.0 {
                return Err(RetrievalPortError::Contract(
                    "cosine distance is undefined for a zero-norm vector".to_owned(),
                ));
            }
            1.0 - (dot / (query_norm.sqrt() * document_norm.sqrt())).clamp(-1.0, 1.0)
        }
        EmbeddingMetricV1::DotProduct => {
            let mut dot = 0.0_f64;
            for (&query_value, &document_value) in query.iter().zip(document) {
                dot += f64::from(query_value) * f64::from(document_value);
            }
            -dot
        }
        EmbeddingMetricV1::EuclideanL2 => {
            let mut squared = 0.0_f64;
            for (&query_value, &document_value) in query.iter().zip(document) {
                let delta = f64::from(query_value) - f64::from(document_value);
                squared += delta * delta;
            }
            squared.sqrt()
        }
    };
    if !distance.is_finite() {
        return Err(RetrievalPortError::Contract(
            "semantic distance is not finite".to_owned(),
        ));
    }
    let scaled = (distance * SEMANTIC_DISTANCE_SCALE).round();
    if scaled < i64::MIN as f64 || scaled > i64::MAX as f64 {
        return Err(RetrievalPortError::Contract(
            "semantic distance exceeds the canonical fixed-point range".to_owned(),
        ));
    }
    Ok(CanonicalSemanticDistanceV1(scaled as i64))
}

fn semantic_checkpoint_digest(
    request: &SemanticRetrievalRequestV1<'_>,
    candidates: &[CompactCandidate],
) -> Result<CursorPayloadDigest, RetrievalPortError> {
    let scope_digest = request
        .base
        .scope
        .compute_digest()
        .map_err(contract_error)?;
    let snapshot_digest = request
        .base
        .snapshot
        .compute_digest()
        .map_err(contract_error)?;
    checkpoint_digest(&(
        SEMANTIC_CHECKPOINT_DOMAIN,
        RetrieverKind::Semantic.as_str(),
        scope_digest,
        snapshot_digest,
        &request.query_digest,
        request.code_generation.as_str(),
        request.vector_generation.as_digest(),
        request.projection.projection_key(),
        request.projection.privacy_domain(),
        request.projection.privacy_key_epoch(),
        &request.capability_manifest_digest,
        candidate_checkpoint_prefix(candidates),
    ))
}

fn effective_deadline_micros(request: &SemanticRetrievalRequestV1<'_>) -> u64 {
    match (
        request.budget.deadline_micros,
        request.base.budget.deadline_micros,
    ) {
        (Some(lane), Some(base)) => lane.min(base),
        (Some(lane), None) => lane,
        (None, Some(base)) => base,
        (None, None) => SEMANTIC_EXACT_FLAT_DEFAULT_DEADLINE_MICROS_V1,
    }
}

fn deadline_exhausted<C: SemanticExecutionControl>(
    request: &SemanticRetrievalRequestV1<'_>,
    control: &C,
) -> bool {
    elapsed_micros(request, control) >= effective_deadline_micros(request)
}

fn elapsed_micros<C: SemanticExecutionControl>(
    _request: &SemanticRetrievalRequestV1<'_>,
    control: &C,
) -> u64 {
    control.elapsed_micros()
}

#[cfg(test)]
thread_local! {
    static SEMANTIC_RETAINED_MATERIALIZATIONS: Cell<usize> = const { Cell::new(0) };
    static SEMANTIC_SCORED_ROWS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_semantic_retained_materializations() -> usize {
    SEMANTIC_RETAINED_MATERIALIZATIONS.with(|count| count.replace(0))
}

#[cfg(test)]
pub(crate) fn take_semantic_scored_rows() -> usize {
    SEMANTIC_SCORED_ROWS.with(|count| count.replace(0))
}

fn rank_key<'a>(
    distance: CanonicalSemanticDistanceV1,
    source_occurrence_id: &'a tracedecay_domain::SourceOccurrenceId,
    retriever_evidence_anchor: &'a tracedecay_domain::RetrievalAnchorId,
    chunk_id: &'a tracedecay_domain::CodeSearchChunkId,
) -> (
    CanonicalSemanticDistanceV1,
    &'a tracedecay_domain::SourceOccurrenceId,
    &'a tracedecay_domain::RetrievalAnchorId,
    &'a tracedecay_domain::CodeSearchChunkId,
) {
    (
        distance,
        source_occurrence_id,
        retriever_evidence_anchor,
        chunk_id,
    )
}

/// One retained scored row, ordered by the deterministic semantic ranking
/// key (ascending distance, then `source_occurrence_id`,
/// `retriever_evidence_anchor`, `chunk_id`). `Ord` mirrors the former
/// `ranked.sort_by` comparator exactly, so a max-heap of these keeps the cap
/// smallest rows and `into_sorted_vec` yields the identical ascending order.
struct SemanticRankedEntryV1 {
    candidate: CompactCandidate,
    chunk_id: CodeSearchChunkId,
    distance: CanonicalSemanticDistanceV1,
}

impl SemanticRankedEntryV1 {
    fn rank_cmp(&self, other: &Self) -> Ordering {
        rank_key(
            self.distance,
            &self.candidate.source_occurrence_id,
            &self.candidate.retriever_evidence_anchor,
            &self.chunk_id,
        )
        .cmp(&rank_key(
            other.distance,
            &other.candidate.source_occurrence_id,
            &other.candidate.retriever_evidence_anchor,
            &other.chunk_id,
        ))
    }
}

/// The lane executes exactly one recall policy, so an ANN key must be the
/// identity that policy mints: serving a key minted under another policy
/// would silently run a different depth sequence than the key committed to.
fn verify_ann_search_index_key(
    search_index_key: &SemanticSearchIndexKeyV1,
    policy: &AdaptiveRecallDepthPolicyV1,
) -> Result<(), RetrievalPortError> {
    let committed = SemanticSearchIndexProfileV1::ann_hnsw_exact_rescore(policy)
        .and_then(|profile| profile.index_key())
        .map_err(contract_error)
        .inspect_err(|error| {
            observe_semantic_lane_failure("ann_recall_policy", port_error_class(error));
        })?;
    if *search_index_key != committed {
        observe_semantic_lane_failure("ann_recall_policy", "contract");
        return Err(RetrievalPortError::Contract(
            "semantic ANN search index key does not commit to the lane's recall policy".to_owned(),
        ));
    }
    Ok(())
}

impl Ord for SemanticRankedEntryV1 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank_cmp(other)
    }
}

impl PartialOrd for SemanticRankedEntryV1 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for SemanticRankedEntryV1 {
    fn eq(&self, other: &Self) -> bool {
        self.rank_cmp(other) == Ordering::Equal
    }
}

impl Eq for SemanticRankedEntryV1 {}

fn budget_usage<C: SemanticExecutionControl>(
    request: &SemanticRetrievalRequestV1<'_>,
    candidates_examined: u64,
    candidates_returned: u64,
    control: &C,
) -> RetrievalBudgetUsage {
    RetrievalBudgetUsage {
        candidates_examined,
        candidates_returned,
        hydrated_results: 0,
        hydration_bytes: 0,
        elapsed_micros: elapsed_micros(request, control),
    }
}

fn port_error_outcome<E>(
    error: RetrievalPortError,
    usage: RetrievalBudgetUsage,
) -> Result<RetrieverOutcome<RetrieverBatch<E>>, RetrievalPortError> {
    match error {
        RetrievalPortError::AuthorityUnavailable(detail) => Ok(RetrieverOutcome::Unavailable(
            RetrievalFailure::AuthorityUnavailable { detail },
        )),
        RetrievalPortError::IncompatibleProjection => Ok(RetrieverOutcome::Unavailable(
            RetrievalFailure::IncompatibleProjection {
                detail: "semantic projection identity is incompatible".to_owned(),
            },
        )),
        RetrievalPortError::StaleEvidence => {
            Ok(RetrieverOutcome::Unavailable(RetrievalFailure::StaleSource))
        }
        RetrievalPortError::Cancelled => Ok(RetrieverOutcome::Cancelled),
        RetrievalPortError::BudgetExceeded => Ok(RetrieverOutcome::BudgetExceeded(usage)),
        error => Err(error),
    }
}

#[cfg(test)]
mod tests;

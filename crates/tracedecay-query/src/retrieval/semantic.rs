//! Exact-flat semantic retrieval lane.
//!
//! The lane consumes only an admitted embedding projection, a request-local
//! query-embedding port, and an immutable vector-generation read port. It
//! performs no artifact admission, vector mutation, ANN lookup, fusion,
//! reranking, hydration, activation, or calls into another retrieval lane.

#[cfg(test)]
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, CompactCandidate,
    CursorPayloadDigest, EmbeddingMetricV1, EmbeddingProjectionKeyV1,
    EphemeralSanitizedQueryViewV1, FixedPointScore, FreshnessCompatibilityV1, ManifestDigest,
    ProjectionKeyV1, QueryDigest, RetrievalBudget, RetrievalBudgetUsage, RetrievalError,
    RetrievalFailure, RetrievalRequest, Retriever, RetrieverBatch, RetrieverContinuation,
    RetrieverCoverage, RetrieverKind, RetrieverOutcome, SemanticSearchIndexKeyV1,
    VectorGenerationIdV1,
};

use super::ports::{
    CodeCandidateBindingV1, CompactCandidateLane, RetrievalPortError, candidate_checkpoint_prefix,
    checkpoint_digest, contract_error, lane_candidate_cap,
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

/// The only search implementation admitted by this quarantined lane.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SemanticSearchKindV1 {
    #[serde(rename = "exact_flat")]
    ExactFlat,
}

/// Canonical fixed-point semantic distance. Smaller values rank first.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalSemanticDistanceV1(i64);

impl CanonicalSemanticDistanceV1 {
    pub const fn micros(self) -> i64 {
        self.0
    }

    fn as_descending_score(self) -> FixedPointScore {
        let order_preserving = (self.0 as u64) ^ (1_u64 << 63);
        FixedPointScore(u64::MAX - order_preserving)
    }
}

/// Typed semantic evidence attached to one generic compact candidate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSemanticEvidenceV1 {
    pub projection_key: EmbeddingProjectionKeyV1,
    pub search_index_key: SemanticSearchIndexKeyV1,
    pub vector_generation: VectorGenerationIdV1,
    pub chunk_id: CodeSearchChunkId,
    pub distance: CanonicalSemanticDistanceV1,
    pub search_kind: SemanticSearchKindV1,
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
        if record.vector_generation != request.vector_generation
            || record.projection_key != *request.projection.projection_key()
        {
            return Err(RetrievalPortError::IncompatibleProjection);
        }
        if record.source_generation != request.code_generation
            || record.binding.occurrence.generation != request.code_generation
        {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        if record.binding.occurrence.chunk.as_ref() != Some(&record.chunk_id)
            || record.binding.candidate_anchor != record.candidate.anchor_id
            || record.binding.source_occurrence != record.candidate.source_occurrence_id
        {
            return Err(RetrievalPortError::Contract(
                "semantic vector row does not bind its chunk and candidate occurrence".to_owned(),
            ));
        }
        if record.candidate.retriever != RetrieverKind::Semantic
            || record.candidate.exact_admission_proof.is_some()
        {
            return Err(RetrievalPortError::Contract(
                "semantic vectors may emit only non-exact semantic candidates".to_owned(),
            ));
        }
        if record.candidate.repository_id.as_ref() != Some(&request.base.scope.root.repository)
            || record.candidate.freshness.compatibility != FreshnessCompatibilityV1::Current
        {
            return Err(RetrievalPortError::Contract(
                "semantic candidate is outside the frozen repository or freshness scope".to_owned(),
            ));
        }
        validate_vector(
            &record.values,
            request.projection.embedding_key().dimensions,
            "stored semantic vector",
        )?;
        canonical_distance(
            request.projection.embedding_key().metric,
            &query.values,
            &record.values,
        )
    }

    fn materialize_record(
        request: &SemanticRetrievalRequestV1<'_>,
        record: &SemanticVectorRecordV1,
        distance: CanonicalSemanticDistanceV1,
    ) -> SemanticRankedEntryV1 {
        #[cfg(test)]
        SEMANTIC_RETAINED_MATERIALIZATIONS.with(|count| count.set(count.get() + 1));
        let mut candidate = record.candidate.clone();
        candidate.raw_score = distance.as_descending_score();
        SemanticRankedEntryV1 {
            candidate,
            evidence: CodeSemanticEvidenceV1 {
                projection_key: request.projection.embedding_key().clone(),
                search_index_key: request.search_index_key.clone(),
                vector_generation: request.vector_generation.clone(),
                chunk_id: record.chunk_id.clone(),
                distance,
                search_kind: SemanticSearchKindV1::ExactFlat,
            },
        }
    }

    fn retain_scored_record(
        request: &SemanticRetrievalRequestV1<'_>,
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
                worst.evidence.distance,
                &worst.candidate.source_occurrence_id,
                &worst.candidate.retriever_evidence_anchor,
                &worst.evidence.chunk_id,
            )) == Ordering::Less
        } else {
            false
        };
        if !retain {
            return;
        }
        let entry = Self::materialize_record(request, record, distance);
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
            return Err(RetrievalPortError::IncompatibleProjection);
        }

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
                    return Err(RetrievalPortError::Contract(
                        "semantic vector generation contains duplicate source occurrences"
                            .to_owned(),
                    ));
                }
                eligible_count += 1;
                Self::retain_scored_record(request, record, distance, cap, &mut ranked);
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

        if self.control.is_cancelled() {
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
            return Err(RetrievalPortError::Contract(
                "semantic vector scan coverage does not match the visited eligible rows".to_owned(),
            ));
        }
        let accounted = summary
            .eligible
            .checked_add(summary.excluded)
            .and_then(|count| count.checked_add(summary.unknown))
            .ok_or_else(|| {
                RetrievalPortError::Contract("semantic scan coverage overflowed".to_owned())
            })?;
        if summary.examined != accounted {
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

        // The heap already retained only the cap smallest rows; drain it in
        // ascending ranking order (identical to sorting the full set and
        // truncating to `cap`).
        let ranked = ranked.into_sorted_vec();
        let truncated = eligible_count.saturating_sub(cap);

        let mut candidates = Vec::with_capacity(ranked.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, entry) in ranked.into_iter().enumerate() {
            let SemanticRankedEntryV1 {
                mut candidate,
                evidence,
            } = entry;
            candidate.ordinal_rank = ordinal as u32;
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        let checkpoint_digest = semantic_checkpoint_digest(request, &candidates)?;
        let batch = RetrieverBatch {
            candidates,
            evidence_by_occurrence,
            coverage: RetrieverCoverage {
                examined: summary.examined,
                eligible: summary.eligible,
                excluded: summary.excluded,
                capped: truncated as u64,
                unknown: summary.unknown,
            },
            continuation: Some(RetrieverContinuation {
                lane: RetrieverKind::Semantic,
                checkpoint_digest,
                exhausted: truncated == 0,
            }),
        };
        batch.validate().map_err(contract_error)?;
        if self.control.is_cancelled() {
            return Ok(RetrieverOutcome::Cancelled);
        }
        if deadline_exhausted(request, self.control) {
            return Ok(RetrieverOutcome::BudgetExceeded(budget_usage(
                request,
                summary.examined,
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
    fn retrieve_semantic(
        &self,
        request: &SemanticRetrievalRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        request.validate()?;
        if self.control.is_cancelled() {
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
                return port_error_outcome(error, budget_usage(request, 0, 0, self.control));
            }
        };
        if self.control.is_cancelled() {
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
        self.retrieve_complete(request, &query)
    }
}

impl<'request, E, V, C> Retriever<SemanticRetrievalRequestV1<'request>, CodeSemanticEvidenceV1>
    for SemanticCodeRetriever<'_, E, V, C>
where
    E: SemanticQueryEmbeddingPort,
    V: SemanticVectorReadPort,
    C: SemanticExecutionControl,
{
    fn retrieve(
        &self,
        request: &SemanticRetrievalRequestV1<'request>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalError> {
        self.retrieve_semantic(request)
            .map_err(|error| RetrievalError::InvalidRequest(error.to_string()))
    }
}

impl<'request, E, V, C>
    CompactCandidateLane<SemanticRetrievalRequestV1<'request>, CodeSemanticEvidenceV1>
    for SemanticCodeRetriever<'_, E, V, C>
where
    E: SemanticQueryEmbeddingPort,
    V: SemanticVectorReadPort,
    C: SemanticExecutionControl,
{
    fn candidates(
        &self,
        request: &SemanticRetrievalRequestV1<'request>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        self.retrieve_semantic(request)
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
}

#[cfg(test)]
pub(crate) fn take_semantic_retained_materializations() -> usize {
    SEMANTIC_RETAINED_MATERIALIZATIONS.with(|count| count.replace(0))
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

/// One retained ExactFlat row, ordered by the deterministic semantic ranking
/// key (ascending distance, then `source_occurrence_id`,
/// `retriever_evidence_anchor`, `chunk_id`). `Ord` mirrors the former
/// `ranked.sort_by` comparator exactly, so a max-heap of these keeps the cap
/// smallest rows and `into_sorted_vec` yields the identical ascending order.
struct SemanticRankedEntryV1 {
    candidate: CompactCandidate,
    evidence: CodeSemanticEvidenceV1,
}

impl SemanticRankedEntryV1 {
    fn rank_cmp(&self, other: &Self) -> Ordering {
        rank_key(
            self.evidence.distance,
            &self.candidate.source_occurrence_id,
            &self.candidate.retriever_evidence_anchor,
            &self.evidence.chunk_id,
        )
        .cmp(&rank_key(
            other.evidence.distance,
            &other.candidate.source_occurrence_id,
            &other.candidate.retriever_evidence_anchor,
            &other.evidence.chunk_id,
        ))
    }
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

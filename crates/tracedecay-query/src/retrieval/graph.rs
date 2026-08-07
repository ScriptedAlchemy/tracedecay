//! Graph lane contracts.
//!
//! The lane emits generation-bound code anchors and ordered path evidence
//! without copying graph rows into a search corpus. The graph adapter exposes
//! its own candidate pool and oracle recall rather than becoming a lexical
//! field.
//!
//! Every graph path preserves its weakest edge authority and coverage;
//! unresolved dispatch cannot become semantic fact.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_application::retrieval::{MAX_APPLICATION_PAGE_SIZE, MAX_CALLABLE_CODE_DEPTH};
use tracedecay_domain::{
    CodeGenerationId, CompactCandidate, CursorPayloadDigest, EdgeAuthorityV1, RelationEdgeKindV1,
    RetrievalBudget, RetrievalFailure, RetrievalRequest, RetrieverBatch, RetrieverContinuation,
    RetrieverKind, RetrieverOutcome, SourceOccurrenceId, SourceSpan, SymbolOccurrenceId,
};

use super::ports::{
    CodeCandidateBindingV1, GraphEvidenceReadPort, LaneBoundEvidence, LaneEvidenceRejections,
    RetrievalPortError, checkpoint_digest, contract_error, lane_bound_evidence, lane_candidate_cap,
};

mod projection;

pub use self::projection::production_code_index_freshness;

/// Live request authority consulted throughout one graph traversal.
pub trait GraphExecutionControl: Send + Sync {
    fn is_cancelled(&self) -> bool;
    /// Monotonic elapsed time in the request-relative domain used by
    /// [`RetrievalBudget::deadline_micros`].
    fn elapsed_micros(&self) -> u64;
}

impl<T> GraphExecutionControl for T
where
    T: super::semantic::SemanticExecutionControl + Send + Sync + ?Sized,
{
    fn is_cancelled(&self) -> bool {
        super::semantic::SemanticExecutionControl::is_cancelled(self)
    }

    fn elapsed_micros(&self) -> u64 {
        super::semantic::SemanticExecutionControl::elapsed_micros(self)
    }
}

/// Wording the graph lane uses when a port-emitted batch fails the shared
/// candidate/evidence binding checks.
const GRAPH_REJECTIONS: LaneEvidenceRejections = LaneEvidenceRejections {
    foreign_candidate: "the graph lane cannot emit candidates from another lane",
    missing_evidence: "graph lane evidence is missing for a returned occurrence",
    unaddressed_binding: "graph lane binding does not address its candidate",
};

/// Typed graph-lane request for bounded traversal from generation-matched
/// anchors.
///
/// Relation and path requests preserve edge authority and weakest coverage
/// state.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphLaneRequest {
    pub base: RetrievalRequest,
    pub generation: CodeGenerationId,
    pub seed_anchors: Vec<CodeCandidateBindingV1>,
    pub edge_kinds: Vec<RelationEdgeKindV1>,
    /// Bounded traversal depth; the profile owns the bound so graph-hop
    /// cutoffs remain evaluation-locked.
    pub max_depth: u32,
    pub budget: RetrievalBudget,
}

impl GraphLaneRequest {
    pub fn validate(&self) -> Result<(), RetrievalPortError> {
        self.base.budget.validate().map_err(contract_error)?;
        self.budget.validate().map_err(contract_error)?;
        self.generation.validate().map_err(contract_error)?;
        if self.max_depth == 0 {
            return Err(RetrievalPortError::Contract(
                "graph traversal depth must be positive".to_owned(),
            ));
        }
        if self.max_depth > MAX_CALLABLE_CODE_DEPTH {
            return Err(RetrievalPortError::Contract(
                "graph traversal depth exceeds the callable code bound".to_owned(),
            ));
        }
        if self.budget.max_candidates_per_lane > MAX_APPLICATION_PAGE_SIZE
            || self.base.budget.max_candidates_per_lane > MAX_APPLICATION_PAGE_SIZE
        {
            return Err(RetrievalPortError::Contract(
                "graph candidate budget exceeds the application page bound".to_owned(),
            ));
        }
        if self.seed_anchors.is_empty() {
            return Err(RetrievalPortError::Contract(
                "graph retrieval requires at least one seed anchor".to_owned(),
            ));
        }
        if self.seed_anchors.len() > MAX_APPLICATION_PAGE_SIZE as usize {
            return Err(RetrievalPortError::Contract(
                "graph seed count exceeds the application page bound".to_owned(),
            ));
        }
        let mut seed_occurrences = BTreeSet::new();
        let mut seed_symbols = BTreeSet::new();
        for seed in &self.seed_anchors {
            if seed.occurrence.generation != self.generation {
                return Err(RetrievalPortError::GenerationMismatch);
            }
            let symbol = seed.occurrence.symbol.as_ref().ok_or_else(|| {
                RetrievalPortError::Contract(
                    "graph seed anchors require a symbol occurrence".to_owned(),
                )
            })?;
            if !seed_occurrences.insert(&seed.source_occurrence) || !seed_symbols.insert(symbol) {
                return Err(RetrievalPortError::Contract(
                    "graph seed anchors must be unique".to_owned(),
                ));
            }
        }
        if self.edge_kinds.is_empty() {
            return Err(RetrievalPortError::Contract(
                "graph retrieval requires at least one edge kind".to_owned(),
            ));
        }
        let mut edge_kinds = BTreeSet::new();
        if self
            .edge_kinds
            .iter()
            .any(|edge_kind| !edge_kinds.insert(*edge_kind))
        {
            return Err(RetrievalPortError::Contract(
                "graph edge kinds must be unique".to_owned(),
            ));
        }
        Ok(())
    }
}

/// One ordered segment from the frozen generation's canonical relation
/// evidence. Occurrence IDs preserve path identity without creating a second
/// graph corpus in the retrieval layer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPathSegmentV1 {
    pub from: SymbolOccurrenceId,
    pub to: SymbolOccurrenceId,
    pub edge_kind: RelationEdgeKindV1,
    pub authority: EdgeAuthorityV1,
    pub evidence_span: SourceSpan,
}

/// Per-occurrence graph-lane evidence: ordered path segments plus the path's
/// weakest edge authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphLaneEvidence {
    pub binding: CodeCandidateBindingV1,
    pub path: Vec<GraphPathSegmentV1>,
    pub weakest_authority: EdgeAuthorityV1,
}

impl LaneBoundEvidence for GraphLaneEvidence {
    fn binding(&self) -> &CodeCandidateBindingV1 {
        &self.binding
    }
}

impl GraphLaneEvidence {
    /// Return the ordered symbol-occurrence IDs represented by this path.
    /// The first ID is the seed and each following ID is one traversed edge's
    /// destination.
    pub fn ordered_path_ids(&self) -> Result<Vec<&SymbolOccurrenceId>, RetrievalPortError> {
        let first = self.path.first().ok_or_else(|| {
            RetrievalPortError::Contract("graph evidence requires a non-empty path".to_owned())
        })?;
        let mut ids = Vec::with_capacity(self.path.len() + 1);
        ids.push(&first.from);
        for segment in &self.path {
            if ids.last().copied() != Some(&segment.from) {
                return Err(RetrievalPortError::Contract(
                    "graph path occurrence IDs must be contiguous and ordered".to_owned(),
                ));
            }
            ids.push(&segment.to);
        }
        Ok(ids)
    }

    pub fn validate(&self, request: &GraphLaneRequest) -> Result<(), RetrievalPortError> {
        request.validate()?;
        self.validate_against_validated_request(request)
    }

    /// Same rejection set as [`Self::validate`], minus the request
    /// revalidation the caller has already performed.
    ///
    /// The lane validates the request once per retrieval; re-running it for
    /// every candidate in the batch is pure hot-path cost.
    fn validate_against_validated_request(
        &self,
        request: &GraphLaneRequest,
    ) -> Result<(), RetrievalPortError> {
        if self.binding.occurrence.generation != request.generation {
            return Err(RetrievalPortError::GenerationMismatch);
        }
        if self.path.len() > request.max_depth as usize {
            return Err(RetrievalPortError::Contract(
                "graph evidence exceeds the request traversal depth".to_owned(),
            ));
        }
        let path_ids = self.ordered_path_ids()?;
        let starts_at_seed = request
            .seed_anchors
            .iter()
            .any(|seed| seed.occurrence.symbol.as_ref() == path_ids.first().copied());
        if !starts_at_seed {
            return Err(RetrievalPortError::Contract(
                "graph evidence path does not start at a request seed".to_owned(),
            ));
        }
        let target = self.binding.occurrence.symbol.as_ref().ok_or_else(|| {
            RetrievalPortError::Contract(
                "graph candidate bindings require a symbol occurrence".to_owned(),
            )
        })?;
        if path_ids.last().copied() != Some(target) {
            return Err(RetrievalPortError::Contract(
                "graph evidence path does not end at its candidate binding".to_owned(),
            ));
        }
        let mut weakest = self.path[0].authority;
        for segment in &self.path {
            if !request.edge_kinds.contains(&segment.edge_kind) {
                return Err(RetrievalPortError::Contract(
                    "graph evidence uses an edge kind outside the request".to_owned(),
                ));
            }
            weakest = weakest.weakest(segment.authority);
        }
        if weakest != self.weakest_authority {
            return Err(RetrievalPortError::Contract(
                "graph evidence does not preserve its weakest edge authority".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Graph-lane retriever contract over generation-matched evidence.
pub trait GraphLaneRetriever {
    /// Retrieve the committed graph candidate prefix for `request`.
    fn retrieve_graph(
        &self,
        request: &GraphLaneRequest,
        control: Arc<dyn GraphExecutionControl>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>, RetrievalPortError>;
}

/// Adapter over one read-only, generation-bound graph evidence port.
/// It validates path identity and authority, commits a deterministic compact
/// candidate prefix, and never owns graph traversal storage.
#[derive(Clone, Debug)]
pub struct GraphLane<P> {
    evidence: P,
}

impl<P> GraphLane<P> {
    pub fn new(evidence: P) -> Self {
        Self { evidence }
    }
}

impl<P> GraphLane<P>
where
    P: GraphEvidenceReadPort,
{
    fn enforce_batch(
        &self,
        request: &GraphLaneRequest,
        batch: &RetrieverBatch<GraphLaneEvidence>,
        control: &dyn GraphExecutionControl,
    ) -> Result<RetrieverBatch<GraphLaneEvidence>, RetrievalPortError> {
        if batch.coverage.eligible < batch.candidates.len() as u64 {
            return Err(RetrievalPortError::Contract(
                "graph coverage cannot report fewer eligible candidates than it emitted".to_owned(),
            ));
        }
        let source_exhausted = batch
            .continuation
            .as_ref()
            .is_none_or(|continuation| continuation.exhausted);
        let cap = lane_candidate_cap(&request.budget, &request.base.budget);
        let mut admitted: Vec<(CompactCandidate, GraphLaneEvidence)> =
            Vec::with_capacity(batch.candidates.len().min(cap));
        for candidate in &batch.candidates {
            check_graph_control(request, control)?;
            let evidence =
                lane_bound_evidence(batch, candidate, RetrieverKind::Graph, &GRAPH_REJECTIONS)?;
            evidence.validate_against_validated_request(request)?;
            let next = (candidate.clone(), evidence.clone());
            if admitted.len() < cap {
                admitted.push(next);
            } else if let Some(worst) = admitted
                .iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| compare_graph_candidates(left, right))
                .map(|(index, _)| index)
                && compare_graph_candidates(&next, &admitted[worst]).is_lt()
            {
                admitted[worst] = next;
            }
        }
        check_graph_control(request, control)?;
        admitted.sort_by(compare_graph_candidates);
        check_graph_control(request, control)?;
        let truncated = admitted.len().saturating_sub(cap);
        let truncated = batch.candidates.len().saturating_sub(admitted.len()) + truncated;
        let mut candidates = Vec::with_capacity(admitted.len());
        let mut evidence_by_occurrence = BTreeMap::new();
        for (ordinal, (mut candidate, evidence)) in admitted.into_iter().enumerate() {
            check_graph_control(request, control)?;
            candidate.ordinal_rank = ordinal as u32;
            evidence_by_occurrence.insert(candidate.source_occurrence_id.clone(), evidence);
            candidates.push(candidate);
        }
        let checkpoint_digest =
            graph_checkpoint_digest(request, &candidates, &evidence_by_occurrence, control)?;
        let mut coverage = batch.coverage;
        coverage.capped = coverage
            .capped
            .checked_add(truncated as u64)
            .ok_or_else(|| {
                RetrievalPortError::Contract("graph coverage count overflowed".to_owned())
            })?;
        let rebuilt = RetrieverBatch {
            candidates,
            evidence_by_occurrence,
            coverage,
            continuation: Some(RetrieverContinuation {
                lane: RetrieverKind::Graph,
                checkpoint_digest,
                exhausted: source_exhausted && truncated == 0,
            }),
        };
        rebuilt.validate().map_err(contract_error)?;
        check_graph_control(request, control)?;
        Ok(rebuilt)
    }
}

impl<P> GraphLaneRetriever for GraphLane<P>
where
    P: GraphEvidenceReadPort,
{
    fn retrieve_graph(
        &self,
        request: &GraphLaneRequest,
        control: Arc<dyn GraphExecutionControl>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>, RetrievalPortError> {
        request.validate()?;
        let outcome = match self
            .evidence
            .read_graph_evidence(request, Arc::clone(&control))
        {
            Ok(outcome) => outcome,
            Err(RetrievalPortError::AuthorityUnavailable(detail)) => {
                return Ok(RetrieverOutcome::Unavailable(
                    RetrievalFailure::AuthorityUnavailable { detail },
                ));
            }
            Err(RetrievalPortError::Cancelled) => return Ok(RetrieverOutcome::Cancelled),
            Err(error) => return Err(error),
        };
        match outcome {
            RetrieverOutcome::Complete(batch) => Ok(RetrieverOutcome::Complete(
                self.enforce_batch(request, &batch, control.as_ref())?,
            )),
            RetrieverOutcome::Partial { value, reason } => Ok(RetrieverOutcome::Partial {
                value: self.enforce_batch(request, &value, control.as_ref())?,
                reason,
            }),
            outcome => Ok(outcome),
        }
    }
}

fn compare_graph_candidates(
    left: &(CompactCandidate, GraphLaneEvidence),
    right: &(CompactCandidate, GraphLaneEvidence),
) -> std::cmp::Ordering {
    right
        .0
        .raw_score
        .cmp(&left.0.raw_score)
        .then_with(|| {
            left.0
                .source_occurrence_id
                .cmp(&right.0.source_occurrence_id)
        })
        .then_with(|| {
            left.0
                .retriever_evidence_anchor
                .cmp(&right.0.retriever_evidence_anchor)
        })
}

fn check_graph_control(
    request: &GraphLaneRequest,
    control: &dyn GraphExecutionControl,
) -> Result<(), RetrievalPortError> {
    if control.is_cancelled() {
        return Err(RetrievalPortError::Cancelled);
    }
    if request
        .budget
        .deadline_micros
        .is_some_and(|deadline| control.elapsed_micros() >= deadline)
    {
        return Err(RetrievalPortError::BudgetExceeded);
    }
    Ok(())
}

fn graph_checkpoint_digest(
    request: &GraphLaneRequest,
    candidates: &[CompactCandidate],
    evidence_by_occurrence: &BTreeMap<SourceOccurrenceId, GraphLaneEvidence>,
    control: &dyn GraphExecutionControl,
) -> Result<CursorPayloadDigest, RetrievalPortError> {
    let mut prefix = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        check_graph_control(request, control)?;
        let evidence = evidence_by_occurrence
            .get(&candidate.source_occurrence_id)
            .ok_or_else(|| {
                RetrievalPortError::Contract(
                    "graph checkpoint evidence is missing for a returned occurrence".to_owned(),
                )
            })?;
        let path_ids: Vec<String> = evidence
            .ordered_path_ids()?
            .into_iter()
            .map(|path_id| path_id.as_str().to_owned())
            .collect();
        let edge_kinds: Vec<RelationEdgeKindV1> = evidence
            .path
            .iter()
            .map(|segment| segment.edge_kind)
            .collect();
        let authorities: Vec<EdgeAuthorityV1> = evidence
            .path
            .iter()
            .map(|segment| segment.authority)
            .collect();
        prefix.push((
            candidate.source_occurrence_id.as_str().to_owned(),
            candidate.retriever_evidence_anchor.as_str().to_owned(),
            candidate.raw_score.micros(),
            path_ids,
            edge_kinds,
            authorities,
            evidence.weakest_authority,
        ));
    }
    checkpoint_digest(&(
        "tracedecay.retrieval-lane-checkpoint.v1",
        RetrieverKind::Graph.as_str(),
        request.generation.as_str(),
        prefix,
    ))
}

#[cfg(test)]
mod tests;

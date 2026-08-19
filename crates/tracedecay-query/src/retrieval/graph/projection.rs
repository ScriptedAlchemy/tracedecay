//! Query-owned translation over one frozen code-index graph reader.

use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_code_index::graph_projection::{CodeGraphEvidenceReader, CodeGraphProjectionError};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CompactCandidate, ComponentRevision, EvidenceRole, FixedPointScore,
    FreshnessCompatibilityV1, LogicalEvidenceId, RetrievalAnchorId, RetrieverBatch,
    RetrieverCoverage, RetrieverKind, RetrieverOutcome, ScoreDomainId, SourceFreshness,
    SourceOccurrenceId, UtcMicros,
};

use super::{GraphExecutionControl, GraphLaneEvidence, GraphLaneRequest, GraphPathSegmentV1};
use crate::retrieval::ports::{
    CodeCandidateBindingV1, CodeOccurrenceRefV1, GraphEvidenceReadPort, RetrievalPortError,
    contract_error,
};

impl GraphEvidenceReadPort for CodeGraphEvidenceReader {
    fn read_graph_evidence(
        &self,
        request: &GraphLaneRequest,
        control: Arc<dyn GraphExecutionControl>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>, RetrievalPortError> {
        read_graph_evidence(self, request, control)
    }
}

fn read_graph_evidence(
    reader: &CodeGraphEvidenceReader,
    request: &GraphLaneRequest,
    control: Arc<dyn GraphExecutionControl>,
) -> Result<RetrieverOutcome<RetrieverBatch<GraphLaneEvidence>>, RetrievalPortError> {
    request.validate()?;
    check_request_control(request, control.as_ref())?;
    if request.generation != *reader.generation() {
        return Err(RetrievalPortError::GenerationMismatch);
    }
    if reader.freshness().compatibility != FreshnessCompatibilityV1::Current {
        return Ok(RetrieverOutcome::Stale(reader.freshness().clone()));
    }
    let seed_symbols = request
        .seed_anchors
        .iter()
        .map(|seed| {
            seed.occurrence.symbol.clone().ok_or_else(|| {
                RetrievalPortError::Contract(
                    "graph seed anchors require a symbol occurrence".to_owned(),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation> =
        Arc::new(GraphTraversalCancellationV1 {
            control: Arc::clone(&control),
            deadline_micros: request.budget.deadline_micros,
        });
    let raw = reader
        .traverse(
            &request.generation,
            &seed_symbols,
            &request.edge_kinds,
            request.max_depth,
            cancellation,
        )
        .map_err(|error| {
            if matches!(error, CodeGraphProjectionError::Cancelled) {
                check_request_control(request, control.as_ref())
                    .err()
                    .unwrap_or(RetrievalPortError::Cancelled)
            } else {
                map_projection_error(error)
            }
        })?;
    check_request_control(request, control.as_ref())?;
    let retriever_revision =
        ComponentRevision::new(crate::retrieval::QUERY_GRAPH_RETRIEVER_REVISION_V1)
            .map_err(contract_error)?;
    let score_domain = ScoreDomainId::new(crate::retrieval::QUERY_GRAPH_SCORE_DOMAIN_V1)
        .map_err(contract_error)?;
    let cap = super::lane_candidate_cap(&request.budget, &request.base.budget);
    let raw_candidate_count = raw.candidates.len();
    let mut candidates = Vec::with_capacity(raw_candidate_count.min(cap));
    let mut evidence_by_occurrence = BTreeMap::new();
    for (ordinal, raw_candidate) in raw.candidates.into_iter().take(cap).enumerate() {
        check_request_control(request, control.as_ref())?;
        let target = raw_candidate.target;
        let occurrence = format!("code-graph:{}", target.as_str());
        let evidence_id = format!("code-symbol:{}", target.as_str());
        let source_occurrence_id =
            SourceOccurrenceId::new(occurrence.clone()).map_err(contract_error)?;
        let candidate = CompactCandidate {
            anchor_id: retrieval_anchor(evidence_id.clone())?,
            logical_evidence_id: LogicalEvidenceId::new(evidence_id).map_err(contract_error)?,
            source_occurrence_id: source_occurrence_id.clone(),
            file_occurrence_id: Some(raw_candidate.binding.file.clone()),
            source_namespace: reader.freshness().source_namespace.clone(),
            repository_id: reader.repository_id().cloned(),
            session_or_thread_id: None,
            logical_copy_cluster_id: None,
            logical_copy_evidence_anchor: None,
            evidence_role: EvidenceRole::Primary,
            retriever: RetrieverKind::Graph,
            retriever_revision: retriever_revision.clone(),
            score_domain: score_domain.clone(),
            raw_score: FixedPointScore(raw_candidate.score_micros),
            ordinal_rank: ordinal as u32,
            exact_admission_proof: None,
            retriever_evidence_anchor: retrieval_anchor(format!("evidence.{occurrence}"))?,
            freshness: reader.freshness().clone(),
        };
        let evidence = GraphLaneEvidence {
            binding: CodeCandidateBindingV1 {
                candidate_anchor: candidate.anchor_id.clone(),
                occurrence: CodeOccurrenceRefV1 {
                    generation: reader.generation().clone(),
                    file: raw_candidate.binding.file,
                    symbol: Some(target),
                    chunk: raw_candidate.binding.chunk,
                },
                language_descriptor_revision: raw_candidate.binding.language_descriptor_revision,
                matched_term_kinds: Vec::new(),
                source_occurrence: source_occurrence_id.clone(),
            },
            path: raw_candidate
                .path
                .into_iter()
                .map(graph_path_segment)
                .collect(),
            weakest_authority: raw_candidate.weakest_authority,
        };
        evidence_by_occurrence.insert(source_occurrence_id, evidence);
        candidates.push(candidate);
    }
    let batch = RetrieverBatch {
        candidates,
        evidence_by_occurrence,
        coverage: RetrieverCoverage {
            examined: raw.coverage.examined,
            eligible: raw.coverage.eligible,
            excluded: raw.coverage.excluded,
            capped: raw_candidate_count.saturating_sub(cap) as u64,
            unknown: raw.coverage.unknown,
        },
        continuation: None,
    };
    check_request_control(request, control.as_ref())?;
    Ok(RetrieverOutcome::Complete(batch))
}

struct GraphTraversalCancellationV1 {
    control: Arc<dyn GraphExecutionControl>,
    deadline_micros: Option<u64>,
}

impl tracedecay_graph_db::GraphCancellation for GraphTraversalCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.control.is_cancelled()
            || self
                .deadline_micros
                .is_some_and(|deadline| self.control.elapsed_micros() >= deadline)
    }
}

fn check_request_control(
    request: &GraphLaneRequest,
    control: &dyn GraphExecutionControl,
) -> Result<(), RetrievalPortError> {
    if control.is_cancelled() {
        Err(RetrievalPortError::Cancelled)
    } else if request
        .budget
        .deadline_micros
        .is_some_and(|deadline| control.elapsed_micros() >= deadline)
    {
        Err(RetrievalPortError::BudgetExceeded)
    } else {
        Ok(())
    }
}

fn graph_path_segment(edge: CanonicalRelationEdgeV1) -> GraphPathSegmentV1 {
    GraphPathSegmentV1 {
        from: edge.from_occurrence,
        to: edge.to_occurrence,
        edge_kind: edge.kind,
        authority: edge.authority,
        evidence_span: edge.evidence_span,
    }
}

fn map_projection_error(error: CodeGraphProjectionError) -> RetrievalPortError {
    match error {
        CodeGraphProjectionError::Contract(message) => RetrievalPortError::Contract(message),
        CodeGraphProjectionError::GenerationMismatch => RetrievalPortError::GenerationMismatch,
        CodeGraphProjectionError::Cancelled => RetrievalPortError::Cancelled,
        CodeGraphProjectionError::BudgetExhausted { .. }
        | CodeGraphProjectionError::DeadlineExceeded => RetrievalPortError::BudgetExceeded,
        CodeGraphProjectionError::ProjectionMismatch { .. } => {
            RetrievalPortError::AuthorityUnavailable(
                "code graph projection is quarantined after recovery verification mismatch"
                    .to_owned(),
            )
        }
        CodeGraphProjectionError::RecoveredGenerationMismatch { .. } => {
            RetrievalPortError::AuthorityUnavailable(
                "code graph generation is quarantined after recovery verification mismatch"
                    .to_owned(),
            )
        }
        unavailable => RetrievalPortError::AuthorityUnavailable(unavailable.to_string()),
    }
}

impl From<CodeGraphProjectionError> for RetrievalPortError {
    fn from(error: CodeGraphProjectionError) -> Self {
        map_projection_error(error)
    }
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

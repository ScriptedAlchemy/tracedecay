//! Independent temporal, task/session, and diagnostic retrieval adapters.
//!
//! Owning authorities emit compact candidates and typed evidence through the
//! ports in this module. The adapters enforce one frozen authorization and
//! owner epoch, bounded work, live cancellation, and deadline checkpoints.
//! Payload hydration remains with the source authority and is deliberately
//! absent from these pre-ranking contracts.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracedecay_contracts::CancellationSignal;
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, EphemeralSanitizedQueryViewV1, FileOccurrenceId,
    ManifestDigest, ProviderId, RetrievalAnchorId, RetrievalBudgetUsage, RetrievalFailure,
    RetrievalRequest, RetrieverBatch, RetrieverKind, RetrieverOutcome, SourceOccurrenceId,
    TemporalLaneEvidenceV1,
};
use tracedecay_temporal_query::execution::ExecutionControl;
use tracedecay_temporal_query::execution::TemporalPortError;

use super::ports::{RetrievalPortError, contract_error};

pub use super::task_session::*;

/// Process-local cooperative controls inherited from daemon admission.
#[derive(Clone, Debug)]
pub struct EvidenceLaneExecutionControlV1 {
    started_at: Instant,
    deadline: Option<Instant>,
    cancellation: Option<CancellationSignal>,
    temporal: Option<ExecutionControl>,
}

impl EvidenceLaneExecutionControlV1 {
    pub fn new(deadline: Option<Instant>, cancellation: CancellationSignal) -> Self {
        Self {
            started_at: Instant::now(),
            deadline,
            cancellation: Some(cancellation),
            temporal: None,
        }
    }

    pub fn from_temporal(control: ExecutionControl) -> Self {
        Self {
            started_at: Instant::now(),
            deadline: None,
            cancellation: None,
            temporal: Some(control),
        }
    }

    fn terminal<E>(&self) -> Option<RetrieverOutcome<RetrieverBatch<E>>> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(CancellationSignal::is_cancelled)
        {
            return Some(RetrieverOutcome::Cancelled);
        }
        if let Some(error) = self
            .temporal
            .as_ref()
            .and_then(|control| control.checkpoint().err())
        {
            return Some(match error {
                TemporalPortError::Cancelled => RetrieverOutcome::Cancelled,
                TemporalPortError::DeadlineExceeded => RetrieverOutcome::TimedOut(self.usage()),
                TemporalPortError::BudgetExceeded { .. } => {
                    RetrieverOutcome::BudgetExceeded(self.usage())
                }
                _ => RetrieverOutcome::Unavailable(RetrievalFailure::AuthorityUnavailable {
                    detail: "temporal execution control rejected task/session lane work".to_owned(),
                }),
            });
        }
        let now = Instant::now();
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            return Some(RetrieverOutcome::TimedOut(RetrievalBudgetUsage {
                elapsed_micros: elapsed_micros(self.started_at, now),
                ..RetrievalBudgetUsage::default()
            }));
        }
        None
    }

    fn usage(&self) -> RetrievalBudgetUsage {
        RetrievalBudgetUsage {
            elapsed_micros: elapsed_micros(self.started_at, Instant::now()),
            ..RetrievalBudgetUsage::default()
        }
    }
}

fn elapsed_micros(started_at: Instant, now: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(started_at).as_micros()).unwrap_or(u64::MAX)
}

/// Compact diagnostic evidence bound to one immutable code generation.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticMatchReasonV1 {
    CodeExact,
    MessageExact,
    MessagePhrase,
    TokenOverlap,
}

/// Compact diagnostic evidence bound to one immutable code generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticLaneEvidenceV1 {
    pub candidate_anchor: RetrievalAnchorId,
    pub source_occurrence: SourceOccurrenceId,
    pub authorization_revision: AuthorizationRevision,
    pub generation: CodeGenerationId,
    pub provider: ProviderId,
    pub file: FileOccurrenceId,
    pub diagnostic_anchor: RetrievalAnchorId,
    pub match_reason: DiagnosticMatchReasonV1,
    pub matched_query_terms: u32,
    pub query_terms: u32,
}

/// Plan-23 request adapter. The participant epoch is the sorted session/source
/// manifest digest used by the authenticated temporal continuation.
pub struct TemporalLaneRequestV1<'a> {
    pub base: &'a RetrievalRequest,
    pub query: &'a EphemeralSanitizedQueryViewV1,
    pub participant_epoch: ManifestDigest,
    pub control: &'a EvidenceLaneExecutionControlV1,
}

impl<'a> TemporalLaneRequestV1<'a> {
    pub fn new(
        base: &'a RetrievalRequest,
        query: &'a EphemeralSanitizedQueryViewV1,
        participant_epoch: ManifestDigest,
        control: &'a EvidenceLaneExecutionControlV1,
    ) -> Self {
        Self {
            base,
            query,
            participant_epoch,
            control,
        }
    }
}

/// Plan-13 diagnostic selector over one immutable code generation.
pub struct DiagnosticLaneRequestV1<'a> {
    pub base: &'a RetrievalRequest,
    pub query: &'a EphemeralSanitizedQueryViewV1,
    pub generation: CodeGenerationId,
    pub control: &'a EvidenceLaneExecutionControlV1,
}

impl<'a> DiagnosticLaneRequestV1<'a> {
    pub fn new(
        base: &'a RetrievalRequest,
        query: &'a EphemeralSanitizedQueryViewV1,
        generation: CodeGenerationId,
        control: &'a EvidenceLaneExecutionControlV1,
    ) -> Self {
        Self {
            base,
            query,
            generation,
            control,
        }
    }
}

pub trait TemporalCandidateExportPortV1 {
    fn export_temporal_candidates(
        &self,
        request: &TemporalLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<TemporalLaneEvidenceV1>>, RetrievalPortError>;
}

pub trait DiagnosticCandidateReadPortV1 {
    fn read_diagnostic_candidates(
        &self,
        request: &DiagnosticLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<DiagnosticLaneEvidenceV1>>, RetrievalPortError>;
}

pub struct TemporalLaneRetrieverV1<'a, P: ?Sized> {
    port: &'a P,
}

impl<'a, P: TemporalCandidateExportPortV1 + ?Sized> TemporalLaneRetrieverV1<'a, P> {
    pub fn new(port: &'a P) -> Self {
        Self { port }
    }

    pub fn execute(
        &self,
        request: &TemporalLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<TemporalLaneEvidenceV1>>, RetrievalPortError> {
        execute_lane(
            RetrieverKind::Temporal,
            request.base,
            request.control,
            || self.port.export_temporal_candidates(request),
            |evidence| {
                evidence.participant_epoch == request.participant_epoch
                    && !evidence.contributions.is_empty()
                    && evidence.contributions.iter().any(|contribution| {
                        contribution.source_occurrence == evidence.source_occurrence
                            && contribution.source_id.as_deref() == Some(&evidence.source_id)
                    })
            },
        )
    }
}

pub struct DiagnosticLaneRetrieverV1<'a, P: ?Sized> {
    port: &'a P,
}

impl<'a, P: DiagnosticCandidateReadPortV1 + ?Sized> DiagnosticLaneRetrieverV1<'a, P> {
    pub fn new(port: &'a P) -> Self {
        Self { port }
    }

    pub fn execute(
        &self,
        request: &DiagnosticLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<DiagnosticLaneEvidenceV1>>, RetrievalPortError>
    {
        execute_lane(
            RetrieverKind::Diagnostic,
            request.base,
            request.control,
            || self.port.read_diagnostic_candidates(request),
            |evidence| {
                evidence.generation == request.generation
                    && evidence.query_terms > 0
                    && evidence.matched_query_terms > 0
                    && evidence.matched_query_terms <= evidence.query_terms
            },
        )
    }
}

pub(super) trait LaneEvidenceBinding {
    fn candidate_anchor(&self) -> &RetrievalAnchorId;
    fn source_occurrence(&self) -> &SourceOccurrenceId;
    fn authorization_revision(&self) -> &AuthorizationRevision;
    fn source_anchor(&self) -> &RetrievalAnchorId;
}

impl LaneEvidenceBinding for TemporalLaneEvidenceV1 {
    fn candidate_anchor(&self) -> &RetrievalAnchorId {
        &self.candidate_anchor
    }

    fn source_occurrence(&self) -> &SourceOccurrenceId {
        &self.source_occurrence
    }

    fn authorization_revision(&self) -> &AuthorizationRevision {
        &self.authorization_revision
    }

    fn source_anchor(&self) -> &RetrievalAnchorId {
        &self.hydration_anchor
    }
}

impl LaneEvidenceBinding for DiagnosticLaneEvidenceV1 {
    fn candidate_anchor(&self) -> &RetrievalAnchorId {
        &self.candidate_anchor
    }

    fn source_occurrence(&self) -> &SourceOccurrenceId {
        &self.source_occurrence
    }

    fn authorization_revision(&self) -> &AuthorizationRevision {
        &self.authorization_revision
    }

    fn source_anchor(&self) -> &RetrievalAnchorId {
        &self.diagnostic_anchor
    }
}

#[tracing::instrument(name = "query.lane.evidence", level = "trace", skip_all)]
pub(super) fn execute_lane<E>(
    lane: RetrieverKind,
    request: &RetrievalRequest,
    control: &EvidenceLaneExecutionControlV1,
    read: impl FnOnce() -> Result<RetrieverOutcome<RetrieverBatch<E>>, RetrievalPortError>,
    evidence_binding_matches: impl Fn(&E) -> bool,
) -> Result<RetrieverOutcome<RetrieverBatch<E>>, RetrievalPortError>
where
    E: LaneEvidenceBinding,
{
    if let Some(terminal) = control.terminal() {
        return Ok(terminal);
    }
    let outcome = read()?;
    if let Some(terminal) = control.terminal() {
        return Ok(terminal);
    }
    validate_lane_outcome(lane, request, &outcome, evidence_binding_matches)?;
    crate::observe::record_lane("query.lane.evidence.residency", &outcome);
    Ok(outcome)
}

fn validate_lane_outcome<E>(
    lane: RetrieverKind,
    request: &RetrievalRequest,
    outcome: &RetrieverOutcome<RetrieverBatch<E>>,
    evidence_binding_matches: impl Fn(&E) -> bool,
) -> Result<(), RetrievalPortError>
where
    E: LaneEvidenceBinding,
{
    let batch = match outcome {
        RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } => batch,
        RetrieverOutcome::Unavailable(_)
        | RetrieverOutcome::Denied
        | RetrieverOutcome::Stale(_)
        | RetrieverOutcome::BudgetExceeded(_)
        | RetrieverOutcome::TimedOut(_)
        | RetrieverOutcome::Cancelled => return Ok(()),
    };
    batch.validate().map_err(contract_error)?;
    if batch.candidates.len() > request.budget.max_candidates_per_lane as usize {
        return Err(RetrievalPortError::Contract(
            "evidence lane exceeded the frozen candidate budget".to_owned(),
        ));
    }
    for candidate in &batch.candidates {
        if candidate.retriever != lane {
            return Err(RetrievalPortError::Contract(
                "evidence lane returned a foreign retriever candidate".to_owned(),
            ));
        }
        let evidence = batch
            .evidence_by_occurrence
            .get(&candidate.source_occurrence_id)
            .ok_or_else(|| {
                RetrievalPortError::Contract("evidence lane omitted occurrence evidence".to_owned())
            })?;
        if evidence.candidate_anchor() != &candidate.anchor_id
            || evidence.source_occurrence() != &candidate.source_occurrence_id
            || evidence.authorization_revision() != &request.snapshot.authorization_revision
            || evidence.source_anchor() != &candidate.retriever_evidence_anchor
            || !evidence_binding_matches(evidence)
        {
            return Err(RetrievalPortError::Contract(
                "evidence lane binding does not match the frozen request".to_owned(),
            ));
        }
    }
    Ok(())
}

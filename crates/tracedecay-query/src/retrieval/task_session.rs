//! Exact Work-to-Plan-23 TaskSession retrieval lane.
//!
//! Work owns task/graph/attempt admission. This module joins that sealed
//! binding to one canonical temporal compact export, then exposes only compact
//! candidates for shared fusion and rank-final selected-anchor hydration.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tracedecay_application::VerifiedWorkGraphVersionV1;
use tracedecay_domain::{
    ComponentRevision, CursorPayloadDigest, EphemeralSanitizedQueryViewV1, ManifestDigest,
    ObservationSourceIdentityV1, RankedCandidate, RetrievalAnchorId, RetrievalBudgetUsage,
    RetrievalCursor, RetrievalRequest, RetrieverBatch, RetrieverContinuation, RetrieverKind,
    RetrieverOutcome, ScoreDomainId, SourceOccurrenceId, TaskId, WorkAttemptIdentityV1,
    canonical_sha256,
};
use tracedecay_temporal_query::TemporalCandidateExport;
use tracedecay_temporal_query::ports::{TemporalExecutionSnapshot, TemporalRetrievalScope};

use super::evidence_lanes::{
    EvidenceLaneExecutionControlV1, LaneEvidenceBinding, TemporalLaneEvidenceV1, execute_lane,
};
use super::ports::{RetrievalPortError, contract_error};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TaskSessionBindingErrorV1 {
    #[error("the selected Work attempt belongs to a foreign task")]
    ForeignTask,
    #[error("the selected Work attempt is not accepted by the verified task graph")]
    AttemptNotAccepted,
    #[error("the selected observation source identity is not sealed")]
    InvalidObservationSource,
}

/// Exact Work-to-session join admitted by the Work graph owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSessionBindingV1 {
    task_id: TaskId,
    verified_version: VerifiedWorkGraphVersionV1,
    accepted_attempt: WorkAttemptIdentityV1,
    source: ObservationSourceIdentityV1,
}

impl TaskSessionBindingV1 {
    pub fn new(
        task_id: TaskId,
        verified_version: VerifiedWorkGraphVersionV1,
        accepted_attempts: &BTreeSet<WorkAttemptIdentityV1>,
        accepted_attempt: WorkAttemptIdentityV1,
        source: ObservationSourceIdentityV1,
    ) -> Result<Self, TaskSessionBindingErrorV1> {
        if accepted_attempt.task_id() != &task_id {
            return Err(TaskSessionBindingErrorV1::ForeignTask);
        }
        if !accepted_attempts.contains(&accepted_attempt) {
            return Err(TaskSessionBindingErrorV1::AttemptNotAccepted);
        }
        source
            .validate()
            .map_err(|_| TaskSessionBindingErrorV1::InvalidObservationSource)?;
        Ok(Self {
            task_id,
            verified_version,
            accepted_attempt,
            source,
        })
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub fn verified_version(&self) -> &VerifiedWorkGraphVersionV1 {
        &self.verified_version
    }

    pub fn accepted_attempt(&self) -> &WorkAttemptIdentityV1 {
        &self.accepted_attempt
    }

    pub fn source(&self) -> &ObservationSourceIdentityV1 {
        &self.source
    }
}

/// Plan-23 identity retained across compact export, global selection, and
/// rank-final hydration. Payload bytes are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSessionPlan23BindingV1 {
    snapshot: TemporalExecutionSnapshot,
    participant_epoch: ManifestDigest,
    continuation: Option<String>,
}

impl TaskSessionPlan23BindingV1 {
    pub fn from_export(export: &TemporalCandidateExport) -> Result<Self, RetrievalPortError> {
        Self::new(
            export.snapshot().clone(),
            export.next_cursor().map(ToOwned::to_owned),
        )
    }

    pub fn new(
        snapshot: TemporalExecutionSnapshot,
        continuation: Option<String>,
    ) -> Result<Self, RetrievalPortError> {
        if !snapshot.has_authoritative_participant_manifest() {
            return Err(RetrievalPortError::Contract(
                "task/session export lacks an authoritative participant manifest".to_owned(),
            ));
        }
        let participant_epoch =
            ManifestDigest::new(snapshot.participant_manifest().epoch_digest().to_owned())
                .map_err(contract_error)?;
        Ok(Self {
            snapshot,
            participant_epoch,
            continuation,
        })
    }

    pub fn snapshot(&self) -> &TemporalExecutionSnapshot {
        &self.snapshot
    }

    pub fn participant_epoch(&self) -> &ManifestDigest {
        &self.participant_epoch
    }

    pub fn continuation(&self) -> Option<&str> {
        self.continuation.as_deref()
    }

    pub fn matches(&self, binding: &TaskSessionBindingV1) -> bool {
        matches!(
            self.snapshot.retrieval_scope(),
            TemporalRetrievalScope::Session(session_id)
                if session_id == binding.source().session_id()
        ) && self.snapshot.provider_scope() == Some(binding.source().provider().as_str())
            && self
                .snapshot
                .participant_manifest()
                .entries()
                .iter()
                .all(|participant| {
                    participant.session_id() == binding.source().session_id()
                        && participant.source_id() == binding.source().provider().as_str()
                        && participant.is_authorized_for_snapshot()
                })
            && self.snapshot.participant_manifest().epoch_digest()
                == self.participant_epoch.as_str()
    }
}

/// Compact per-occurrence evidence for the exact Work-to-session join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSessionLaneEvidenceV1 {
    pub task_id: TaskId,
    pub graph_version: tracedecay_domain::WorkGraphVersionV1,
    pub graph_digest: ManifestDigest,
    pub accepted_attempt: WorkAttemptIdentityV1,
    pub source: ObservationSourceIdentityV1,
    pub plan23: TaskSessionPlan23BindingV1,
    pub temporal: TemporalLaneEvidenceV1,
}

/// One exact TaskSession lane request. Both authority bindings remain borrowed.
pub struct TaskSessionLaneRequestV1<'a> {
    pub base: &'a RetrievalRequest,
    pub query: &'a EphemeralSanitizedQueryViewV1,
    pub binding: &'a TaskSessionBindingV1,
    pub plan23: &'a TaskSessionPlan23BindingV1,
    pub control: &'a EvidenceLaneExecutionControlV1,
}

impl<'a> TaskSessionLaneRequestV1<'a> {
    pub fn new(
        base: &'a RetrievalRequest,
        query: &'a EphemeralSanitizedQueryViewV1,
        binding: &'a TaskSessionBindingV1,
        plan23: &'a TaskSessionPlan23BindingV1,
        control: &'a EvidenceLaneExecutionControlV1,
    ) -> Self {
        Self {
            base,
            query,
            binding,
            plan23,
            control,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TaskSessionCandidateSelectionErrorV1 {
    #[error("global selection contains a candidate without TaskSession evidence")]
    ForeignLane,
    #[error("global selection repeats a TaskSession hydration anchor")]
    DuplicateAnchor,
}

/// Globally ranked TaskSession slice allowed across rank-before-hydrate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSessionCandidateSelectionV1 {
    ranked_candidates: Vec<RankedCandidate>,
    selected_anchors: Vec<RetrievalAnchorId>,
    continuation: Option<RetrievalCursor>,
}

impl TaskSessionCandidateSelectionV1 {
    pub fn new(
        ranked_candidates: Vec<RankedCandidate>,
        continuation: Option<RetrievalCursor>,
    ) -> Result<Self, TaskSessionCandidateSelectionErrorV1> {
        let mut selected_anchors = Vec::with_capacity(ranked_candidates.len());
        let mut unique = BTreeSet::new();
        for ranked in &ranked_candidates {
            if !ranked
                .candidate
                .contributions
                .iter()
                .any(|contribution| contribution.retriever == RetrieverKind::TaskSession)
            {
                return Err(TaskSessionCandidateSelectionErrorV1::ForeignLane);
            }
            if !unique.insert(ranked.candidate.anchor_id.clone()) {
                return Err(TaskSessionCandidateSelectionErrorV1::DuplicateAnchor);
            }
            selected_anchors.push(ranked.candidate.anchor_id.clone());
        }
        Ok(Self {
            ranked_candidates,
            selected_anchors,
            continuation,
        })
    }

    pub fn ranked_candidates(&self) -> &[RankedCandidate] {
        &self.ranked_candidates
    }

    pub fn selected_anchors(&self) -> &[RetrievalAnchorId] {
        &self.selected_anchors
    }

    pub fn continuation(&self) -> Option<&RetrievalCursor> {
        self.continuation.as_ref()
    }
}

pub trait TaskSessionCandidateExportPortV1 {
    fn export_task_session_candidates(
        &self,
        request: &TaskSessionLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<TaskSessionLaneEvidenceV1>>, RetrievalPortError>;
}

/// Production Work-to-Plan-23 join over one borrowed temporal export.
pub struct CanonicalTaskSessionCandidateExportPortV1<'a> {
    export: &'a TemporalCandidateExport,
    retriever_revision: ComponentRevision,
    score_domain: ScoreDomainId,
    policy_revision: ComponentRevision,
}

impl<'a> CanonicalTaskSessionCandidateExportPortV1<'a> {
    pub fn new(
        export: &'a TemporalCandidateExport,
        retriever_revision: ComponentRevision,
        score_domain: ScoreDomainId,
        policy_revision: ComponentRevision,
    ) -> Self {
        Self {
            export,
            retriever_revision,
            score_domain,
            policy_revision,
        }
    }
}

impl TaskSessionCandidateExportPortV1 for CanonicalTaskSessionCandidateExportPortV1<'_> {
    fn export_task_session_candidates(
        &self,
        request: &TaskSessionLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<TaskSessionLaneEvidenceV1>>, RetrievalPortError>
    {
        if self.export.snapshot() != request.plan23.snapshot()
            || self.export.next_cursor() != request.plan23.continuation()
            || !request.plan23.matches(request.binding)
        {
            return Ok(RetrieverOutcome::Denied);
        }
        let temporal = self
            .export
            .to_retriever_batch(
                request.base,
                self.retriever_revision.clone(),
                self.score_domain.clone(),
                self.policy_revision.clone(),
            )
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        if temporal.candidates.len() > request.base.budget.max_candidates_per_lane as usize {
            let candidates_returned = u64::try_from(temporal.candidates.len()).map_err(|_| {
                RetrievalPortError::Contract(
                    "task/session candidate count exceeds the usage counter".to_owned(),
                )
            })?;
            return Ok(RetrieverOutcome::BudgetExceeded(RetrievalBudgetUsage {
                candidates_examined: temporal.coverage.examined,
                candidates_returned,
                ..RetrievalBudgetUsage::default()
            }));
        }

        let mut candidates = temporal.candidates;
        for candidate in &mut candidates {
            candidate.retriever = RetrieverKind::TaskSession;
        }
        let evidence_by_occurrence = temporal
            .evidence_by_occurrence
            .into_iter()
            .map(|(occurrence, temporal)| {
                (
                    occurrence,
                    TaskSessionLaneEvidenceV1 {
                        task_id: request.binding.task_id().clone(),
                        graph_version: request.binding.verified_version().graph_version(),
                        graph_digest: request
                            .binding
                            .verified_version()
                            .recovered_graph_digest()
                            .clone(),
                        accepted_attempt: request.binding.accepted_attempt().clone(),
                        source: request.binding.source().clone(),
                        plan23: request.plan23.clone(),
                        temporal,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let checkpoint_digest = canonical_sha256(&(
            "tracedecay.task-session-lane-checkpoint.v1",
            request.binding.task_id(),
            request.binding.verified_version().graph_version().get(),
            request
                .binding
                .verified_version()
                .recovered_graph_digest()
                .as_str(),
            request.binding.accepted_attempt(),
            request.binding.source(),
            request.plan23.participant_epoch().as_str(),
            temporal
                .continuation
                .as_ref()
                .map(|continuation| continuation.checkpoint_digest.as_str()),
        ))
        .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        let batch = RetrieverBatch {
            candidates,
            evidence_by_occurrence,
            coverage: temporal.coverage,
            continuation: Some(RetrieverContinuation {
                lane: RetrieverKind::TaskSession,
                checkpoint_digest: CursorPayloadDigest::new(checkpoint_digest.as_str())
                    .map_err(contract_error)?,
                exhausted: temporal
                    .continuation
                    .as_ref()
                    .is_none_or(|continuation| continuation.exhausted),
            }),
        };
        batch.validate().map_err(contract_error)?;
        Ok(RetrieverOutcome::Complete(batch))
    }
}

pub struct TaskSessionLaneRetrieverV1<'a, P: ?Sized> {
    port: &'a P,
}

impl<'a, P: TaskSessionCandidateExportPortV1 + ?Sized> TaskSessionLaneRetrieverV1<'a, P> {
    pub fn new(port: &'a P) -> Self {
        Self { port }
    }

    pub fn execute(
        &self,
        request: &TaskSessionLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<TaskSessionLaneEvidenceV1>>, RetrievalPortError>
    {
        execute_lane(
            RetrieverKind::TaskSession,
            request.base,
            request.control,
            || self.port.export_task_session_candidates(request),
            |evidence| task_session_evidence_matches(evidence, request),
        )
    }
}

fn task_session_evidence_matches(
    evidence: &TaskSessionLaneEvidenceV1,
    request: &TaskSessionLaneRequestV1<'_>,
) -> bool {
    evidence.task_id == *request.binding.task_id()
        && evidence.graph_version == request.binding.verified_version().graph_version()
        && evidence.graph_digest == *request.binding.verified_version().recovered_graph_digest()
        && evidence.accepted_attempt == *request.binding.accepted_attempt()
        && evidence.source == *request.binding.source()
        && evidence.plan23 == *request.plan23
        && evidence.temporal.participant_epoch == *request.plan23.participant_epoch()
        && evidence.temporal.session_id == *request.binding.source().session_id()
        && evidence.temporal.source_id == request.binding.source().provider().as_str()
        && !evidence.temporal.contributions.is_empty()
        && evidence.temporal.contributions.iter().any(|contribution| {
            contribution.source_occurrence == evidence.temporal.source_occurrence
                && contribution.source_id.as_deref()
                    == Some(request.binding.source().provider().as_str())
        })
}

impl LaneEvidenceBinding for TaskSessionLaneEvidenceV1 {
    fn candidate_anchor(&self) -> &RetrievalAnchorId {
        &self.temporal.candidate_anchor
    }

    fn source_occurrence(&self) -> &SourceOccurrenceId {
        &self.temporal.source_occurrence
    }

    fn authorization_revision(&self) -> &tracedecay_domain::AuthorizationRevision {
        &self.temporal.authorization_revision
    }

    fn source_anchor(&self) -> &RetrievalAnchorId {
        &self.temporal.hydration_anchor
    }
}

//! Independent temporal, task/session, and diagnostic retrieval adapters.
//!
//! Owning authorities emit compact candidates and typed evidence through the
//! ports in this module. The adapters enforce one frozen authorization and
//! owner epoch, bounded work, live cancellation, and deadline checkpoints.
//! Payload hydration remains with the source authority and is deliberately
//! absent from these pre-ranking contracts.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracedecay_application::{
    CancellationSignal, DiagnosticProviderResult, DiagnosticProviderState, FreshnessState,
    ProviderSourceIdentity,
};
use tracedecay_domain::{
    AuthorizationRevision, CodeGenerationId, CompactCandidate, ComponentRevision,
    CursorPayloadDigest, EphemeralSanitizedQueryViewV1, EvidenceRole, FileOccurrenceId,
    FixedPointScore, FreshnessCompatibilityV1, GenerationDiagnosticV1, LogicalEvidenceId,
    ManifestDigest, ProviderId, RetrievalAnchorId, RetrievalBudgetUsage, RetrievalFailure,
    RetrievalRequest, RetrieverBatch, RetrieverContinuation, RetrieverCoverage, RetrieverKind,
    RetrieverOutcome, ScoreDomainId, SessionId, SourceFreshness, SourceInstanceKey,
    SourceNamespace, SourceOccurrenceId, TaskId, canonical_sha256,
};
use tracedecay_temporal_query::TemporalCandidateExport;

use super::ports::{RetrievalPortError, contract_error};

pub use tracedecay_domain::{
    TemporalCandidateChannelV1, TemporalCandidateContributionV1, TemporalLaneEvidenceV1,
};

/// Process-local cooperative controls inherited from daemon admission.
#[derive(Clone, Debug)]
pub struct EvidenceLaneExecutionControlV1 {
    started_at: Instant,
    deadline: Option<Instant>,
    cancellation: CancellationSignal,
}

impl EvidenceLaneExecutionControlV1 {
    pub fn new(deadline: Option<Instant>, cancellation: CancellationSignal) -> Self {
        Self {
            started_at: Instant::now(),
            deadline,
            cancellation,
        }
    }

    fn terminal<E>(&self) -> Option<RetrieverOutcome<RetrieverBatch<E>>> {
        if self.cancellation.is_cancelled() {
            return Some(RetrieverOutcome::Cancelled);
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

/// Task relation that selected source-owned evidence.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskEvidenceRelationV1 {
    Session,
    Message,
    Attempt,
    Review,
    Outcome,
    Artifact,
    Receipt,
    Handoff,
    Code,
    Git,
    Ci,
    Diagnostic,
}

/// Compact Plan-24 topology evidence. Owning payloads remain source-local.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskSessionLaneEvidenceV1 {
    pub candidate_anchor: RetrievalAnchorId,
    pub source_occurrence: SourceOccurrenceId,
    pub authorization_revision: AuthorizationRevision,
    pub graph_epoch: ManifestDigest,
    pub task_id: TaskId,
    pub relation: TaskEvidenceRelationV1,
    pub owning_anchor: RetrievalAnchorId,
    pub linked_session: Option<SessionId>,
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

/// Plan-24 task-rooted selector over an injected canonical graph authority.
pub struct TaskSessionLaneRequestV1<'a> {
    pub base: &'a RetrievalRequest,
    pub query: &'a EphemeralSanitizedQueryViewV1,
    pub task_id: TaskId,
    pub graph_epoch: ManifestDigest,
    pub control: &'a EvidenceLaneExecutionControlV1,
}

impl<'a> TaskSessionLaneRequestV1<'a> {
    pub fn new(
        base: &'a RetrievalRequest,
        query: &'a EphemeralSanitizedQueryViewV1,
        task_id: TaskId,
        graph_epoch: ManifestDigest,
        control: &'a EvidenceLaneExecutionControlV1,
    ) -> Self {
        Self {
            base,
            query,
            task_id,
            graph_epoch,
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

/// Production bridge from the canonical temporal kernel's frozen compact
/// export into the shared retrieval lane. The export remains borrowed so the
/// same authoritative value can later hydrate only globally selected anchors.
pub struct CanonicalTemporalCandidateExportPortV1<'a> {
    export: &'a TemporalCandidateExport,
    retriever_revision: ComponentRevision,
    score_domain: ScoreDomainId,
    policy_revision: ComponentRevision,
}

impl<'a> CanonicalTemporalCandidateExportPortV1<'a> {
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

impl TemporalCandidateExportPortV1 for CanonicalTemporalCandidateExportPortV1<'_> {
    fn export_temporal_candidates(
        &self,
        request: &TemporalLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<TemporalLaneEvidenceV1>>, RetrievalPortError> {
        let batch = self
            .export
            .to_retriever_batch(
                request.base,
                self.retriever_revision.clone(),
                self.score_domain.clone(),
                self.policy_revision.clone(),
            )
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        if batch.candidates.len() > request.base.budget.max_candidates_per_lane as usize {
            let candidates_returned = u64::try_from(batch.candidates.len()).map_err(|_| {
                RetrievalPortError::Contract(
                    "temporal candidate count exceeds the usage counter".to_owned(),
                )
            })?;
            return Ok(RetrieverOutcome::BudgetExceeded(RetrievalBudgetUsage {
                candidates_examined: batch.coverage.examined,
                candidates_returned,
                ..RetrievalBudgetUsage::default()
            }));
        }
        Ok(RetrieverOutcome::Complete(batch))
    }
}

pub trait TaskSessionCandidateReadPortV1 {
    fn read_task_session_candidates(
        &self,
        request: &TaskSessionLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<TaskSessionLaneEvidenceV1>>, RetrievalPortError>;
}

pub trait DiagnosticCandidateReadPortV1 {
    fn read_diagnostic_candidates(
        &self,
        request: &DiagnosticLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<DiagnosticLaneEvidenceV1>>, RetrievalPortError>;
}

/// Production diagnostic bridge over the canonical application provider
/// result. It never reads diagnostic tables or dashboard state directly.
pub struct CanonicalDiagnosticCandidatePortV1<'a> {
    results: &'a [DiagnosticProviderResult<Vec<GenerationDiagnosticV1>>],
    retriever_revision: ComponentRevision,
    score_domain: ScoreDomainId,
    policy_revision: ComponentRevision,
}

impl<'a> CanonicalDiagnosticCandidatePortV1<'a> {
    pub fn new(
        results: &'a [DiagnosticProviderResult<Vec<GenerationDiagnosticV1>>],
        retriever_revision: ComponentRevision,
        score_domain: ScoreDomainId,
        policy_revision: ComponentRevision,
    ) -> Self {
        Self {
            results,
            retriever_revision,
            score_domain,
            policy_revision,
        }
    }
}

impl DiagnosticCandidateReadPortV1 for CanonicalDiagnosticCandidatePortV1<'_> {
    fn read_diagnostic_candidates(
        &self,
        request: &DiagnosticLaneRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<DiagnosticLaneEvidenceV1>>, RetrievalPortError>
    {
        diagnostic_provider_outcome(
            self.results,
            request,
            &self.retriever_revision,
            &self.score_domain,
            &self.policy_revision,
        )
    }
}

struct ScoredDiagnosticV1<'a> {
    record: &'a GenerationDiagnosticV1,
    provider: ProviderId,
    freshness: SourceFreshness,
    reason: DiagnosticMatchReasonV1,
    matched_terms: u32,
    query_terms: u32,
    score: u64,
}

fn diagnostic_provider_outcome(
    results: &[DiagnosticProviderResult<Vec<GenerationDiagnosticV1>>],
    request: &DiagnosticLaneRequestV1<'_>,
    retriever_revision: &ComponentRevision,
    score_domain: &ScoreDomainId,
    policy_revision: &ComponentRevision,
) -> Result<RetrieverOutcome<RetrieverBatch<DiagnosticLaneEvidenceV1>>, RetrievalPortError> {
    let query = request.query.as_str().to_lowercase();
    let query_terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .collect::<BTreeSet<_>>();
    if query_terms.is_empty() {
        return Err(RetrievalPortError::Contract(
            "diagnostic query has no canonical terms".to_owned(),
        ));
    }
    let query_term_count = u32::try_from(query_terms.len()).map_err(|_| {
        RetrievalPortError::Contract("diagnostic query term count exceeds its contract".to_owned())
    })?;
    let mut scored = Vec::new();
    let mut examined = 0_u64;
    let mut excluded = 0_u64;
    let mut unknown = 0_u64;
    let mut provider_digests = Vec::new();
    let mut complete_provider = false;
    let mut partial_provider = false;
    let mut stale_provider = None;
    let mut cancelled = false;
    let mut timed_out = false;
    let mut unavailable = false;

    for result in results {
        result
            .validate()
            .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
        validate_diagnostic_provider_scope(result, request)?;
        provider_digests.push(
            result
                .identity
                .compute_digest()
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?
                .to_string(),
        );
        match result.state {
            DiagnosticProviderState::SupportedComplete => complete_provider = true,
            DiagnosticProviderState::Partial => partial_provider = true,
            DiagnosticProviderState::Stale => {
                stale_provider = Some(provider_freshness(
                    result,
                    policy_revision.clone(),
                    FreshnessCompatibilityV1::Stale,
                )?)
            }
            DiagnosticProviderState::Cancelled => cancelled = true,
            DiagnosticProviderState::TimedOut => timed_out = true,
            DiagnosticProviderState::Unsupported
            | DiagnosticProviderState::Absent
            | DiagnosticProviderState::Indexing
            | DiagnosticProviderState::Failed
            | DiagnosticProviderState::Unavailable => unavailable = true,
        }
        let payload = match result.state {
            DiagnosticProviderState::SupportedComplete | DiagnosticProviderState::Partial => {
                match &result.payload {
                    Some(payload) => payload.as_slice(),
                    None => &[],
                }
            }
            DiagnosticProviderState::Unsupported
            | DiagnosticProviderState::Absent
            | DiagnosticProviderState::Indexing
            | DiagnosticProviderState::Stale
            | DiagnosticProviderState::Cancelled
            | DiagnosticProviderState::TimedOut
            | DiagnosticProviderState::Failed
            | DiagnosticProviderState::Unavailable => &[],
        };
        examined = examined
            .checked_add(u64::try_from(payload.len()).map_err(|_| {
                RetrievalPortError::Contract(
                    "diagnostic payload count exceeds its coverage counter".to_owned(),
                )
            })?)
            .ok_or_else(|| {
                RetrievalPortError::Contract("diagnostic coverage counter overflowed".to_owned())
            })?;
        unknown = unknown
            .checked_add(
                result
                    .identity
                    .coverage
                    .requested
                    .saturating_sub(result.identity.coverage.returned),
            )
            .ok_or_else(|| {
                RetrievalPortError::Contract("diagnostic unknown counter overflowed".to_owned())
            })?;
        for record in payload {
            record
                .validate()
                .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
            validate_diagnostic_record_binding(record, result, request)?;
            if !record.is_current() {
                excluded = excluded.checked_add(1).ok_or_else(|| {
                    RetrievalPortError::Contract(
                        "diagnostic exclusion counter overflowed".to_owned(),
                    )
                })?;
                continue;
            }
            let Some((reason, matched_terms, score)) =
                score_diagnostic(record, &query, &query_terms)?
            else {
                excluded = excluded.checked_add(1).ok_or_else(|| {
                    RetrievalPortError::Contract(
                        "diagnostic exclusion counter overflowed".to_owned(),
                    )
                })?;
                continue;
            };
            scored.push(ScoredDiagnosticV1 {
                record,
                provider: result.identity.producer.provider.clone(),
                freshness: provider_freshness(
                    result,
                    policy_revision.clone(),
                    match result.identity.freshness.state {
                        FreshnessState::Current => FreshnessCompatibilityV1::Current,
                        FreshnessState::Stale => FreshnessCompatibilityV1::Stale,
                        FreshnessState::Unknown => FreshnessCompatibilityV1::Unknown,
                    },
                )?,
                reason,
                matched_terms,
                query_terms: query_term_count,
                score,
            });
        }
    }

    scored.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| {
                left.record
                    .diagnostic_anchor
                    .cmp(&right.record.diagnostic_anchor)
            })
            .then_with(|| left.provider.cmp(&right.provider))
    });
    let eligible = u64::try_from(scored.len()).map_err(|_| {
        RetrievalPortError::Contract("diagnostic eligible count exceeds its contract".to_owned())
    })?;
    let maximum = request.base.budget.max_candidates_per_lane as usize;
    let capped = u64::try_from(scored.len().saturating_sub(maximum)).map_err(|_| {
        RetrievalPortError::Contract("diagnostic capped count exceeds its contract".to_owned())
    })?;
    scored.truncate(maximum);
    let mut candidates = Vec::with_capacity(scored.len());
    let mut evidence_by_occurrence = BTreeMap::new();
    for (ordinal, diagnostic) in scored.into_iter().enumerate() {
        let source_occurrence = SourceOccurrenceId::try_from(format!(
            "diagnostic:{}:{}",
            diagnostic.provider, diagnostic.record.diagnostic_anchor
        ))
        .map_err(contract_error)?;
        let source_namespace =
            SourceNamespace::try_from("diagnostic".to_owned()).map_err(contract_error)?;
        let ordinal_rank = u32::try_from(ordinal).map_err(|_| {
            RetrievalPortError::Contract("diagnostic ordinal exceeds its contract".to_owned())
        })?;
        let candidate = CompactCandidate {
            anchor_id: diagnostic.record.diagnostic_anchor.clone(),
            logical_evidence_id: LogicalEvidenceId::try_from(
                diagnostic.record.message_digest.to_string(),
            )
            .map_err(contract_error)?,
            source_occurrence_id: source_occurrence.clone(),
            file_occurrence_id: Some(diagnostic.record.file_occurrence_id.clone()),
            source_namespace,
            repository_id: Some(diagnostic.record.repository.clone()),
            session_or_thread_id: None,
            logical_copy_cluster_id: None,
            logical_copy_evidence_anchor: None,
            evidence_role: EvidenceRole::Primary,
            retriever: RetrieverKind::Diagnostic,
            retriever_revision: retriever_revision.clone(),
            score_domain: score_domain.clone(),
            raw_score: FixedPointScore(diagnostic.score),
            ordinal_rank,
            exact_admission_proof: None,
            retriever_evidence_anchor: diagnostic.record.diagnostic_anchor.clone(),
            freshness: diagnostic.freshness,
        };
        let evidence = DiagnosticLaneEvidenceV1 {
            candidate_anchor: candidate.anchor_id.clone(),
            source_occurrence: source_occurrence.clone(),
            authorization_revision: request.base.snapshot.authorization_revision.clone(),
            generation: request.generation.clone(),
            provider: diagnostic.provider,
            file: diagnostic.record.file_occurrence_id.clone(),
            diagnostic_anchor: diagnostic.record.diagnostic_anchor.clone(),
            match_reason: diagnostic.reason,
            matched_query_terms: diagnostic.matched_terms,
            query_terms: diagnostic.query_terms,
        };
        candidates.push(candidate);
        evidence_by_occurrence.insert(source_occurrence, evidence);
    }
    let checkpoint = canonical_sha256(&(
        "tracedecay.diagnostic-lane-checkpoint.v1",
        &request.generation,
        &request.base.snapshot.authorization_revision,
        provider_digests,
        candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.anchor_id.to_string(),
                    candidate.source_occurrence_id.to_string(),
                    candidate.raw_score.micros(),
                )
            })
            .collect::<Vec<_>>(),
    ))
    .map_err(|error| RetrievalPortError::Contract(error.to_string()))?;
    let batch = RetrieverBatch {
        candidates,
        evidence_by_occurrence,
        coverage: RetrieverCoverage {
            examined,
            eligible,
            excluded,
            capped,
            unknown,
        },
        continuation: Some(RetrieverContinuation {
            lane: RetrieverKind::Diagnostic,
            checkpoint_digest: CursorPayloadDigest::new(checkpoint.as_str())
                .map_err(contract_error)?,
            exhausted: capped == 0,
        }),
    };
    batch.validate().map_err(contract_error)?;

    let incomplete =
        partial_provider || stale_provider.is_some() || unavailable || cancelled || timed_out;
    if incomplete && (!batch.candidates.is_empty() || complete_provider || partial_provider) {
        return Ok(RetrieverOutcome::Partial {
            value: batch,
            reason: if stale_provider.is_some() {
                RetrievalFailure::StaleSource
            } else {
                RetrievalFailure::AuthorityUnavailable {
                    detail: "one or more diagnostic providers did not complete".to_owned(),
                }
            },
        });
    }
    if cancelled {
        return Ok(RetrieverOutcome::Cancelled);
    }
    if timed_out {
        return Ok(RetrieverOutcome::TimedOut(request.control.usage()));
    }
    if let Some(freshness) = stale_provider {
        return Ok(RetrieverOutcome::Stale(freshness));
    }
    if complete_provider {
        return Ok(RetrieverOutcome::Complete(batch));
    }
    Ok(RetrieverOutcome::Unavailable(
        RetrievalFailure::AuthorityUnavailable {
            detail: "no diagnostic provider completed for the frozen generation".to_owned(),
        },
    ))
}

fn validate_diagnostic_provider_scope(
    result: &DiagnosticProviderResult<Vec<GenerationDiagnosticV1>>,
    request: &DiagnosticLaneRequestV1<'_>,
) -> Result<(), RetrievalPortError> {
    if !matches!(
        &result.identity.source,
        ProviderSourceIdentity::CleanGeneration { generation }
            if generation == &request.generation
    ) || result.identity.scope.repository_id != request.base.scope.root.repository
        || request
            .base
            .scope
            .root
            .worktree
            .as_ref()
            .is_some_and(|worktree| &result.identity.scope.worktree_id != worktree)
        || request
            .base
            .scope
            .root
            .reference
            .as_ref()
            .is_some_and(|reference| result.identity.scope.reference.as_ref() != Some(reference))
    {
        return Err(RetrievalPortError::Contract(
            "diagnostic provider is outside the frozen generation scope".to_owned(),
        ));
    }
    Ok(())
}

fn validate_diagnostic_record_binding(
    record: &GenerationDiagnosticV1,
    result: &DiagnosticProviderResult<Vec<GenerationDiagnosticV1>>,
    request: &DiagnosticLaneRequestV1<'_>,
) -> Result<(), RetrievalPortError> {
    if record.generation_id != request.generation
        || record.repository != request.base.scope.root.repository
        || request
            .base
            .scope
            .root
            .worktree
            .as_ref()
            .is_some_and(|worktree| record.worktree.as_ref() != Some(worktree))
        || request
            .base
            .scope
            .root
            .reference
            .as_ref()
            .is_some_and(|reference| record.reference.as_ref() != Some(reference))
        || record.file_occurrence_id != result.identity.document.file
        || record.content_digest != result.identity.document.content_digest
        || record.provenance.producer != result.identity.producer.provider
    {
        return Err(RetrievalPortError::Contract(
            "diagnostic record is outside its provider identity".to_owned(),
        ));
    }
    Ok(())
}

fn provider_freshness(
    result: &DiagnosticProviderResult<Vec<GenerationDiagnosticV1>>,
    policy_revision: ComponentRevision,
    compatibility: FreshnessCompatibilityV1,
) -> Result<SourceFreshness, RetrievalPortError> {
    Ok(SourceFreshness {
        source_namespace: SourceNamespace::try_from("diagnostic".to_owned())
            .map_err(contract_error)?,
        source_instance: SourceInstanceKey::try_from(result.identity.producer.provider.to_string())
            .map_err(contract_error)?,
        source_watermark: None,
        projection_watermark: None,
        observed_at: result.identity.freshness.observed_at,
        source_generation: None,
        generation_lag: None,
        compatibility,
        policy_revision,
    })
}

pub(crate) fn score_diagnostic(
    record: &GenerationDiagnosticV1,
    query: &str,
    query_terms: &BTreeSet<&str>,
) -> Result<Option<(DiagnosticMatchReasonV1, u32, u64)>, RetrievalPortError> {
    let code = record.code.to_lowercase();
    let message = record.message.to_lowercase();
    let query_term_count = u32::try_from(query_terms.len()).map_err(|_| {
        RetrievalPortError::Contract("diagnostic query term count exceeds its contract".to_owned())
    })?;
    if code == query {
        return Ok(Some((
            DiagnosticMatchReasonV1::CodeExact,
            query_term_count,
            1_000_000,
        )));
    }
    if message == query {
        return Ok(Some((
            DiagnosticMatchReasonV1::MessageExact,
            query_term_count,
            950_000,
        )));
    }
    if message.contains(query) {
        return Ok(Some((
            DiagnosticMatchReasonV1::MessagePhrase,
            query_term_count,
            900_000,
        )));
    }
    let matched = query_terms
        .iter()
        .filter(|term| code.contains(**term) || message.contains(**term))
        .count();
    if matched == 0 {
        return Ok(None);
    }
    let matched_terms = u32::try_from(matched).map_err(|_| {
        RetrievalPortError::Contract(
            "diagnostic matched term count exceeds its contract".to_owned(),
        )
    })?;
    let numerator = u64::from(matched_terms)
        .checked_mul(300_000)
        .ok_or_else(|| RetrievalPortError::Contract("diagnostic score overflowed".to_owned()))?;
    let score = 500_000_u64
        .checked_add(numerator / u64::from(query_term_count))
        .ok_or_else(|| RetrievalPortError::Contract("diagnostic score overflowed".to_owned()))?;
    Ok(Some((
        DiagnosticMatchReasonV1::TokenOverlap,
        matched_terms,
        score,
    )))
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

pub struct TaskSessionLaneRetrieverV1<'a, P: ?Sized> {
    port: &'a P,
}

impl<'a, P: TaskSessionCandidateReadPortV1 + ?Sized> TaskSessionLaneRetrieverV1<'a, P> {
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
            || self.port.read_task_session_candidates(request),
            |evidence| {
                evidence.graph_epoch == request.graph_epoch && evidence.task_id == request.task_id
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

trait LaneEvidenceBinding {
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

impl LaneEvidenceBinding for TaskSessionLaneEvidenceV1 {
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
        &self.owning_anchor
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

fn execute_lane<E>(
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

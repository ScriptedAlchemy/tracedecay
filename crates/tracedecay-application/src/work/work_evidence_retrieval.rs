//! Work evidence adapters over mounted retrieval authorities.
//!
//! Work admits the exact task/version/accepted-attempt root. The `TaskSession`
//! path then borrows one canonical session-temporal snapshot, ranks its compact
//! anchors through the active evaluated federated profile, reauthorizes Work on
//! both sides of selection, and hydrates only the globally selected anchors.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_contracts::retrieval::SessionRetrievalStructuralRefusalV1;
use tracedecay_contracts::{
    OpaqueCursor, RequestContext, ResolvedScope, WorkAnchorHydrationFuture,
    WorkAnchorHydrationPortV1, WorkAnchorHydrationRequestV1, WorkEvidenceCoverageStateV1,
    WorkEvidenceFreshnessV1, WorkEvidenceHydrationErrorV1, WorkEvidenceRetrievalPortV1,
    WorkTaskSessionContinuationV1, WorkTaskSessionCoverageV1, WorkTaskSessionEvidenceV1,
    WorkTaskSessionFuture, WorkTaskSessionHydrationStateV1, WorkTaskSessionHydrationV1,
    WorkTaskSessionPortV1, WorkTaskSessionRankContributionV1, WorkTaskSessionRankedAnchorV1,
    WorkTaskSessionReauthorizationErrorV1, WorkTaskSessionReauthorizationPortV1,
    WorkTaskSessionRequestV1,
};
use tracedecay_domain::{
    AuthorizationRevision, ComponentRevision, EphemeralSanitizedQueryViewV1, FreshnessVectorDigest,
    HydrationStateV1, PrincipalId, QueryNormalizationRevision, RetrievalCursor, RetrievalGrainV1,
    RetrievalRequest, RetrievalScope, SanitizerRevision, ScoreDomainId, SingleRootScopeV1,
    VectorWatermark,
};
use tracedecay_query::retrieval::QueryAuthorityV1;
use tracedecay_query::retrieval::evidence_lanes::{
    TaskSessionBindingV1, TaskSessionCandidateSelectionV1, TaskSessionLaneEvidenceV1,
};
use tracedecay_session_memory::session::{
    SessionDataFreshness, SessionRetrievalScope, SessionTemporalQuery,
    TaskSessionRetrievalOutcomeV1,
};
use tracedecay_session_temporal_store::execution::{
    TaskSessionExecutionOmissionReasonV1, TaskSessionRankSelectorV1,
    TaskSessionReauthorizationStageV1, TaskSessionSelectionCallbackErrorV1,
};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ports::ExecutionLimits;
use tracedecay_temporal_query::ranking::DiversityLimits;

const WORK_EVIDENCE_CONTEXT_BYTES: u64 = 64 * 1024;
const WORK_TASK_SESSION_SANITIZER_REVISION: &str = "sanitizer.work-task-session.v1";
const WORK_TASK_SESSION_NORMALIZATION_REVISION: &str = "normalization.work-task-session.v1";

pub type WorkFederatedQueryAuthorityFutureV1<'a> =
    Pin<Box<dyn Future<Output = Option<Arc<QueryAuthorityV1>>> + Send + 'a>>;

pub type WorkTaskSessionAdmittedRetrievalFutureV1<'a> =
    Pin<Box<dyn Future<Output = TaskSessionRetrievalOutcomeV1> + Send + 'a>>;

/// Resolves the currently activated evaluated authority for an exact scope.
/// Resolution occurs per request so an accepted-profile activation does not
/// leave a long-lived Work runtime bound to a superseded profile.
pub trait WorkFederatedQueryAuthorityPortV1: Send + Sync {
    fn authority_for<'a>(
        &'a self,
        scope: &'a ResolvedScope,
    ) -> WorkFederatedQueryAuthorityFutureV1<'a>;
}

/// Already-admitted TaskSession retrieval used by Work evidence.
///
/// This is the `retrieve_task_session_admitted` method of the mounted session
/// retrieval authority, inverted here so this crate does not depend on
/// session-runtime.
pub trait WorkTaskSessionAdmittedRetrievalPortV1: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn retrieve_task_session_admitted<'a>(
        &'a self,
        _context: &'a RequestContext,
        _temporal_query: SessionTemporalQuery,
        _task_binding: TaskSessionBindingV1,
        _retrieval_request: RetrievalRequest,
        _query: EphemeralSanitizedQueryViewV1,
        _retriever_revision: ComponentRevision,
        _score_domain: ScoreDomainId,
        _policy_revision: ComponentRevision,
        _selector: &'a dyn TaskSessionRankSelectorV1,
    ) -> WorkTaskSessionAdmittedRetrievalFutureV1<'a> {
        Box::pin(async { TaskSessionRetrievalOutcomeV1::Unavailable })
    }
}

impl<T> WorkTaskSessionAdmittedRetrievalPortV1 for Arc<T>
where
    T: WorkTaskSessionAdmittedRetrievalPortV1 + ?Sized,
{
    fn retrieve_task_session_admitted<'a>(
        &'a self,
        context: &'a RequestContext,
        temporal_query: SessionTemporalQuery,
        task_binding: TaskSessionBindingV1,
        retrieval_request: RetrievalRequest,
        query: EphemeralSanitizedQueryViewV1,
        retriever_revision: ComponentRevision,
        score_domain: ScoreDomainId,
        policy_revision: ComponentRevision,
        selector: &'a dyn TaskSessionRankSelectorV1,
    ) -> WorkTaskSessionAdmittedRetrievalFutureV1<'a> {
        (**self).retrieve_task_session_admitted(
            context,
            temporal_query,
            task_binding,
            retrieval_request,
            query,
            retriever_revision,
            score_domain,
            policy_revision,
            selector,
        )
    }
}

/// Adapter for the canonical project session retrieval authority.
///
/// `WorkEvidenceRetrievalV1` is the contracts DTO; this type is the application
/// adapter that implements [`WorkEvidenceRetrievalPortV1`].
#[derive(Clone)]
pub struct WorkTaskSessionEvidenceRetrievalV1 {
    retrieval: Arc<dyn WorkTaskSessionAdmittedRetrievalPortV1>,
    federated_authority: Option<Arc<dyn WorkFederatedQueryAuthorityPortV1>>,
}

impl WorkTaskSessionEvidenceRetrievalV1 {
    pub fn new(retrieval: impl WorkTaskSessionAdmittedRetrievalPortV1 + 'static) -> Self {
        Self {
            retrieval: Arc::new(retrieval),
            federated_authority: None,
        }
    }

    pub fn with_federated_authority(
        mut self,
        authority: Arc<dyn WorkFederatedQueryAuthorityPortV1>,
    ) -> Self {
        self.federated_authority = Some(authority);
        self
    }

    fn temporal_query(
        &self,
        request: &WorkTaskSessionRequestV1,
    ) -> Result<SessionTemporalQuery, WorkEvidenceHydrationErrorV1> {
        let page_size = usize::try_from(request.page_size)
            .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?;
        let context_bytes = WORK_EVIDENCE_CONTEXT_BYTES;
        let execution_limits = ExecutionLimits {
            candidate_total_bytes: context_bytes as usize,
            candidate_item_bytes: context_bytes as usize,
            record_total_bytes: context_bytes as usize,
            record_item_bytes: context_bytes as usize,
            hydration_limit: page_size,
            hydration_total_bytes: context_bytes as usize,
            hydration_payload_bytes: context_bytes as usize,
            hydration_chunk_bytes: context_bytes as usize,
            ..ExecutionLimits::default()
        };
        SessionTemporalQuery::new(
            request.source.session_id().clone(),
            Some(request.source.provider().as_str().to_owned()),
            task_session_query_text(request),
            request
                .continuation
                .as_ref()
                .and_then(|continuation| continuation.temporal_cursor.as_ref())
                .map(|cursor| cursor.as_str().to_owned()),
            request.temporal,
            RetrievalGrainV1::Occurrence,
            page_size,
            DiversityLimits::default(),
            ContextBudget {
                max_bytes: context_bytes,
                max_tokens: context_bytes / 4,
                estimator_version: "words-v1".to_owned(),
            },
        )
        .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)
        .map(|query| {
            query
                .with_retrieval_scope(SessionRetrievalScope::Session(
                    request.source.session_id().clone(),
                ))
                .with_execution_limits(execution_limits)
        })
    }
}

impl WorkEvidenceRetrievalPortV1 for WorkTaskSessionEvidenceRetrievalV1 {
    fn clone_arc(&self) -> Arc<dyn WorkEvidenceRetrievalPortV1> {
        Arc::new(self.clone())
    }
}

impl WorkTaskSessionPortV1 for WorkTaskSessionEvidenceRetrievalV1 {
    fn retrieve_task_session<'a>(
        &'a self,
        context: &'a RequestContext,
        request: WorkTaskSessionRequestV1,
        reauthorization: &'a dyn WorkTaskSessionReauthorizationPortV1,
    ) -> WorkTaskSessionFuture<'a> {
        Box::pin(hotpath::future!(
            async move {
                if request.continuation.as_ref().is_some_and(|continuation| {
                    continuation.verified_version != request.verified_version
                        || continuation.attempt != request.attempt
                        || continuation.source != request.source
                }) {
                    return Err(WorkEvidenceHydrationErrorV1::Stale);
                }
                let authority_port = self
                    .federated_authority
                    .as_ref()
                    .ok_or(WorkEvidenceHydrationErrorV1::Unavailable)?;
                let authority = authority_port
                    .authority_for(context.scope())
                    .await
                    .ok_or(WorkEvidenceHydrationErrorV1::Unavailable)?;
                let task_binding = TaskSessionBindingV1::new(
                    request.task_id.clone(),
                    request.verified_version.clone(),
                    &request.accepted_attempts,
                    request.attempt.clone(),
                    request.source.clone(),
                )
                .map_err(|_| WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized)?;
                let temporal_query = self.temporal_query(&request)?;
                let retrieval_request = retrieval_request(context, &request, authority.as_ref())?;
                let query = EphemeralSanitizedQueryViewV1::sanitize(
                    task_session_query_text(&request),
                    SanitizerRevision::new(WORK_TASK_SESSION_SANITIZER_REVISION)
                        .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?,
                    QueryNormalizationRevision::new(WORK_TASK_SESSION_NORMALIZATION_REVISION)
                        .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?,
                )
                .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?;
                let score_domain = authority
                    .task_session_score_domain()
                    .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?;
                let policy_revision =
                    ComponentRevision::new(authority.profile().evaluation_result_anchor.as_str())
                        .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?;
                let ranking_cursor = request
                    .continuation
                    .as_ref()
                    .and_then(|continuation| continuation.ranking_cursor.as_ref())
                    .map(|cursor| serde_json::from_str::<RetrievalCursor>(cursor.as_str()))
                    .transpose()
                    .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?;
                let selector = WorkTaskSessionSelectorV1 {
                    authority: authority.as_ref(),
                    context,
                    request: &request,
                    reauthorization,
                    page_size: usize::try_from(request.page_size)
                        .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?,
                    ranking_cursor,
                };
                let outcome = self
                    .retrieval
                    .retrieve_task_session_admitted(
                        context,
                        temporal_query,
                        task_binding,
                        retrieval_request,
                        query,
                        authority.ranking_revision().clone(),
                        score_domain,
                        policy_revision,
                        &selector,
                    )
                    .await;
                task_session_evidence(&request, outcome, &selector)
            },
            label = "daemon.session_retrieval.evidence"
        ))
    }
}

impl WorkAnchorHydrationPortV1 for WorkTaskSessionEvidenceRetrievalV1 {
    fn hydrate_anchor<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: WorkAnchorHydrationRequestV1,
    ) -> WorkAnchorHydrationFuture<'a> {
        Box::pin(async { Err(WorkEvidenceHydrationErrorV1::Unavailable) })
    }
}

struct WorkTaskSessionSelectorV1<'a> {
    authority: &'a QueryAuthorityV1,
    context: &'a RequestContext,
    request: &'a WorkTaskSessionRequestV1,
    reauthorization: &'a dyn WorkTaskSessionReauthorizationPortV1,
    page_size: usize,
    ranking_cursor: Option<RetrievalCursor>,
}

impl TaskSessionRankSelectorV1 for WorkTaskSessionSelectorV1<'_> {
    fn reauthorize(
        &self,
        binding: &TaskSessionBindingV1,
        _stage: TaskSessionReauthorizationStageV1,
    ) -> Result<(), TaskSessionSelectionCallbackErrorV1> {
        if !binding_matches_request(binding, self.request) {
            return Err(TaskSessionSelectionCallbackErrorV1::Denied);
        }
        self.reauthorization
            .reauthorize_task_session(self.context, self.request)
            .map_err(map_reauthorization_error)
    }

    fn select(
        &self,
        binding: &TaskSessionBindingV1,
        request: &RetrievalRequest,
        query: &EphemeralSanitizedQueryViewV1,
        outcome: &tracedecay_domain::RetrieverOutcome<
            tracedecay_domain::RetrieverBatch<TaskSessionLaneEvidenceV1>,
        >,
    ) -> Result<TaskSessionCandidateSelectionV1, TaskSessionSelectionCallbackErrorV1> {
        if !binding_matches_request(binding, self.request) {
            return Err(TaskSessionSelectionCallbackErrorV1::Denied);
        }
        self.authority
            .select_task_session(
                request,
                query,
                outcome.clone(),
                self.page_size,
                self.ranking_cursor.as_ref(),
            )
            .map_err(|error| TaskSessionSelectionCallbackErrorV1::Invalid(error.to_string()))
    }
}

fn binding_matches_request(
    binding: &TaskSessionBindingV1,
    request: &WorkTaskSessionRequestV1,
) -> bool {
    binding.task_id() == &request.task_id
        && binding.verified_version() == &request.verified_version
        && binding.accepted_attempt() == &request.attempt
        && binding.source() == &request.source
        && request
            .accepted_attempts
            .contains(binding.accepted_attempt())
}

fn map_reauthorization_error(
    error: WorkTaskSessionReauthorizationErrorV1,
) -> TaskSessionSelectionCallbackErrorV1 {
    match error {
        WorkTaskSessionReauthorizationErrorV1::Denied => {
            TaskSessionSelectionCallbackErrorV1::Denied
        }
        WorkTaskSessionReauthorizationErrorV1::Stale => TaskSessionSelectionCallbackErrorV1::Stale,
        WorkTaskSessionReauthorizationErrorV1::Unavailable => {
            TaskSessionSelectionCallbackErrorV1::Unavailable
        }
    }
}

fn retrieval_request(
    context: &RequestContext,
    request: &WorkTaskSessionRequestV1,
    authority: &QueryAuthorityV1,
) -> Result<RetrievalRequest, WorkEvidenceHydrationErrorV1> {
    if request.page_size == 0
        || request.page_size > authority.profile().retrieval_budget.max_hydrated_results
        || request.page_size > authority.profile().retrieval_budget.max_candidates_per_lane
    {
        return Err(WorkEvidenceHydrationErrorV1::Unavailable);
    }
    Ok(RetrievalRequest {
        principal: PrincipalId::new(context.actor().as_str())
            .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?,
        scope: RetrievalScope {
            privacy_domain: authority.privacy_domain().clone(),
            root: SingleRootScopeV1 {
                repository: context.scope().repository_id.clone(),
                worktree: Some(context.scope().worktree_id.clone()),
                reference: context.scope().reference.clone(),
            },
        },
        temporal_mode: request.temporal,
        snapshot: tracedecay_domain::RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(
                request.verified_version.recovered_graph_digest().as_str(),
            )
            .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?,
            authorization_revision: AuthorizationRevision::new(format!(
                "{}@{}",
                context.grant().grant_id.as_str(),
                context.grant().revision,
            ))
            .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?,
            captured_at: request.observed_at,
        },
        profile_id: authority.profile().profile_id.clone(),
        budget: authority.profile().retrieval_budget,
    })
}

fn task_session_query_text(request: &WorkTaskSessionRequestV1) -> String {
    format!(
        "{} {} {} {}",
        request.task_id.as_str(),
        format_args!(
            "{}:{}",
            request.attempt.run_id().as_str(),
            request.attempt.attempt_id().as_str()
        ),
        request.source.provider().as_str(),
        request.source.session_id().as_str(),
    )
}

fn task_session_evidence(
    request: &WorkTaskSessionRequestV1,
    outcome: TaskSessionRetrievalOutcomeV1,
    selector: &dyn TaskSessionRankSelectorV1,
) -> Result<WorkTaskSessionEvidenceV1, WorkEvidenceHydrationErrorV1> {
    let report = match outcome {
        TaskSessionRetrievalOutcomeV1::Complete(report) => report,
        TaskSessionRetrievalOutcomeV1::Omitted(omission) => {
            return Err(match omission.reason {
                TaskSessionExecutionOmissionReasonV1::Denied => {
                    WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized
                }
                TaskSessionExecutionOmissionReasonV1::Stale => WorkEvidenceHydrationErrorV1::Stale,
                TaskSessionExecutionOmissionReasonV1::Unavailable => {
                    WorkEvidenceHydrationErrorV1::Unavailable
                }
            });
        }
        TaskSessionRetrievalOutcomeV1::WrongScope | TaskSessionRetrievalOutcomeV1::Denied => {
            return Err(WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized);
        }
        TaskSessionRetrievalOutcomeV1::Stale { .. } => {
            return Err(WorkEvidenceHydrationErrorV1::Stale);
        }
        TaskSessionRetrievalOutcomeV1::TimedOut => {
            return Err(WorkEvidenceHydrationErrorV1::TimedOut);
        }
        TaskSessionRetrievalOutcomeV1::Cancelled => {
            return Err(WorkEvidenceHydrationErrorV1::Cancelled);
        }
        TaskSessionRetrievalOutcomeV1::ResetRequired => {
            return Err(WorkEvidenceHydrationErrorV1::ResetRequired);
        }
        TaskSessionRetrievalOutcomeV1::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        } => {
            return Err(cursor_manifest_hydration_refusal(kind, observed, maximum));
        }
        TaskSessionRetrievalOutcomeV1::BudgetExhausted { stage } => {
            return Err(budget_hydration_refusal(stage));
        }
        TaskSessionRetrievalOutcomeV1::Unavailable => {
            return Err(WorkEvidenceHydrationErrorV1::Unavailable);
        }
    };
    if !binding_matches_request(&report.binding, request) {
        return Err(WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized);
    }
    reauthorize_work_stage(
        selector,
        &report.binding,
        TaskSessionReauthorizationStageV1::BeforeExpansion,
    )?;
    let result = report.temporal.result();
    let participant_epoch = tracedecay_domain::ManifestDigest::new(
        result.snapshot.participant_manifest().epoch_digest(),
    )
    .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?;
    if request
        .continuation
        .as_ref()
        .is_some_and(|continuation| continuation.participant_epoch != participant_epoch)
    {
        return Err(WorkEvidenceHydrationErrorV1::Stale);
    }
    let ranked_anchors = report
        .selection
        .ranked_candidates()
        .iter()
        .map(|ranked| {
            let contributions = ranked
                .candidate
                .contributions
                .iter()
                .map(|contribution| {
                    Ok(WorkTaskSessionRankContributionV1 {
                        retriever: contribution.retriever,
                        retriever_revision: contribution.retriever_revision.clone(),
                        source_occurrence: contribution.source_occurrence_id.clone(),
                        ordinal_rank: contribution.ordinal_rank,
                        raw_score_micros: i64::try_from(contribution.raw_score.micros())
                            .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?,
                        score_domain: contribution.score_domain.clone(),
                        calibration_profile: contribution.calibration_profile_id.clone(),
                        calibrated_feature_micros: contribution.calibrated_feature_micros,
                        weight_micros: contribution.weight_micros,
                        weighted_contribution_micros: contribution.weighted_contribution_micros,
                    })
                })
                .collect::<Result<Vec<_>, WorkEvidenceHydrationErrorV1>>()?;
            Ok(WorkTaskSessionRankedAnchorV1 {
                anchor_id: ranked.candidate.anchor_id.clone(),
                final_ordinal: ranked.final_ordinal,
                utility_micros: ranked.candidate.utility_micros,
                contributions,
            })
        })
        .collect::<Result<Vec<_>, WorkEvidenceHydrationErrorV1>>()?;
    let hydrated = result
        .hydrated
        .iter()
        .map(|hydrated| WorkTaskSessionHydrationV1 {
            rank: hydrated.rank(),
            anchor_id: hydrated.anchor_id().clone(),
            state: work_hydration_state(hydrated.state()),
            content: hydrated.content().map(ToOwned::to_owned),
        })
        .collect::<Vec<_>>();
    let counts = &result.coverage;
    let continuation = task_session_continuation(
        request,
        participant_epoch.clone(),
        result.next_cursor.as_deref(),
        report.selection.continuation(),
    )?;
    reauthorize_work_stage(
        selector,
        &report.binding,
        TaskSessionReauthorizationStageV1::BeforeContinuation,
    )?;
    let coverage = if counts.hidden == 0
        && counts.unknown == 0
        && counts.redacted == 0
        && continuation.is_none()
    {
        WorkEvidenceCoverageStateV1::Complete
    } else {
        WorkEvidenceCoverageStateV1::Partial
    };
    Ok(WorkTaskSessionEvidenceV1 {
        task_id: request.task_id.clone(),
        verified_version: request.verified_version.clone(),
        attempt: request.attempt.clone(),
        source: request.source.clone(),
        participant_epoch,
        ranked_anchors,
        hydrated,
        coverage,
        coverage_counts: WorkTaskSessionCoverageV1 {
            visible: counts.visible,
            hidden: counts.hidden,
            unknown: counts.unknown,
            redacted: counts.redacted,
        },
        freshness: work_freshness(report.temporal.freshness()),
        redacted: counts.redacted > 0
            || result
                .hydrated
                .iter()
                .any(|hydrated| hydrated.state() == HydrationStateV1::Redacted),
        continuation,
    })
}

const fn cursor_manifest_hydration_refusal(
    kind: tracedecay_domain::CursorManifestLimitKindV1,
    observed: usize,
    maximum: usize,
) -> WorkEvidenceHydrationErrorV1 {
    WorkEvidenceHydrationErrorV1::StructuralRefusal(
        SessionRetrievalStructuralRefusalV1::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        },
    )
}

const fn budget_hydration_refusal(
    stage: tracedecay_session_memory::session::SessionRetrievalBudgetStageV1,
) -> WorkEvidenceHydrationErrorV1 {
    WorkEvidenceHydrationErrorV1::StructuralRefusal(
        SessionRetrievalStructuralRefusalV1::BudgetExhausted { stage },
    )
}

fn reauthorize_work_stage(
    selector: &dyn TaskSessionRankSelectorV1,
    binding: &TaskSessionBindingV1,
    stage: TaskSessionReauthorizationStageV1,
) -> Result<(), WorkEvidenceHydrationErrorV1> {
    selector
        .reauthorize(binding, stage)
        .map_err(|error| match error {
            TaskSessionSelectionCallbackErrorV1::Denied => {
                WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized
            }
            TaskSessionSelectionCallbackErrorV1::Stale => WorkEvidenceHydrationErrorV1::Stale,
            TaskSessionSelectionCallbackErrorV1::Unavailable
            | TaskSessionSelectionCallbackErrorV1::Invalid(_) => {
                WorkEvidenceHydrationErrorV1::Unavailable
            }
        })
}

fn task_session_continuation(
    request: &WorkTaskSessionRequestV1,
    participant_epoch: tracedecay_domain::ManifestDigest,
    temporal_cursor: Option<&str>,
    ranking_cursor: Option<&RetrievalCursor>,
) -> Result<Option<WorkTaskSessionContinuationV1>, WorkEvidenceHydrationErrorV1> {
    if temporal_cursor.is_none() && ranking_cursor.is_none() {
        return Ok(None);
    }
    Ok(Some(WorkTaskSessionContinuationV1 {
        verified_version: request.verified_version.clone(),
        attempt: request.attempt.clone(),
        source: request.source.clone(),
        participant_epoch,
        temporal_cursor: temporal_cursor
            .map(OpaqueCursor::new)
            .transpose()
            .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?,
        ranking_cursor: ranking_cursor
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?
            .map(OpaqueCursor::new)
            .transpose()
            .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?,
    }))
}

const fn work_hydration_state(state: HydrationStateV1) -> WorkTaskSessionHydrationStateV1 {
    match state {
        HydrationStateV1::Available => WorkTaskSessionHydrationStateV1::Available,
        HydrationStateV1::RetainedButUnavailable => {
            WorkTaskSessionHydrationStateV1::RetainedButUnavailable
        }
        HydrationStateV1::Redacted => WorkTaskSessionHydrationStateV1::Redacted,
        HydrationStateV1::Deleted => WorkTaskSessionHydrationStateV1::Deleted,
        HydrationStateV1::RetentionExpired => WorkTaskSessionHydrationStateV1::RetentionExpired,
        HydrationStateV1::Unauthorized => WorkTaskSessionHydrationStateV1::Unauthorized,
        HydrationStateV1::Locked => WorkTaskSessionHydrationStateV1::Locked,
        HydrationStateV1::UnverifiableLegacy => WorkTaskSessionHydrationStateV1::UnverifiableLegacy,
    }
}

const fn work_freshness(freshness: SessionDataFreshness) -> WorkEvidenceFreshnessV1 {
    match freshness {
        SessionDataFreshness::Fresh => WorkEvidenceFreshnessV1::Current,
        SessionDataFreshness::Stored { .. } | SessionDataFreshness::Partial { .. } => {
            WorkEvidenceFreshnessV1::Stale
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_contracts::{
        CancellationContext, CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext,
        RequestId, ResolvedScope, WorkTaskSessionReauthorizationErrorV1,
        WorkTaskSessionReauthorizationPortV1, WorkTaskSessionRequestV1,
    };
    use tracedecay_domain::{
        ActorId, CalibrationProfileId, DiversityPolicy, FusionProfile, ManifestDigest,
        PrivacyDomainId, RetrievalAnchorId, RetrievalBudget, RetrievalCursorKeyId, RetrieverKind,
        ScoreDomainCalibrationV1, ScoreDomainId, SourceStoreId, UtcMicros, WorkGraphVersionV1,
        WorkProductEventSequenceV1, WorkProductSourceWatermarkV1,
    };
    use tracedecay_query::retrieval::QueryAuthorityV1;
    use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::{WorkFederatedQueryAuthorityFutureV1, WorkFederatedQueryAuthorityPortV1};

    pub fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("TaskSession fixture identity")
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("TaskSession fixture digest")
    }

    pub fn context(scope: ResolvedScope) -> RequestContext {
        let grant = CapabilityGrantSnapshot::new(
            id("grant.work-task-session"),
            1,
            digest('a'),
            id::<ActorId>("actor.work-task-session.issuer"),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            scope.clone(),
            BTreeSet::from([
                CapabilityId::new("capability.work.evidence.read").expect("capability")
            ]),
            BTreeSet::from([UseCaseId::new("use-case.work.evidence.read").expect("use case")]),
            DisclosureClass::Evidence,
        )
        .expect("TaskSession fixture grant");
        RequestContext::new(
            id::<ActorId>("actor.work-task-session.requester"),
            scope,
            grant,
            RequestId::new("request.work-task-session").expect("request id"),
            Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            CancellationContext::active("cancel.work-task-session").expect("cancellation"),
        )
        .expect("TaskSession fixture context")
    }

    pub fn verified_version() -> tracedecay_contracts::VerifiedWorkGraphVersionV1 {
        tracedecay_contracts::VerifiedWorkGraphVersionV1::new(
            WorkGraphVersionV1::new(5).expect("graph version"),
            WorkProductEventSequenceV1::new(5).expect("event sequence"),
            WorkProductSourceWatermarkV1::new(BTreeMap::<SourceStoreId, u64>::new())
                .expect("source watermark"),
            digest('b'),
        )
        .expect("verified Work version")
    }

    pub fn federated_authority(privacy_domain: PrivacyDomainId) -> QueryAuthorityV1 {
        let budget = RetrievalBudget {
            max_candidates_per_lane: 32,
            max_fused_candidates: 16,
            max_hydrated_results: 8,
            max_hydration_bytes: 65_536,
            deadline_micros: None,
        };
        let calibrations = RetrieverKind::ALL_LANES
            .into_iter()
            .map(|lane| {
                (
                    lane,
                    id::<CalibrationProfileId>(&format!(
                        "calibration.{}.work-task-session",
                        lane.as_str()
                    )),
                )
            })
            .collect();
        let score_domain_calibrations = RetrieverKind::ALL_LANES
            .into_iter()
            .map(|lane| {
                let score_domain = id::<ScoreDomainId>(&format!("score.{}.v1", lane.as_str()));
                (
                    score_domain.clone(),
                    ScoreDomainCalibrationV1 {
                        calibration_profile_id: id(&format!(
                            "calibration.{}.work-task-session",
                            lane.as_str()
                        )),
                        score_domain,
                        raw_min_micros: 0,
                        raw_max_micros: 1_000_000,
                    },
                )
            })
            .collect();
        let profile = FusionProfile {
            profile_id: id("profile.work-task-session"),
            evaluation_result_anchor: RetrievalAnchorId::new("evaluation.work-task-session")
                .expect("evaluation anchor"),
            calibrations,
            score_domain_calibrations,
            minimum_calibrated_feature_micros: BTreeMap::new(),
            weights_micros: RetrieverKind::ALL_LANES
                .into_iter()
                .map(|lane| (lane, 100_000))
                .collect(),
            diversity_policy_id: id("diversity.work-task-session"),
            rerank_policy_id: None,
            retrieval_budget: budget,
        };
        let diversity = DiversityPolicy {
            policy_id: id("diversity.work-task-session"),
            evaluation_result_anchor: Some(
                RetrievalAnchorId::new("evaluation.work-task-session").expect("evaluation anchor"),
            ),
            per_source_namespace: None,
            per_source_instance: None,
            per_repository: None,
            per_file: None,
            per_session_or_thread: None,
            per_copy_cluster: None,
            per_evidence_role: None,
        };
        QueryAuthorityV1::new_federated(
            profile,
            diversity,
            id("ranking.work-task-session.v1"),
            RetrievalCursorKeyringV1::new(
                privacy_domain,
                id::<RetrievalCursorKeyId>("cursor-key.work-task-session"),
                1,
                vec![7_u8; 32],
                1_000_000,
            )
            .expect("cursor keyring"),
        )
        .expect("federated TaskSession authority")
    }

    pub struct StaticFederatedAuthority(pub Arc<QueryAuthorityV1>);

    impl WorkFederatedQueryAuthorityPortV1 for StaticFederatedAuthority {
        fn authority_for<'a>(
            &'a self,
            _scope: &'a ResolvedScope,
        ) -> WorkFederatedQueryAuthorityFutureV1<'a> {
            let authority = Arc::clone(&self.0);
            Box::pin(async move { Some(authority) })
        }
    }

    #[derive(Default)]
    pub struct CountingReauthorization(pub AtomicUsize);

    impl WorkTaskSessionReauthorizationPortV1 for CountingReauthorization {
        fn reauthorize_task_session(
            &self,
            _context: &RequestContext,
            _request: &WorkTaskSessionRequestV1,
        ) -> Result<(), WorkTaskSessionReauthorizationErrorV1> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    pub struct FailingReauthorization {
        pub calls: AtomicUsize,
        fail_at: usize,
        error: WorkTaskSessionReauthorizationErrorV1,
    }

    impl FailingReauthorization {
        pub fn new(fail_at: usize, error: WorkTaskSessionReauthorizationErrorV1) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail_at,
                error,
            }
        }
    }

    impl WorkTaskSessionReauthorizationPortV1 for FailingReauthorization {
        fn reauthorize_task_session(
            &self,
            _context: &RequestContext,
            _request: &WorkTaskSessionRequestV1,
        ) -> Result<(), WorkTaskSessionReauthorizationErrorV1> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == self.fail_at {
                Err(self.error)
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use tracedecay_contracts::WorkEvidenceHydrationErrorV1;
    use tracedecay_contracts::retrieval::SessionRetrievalStructuralRefusalV1;

    use super::{budget_hydration_refusal, cursor_manifest_hydration_refusal};

    #[test]
    fn task_session_structural_refusals_retain_exact_hydration_causes() {
        assert_eq!(
            cursor_manifest_hydration_refusal(
                tracedecay_domain::CursorManifestLimitKindV1::Participants,
                257,
                256,
            ),
            WorkEvidenceHydrationErrorV1::StructuralRefusal(
                SessionRetrievalStructuralRefusalV1::CursorManifestLimitExceeded {
                    kind: tracedecay_domain::CursorManifestLimitKindV1::Participants,
                    observed: 257,
                    maximum: 256,
                }
            )
        );
        assert_eq!(
            budget_hydration_refusal(
                tracedecay_session_memory::session::SessionRetrievalBudgetStageV1::ContextTokens
            ),
            WorkEvidenceHydrationErrorV1::StructuralRefusal(
                SessionRetrievalStructuralRefusalV1::BudgetExhausted {
                    stage:
                        tracedecay_session_memory::session::SessionRetrievalBudgetStageV1::ContextTokens,
                }
            )
        );
    }
}

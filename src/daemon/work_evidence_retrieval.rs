//! Work evidence adapters over the daemon's mounted retrieval authorities.
//!
//! Work admits the exact task/version/accepted-attempt root. The TaskSession
//! path then borrows one canonical Plan-23 snapshot, ranks its compact anchors
//! through the active evaluated federated profile, reauthorizes Work on both
//! sides of selection, and hydrates only the globally selected anchors.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_application::{
    OpaqueCursor, RequestContext, ResolvedScope, WorkAnchorHydrationFuture,
    WorkAnchorHydrationPortV1, WorkAnchorHydrationRequestV1, WorkEvidenceCoverageStateV1,
    WorkEvidenceFreshnessV1, WorkEvidenceHydrationErrorV1, WorkTaskSessionContinuationV1,
    WorkTaskSessionCoverageV1, WorkTaskSessionEvidenceV1, WorkTaskSessionFuture,
    WorkTaskSessionHydrationStateV1, WorkTaskSessionHydrationV1, WorkTaskSessionPortV1,
    WorkTaskSessionRankContributionV1, WorkTaskSessionRankedAnchorV1,
    WorkTaskSessionReauthorizationErrorV1, WorkTaskSessionReauthorizationPortV1,
    WorkTaskSessionRequestV1,
};
use tracedecay_domain::{
    AuthorizationRevision, ComponentRevision, EphemeralSanitizedQueryViewV1, FreshnessVectorDigest,
    HydrationStateV1, PrincipalId, QueryNormalizationRevision, RetrievalCursor, RetrievalGrainV1,
    RetrievalRequest, RetrievalScope, SanitizerRevision, SingleRootScopeV1, VectorWatermark,
};
use tracedecay_global_db::session_temporal::execution::{
    TaskSessionExecutionOmissionReasonV1, TaskSessionRankSelectorV1,
    TaskSessionReauthorizationStageV1, TaskSessionSelectionCallbackErrorV1,
};
use tracedecay_query::retrieval::QueryAuthorityV1;
use tracedecay_query::retrieval::evidence_lanes::{
    TaskSessionBindingV1, TaskSessionCandidateSelectionV1, TaskSessionLaneEvidenceV1,
};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ports::ExecutionLimits;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::session::{
    SessionDataFreshness, SessionRetrievalScope, SessionTemporalQuery,
    TaskSessionRetrievalOutcomeV1,
};

use crate::daemon::session_retrieval::SessionApplicationRetrievalPortV1;

const WORK_EVIDENCE_CONTEXT_BYTES: u64 = 64 * 1024;
const WORK_TASK_SESSION_SANITIZER_REVISION: &str = "sanitizer.work-task-session.v1";
const WORK_TASK_SESSION_NORMALIZATION_REVISION: &str = "normalization.work-task-session.v1";

pub(crate) type WorkFederatedQueryAuthorityFutureV1<'a> =
    Pin<Box<dyn Future<Output = Option<Arc<QueryAuthorityV1>>> + Send + 'a>>;

/// Resolves the currently activated evaluated authority for an exact scope.
/// Resolution occurs per request so an accepted-profile activation does not
/// leave a long-lived Work runtime bound to a superseded profile.
pub(crate) trait WorkFederatedQueryAuthorityPortV1: Send + Sync {
    fn authority_for<'a>(
        &'a self,
        scope: &'a ResolvedScope,
    ) -> WorkFederatedQueryAuthorityFutureV1<'a>;
}

/// Cloneable adapter for the canonical project session retrieval authority.
#[derive(Clone)]
pub(crate) struct DaemonWorkEvidenceRetrievalV1 {
    retrieval: Arc<dyn SessionApplicationRetrievalPortV1>,
    federated_authority: Option<Arc<dyn WorkFederatedQueryAuthorityPortV1>>,
}

impl DaemonWorkEvidenceRetrievalV1 {
    pub(crate) fn new(retrieval: Arc<dyn SessionApplicationRetrievalPortV1>) -> Self {
        Self {
            retrieval,
            federated_authority: None,
        }
    }

    pub(crate) fn with_federated_authority(
        mut self,
        authority: Arc<dyn WorkFederatedQueryAuthorityPortV1>,
    ) -> Self {
        self.federated_authority = Some(authority);
        self
    }

    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.retrieval, &other.retrieval)
            && match (&self.federated_authority, &other.federated_authority) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
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

impl WorkTaskSessionPortV1 for DaemonWorkEvidenceRetrievalV1 {
    fn retrieve_task_session<'a>(
        &'a self,
        context: &'a RequestContext,
        request: WorkTaskSessionRequestV1,
        reauthorization: &'a dyn WorkTaskSessionReauthorizationPortV1,
    ) -> WorkTaskSessionFuture<'a> {
        Box::pin(async move {
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
        })
    }
}

impl WorkAnchorHydrationPortV1 for DaemonWorkEvidenceRetrievalV1 {
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
        TaskSessionRetrievalOutcomeV1::Cancelled => {
            return Err(WorkEvidenceHydrationErrorV1::Cancelled);
        }
        TaskSessionRetrievalOutcomeV1::ResetRequired => {
            return Err(WorkEvidenceHydrationErrorV1::ResetRequired);
        }
        TaskSessionRetrievalOutcomeV1::Unavailable
        | TaskSessionRetrievalOutcomeV1::BudgetExhausted => {
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

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_application::{
        CancellationContext, CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestId,
        WorkProductSelectionScopeV1,
    };
    use tracedecay_domain::{
        ActorId, AttemptId, CalibrationProfileId, DiversityPolicy, FusionProfile, ManifestDigest,
        ObservationSourceIdentityV1, PrivacyDomainId, ProjectId, ProviderId, RepositoryId,
        RetrievalAnchorId, RetrievalBudget, RetrievalCursorKeyId, RetrieverKind, RunId,
        ScoreDomainCalibrationV1, ScoreDomainId, SessionId, SourceStoreId, TaskId, TemporalModeV1,
        UtcMicros, WorkAttemptIdentityV1, WorkGraphVersionV1, WorkProductEventSequenceV1,
        WorkProductSourceWatermarkV1, WorktreeId,
    };
    use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;

    pub(super) fn id<T>(value: &str) -> T
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

    pub(super) fn context(scope: ResolvedScope) -> RequestContext {
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

    pub(super) fn verified_version() -> tracedecay_application::VerifiedWorkGraphVersionV1 {
        tracedecay_application::VerifiedWorkGraphVersionV1::new(
            WorkGraphVersionV1::new(5).expect("graph version"),
            WorkProductEventSequenceV1::new(5).expect("event sequence"),
            WorkProductSourceWatermarkV1::new(BTreeMap::<SourceStoreId, u64>::new())
                .expect("source watermark"),
            digest('b'),
        )
        .expect("verified Work version")
    }

    pub(crate) fn federated_authority(privacy_domain: PrivacyDomainId) -> QueryAuthorityV1 {
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

    pub(crate) struct StaticFederatedAuthority(pub(crate) Arc<QueryAuthorityV1>);

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
    pub(super) struct CountingReauthorization(pub(super) AtomicUsize);

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

    struct FailingReauthorization {
        calls: AtomicUsize,
        fail_at: usize,
        error: WorkTaskSessionReauthorizationErrorV1,
    }

    impl FailingReauthorization {
        fn new(fail_at: usize, error: WorkTaskSessionReauthorizationErrorV1) -> Self {
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

    #[tokio::test]
    async fn registered_project_session_hydrates_provider_qualified_task_evidence() {
        let profile = tempfile::tempdir().expect("profile root");
        let project = profile.path().join("project");
        std::fs::create_dir_all(&project).expect("project root");
        let project_id = id::<ProjectId>("project.work-task-session");
        let repository_id = id::<RepositoryId>("repository.work-task-session");
        let worktree_id = id::<WorktreeId>("worktree.work-task-session");
        let runtime = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            profile.path(),
            &project,
            project_id.clone(),
        )
        .await
        .expect("registered project session runtime");
        let database = runtime
            .registered_database_arc(
                crate::application::host_admission::HostAdmissionScope::Project,
            )
            .expect("registered project session database");
        let session_id = id::<SessionId>("session.work-task-session");
        let task_id = id::<TaskId>("task.work-task-session");
        let attempt = WorkAttemptIdentityV1::new(
            task_id.clone(),
            id::<RunId>("run.work-task-session"),
            id::<AttemptId>("attempt.work-task-session"),
        )
        .expect("accepted Work attempt");
        let query_text = format!(
            "{} {}:{} codex {}",
            task_id.as_str(),
            attempt.run_id().as_str(),
            attempt.attempt_id().as_str(),
            session_id.as_str(),
        );
        crate::dashboard::observation_seed::seed_session_message_observation_for_test(
            database.as_ref(),
            crate::dashboard::observation_seed::DashboardSessionMessageSeedV1 {
                project_id: project_id.as_str(),
                provider: "codex",
                session_id: session_id.as_str(),
                message_id: "message.work-task-session.1",
                role: "assistant",
                content: &format!("{query_text} completed with durable provider evidence"),
                model: Some("gpt-5.6"),
                timestamp: 101,
                ordinal: 1,
            },
        )
        .await
        .expect("seed canonical provider observation");
        crate::dashboard::observation_seed::materialize_session_temporal_refresh_for_test(
            database.as_ref(),
            session_id.as_str(),
        )
        .await
        .expect("materialize provider session temporal projection");

        let root =
            crate::daemon::session_retrieval::DaemonSessionRetrievalRoot::project_identity_for_test(
                project_id,
                repository_id,
                worktree_id,
                project.display().to_string(),
            );
        let scope = root
            .identity()
            .session_request_scope()
            .expect("resolved Work scope");
        let retrieval = crate::daemon::session_retrieval::DaemonSessionRetrievalService::new(
            database, root, None,
        )
        .expect("mounted project retrieval service");
        let privacy_domain = id::<PrivacyDomainId>("privacy.work-task-session");
        let adapter = DaemonWorkEvidenceRetrievalV1::new(Arc::new(retrieval))
            .with_federated_authority(Arc::new(StaticFederatedAuthority(Arc::new(
                federated_authority(privacy_domain),
            ))));
        let source =
            ObservationSourceIdentityV1::for_provider(id::<ProviderId>("codex"), session_id)
                .expect("provider-qualified session");
        let request = WorkTaskSessionRequestV1 {
            selection: WorkProductSelectionScopeV1::ProfileOwnedNoGit,
            task_id,
            verified_version: verified_version(),
            accepted_attempts: BTreeSet::from([attempt.clone()]),
            attempt,
            source,
            temporal: TemporalModeV1::Forensic,
            page_size: 8,
            continuation: None,
            observed_at: UtcMicros(500),
        };
        let reauthorization = CountingReauthorization::default();

        let request_context = context(scope);
        for temporal in [
            TemporalModeV1::Current,
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(200_000_000),
            },
            TemporalModeV1::Evolution,
            TemporalModeV1::Forensic,
        ] {
            let mut mode_request = request.clone();
            mode_request.temporal = temporal;
            let evidence = adapter
                .retrieve_task_session(&request_context, mode_request, &reauthorization)
                .await
                .expect("real TaskSession evidence");

            assert_eq!(evidence.task_id, request.task_id);
            assert_eq!(evidence.source, request.source);
            assert_eq!(evidence.attempt, request.attempt);
            assert!(
                evidence
                    .hydrated
                    .iter()
                    .filter_map(|hydrated| hydrated.content.as_deref())
                    .any(|content| content
                        .windows(b"durable provider evidence".len())
                        .any(|window| window == b"durable provider evidence")),
                "the mounted adapter must hydrate the owning provider message in {temporal:?}: {evidence:?}",
            );
        }
        assert!(
            reauthorization.0.load(Ordering::SeqCst) >= 16,
            "every temporal mode must reopen Work authority at all four stages",
        );

        for (fail_at, error, expected) in [
            (
                2,
                WorkTaskSessionReauthorizationErrorV1::Denied,
                WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized,
            ),
            (
                1,
                WorkTaskSessionReauthorizationErrorV1::Stale,
                WorkEvidenceHydrationErrorV1::Stale,
            ),
            (
                3,
                WorkTaskSessionReauthorizationErrorV1::Unavailable,
                WorkEvidenceHydrationErrorV1::Unavailable,
            ),
        ] {
            let reauthorization = FailingReauthorization::new(fail_at, error);
            let actual = adapter
                .retrieve_task_session(&request_context, request.clone(), &reauthorization)
                .await
                .expect_err("reauthorization failure must remain typed");
            assert_eq!(actual, expected);
            assert_eq!(reauthorization.calls.load(Ordering::SeqCst), fail_at);
        }
    }
}

#[cfg(test)]
#[path = "work_evidence_retrieval/continuation_tests.rs"]
mod continuation_tests;

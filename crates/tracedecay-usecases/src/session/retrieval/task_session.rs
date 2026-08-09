use super::*;

use tracedecay_domain::{
    ComponentRevision, EphemeralSanitizedQueryViewV1, RetrievalRequest, ScoreDomainId,
};
use tracedecay_global_db::session_temporal::execution::{
    AuthorizedTaskSessionExecutionRequestV1, TaskSessionExecutionOmissionV1,
    TaskSessionRankSelectorV1, TaskSessionSelectionCallbackErrorV1,
    TaskSessionTemporalExecutionOutcomeV1, TaskSessionTemporalExecutionPortV1,
    TaskSessionTemporalExecutionReportV1,
};
use tracedecay_query::retrieval::evidence_lanes::TaskSessionBindingV1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskSessionRetrievalOutcomeV1 {
    Complete(TaskSessionTemporalExecutionReportV1),
    Omitted(TaskSessionExecutionOmissionV1),
    WrongScope,
    Denied,
    Stale { freshness: SessionDataFreshness },
    Unavailable,
    BudgetExhausted,
    Cancelled,
}

pub(super) struct AdmittedSessionTemporalExecution {
    pub(super) execution: AuthorizedTemporalExecutionRequest,
    pub(super) cancellation_control: ExecutionControl,
}

#[derive(Clone, Copy)]
pub(super) enum SessionExecutionAdmissionFailure {
    WrongScope,
    Denied,
    Unavailable,
    BudgetExhausted,
    Cancelled,
}

impl SessionExecutionAdmissionFailure {
    pub(super) fn into_outcome<T>(self) -> SessionRetrievalOutcome<T> {
        match self {
            Self::WrongScope => SessionRetrievalOutcome::WrongScope,
            Self::Denied => SessionRetrievalOutcome::Denied,
            Self::Unavailable => SessionRetrievalOutcome::Unavailable,
            Self::BudgetExhausted => SessionRetrievalOutcome::BudgetExhausted,
            Self::Cancelled => SessionRetrievalOutcome::Cancelled,
        }
    }
}

impl<A, P, E> SessionRetrievalService<A, P, E>
where
    A: SessionScopeAuthorizer,
    E: VersionedTokenEstimator + Sync,
{
    pub(super) fn admit_execution(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        query: &SessionTemporalQuery,
    ) -> Result<AdmittedSessionTemporalExecution, SessionExecutionAdmissionFailure> {
        if application_request_interruption(context, binding.cancellation()).is_some() {
            return Err(SessionExecutionAdmissionFailure::Cancelled);
        }
        let authorization = SessionScopeAuthorizationRequest::new(
            context.actor().clone(),
            binding.identity().clone(),
            query.session_id.clone(),
            query.provider.clone(),
            query.temporal_mode,
            query.grain,
            SessionAccess::Hydrate,
        )
        .map(|request| request.with_retrieval_scope(query.retrieval_scope.clone()))
        .map_err(|_| SessionExecutionAdmissionFailure::Unavailable)?;
        let grant = self
            .authorizer
            .authorize(context, binding, &authorization)
            .map_err(map_authorization_failure)?;
        grant
            .validate(context, binding, &authorization)
            .map_err(map_authorization_failure)?;
        if !within_request_budgets(binding, query)
            || self.estimator.version() != query.context_budget.estimator_version
        {
            return Err(SessionExecutionAdmissionFailure::BudgetExhausted);
        }
        if application_request_interruption(context, binding.cancellation()).is_some() {
            return Err(SessionExecutionAdmissionFailure::Cancelled);
        }

        let root_digest = digest_root(grant.scope().authorized_root().identity());
        let grant_digest = digest_grant(&grant);
        let access_digest = digest_policy(grant.policy_digest());
        let filter_digest = digest_filters(query);
        let request_digest = digest_request(
            context,
            binding,
            query,
            &grant,
            self.configuration,
            &root_digest,
            &grant_digest,
        );
        let control = ExecutionControl::new(Some(execution_deadline(context))).with_work_limit(
            usize::try_from(binding.budgets().max_work_units()).unwrap_or(usize::MAX),
        );
        let cancellation_control = control.clone();
        if binding.cancellation().is_cancelled() || context.cancellation().is_cancelled() {
            control.cancel();
        }
        let snapshot_request = tracedecay_temporal_query::ports::TemporalSnapshotRequest::new(
            query.session_id.clone(),
            root_digest,
            request_digest,
            access_digest,
            query.temporal_mode,
            query.grain,
        )
        .and_then(|request| request.with_filter_digest(filter_digest))
        .and_then(|request| request.with_semantic_filter(query.semantic_filter.clone()))
        .and_then(|request| {
            temporal_authorized_root(grant.scope().authorized_root().identity())
                .and_then(|root| request.with_authorized_root(root))
        })
        .map(|request| {
            request.with_retrieval_scope(temporal_retrieval_scope(&query.retrieval_scope))
        })
        .and_then(|request| request.with_provider_scope(query.provider.clone()))
        .map_err(|_| SessionExecutionAdmissionFailure::Unavailable)?
        .with_limits(query.execution_limits)
        .with_execution_control(control);
        let execution = AuthorizedTemporalExecutionRequest::new(
            snapshot_request,
            query.query.clone(),
            query.cursor.clone(),
            query.limit,
            query.diversity,
            query.context_budget.clone(),
            self.configuration.schema_version,
            self.configuration.ranking_version,
            sha256_binding(binding.configuration_digest().as_bytes()),
        );
        let execution = match query.direct_anchor.clone() {
            Some(anchor_id) => execution.with_direct_anchor(anchor_id),
            None => execution,
        };
        Ok(AdmittedSessionTemporalExecution {
            execution,
            cancellation_control,
        })
    }
}

fn map_authorization_failure(error: SessionAuthorizationError) -> SessionExecutionAdmissionFailure {
    match error {
        SessionAuthorizationError::WrongScope
        | SessionAuthorizationError::WrongTarget
        | SessionAuthorizationError::WrongAccess
        | SessionAuthorizationError::UnresolvedGitRoute
        | SessionAuthorizationError::UnresolvedApplicationScope => {
            SessionExecutionAdmissionFailure::WrongScope
        }
        SessionAuthorizationError::WrongContext | SessionAuthorizationError::Denied => {
            SessionExecutionAdmissionFailure::Denied
        }
        SessionAuthorizationError::Unavailable
        | SessionAuthorizationError::InvalidGrantId
        | SessionAuthorizationError::InvalidProviderScope
        | SessionAuthorizationError::ZeroRevision => SessionExecutionAdmissionFailure::Unavailable,
    }
}

impl<A, P, E> SessionRetrievalService<A, P, E>
where
    A: SessionScopeAuthorizer,
    P: SessionTemporalExecutionPort + TaskSessionTemporalExecutionPortV1,
    E: VersionedTokenEstimator + Sync,
{
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_task_session(
        &self,
        context: &RequestContext,
        session_binding: &SessionRequestBinding,
        temporal_query: SessionTemporalQuery,
        task_binding: TaskSessionBindingV1,
        retrieval_request: RetrievalRequest,
        query: EphemeralSanitizedQueryViewV1,
        retriever_revision: ComponentRevision,
        score_domain: ScoreDomainId,
        policy_revision: ComponentRevision,
        selector: &dyn TaskSessionRankSelectorV1,
    ) -> TaskSessionRetrievalOutcomeV1 {
        let admitted = match self.admit_execution(context, session_binding, &temporal_query) {
            Ok(admitted) => admitted,
            Err(failure) => return task_session_admission_failure(failure),
        };
        let expected_execution = admitted.execution.clone();
        let request = match AuthorizedTaskSessionExecutionRequestV1::new(
            admitted.execution,
            retrieval_request,
            query,
            task_binding,
            retriever_revision,
            score_domain,
            policy_revision,
        ) {
            Ok(request) => request,
            Err(error) => return map_task_session_callback_error(error),
        };
        let Ok(result) = run_application_request_interruptible(
            context,
            session_binding.cancellation(),
            self.execution
                .execute_task_session(request, selector, &self.estimator),
            || admitted.cancellation_control.cancel(),
        )
        .await
        else {
            return TaskSessionRetrievalOutcomeV1::Cancelled;
        };
        match result {
            Ok(TaskSessionTemporalExecutionOutcomeV1::Complete(report))
                if expected_execution.validates_report(&report.temporal) =>
            {
                if temporal_query
                    .freshness_policy
                    .accepts(report.temporal.freshness())
                {
                    TaskSessionRetrievalOutcomeV1::Complete(report)
                } else {
                    TaskSessionRetrievalOutcomeV1::Stale {
                        freshness: report.temporal.freshness(),
                    }
                }
            }
            Ok(TaskSessionTemporalExecutionOutcomeV1::Complete(_)) => {
                TaskSessionRetrievalOutcomeV1::Unavailable
            }
            Ok(TaskSessionTemporalExecutionOutcomeV1::Omitted(omission)) => {
                TaskSessionRetrievalOutcomeV1::Omitted(omission)
            }
            Err(error) => map_task_session_execution_error(error),
        }
    }
}

fn task_session_admission_failure(
    failure: SessionExecutionAdmissionFailure,
) -> TaskSessionRetrievalOutcomeV1 {
    match failure {
        SessionExecutionAdmissionFailure::WrongScope => TaskSessionRetrievalOutcomeV1::WrongScope,
        SessionExecutionAdmissionFailure::Denied => TaskSessionRetrievalOutcomeV1::Denied,
        SessionExecutionAdmissionFailure::Unavailable => TaskSessionRetrievalOutcomeV1::Unavailable,
        SessionExecutionAdmissionFailure::BudgetExhausted => {
            TaskSessionRetrievalOutcomeV1::BudgetExhausted
        }
        SessionExecutionAdmissionFailure::Cancelled => TaskSessionRetrievalOutcomeV1::Cancelled,
    }
}

fn map_task_session_callback_error(
    error: TaskSessionSelectionCallbackErrorV1,
) -> TaskSessionRetrievalOutcomeV1 {
    match error {
        TaskSessionSelectionCallbackErrorV1::Denied => TaskSessionRetrievalOutcomeV1::Denied,
        TaskSessionSelectionCallbackErrorV1::Stale
        | TaskSessionSelectionCallbackErrorV1::Unavailable
        | TaskSessionSelectionCallbackErrorV1::Invalid(_) => {
            TaskSessionRetrievalOutcomeV1::Unavailable
        }
    }
}

fn map_task_session_execution_error(
    error: SessionTemporalExecutionError,
) -> TaskSessionRetrievalOutcomeV1 {
    match map_execution_error(error) {
        SessionRetrievalOutcome::WrongScope => TaskSessionRetrievalOutcomeV1::WrongScope,
        SessionRetrievalOutcome::Denied => TaskSessionRetrievalOutcomeV1::Denied,
        SessionRetrievalOutcome::Stale { freshness } => {
            TaskSessionRetrievalOutcomeV1::Stale { freshness }
        }
        SessionRetrievalOutcome::BudgetExhausted => TaskSessionRetrievalOutcomeV1::BudgetExhausted,
        SessionRetrievalOutcome::Cancelled => TaskSessionRetrievalOutcomeV1::Cancelled,
        SessionRetrievalOutcome::Unavailable
        | SessionRetrievalOutcome::Complete { .. }
        | SessionRetrievalOutcome::Partial { .. }
        | SessionRetrievalOutcome::CompleteZero { .. }
        | SessionRetrievalOutcome::Locked
        | SessionRetrievalOutcome::Redacted
        | SessionRetrievalOutcome::Deleted
        | SessionRetrievalOutcome::CursorManifestLimitExceeded { .. } => {
            TaskSessionRetrievalOutcomeV1::Unavailable
        }
    }
}

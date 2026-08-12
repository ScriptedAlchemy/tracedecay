use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::retained_surfaces::{
    ClosedUtcIntervalV1, GitScopeV1, HydrationStateResultV1, MessageRelationshipScopeV1,
    MessageSearchHitV1, MessageSearchRequestV1, MessageSearchResultV1, MessageTypeFilterV1,
    RetainedOutcomeStatusV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
    SessionCoverageIntervalV1, SessionCoverageModeV1, SessionCoverageReasonV1,
    SessionCoverageRequestV1, SessionCoverageStateV1, SessionMessageV1, SessionRecordV1,
    SessionRefreshRequestV1, SessionSourceCoverageV1 as WireSourceCoverageV1, SessionsForRequestV1,
    TemporalCoverageV1, TemporalExplanationV1, TemporalFreshnessV1, TemporalMetadataV1,
    TemporalOmissionV1, TemporalWatermarksV1, ValidCoverageIntervalV1, WorkflowsRequestV1,
};
use tracedecay_application::{
    ApplicationOutcome, CancellationStage, RequestAdmission, RetainedSessionExecutionPortV1,
    RetainedSessionRequestV1, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    RetainedSurfaceExecutionFutureV1, now_micros,
};
use tracedecay_domain::{
    HydrationStateV1, ManifestDigest, ProjectId, RetrievalGrainV1, SessionId,
    SessionSourceCoverageIntervalV1, SessionSourceCoverageReasonV1, SessionSourceCoverageStateV1,
    SessionSourceCoverageV1, TemporalCoverageCountsV1, TemporalModeV1, UserProfileId,
    ValidCoverageIntervalV1 as DomainValidCoverageIntervalV1, canonical_sha256,
};
use tracedecay_sessions::WorkflowIndexReadPort;
use tracedecay_sessions::runtime::git_correlation::GitScopeFilter;
use tracedecay_sessions::runtime::{
    ProviderScope, SessionMessageRecord, SessionMessageSearchResult, SessionMessageType,
    SessionRecord, SessionSearchScope,
};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ports::{
    TemporalCandidateFilterV1, TemporalMessageTypeFilterV1, TemporalSessionScopeFilterV1,
};
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::context::{SessionRootId, SessionStoreId};
use tracedecay_usecases::session::{
    SessionDataFreshness, SessionFreshnessPolicy, SessionRetrievalScope, SessionTemporalQuery,
};

use super::receipts::{evidence_outcome, session_refresh_effect_outcome};
use super::session_refresh::{RetainedSessionRefreshPortV1, admitted_session_refresh_command};
use crate::daemon::session_retrieval::{
    SessionApplicationRetrievalPortV1, SessionRetrievalPageView, SessionRetrievalServiceOutcome,
    SessionRetrievalStoreScope, SessionTemporalMetadataView,
};
use crate::errors::TraceDecayError;
use crate::global_db::RegisteredGlobalDb;
use crate::timeutil::{SearchTimeBound, parse_search_time_filter_bound};

mod refresh;
#[cfg(test)]
mod retained_effect_tests;

const MESSAGE_SEARCH_ROOT_SESSION_ID: &str = "session.message-search.root";
const MESSAGE_SEARCH_CONTEXT_BYTES: u64 = 64 * 1024;

pub(super) struct ProjectRetainedSessionAuthoritiesV1 {
    pub(super) project_root: PathBuf,
    pub(super) project_id: ProjectId,
    pub(super) profile_id: UserProfileId,
    pub(super) session_store_id: SessionStoreId,
    pub(super) session_root_id: SessionRootId,
    pub(super) configuration_digest: ManifestDigest,
    pub(super) refresh: Arc<dyn RetainedSessionRefreshPortV1>,
    pub(super) retrieval: Arc<dyn SessionApplicationRetrievalPortV1>,
    pub(super) session_database: Arc<RegisteredGlobalDb>,
    pub(super) workflow_index: Arc<dyn WorkflowIndexReadPort>,
}

pub(super) struct DirectRetainedSessionPortV1 {
    authorities: ProjectRetainedSessionAuthoritiesV1,
}

impl DirectRetainedSessionPortV1 {
    pub(super) const fn project(authorities: ProjectRetainedSessionAuthoritiesV1) -> Self {
        Self { authorities }
    }

    async fn execute_message_search(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &MessageSearchRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        ensure_project_message_scope(context, request, &self.authorities)?;
        let input = MessageSearchInput::parse(request)?;
        let query = input.query()?;
        let outcome = retrieve_bounded(context, self.authorities.retrieval.as_ref(), query).await?;
        let result = input.result(outcome)?;
        evidence_outcome(
            context,
            RetainedSurfaceOperation::MessageSearch,
            RetainedSurfaceResultV1::MessageSearch(result),
        )
    }

    async fn execute_session_refresh(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &SessionRefreshRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        ensure_session_refresh_identity(context, request, &self.authorities)?;
        let operation = request.operation();
        let command = admitted_session_refresh_command(
            request,
            context.request_context,
            context.cancellation_signal,
            &self.authorities.profile_id,
            &self.authorities.session_store_id,
            &self.authorities.session_root_id,
            &self.authorities.configuration_digest,
        )?;
        if operation == RetainedSurfaceOperation::SessionRefreshStatus {
            let handled = self
                .bounded(context, async {
                    Ok::<_, TraceDecayError>(self.authorities.refresh.execute(command).await)
                })
                .await?;
            return evidence_outcome(context, operation, refresh::status_result(handled)?);
        }

        // Once an administrative refresh crosses its effect boundary it must
        // settle to a durable receipt. A transport cancellation may win before
        // this CAS; after it wins, cancellation cannot drop the in-flight DB
        // transaction and misreport an unknown effect as a pre-admission error.
        if !context.cancellation_signal.try_begin_commit() {
            return Err(RetainedSurfaceExecutionErrorV1::Cancelled(
                CancellationStage::BeforeEffect,
            ));
        }
        let handled = self.authorities.refresh.execute(command).await;
        let projected = match operation {
            RetainedSurfaceOperation::SessionRefreshBegin => refresh::begin_result(handled)?,
            RetainedSurfaceOperation::SessionRefreshCancel => {
                refresh::cancel_result(handled, request.request.handle.as_deref())?
            }
            _ => return Err(RetainedSurfaceExecutionErrorV1::Unsupported),
        };
        session_refresh_effect_outcome(
            context,
            operation,
            &self.authorities.configuration_digest,
            request,
            &projected.operation_id,
            projected.result,
            projected.reconciliation_required,
        )
    }

    async fn execute_sessions_for(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &SessionsForRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        ensure_mounted_project_context(context, &self.authorities)?;
        let result = self
            .bounded(context, async {
                Ok::<_, TraceDecayError>(
                    super::session_queries::sessions_for(
                        Some(self.authorities.session_database.as_ref()),
                        request,
                    )
                    .await,
                )
            })
            .await??;
        evidence_outcome(
            context,
            RetainedSurfaceOperation::SessionsFor,
            RetainedSurfaceResultV1::SessionsFor(result),
        )
    }

    async fn execute_workflows(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &WorkflowsRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        ensure_mounted_project_context(context, &self.authorities)?;
        let result = self
            .bounded(context, async {
                Ok::<_, TraceDecayError>(
                    super::session_queries::workflows(
                        Some(self.authorities.workflow_index.as_ref()),
                        request,
                    )
                    .await,
                )
            })
            .await??;
        evidence_outcome(
            context,
            RetainedSurfaceOperation::Workflows,
            RetainedSurfaceResultV1::Workflows(result),
        )
    }

    async fn bounded<T, F>(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        future: F,
    ) -> Result<T, RetainedSurfaceExecutionErrorV1>
    where
        F: std::future::Future<Output = Result<T, TraceDecayError>>,
    {
        tokio::select! {
            () = context.cancellation_signal.cancelled() => Err(
                RetainedSurfaceExecutionErrorV1::Cancelled(CancellationStage::DuringRead)
            ),
            result = super::bounded_execution(context, future) => result,
        }
    }
}

impl RetainedSessionExecutionPortV1 for DirectRetainedSessionPortV1 {
    fn execute_session<'a>(
        &'a self,
        context: RetainedSurfaceExecutionContextV1<'a>,
        request: RetainedSessionRequestV1<'a>,
    ) -> RetainedSurfaceExecutionFutureV1<'a> {
        Box::pin(async move {
            match request {
                RetainedSessionRequestV1::SessionRefresh(request) => {
                    self.execute_session_refresh(&context, request).await
                }
                RetainedSessionRequestV1::MessageSearch(request) => {
                    self.execute_message_search(&context, request).await
                }
                RetainedSessionRequestV1::SessionsFor(request) => {
                    self.execute_sessions_for(&context, request).await
                }
                RetainedSessionRequestV1::Workflows(request) => {
                    self.execute_workflows(&context, request).await
                }
            }
        })
    }
}

struct MessageSearchInput {
    query: String,
    goals: bool,
    provider: ProviderScope,
    project_key: Option<String>,
    include_subagents: bool,
    catch_up: bool,
    cursor: Option<String>,
    parent_session_id: Option<String>,
    since: Option<i64>,
    until: Option<i64>,
    scope: SessionSearchScope,
    message_type: SessionMessageType,
    limit: usize,
    git: GitScopeFilter,
    workflow_run: Option<String>,
    workflow_agent: Option<String>,
}

impl MessageSearchInput {
    fn parse(request: &MessageSearchRequestV1) -> Result<Self, RetainedSurfaceExecutionErrorV1> {
        let goals = request.goals;
        let query = optional_string(request.query.as_deref())?;
        let query = match query {
            Some(query) => query,
            None if goals => String::new(),
            None => return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest),
        };
        let provider = ProviderScope::parse_optional(request.provider.as_deref())
            .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
        let include_subagents = request.include_subagents.unwrap_or(true);
        let mut scope = match request.scope.unwrap_or(MessageRelationshipScopeV1::All) {
            MessageRelationshipScopeV1::All => SessionSearchScope::All,
            MessageRelationshipScopeV1::ParentsOnly => SessionSearchScope::ParentsOnly,
            MessageRelationshipScopeV1::SubagentsOnly => SessionSearchScope::SubagentsOnly,
        };
        if !include_subagents && scope == SessionSearchScope::SubagentsOnly {
            return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
        }
        if !include_subagents && scope == SessionSearchScope::All {
            scope = SessionSearchScope::ParentsOnly;
        }
        let message_type = match request.message_type.unwrap_or(MessageTypeFilterV1::All) {
            MessageTypeFilterV1::All => SessionMessageType::All,
            MessageTypeFilterV1::DirectUser => SessionMessageType::DirectUser,
            MessageTypeFilterV1::ToolResult => SessionMessageType::ToolResult,
        };
        let workflow_run = optional_string(request.workflow_run.as_deref())?;
        let workflow_agent = optional_string(request.workflow_agent.as_deref())?;
        if workflow_agent.is_some() && workflow_run.is_none() {
            return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
        }
        let since = time_filter(
            request.since.as_ref().or(request.time_from.as_ref()),
            SearchTimeBound::Start,
        )?;
        let until = time_filter(
            request.until.as_ref().or(request.time_to.as_ref()),
            SearchTimeBound::End,
        )?;
        if since.zip(until).is_some_and(|(since, until)| since > until) {
            return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
        }
        let branch = optional_string(request.branch.as_deref())?;
        let worktree = optional_string(request.worktree.as_deref())?;
        let commit = optional_string(request.commit.as_deref())?;
        let git =
            GitScopeFilter::from_args(branch.as_deref(), worktree.as_deref(), commit.as_deref())
                .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
        Ok(Self {
            query,
            goals,
            provider,
            project_key: optional_string(request.project_key.as_deref())?,
            include_subagents,
            catch_up: request.catch_up.unwrap_or(false),
            cursor: optional_string(request.cursor.as_deref())?,
            parent_session_id: optional_string(request.parent_session_id.as_deref())?,
            since,
            until,
            scope,
            message_type,
            limit: request.limit.unwrap_or(10).clamp(1, 50) as usize,
            git,
            workflow_run,
            workflow_agent,
        })
    }

    fn query(&self) -> Result<SessionTemporalQuery, RetainedSurfaceExecutionErrorV1> {
        let semantic_filter = TemporalCandidateFilterV1 {
            project_key: self.project_key.clone(),
            parent_session_id: self.parent_session_id.clone(),
            source: None,
            include_summaries: false,
            session_scope: match self.scope {
                SessionSearchScope::All => TemporalSessionScopeFilterV1::All,
                SessionSearchScope::ParentsOnly => TemporalSessionScopeFilterV1::ParentsOnly,
                SessionSearchScope::SubagentsOnly => TemporalSessionScopeFilterV1::SubagentsOnly,
            },
            message_type: match self.message_type {
                SessionMessageType::All => TemporalMessageTypeFilterV1::All,
                SessionMessageType::DirectUser => TemporalMessageTypeFilterV1::DirectUser,
                SessionMessageType::ToolResult => TemporalMessageTypeFilterV1::ToolResult,
            },
            roles: Vec::new(),
            start_time: self.since,
            end_time: self.until,
            git_branch: self.git.branch.clone(),
            git_worktree: self.git.worktree.clone(),
            git_commit: self.git.commit.clone(),
            workflow_run: self.workflow_run.clone(),
            workflow_agent: self.workflow_agent.clone(),
            goals: self.goals,
        };
        let filter_digest = canonical_sha256(&(
            "tracedecay.daemon.retained.message-search.filter.v1",
            &semantic_filter,
        ))
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
        SessionTemporalQuery::new(
            SessionId::new(MESSAGE_SEARCH_ROOT_SESSION_ID)
                .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?,
            self.provider.provider_id().map(str::to_owned),
            &self.query,
            self.cursor.clone(),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
            self.limit,
            DiversityLimits::default(),
            ContextBudget {
                max_bytes: MESSAGE_SEARCH_CONTEXT_BYTES,
                max_tokens: MESSAGE_SEARCH_CONTEXT_BYTES / 4,
                estimator_version: "words-v1".to_owned(),
            },
        )
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)
        .map(|query| {
            query
                .with_retrieval_scope(SessionRetrievalScope::AllSessionsInAuthorizedRoot)
                .with_freshness_policy(if self.catch_up {
                    SessionFreshnessPolicy::RequireFresh
                } else {
                    SessionFreshnessPolicy::AllowStored
                })
                .with_compatibility_filter_digest(filter_digest.as_str().to_owned())
                .with_semantic_filter(semantic_filter)
        })
    }

    fn result(
        &self,
        outcome: SessionRetrievalServiceOutcome,
    ) -> Result<MessageSearchResultV1, RetainedSurfaceExecutionErrorV1> {
        let mut result = self.base_result();
        match outcome {
            SessionRetrievalServiceOutcome::Complete { page, freshness } => {
                result.outcome = RetainedOutcomeStatusV1::Complete;
                apply_page(&mut result, page, freshness)?;
            }
            SessionRetrievalServiceOutcome::CompleteZero {
                temporal,
                freshness,
            } => apply_temporal(&mut result, temporal, freshness),
            SessionRetrievalServiceOutcome::Stale {
                temporal,
                freshness,
            } => {
                result.status = RetainedOutcomeStatusV1::Stale;
                result.outcome = RetainedOutcomeStatusV1::Stale;
                result.refresh_required = self.catch_up;
                apply_temporal(&mut result, temporal, freshness);
            }
            SessionRetrievalServiceOutcome::Partial {
                page,
                freshness,
                omitted,
            } => {
                result.status = RetainedOutcomeStatusV1::Partial;
                result.outcome = RetainedOutcomeStatusV1::Partial;
                result.omitted = Some(omitted);
                result.refresh_required =
                    self.catch_up && !matches!(freshness, SessionDataFreshness::Fresh);
                apply_page(&mut result, page, freshness)?;
            }
            SessionRetrievalServiceOutcome::Redacted => {
                result.status = RetainedOutcomeStatusV1::Redacted;
                result.outcome = RetainedOutcomeStatusV1::Redacted;
            }
            SessionRetrievalServiceOutcome::Deleted => {
                result.status = RetainedOutcomeStatusV1::Deleted;
                result.outcome = RetainedOutcomeStatusV1::Deleted;
            }
            SessionRetrievalServiceOutcome::WrongScope | SessionRetrievalServiceOutcome::Denied => {
                return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
            }
            SessionRetrievalServiceOutcome::ResetRequired { store_scope } => {
                return Err(match store_scope {
                    SessionRetrievalStoreScope::Project => {
                        RetainedSurfaceExecutionErrorV1::ProjectResetRequired
                    }
                    SessionRetrievalStoreScope::Profile => {
                        RetainedSurfaceExecutionErrorV1::ProfileResetRequired
                    }
                });
            }
            SessionRetrievalServiceOutcome::Locked
            | SessionRetrievalServiceOutcome::Unavailable(_) => {
                return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
            }
            SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { .. }
            | SessionRetrievalServiceOutcome::BudgetExhausted => {
                return Err(RetainedSurfaceExecutionErrorV1::Saturated);
            }
            SessionRetrievalServiceOutcome::Cancelled => {
                return Err(RetainedSurfaceExecutionErrorV1::Cancelled(
                    CancellationStage::DuringRead,
                ));
            }
        }
        Ok(result)
    }

    fn base_result(&self) -> MessageSearchResultV1 {
        MessageSearchResultV1 {
            catch_up: self.catch_up,
            catch_up_failures: Vec::new(),
            catch_up_performed: false,
            catch_up_provider: self.provider.response_label().to_owned(),
            count: Some(0),
            goals: self.goals,
            include_subagents: self.include_subagents,
            message_type: self.message_type.as_str().to_owned(),
            next_action: None,
            outcome: RetainedOutcomeStatusV1::CompleteZero,
            parent_session_id: self.parent_session_id.clone(),
            project_key: self.project_key.clone(),
            provider: self.provider.response_label().to_owned(),
            query: Some(self.query.clone()),
            refresh_required: false,
            requested_provider: self.provider.provider_id().map(str::to_owned),
            results: Some(Vec::new()),
            scope: self.scope.as_str().to_owned(),
            since: self.since,
            status: RetainedOutcomeStatusV1::Ok,
            until: self.until,
            error: None,
            git_filter: (!self.git.is_empty()).then(|| GitScopeV1 {
                branch: self.git.branch.clone(),
                worktree: self.git.worktree.clone(),
                commit: self.git.commit.clone(),
            }),
            git_filter_applied: (!self.git.is_empty()).then_some(true),
            message: None,
            omitted: None,
            project_scope: None,
            registry_truncated: None,
            roots: None,
            searched_project_count: None,
            selected_project_root: None,
            service_status: None,
            skipped: None,
            skipped_project_count: None,
            store_scope: Some("project".to_owned()),
            temporal: None,
            workflow_agent: self.workflow_agent.clone(),
            workflow_filter_applied: self.workflow_run.as_ref().map(|_| true),
            workflow_run: self.workflow_run.clone(),
            workflow_run_parent_session: None,
        }
    }
}

fn ensure_project_message_scope(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    request: &MessageSearchRequestV1,
    authorities: &ProjectRetainedSessionAuthoritiesV1,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    ensure_mounted_project_context(context, authorities)?;
    if request
        .project_scope
        .as_deref()
        .is_some_and(|scope| scope != "project")
        || request
            .project_id
            .as_deref()
            .is_some_and(|project_id| project_id != authorities.project_id.as_str())
        || request
            .project_path
            .as_deref()
            .is_some_and(|path| Path::new(path) != authorities.project_root.as_path())
        || request
            .project_selector
            .as_ref()
            .is_some_and(|selector| selector.project_id.as_str() != authorities.project_id.as_str())
    {
        return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
    }
    Ok(())
}

fn ensure_session_refresh_identity(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    request: &SessionRefreshRequestV1,
    authorities: &ProjectRetainedSessionAuthoritiesV1,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    ensure_mounted_project_context(context, authorities)?;
    let selector = &request.request;
    let scope = context.request_context.scope();
    let branch_matches = scope
        .reference
        .as_ref()
        .and_then(|reference| reference.as_str().strip_prefix("refs/heads/"))
        .is_some_and(|branch| branch == selector.project.branch_id);
    (selector.project.id == authorities.project_id.as_str()
        && selector.project.profile_id == authorities.profile_id.as_str()
        && selector.project.repository_id == scope.repository_id.as_str()
        && selector.project.worktree_id == scope.worktree_id.as_str()
        && branch_matches
        && selector.session.store_id == authorities.session_store_id.as_str()
        && selector.session.root_id == authorities.session_root_id.as_str())
    .then_some(())
    .ok_or(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)
}

fn ensure_mounted_project_context(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    authorities: &ProjectRetainedSessionAuthoritiesV1,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    (context.request_context.scope().project_id == authorities.project_id)
        .then_some(())
        .ok_or(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)
}

fn optional_string(value: Option<&str>) -> Result<Option<String>, RetainedSurfaceExecutionErrorV1> {
    value
        .map(str::trim)
        .map(|value| {
            (!value.is_empty())
                .then(|| value.to_owned())
                .ok_or(RetainedSurfaceExecutionErrorV1::InvalidRequest)
        })
        .transpose()
}

fn time_filter(
    value: Option<&tracedecay_application::retained_surfaces::RetainedTimeFilterV1>,
    bound: SearchTimeBound,
) -> Result<Option<i64>, RetainedSurfaceExecutionErrorV1> {
    let value = match value {
        None => return Ok(None),
        Some(tracedecay_application::retained_surfaces::RetainedTimeFilterV1::Micros(value)) => {
            i64::try_from(*value).map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?
        }
        Some(tracedecay_application::retained_surfaces::RetainedTimeFilterV1::Expression(
            value,
        )) => {
            let value = value.trim();
            if value.is_empty() {
                return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
            }
            match value.parse::<i64>() {
                Ok(value) if value >= 0 => value,
                Ok(_) => return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest),
                Err(_) => parse_search_time_filter_bound(
                    value,
                    crate::tracedecay::current_timestamp(),
                    bound,
                )
                .ok_or(RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
            }
        }
    };
    Ok(Some(value))
}

async fn retrieve_bounded(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    service: &dyn SessionApplicationRetrievalPortV1,
    query: SessionTemporalQuery,
) -> Result<SessionRetrievalServiceOutcome, RetainedSurfaceExecutionErrorV1> {
    let now = now_micros();
    match context.request_context.admission_at(now) {
        RequestAdmission::Admitted => {}
        RequestAdmission::Cancelled => {
            return Err(RetainedSurfaceExecutionErrorV1::Cancelled(
                CancellationStage::BeforeRead,
            ));
        }
        RequestAdmission::TimedOut => {
            return Err(RetainedSurfaceExecutionErrorV1::TimedOut(
                CancellationStage::BeforeRead,
            ));
        }
    }
    let remaining = context
        .request_context
        .deadline()
        .expires_at
        .0
        .saturating_sub(now.0);
    let remaining = u64::try_from(remaining)
        .ok()
        .map(Duration::from_micros)
        .ok_or(RetainedSurfaceExecutionErrorV1::TimedOut(
            CancellationStage::BeforeRead,
        ))?;
    let retrieval = service.retrieve_admitted_with_cancellation(
        context.request_context,
        context.cancellation_signal,
        query,
    );
    tokio::select! {
        _ = context.cancellation_signal.cancelled() => {
            Err(RetainedSurfaceExecutionErrorV1::Cancelled(
                CancellationStage::DuringRead,
            ))
        }
        outcome = tokio::time::timeout(remaining, retrieval) => {
            outcome.map_err(|_| {
                RetainedSurfaceExecutionErrorV1::TimedOut(CancellationStage::DuringRead)
            })
        }
    }
}

fn apply_page(
    result: &mut MessageSearchResultV1,
    page: SessionRetrievalPageView,
    freshness: SessionDataFreshness,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    result.selected_project_root = page.temporal.authorized_root.clone();
    result.count = Some(page.results.len());
    result.results = Some(
        page.results
            .into_iter()
            .map(message_search_hit)
            .collect::<Result<Vec<_>, _>>()?,
    );
    result.temporal = Some(temporal(page.temporal, freshness));
    Ok(())
}

fn apply_temporal(
    result: &mut MessageSearchResultV1,
    temporal_view: SessionTemporalMetadataView,
    freshness: SessionDataFreshness,
) {
    result.selected_project_root = temporal_view.authorized_root.clone();
    result.temporal = Some(temporal(temporal_view, freshness));
}

fn message_search_hit(
    result: SessionMessageSearchResult,
) -> Result<MessageSearchHitV1, RetainedSurfaceExecutionErrorV1> {
    if !result.score.is_finite() {
        return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
    }
    Ok(MessageSearchHitV1 {
        session: session_record(result.session),
        message: session_message(result.message),
        score: result.score,
        project_id: None,
        root: None,
    })
}

fn session_record(record: SessionRecord) -> SessionRecordV1 {
    SessionRecordV1 {
        provider: record.provider,
        session_id: record.session_id,
        project_key: record.project_key,
        project_path: record.project_path,
        title: record.title,
        started_at: record.started_at,
        ended_at: record.ended_at,
        transcript_path: record.transcript_path,
        metadata_json: record.metadata_json,
        parent_session_id: record.parent_session_id,
        is_subagent: record.is_subagent,
        agent_id: record.agent_id,
        parent_tool_use_id: record.parent_tool_use_id,
    }
}

fn session_message(message: SessionMessageRecord) -> SessionMessageV1 {
    SessionMessageV1 {
        provider: message.provider,
        message_id: message.message_id,
        session_id: message.session_id,
        role: message.role,
        timestamp: message.timestamp,
        ordinal: message.ordinal,
        text: message.text,
        kind: message.kind,
        model: message.model,
        tool_names: message.tool_names,
        source_path: message.source_path,
        source_offset: message.source_offset,
        metadata_json: message.metadata_json,
    }
}

fn temporal(
    value: SessionTemporalMetadataView,
    freshness: SessionDataFreshness,
) -> TemporalMetadataV1 {
    TemporalMetadataV1 {
        anchors: value
            .anchors
            .into_iter()
            .map(|anchor| anchor.as_str().to_owned())
            .collect(),
        watermarks: TemporalWatermarksV1 {
            generation: value.watermarks.generation,
            source: value.watermarks.source,
            projection: value.watermarks.projection,
            index: value.watermarks.index,
            summary: value.watermarks.summary,
        },
        coverage: coverage(value.coverage),
        source_coverage: value
            .source_coverage
            .into_iter()
            .map(source_coverage)
            .collect(),
        explanations: value
            .explanations
            .into_iter()
            .map(|explanation| TemporalExplanationV1 {
                anchor: explanation.anchor.as_str().to_owned(),
                summary: explanation.summary,
            })
            .collect(),
        omissions: value
            .omissions
            .into_iter()
            .map(|omission| TemporalOmissionV1 {
                rank: omission.rank,
                anchor: omission.anchor.as_str().to_owned(),
                reason: hydration(omission.reason),
            })
            .collect(),
        next_cursor: value.cursor,
        freshness: Some(match freshness {
            SessionDataFreshness::Fresh => TemporalFreshnessV1::Fresh,
            SessionDataFreshness::Stored { generation_lag } => {
                TemporalFreshnessV1::Stored { generation_lag }
            }
            SessionDataFreshness::Partial { generation_lag } => {
                TemporalFreshnessV1::Partial { generation_lag }
            }
        }),
    }
}

const fn coverage(value: TemporalCoverageCountsV1) -> TemporalCoverageV1 {
    TemporalCoverageV1 {
        visible: value.visible,
        hidden: value.hidden,
        unknown: value.unknown,
        redacted: value.redacted,
    }
}

pub(super) fn source_coverage(value: SessionSourceCoverageV1) -> WireSourceCoverageV1 {
    WireSourceCoverageV1 {
        source_id: value.source_id().as_str().to_owned(),
        observed_frontier: value.observed_frontier().value(),
        committed_frontier: value.committed_frontier().value(),
        target_watermark: value.target_watermark().value(),
        request: SessionCoverageRequestV1 {
            mode: coverage_mode(value.request().mode()),
        },
        covered_intervals: value
            .covered_intervals()
            .iter()
            .cloned()
            .map(coverage_interval)
            .collect(),
        missing_intervals: value
            .missing_intervals()
            .iter()
            .cloned()
            .map(coverage_interval)
            .collect(),
        state: coverage_state(value.state()),
        reason: coverage_reason(value.reason()),
    }
}

fn coverage_interval(value: SessionSourceCoverageIntervalV1) -> SessionCoverageIntervalV1 {
    SessionCoverageIntervalV1 {
        knowledge: ClosedUtcIntervalV1 {
            from_inclusive: value.knowledge.from_inclusive().map(|value| value.0),
            through_inclusive: value.knowledge.through_inclusive().map(|value| value.0),
        },
        valid: match value.valid {
            DomainValidCoverageIntervalV1::Known(interval) => {
                ValidCoverageIntervalV1::Known(ClosedUtcIntervalV1 {
                    from_inclusive: interval.from_inclusive().map(|value| value.0),
                    through_inclusive: interval.through_inclusive().map(|value| value.0),
                })
            }
            DomainValidCoverageIntervalV1::Unknown => ValidCoverageIntervalV1::Unknown,
        },
    }
}

const fn coverage_mode(value: TemporalModeV1) -> SessionCoverageModeV1 {
    match value {
        TemporalModeV1::Current => SessionCoverageModeV1::Current,
        TemporalModeV1::AsOf { cutoff } => SessionCoverageModeV1::AsOf { cutoff: cutoff.0 },
        TemporalModeV1::Evolution => SessionCoverageModeV1::Evolution,
        TemporalModeV1::Forensic => SessionCoverageModeV1::Forensic,
    }
}

const fn coverage_state(value: SessionSourceCoverageStateV1) -> SessionCoverageStateV1 {
    match value {
        SessionSourceCoverageStateV1::Fresh => SessionCoverageStateV1::Fresh,
        SessionSourceCoverageStateV1::Stale => SessionCoverageStateV1::Stale,
        SessionSourceCoverageStateV1::Partial => SessionCoverageStateV1::Partial,
        SessionSourceCoverageStateV1::Locked => SessionCoverageStateV1::Locked,
        SessionSourceCoverageStateV1::Redacted => SessionCoverageStateV1::Redacted,
        SessionSourceCoverageStateV1::RetentionWithheld => {
            SessionCoverageStateV1::RetentionWithheld
        }
        SessionSourceCoverageStateV1::Unavailable => SessionCoverageStateV1::Unavailable,
    }
}

fn coverage_reason(value: &SessionSourceCoverageReasonV1) -> SessionCoverageReasonV1 {
    match value {
        SessionSourceCoverageReasonV1::CaughtUp => SessionCoverageReasonV1::CaughtUp,
        SessionSourceCoverageReasonV1::ProjectionBehindSource { lag } => {
            SessionCoverageReasonV1::ProjectionBehindSource { lag: *lag }
        }
        SessionSourceCoverageReasonV1::SourceBehindTarget { lag } => {
            SessionCoverageReasonV1::SourceBehindTarget { lag: *lag }
        }
        SessionSourceCoverageReasonV1::ProjectionAndSourceBehind {
            projection_lag,
            source_lag,
        } => SessionCoverageReasonV1::ProjectionAndSourceBehind {
            projection_lag: *projection_lag,
            source_lag: *source_lag,
        },
        SessionSourceCoverageReasonV1::Locked => SessionCoverageReasonV1::Locked,
        SessionSourceCoverageReasonV1::Redacted => SessionCoverageReasonV1::Redacted,
        SessionSourceCoverageReasonV1::RetentionWithheld => {
            SessionCoverageReasonV1::RetentionWithheld
        }
        SessionSourceCoverageReasonV1::Unavailable => SessionCoverageReasonV1::Unavailable,
    }
}

const fn hydration(value: HydrationStateV1) -> HydrationStateResultV1 {
    match value {
        HydrationStateV1::Available => HydrationStateResultV1::Available,
        HydrationStateV1::RetainedButUnavailable => HydrationStateResultV1::RetainedButUnavailable,
        HydrationStateV1::Redacted => HydrationStateResultV1::Redacted,
        HydrationStateV1::Deleted => HydrationStateResultV1::Deleted,
        HydrationStateV1::RetentionExpired => HydrationStateResultV1::RetentionExpired,
        HydrationStateV1::Unauthorized => HydrationStateResultV1::Unauthorized,
        HydrationStateV1::Locked => HydrationStateResultV1::Locked,
        HydrationStateV1::UnverifiableLegacy => HydrationStateResultV1::UnverifiableLegacy,
    }
}

//! Retained LCM request mapping over admitted daemon temporal retrieval.

use tracedecay_application::retained_surfaces::{
    LcmDescribeRequestV1, LcmDescribeResultV1, LcmDescribeTargetV1, LcmExpandQueryRequestV1,
    LcmExpandRequestV1, LcmExpandResultV1, LcmExpandTargetV1, LcmGrepRequestV1, LcmGrepResultV1,
    LcmGrepSortV1, LcmLoadSessionRequestV1, LcmLoadSessionResultV1, LcmNodeIdV1, LcmSearchScopeV1,
    RetainedOutcomeStatusV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationOutcome, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
};
use tracedecay_domain::{HydrationStateV1, RetrievalGrainV1, SessionId, TemporalModeV1};
use tracedecay_sessions::runtime::git_correlation::GitScopeFilter;
use tracedecay_sessions::runtime::lcm::{
    LcmContentSlice, LcmDescribeTarget, LcmExpandQueryPagination, LcmExpandQueryResponse,
    LcmExpandTarget, LcmSourceRef,
};
use tracedecay_sessions::runtime::{
    SessionMessageType, SessionSearchScope, SessionSearchTimeRange,
};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ports::ExecutionLimits;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::session::{SessionRetrievalScope, SessionTemporalQuery};

use super::super::receipts::evidence_outcome;
use super::output;
use super::{
    bounded_text, cursor, message_type, optional_provider, optional_usize, relationship_scope,
    required, role_name, session_id, specific_provider, temporal_mode, time_filter, trimmed,
    unsigned_i64,
};
use crate::daemon::session_retrieval::{
    LcmDescribeServiceCommand, LcmDescribeServiceOutcome, LcmExpandServiceCommand,
    LcmExpandServiceOutcome, SessionApplicationRetrievalPortV1, SessionRetrievalCommand,
    SessionRetrievalFilters, SessionRetrievalServiceOutcome, SessionRetrievalStoreScope,
    SessionTemporalMetadataView,
};
use crate::timeutil::SearchTimeBound;

const MAX_RESULTS: usize = 100;
// The admitted retrieval ceiling. Default ExecutionLimits are multi-MiB and
// fail within_request_budgets as a persistent BudgetExhausted / Saturated, so
// every query built here is sized against the one shared constant.
const ADMITTED_RETRIEVAL_BYTE_LIMIT: usize =
    crate::daemon::session_retrieval::APPLICATION_RETRIEVAL_MAX_BYTES as usize;
const DEFAULT_CONTENT_LIMIT: usize = 4_096;
const MAX_CONTENT_LIMIT: usize = 8_192;
const MAX_LOAD_CONTENT_LIMIT: usize = 20_000;
const DEFAULT_QUERY_CONTEXT_LIMIT: usize = 32_000;
const MAX_QUERY_CONTEXT_LIMIT: usize = 65_536;
const MAX_QUERY_PROMPT_CHARS: usize = 2_048;
const MAX_QUERY_QUERY_CHARS: usize = 1_024;

pub(super) async fn execute_load_session(
    service: Option<&dyn SessionApplicationRetrievalPortV1>,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    request: &LcmLoadSessionRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let service = service.ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let session_id = session_id(&request.session_id)?;
    let provider = optional_provider(request.provider.as_deref())?;
    let requested_content_limit =
        optional_usize(request.content_limit)?.unwrap_or(DEFAULT_CONTENT_LIMIT);
    if requested_content_limit == 0 {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    let content_limit = requested_content_limit.min(MAX_LOAD_CONTENT_LIMIT);
    let content_limit_clamped_from =
        (requested_content_limit > content_limit).then_some(requested_content_limit);
    let slice = LcmContentSlice {
        offset: optional_usize(request.content_offset)?.unwrap_or(0),
        limit: content_limit,
    };
    let mut roles = request.roles.clone().unwrap_or_default();
    if roles.iter().any(|role| role.trim().is_empty()) {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    roles
        .iter_mut()
        .for_each(|role| *role = role.trim().to_owned());
    if let Some(role) = trimmed(request.role.as_deref())?
        && !roles.iter().any(|candidate| candidate == role)
    {
        roles.push(role.to_owned());
    }
    let query = retrieval_query(
        &session_id,
        provider,
        "",
        cursor(request.cursor.as_deref())?,
        temporal_mode(
            request.temporal_mode,
            request.as_of_micros,
            TemporalModeV1::Forensic,
        )?,
        bounded_limit(request.limit, 50)?,
        default_context_budget(),
        SessionRetrievalScope::Session(session_id.clone()),
        SessionSearchScope::All,
        SessionMessageType::All,
        roles,
        SessionSearchTimeRange {
            start_time: unsigned_i64(request.start_time)?,
            end_time: unsigned_i64(request.end_time)?,
        },
        None,
        false,
        GitScopeFilter::default(),
    )?;
    let (results, temporal, status, omitted) = retrieval_page(
        service
            .retrieve_admitted_with_cancellation(
                context.request_context,
                context.cancellation_signal,
                query,
            )
            .await,
    )?;
    let messages = results
        .into_iter()
        .map(|result| output::sliced_message(result, slice))
        .collect();
    evidence_outcome(
        context,
        RetainedSurfaceOperation::LcmLoadSession,
        RetainedSurfaceResultV1::LcmLoadSession(LcmLoadSessionResultV1 {
            status,
            messages,
            provider: Some(provider.unwrap_or("all").to_owned()),
            session_id: Some(session_id.as_str().to_owned()),
            content_limit: Some(content_limit),
            content_limit_clamped_from,
            omitted: Some(omitted),
            temporal: Some(output::temporal_fields(temporal)),
            error: None,
            service_status: None,
            capped_sessions: None,
        }),
    )
}

pub(super) async fn execute_grep(
    service: Option<&dyn SessionApplicationRetrievalPortV1>,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    request: &LcmGrepRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let service = service.ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let query_text = required(&request.query)?;
    if !matches!(request.sort, None | Some(LcmGrepSortV1::Relevance)) {
        return Err(RetainedSurfaceExecutionErrorV1::Unsupported);
    }
    let scope = request.scope.unwrap_or(LcmSearchScopeV1::All);
    let anchor = match scope {
        LcmSearchScopeV1::All => session_id("session.lcm-grep.root"),
        LcmSearchScopeV1::Current | LcmSearchScopeV1::Session => request
            .session_id
            .as_deref()
            .ok_or(RetainedSurfaceExecutionErrorV1::InvalidRequest)
            .and_then(session_id),
    }?;
    let retrieval_scope = match scope {
        LcmSearchScopeV1::All => SessionRetrievalScope::AllSessionsInAuthorizedRoot,
        LcmSearchScopeV1::Current | LcmSearchScopeV1::Session => {
            SessionRetrievalScope::Session(anchor.clone())
        }
    };
    let provider = optional_provider(request.provider.as_deref())?;
    let relationship_scope = relationship_scope(request.relationship_scope);
    let message_type = message_type(request.message_type);
    let roles = request
        .role
        .map(|role| vec![role_name(role).to_owned()])
        .unwrap_or_default();
    let start = request.start_time.as_ref().or(request.since.as_ref());
    let end = request.end_time.as_ref().or(request.until.as_ref());
    let git_filter = GitScopeFilter::from_args(
        trimmed(request.branch.as_deref())?,
        trimmed(request.worktree.as_deref())?,
        trimmed(request.commit.as_deref())?,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    let query = retrieval_query(
        &anchor,
        provider,
        query_text,
        cursor(request.cursor.as_deref())?,
        temporal_mode(
            request.temporal_mode,
            request.as_of_micros,
            TemporalModeV1::Current,
        )?,
        bounded_limit(request.limit, 10)?,
        default_context_budget(),
        retrieval_scope,
        relationship_scope,
        message_type,
        roles,
        SessionSearchTimeRange {
            start_time: time_filter(start, SearchTimeBound::Start)?,
            end_time: time_filter(end, SearchTimeBound::End)?,
        },
        trimmed(request.source.as_deref())?.map(str::to_owned),
        request.include_summaries.unwrap_or(false),
        git_filter,
    )?;
    let (results, temporal, status, omitted) = retrieval_page(
        service
            .retrieve_admitted_with_cancellation(
                context.request_context,
                context.cancellation_signal,
                query,
            )
            .await,
    )?;
    let hits = results
        .into_iter()
        .map(|result| output::grep_hit(result, DEFAULT_CONTENT_LIMIT))
        .collect::<Vec<_>>();
    evidence_outcome(
        context,
        RetainedSurfaceOperation::LcmGrep,
        RetainedSurfaceResultV1::LcmGrep(LcmGrepResultV1 {
            status,
            count: Some(hits.len()),
            hits,
            provider: Some(provider.unwrap_or("all").to_owned()),
            query: Some(query_text.to_owned()),
            sort: Some("relevance".to_owned()),
            relationship_scope: Some(relationship_scope.as_str().to_owned()),
            message_type: Some(message_type.as_str().to_owned()),
            capped_sessions: Some(Default::default()),
            omitted: Some(omitted),
            temporal: Some(output::temporal_fields(temporal)),
            error: None,
            service_status: None,
        }),
    )
}

pub(super) async fn execute_describe(
    service: Option<&dyn SessionApplicationRetrievalPortV1>,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    request: &LcmDescribeRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let service = service.ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let provider = specific_provider(&request.provider)?;
    let session_id = session_id(&request.session_id)?;
    let (target, grain) = match request.target.as_ref() {
        None | Some(LcmDescribeTargetV1::Session) => {
            (LcmDescribeTarget::Session, RetrievalGrainV1::Session)
        }
        Some(LcmDescribeTargetV1::SummaryNode { node_id }) => (
            LcmDescribeTarget::SummaryNode {
                node_id: required(node_id)?.to_owned(),
            },
            RetrievalGrainV1::Summary,
        ),
        Some(LcmDescribeTargetV1::ExternalPayload { payload_ref }) => (
            LcmDescribeTarget::ExternalPayload {
                payload_ref: required(payload_ref)?.to_owned(),
            },
            RetrievalGrainV1::Occurrence,
        ),
    };
    let outcome = service
        .describe_lcm_admitted(
            context.request_context,
            context.cancellation_signal,
            LcmDescribeServiceCommand::new(
                provider,
                session_id.clone(),
                target,
                grain,
                SessionRetrievalStoreScope::Profile,
            ),
        )
        .await;
    let result = match outcome {
        LcmDescribeServiceOutcome::Complete {
            description,
            temporal,
            grain,
            state,
            lineage,
            retrieval,
        } => LcmDescribeResultV1 {
            status: RetainedOutcomeStatusV1::Ok,
            description: Some(output::description(description)),
            provider: Some(provider.to_owned()),
            session_id: Some(session_id.as_str().to_owned()),
            grain: Some(grain.as_str().to_owned()),
            state: Some(output::hydration(state)),
            lineage: Some(output::lineage(lineage)),
            retrieval: Some(output::retrieval(retrieval)),
            omitted: Some(retrieval.omitted()),
            temporal: Some(output::temporal_fields(temporal)),
            error: None,
            service_status: None,
            capped_sessions: None,
        },
        LcmDescribeServiceOutcome::Partial {
            description,
            temporal,
            grain,
            state,
            lineage,
            retrieval,
        } => LcmDescribeResultV1 {
            status: RetainedOutcomeStatusV1::Partial,
            description: description.map(output::description),
            provider: Some(provider.to_owned()),
            session_id: Some(session_id.as_str().to_owned()),
            grain: Some(grain.as_str().to_owned()),
            state: state.map(output::hydration),
            lineage: Some(output::lineage(lineage)),
            retrieval: Some(output::retrieval(retrieval)),
            omitted: Some(retrieval.omitted()),
            temporal: Some(output::temporal_fields(temporal)),
            error: None,
            service_status: None,
            capped_sessions: None,
        },
        LcmDescribeServiceOutcome::Stale {
            temporal,
            retrieval,
        } => LcmDescribeResultV1 {
            status: RetainedOutcomeStatusV1::Stale,
            description: None,
            provider: Some(provider.to_owned()),
            session_id: Some(session_id.as_str().to_owned()),
            grain: None,
            state: None,
            lineage: None,
            retrieval: Some(output::retrieval(retrieval)),
            omitted: Some(retrieval.omitted()),
            temporal: Some(output::temporal_fields(temporal)),
            error: None,
            service_status: None,
            capped_sessions: None,
        },
        terminal => return Err(describe_error(terminal)),
    };
    evidence_outcome(
        context,
        RetainedSurfaceOperation::LcmDescribe,
        RetainedSurfaceResultV1::LcmDescribe(result),
    )
}

pub(super) async fn execute_expand(
    service: Option<&dyn SessionApplicationRetrievalPortV1>,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    request: &LcmExpandRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let service = service.ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let provider = specific_provider(&request.provider)?;
    let session_id = session_id(&request.session_id)?;
    let (target, grain, summary) = match &request.target {
        LcmExpandTargetV1::RawMessage { store_id } => {
            let store_id = i64::try_from(*store_id)
                .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
            (
                LcmExpandTarget::RawMessage { store_id },
                RetrievalGrainV1::Occurrence,
                false,
            )
        }
        LcmExpandTargetV1::SummaryNode { node_id } => (
            LcmExpandTarget::SummaryNode {
                node_id: required(node_id)?.to_owned(),
            },
            RetrievalGrainV1::Summary,
            true,
        ),
        LcmExpandTargetV1::ExternalPayload { payload_ref } => (
            LcmExpandTarget::ExternalPayload {
                payload_ref: required(payload_ref)?.to_owned(),
            },
            RetrievalGrainV1::Occurrence,
            false,
        ),
    };
    if !summary && (request.source_limit.is_some() || request.cursor.is_some()) {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    let source_limit = summary
        .then(|| bounded_limit(request.source_limit, 50))
        .transpose()?;
    let outcome = service
        .expand_lcm_admitted(
            context.request_context,
            context.cancellation_signal,
            LcmExpandServiceCommand::new(
                provider,
                session_id.clone(),
                target,
                grain,
                LcmContentSlice {
                    offset: optional_usize(request.content_offset)?.unwrap_or(0),
                    limit: bounded_value(
                        request.content_limit,
                        DEFAULT_CONTENT_LIMIT,
                        MAX_CONTENT_LIMIT,
                    )?,
                },
                source_limit,
                cursor(request.cursor.as_deref())?,
                SessionRetrievalStoreScope::Profile,
            ),
        )
        .await;
    let result = expand_result(outcome, provider, &session_id)?;
    evidence_outcome(
        context,
        RetainedSurfaceOperation::LcmExpand,
        RetainedSurfaceResultV1::LcmExpand(Box::new(result)),
    )
}

pub(super) async fn execute_expand_query(
    service: Option<&dyn SessionApplicationRetrievalPortV1>,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    request: &LcmExpandQueryRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let service = service.ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let provider = specific_provider(&request.provider)?;
    let session_id = session_id(&request.session_id)?;
    let prompt = bounded_text(&request.prompt, MAX_QUERY_PROMPT_CHARS)?;
    let query = request
        .query
        .as_deref()
        .map(|value| bounded_text(value, MAX_QUERY_QUERY_CHARS))
        .transpose()?;
    let node_ids = request
        .node_ids
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|node| match node {
            LcmNodeIdV1::Text(value) => required(value).map(str::to_owned),
            LcmNodeIdV1::Numeric(_) => Err(RetainedSurfaceExecutionErrorV1::InvalidRequest),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let max_results = bounded_value(request.max_results, 5, MAX_RESULTS)?;
    let max_tokens = bounded_value(request.max_tokens, 2_000, MAX_CONTENT_LIMIT)?;
    let context_max_tokens = bounded_value(
        request.context_max_tokens,
        DEFAULT_QUERY_CONTEXT_LIMIT,
        MAX_QUERY_CONTEXT_LIMIT,
    )?;
    let cursor = cursor(request.cursor.as_deref())?;
    if cursor.is_some() && node_ids.len() > 1 {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    let (response, status, omitted, temporal) = if node_ids.is_empty() {
        expand_query_from_search(
            service,
            context,
            provider,
            &session_id,
            prompt,
            query,
            cursor,
            max_results,
            max_tokens,
            context_max_tokens,
        )
        .await?
    } else {
        expand_query_from_nodes(
            service,
            context,
            provider,
            &session_id,
            prompt,
            query,
            node_ids,
            cursor,
            max_results,
            max_tokens,
            context_max_tokens,
        )
        .await?
    };
    evidence_outcome(
        context,
        RetainedSurfaceOperation::LcmExpandQuery,
        RetainedSurfaceResultV1::LcmExpandQuery(output::expand_query_result(
            response,
            status,
            omitted,
            provider,
            session_id.as_str(),
            output::temporal_fields(temporal),
        )),
    )
}

fn retrieval_query(
    session_id: &SessionId,
    provider: Option<&str>,
    query_text: &str,
    cursor: Option<String>,
    temporal_mode: TemporalModeV1,
    limit: usize,
    context_budget: ContextBudget,
    retrieval_scope: SessionRetrievalScope,
    relationship_scope: SessionSearchScope,
    message_type: SessionMessageType,
    roles: Vec<String>,
    time_range: SessionSearchTimeRange,
    source: Option<String>,
    include_summaries: bool,
    git_filter: GitScopeFilter,
) -> Result<SessionTemporalQuery, RetainedSurfaceExecutionErrorV1> {
    let query = SessionTemporalQuery::new(
        session_id.clone(),
        provider.map(str::to_owned),
        query_text,
        cursor,
        temporal_mode,
        RetrievalGrainV1::Occurrence,
        limit,
        DiversityLimits::default(),
        context_budget,
    )
    .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?
    .with_retrieval_scope(retrieval_scope)
    .with_execution_limits(admitted_execution_limits(limit));
    Ok(SessionRetrievalCommand::new(
        query,
        SessionRetrievalFilters {
            project_key: None,
            parent_session_id: None,
            source,
            include_summaries,
            scope: relationship_scope,
            message_type,
            roles,
            time_range,
            git_filter,
            workflow_scope: None,
        },
        false,
    )
    .query()
    .clone())
}

fn admitted_execution_limits(limit: usize) -> ExecutionLimits {
    ExecutionLimits {
        candidate_limit: limit,
        candidate_total_bytes: ADMITTED_RETRIEVAL_BYTE_LIMIT,
        candidate_item_bytes: ADMITTED_RETRIEVAL_BYTE_LIMIT,
        candidate_metadata_field_bytes: 16 * 1024,
        record_limit: limit,
        record_total_bytes: ADMITTED_RETRIEVAL_BYTE_LIMIT,
        record_item_bytes: ADMITTED_RETRIEVAL_BYTE_LIMIT,
        hydration_limit: limit,
        hydration_total_bytes: ADMITTED_RETRIEVAL_BYTE_LIMIT,
        hydration_payload_bytes: ADMITTED_RETRIEVAL_BYTE_LIMIT,
        hydration_chunk_bytes: 16 * 1024,
        ..ExecutionLimits::default()
    }
}

fn retrieval_page(
    outcome: SessionRetrievalServiceOutcome,
) -> Result<
    (
        Vec<tracedecay_sessions::runtime::SessionMessageSearchResult>,
        SessionTemporalMetadataView,
        RetainedOutcomeStatusV1,
        u64,
    ),
    RetainedSurfaceExecutionErrorV1,
> {
    match outcome {
        SessionRetrievalServiceOutcome::Complete { page, .. } => {
            Ok((page.results, page.temporal, RetainedOutcomeStatusV1::Ok, 0))
        }
        SessionRetrievalServiceOutcome::CompleteZero { temporal, .. } => {
            Ok((Vec::new(), temporal, RetainedOutcomeStatusV1::Ok, 0))
        }
        SessionRetrievalServiceOutcome::Partial { page, omitted, .. } => Ok((
            page.results,
            page.temporal,
            RetainedOutcomeStatusV1::Partial,
            omitted,
        )),
        SessionRetrievalServiceOutcome::Stale { temporal, .. } => {
            Ok((Vec::new(), temporal, RetainedOutcomeStatusV1::Stale, 0))
        }
        terminal => Err(retrieval_error(terminal)),
    }
}

fn retrieval_error(outcome: SessionRetrievalServiceOutcome) -> RetainedSurfaceExecutionErrorV1 {
    match outcome {
        SessionRetrievalServiceOutcome::ResetRequired { store_scope } => {
            reset_required_error(store_scope)
        }
        SessionRetrievalServiceOutcome::WrongScope
        | SessionRetrievalServiceOutcome::Denied
        | SessionRetrievalServiceOutcome::Redacted
        | SessionRetrievalServiceOutcome::Deleted => {
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        }
        SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { .. }
        | SessionRetrievalServiceOutcome::BudgetExhausted => {
            RetainedSurfaceExecutionErrorV1::Saturated
        }
        SessionRetrievalServiceOutcome::Cancelled => RetainedSurfaceExecutionErrorV1::Cancelled(
            tracedecay_application::CancellationStage::DuringRead,
        ),
        SessionRetrievalServiceOutcome::Locked
        | SessionRetrievalServiceOutcome::Unavailable(_)
        | SessionRetrievalServiceOutcome::Complete { .. }
        | SessionRetrievalServiceOutcome::CompleteZero { .. }
        | SessionRetrievalServiceOutcome::Partial { .. }
        | SessionRetrievalServiceOutcome::Stale { .. } => {
            RetainedSurfaceExecutionErrorV1::Unavailable
        }
    }
}

fn describe_error(outcome: LcmDescribeServiceOutcome) -> RetainedSurfaceExecutionErrorV1 {
    match outcome {
        LcmDescribeServiceOutcome::ResetRequired { store_scope } => {
            reset_required_error(store_scope)
        }
        LcmDescribeServiceOutcome::WrongScope
        | LcmDescribeServiceOutcome::Denied
        | LcmDescribeServiceOutcome::Redacted
        | LcmDescribeServiceOutcome::Deleted => {
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        }
        LcmDescribeServiceOutcome::BudgetExhausted => RetainedSurfaceExecutionErrorV1::Saturated,
        LcmDescribeServiceOutcome::Cancelled => RetainedSurfaceExecutionErrorV1::Cancelled(
            tracedecay_application::CancellationStage::DuringRead,
        ),
        LcmDescribeServiceOutcome::Locked
        | LcmDescribeServiceOutcome::Unavailable(_)
        | LcmDescribeServiceOutcome::Complete { .. }
        | LcmDescribeServiceOutcome::Partial { .. }
        | LcmDescribeServiceOutcome::Stale { .. } => RetainedSurfaceExecutionErrorV1::Unavailable,
    }
}

fn expand_error(outcome: LcmExpandServiceOutcome) -> RetainedSurfaceExecutionErrorV1 {
    match outcome {
        LcmExpandServiceOutcome::ResetRequired { store_scope } => reset_required_error(store_scope),
        LcmExpandServiceOutcome::WrongScope
        | LcmExpandServiceOutcome::Denied
        | LcmExpandServiceOutcome::Redacted
        | LcmExpandServiceOutcome::Deleted => {
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        }
        LcmExpandServiceOutcome::BudgetExhausted => RetainedSurfaceExecutionErrorV1::Saturated,
        LcmExpandServiceOutcome::Cancelled => RetainedSurfaceExecutionErrorV1::Cancelled(
            tracedecay_application::CancellationStage::DuringRead,
        ),
        LcmExpandServiceOutcome::Locked
        | LcmExpandServiceOutcome::Unavailable(_)
        | LcmExpandServiceOutcome::Complete { .. }
        | LcmExpandServiceOutcome::Partial { .. }
        | LcmExpandServiceOutcome::Stale { .. } => RetainedSurfaceExecutionErrorV1::Unavailable,
    }
}

fn reset_required_error(
    store_scope: SessionRetrievalStoreScope,
) -> RetainedSurfaceExecutionErrorV1 {
    match store_scope {
        SessionRetrievalStoreScope::Project => {
            RetainedSurfaceExecutionErrorV1::ProjectResetRequired
        }
        SessionRetrievalStoreScope::Profile => {
            RetainedSurfaceExecutionErrorV1::ProfileResetRequired
        }
    }
}

fn expand_result(
    outcome: LcmExpandServiceOutcome,
    provider: &str,
    session_id: &SessionId,
) -> Result<LcmExpandResultV1, RetainedSurfaceExecutionErrorV1> {
    let (status, expansion, temporal, grain, state, retrieval) = match outcome {
        LcmExpandServiceOutcome::Complete {
            expansion,
            temporal,
            grain,
            state,
            retrieval,
        } => (
            RetainedOutcomeStatusV1::Ok,
            Some(expansion),
            temporal,
            Some(grain),
            Some(state),
            retrieval,
        ),
        LcmExpandServiceOutcome::Partial {
            expansion,
            temporal,
            grain,
            state,
            retrieval,
        } => (
            RetainedOutcomeStatusV1::Partial,
            expansion,
            temporal,
            Some(grain),
            state,
            retrieval,
        ),
        LcmExpandServiceOutcome::Stale {
            temporal,
            retrieval,
        } => (
            RetainedOutcomeStatusV1::Stale,
            None,
            temporal,
            None,
            None,
            retrieval,
        ),
        terminal => return Err(expand_error(terminal)),
    };
    Ok(LcmExpandResultV1 {
        status,
        expansion: expansion.map(output::expansion),
        provider: Some(provider.to_owned()),
        session_id: Some(session_id.as_str().to_owned()),
        grain: grain.map(|value| value.as_str().to_owned()),
        state: state.map(output::hydration),
        retrieval: Some(output::retrieval(retrieval)),
        omitted: Some(retrieval.omitted()),
        temporal: Some(output::temporal_fields(temporal)),
        error: None,
        service_status: None,
        capped_sessions: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn expand_query_from_search(
    service: &dyn SessionApplicationRetrievalPortV1,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    provider: &str,
    session_id: &SessionId,
    prompt: &str,
    query: Option<&str>,
    cursor: Option<String>,
    max_results: usize,
    max_tokens: usize,
    context_max_tokens: usize,
) -> Result<
    (
        LcmExpandQueryResponse,
        RetainedOutcomeStatusV1,
        u64,
        SessionTemporalMetadataView,
    ),
    RetainedSurfaceExecutionErrorV1,
> {
    let temporal_query = retrieval_query(
        session_id,
        Some(provider),
        query.unwrap_or(prompt),
        cursor,
        TemporalModeV1::Current,
        max_results,
        admitted_context_budget(context_max_tokens),
        SessionRetrievalScope::Session(session_id.clone()),
        SessionSearchScope::All,
        SessionMessageType::All,
        Vec::new(),
        SessionSearchTimeRange::default(),
        None,
        false,
        GitScopeFilter::default(),
    )?;
    let (results, temporal, mut status, service_omitted) = retrieval_page(
        service
            .retrieve_admitted_with_cancellation(
                context.request_context,
                context.cancellation_signal,
                temporal_query,
            )
            .await,
    )?;
    let sources = results
        .into_iter()
        .map(|result| ("raw_message", None, result.message.text))
        .collect();
    let (response, dropped) =
        output::assemble_query(prompt, query, max_tokens, context_max_tokens, sources)?;
    let dropped = u64::try_from(dropped).map_err(|_| RetainedSurfaceExecutionErrorV1::Saturated)?;
    let omitted = service_omitted.saturating_add(dropped);
    if status == RetainedOutcomeStatusV1::Ok && omitted > 0 {
        status = RetainedOutcomeStatusV1::Partial;
    }
    Ok((response, status, omitted, temporal))
}

#[allow(clippy::too_many_arguments)]
async fn expand_query_from_nodes(
    service: &dyn SessionApplicationRetrievalPortV1,
    context: &RetainedSurfaceExecutionContextV1<'_>,
    provider: &str,
    session_id: &SessionId,
    prompt: &str,
    query: Option<&str>,
    node_ids: Vec<String>,
    cursor: Option<String>,
    max_results: usize,
    max_tokens: usize,
    context_max_tokens: usize,
) -> Result<
    (
        LcmExpandQueryResponse,
        RetainedOutcomeStatusV1,
        u64,
        SessionTemporalMetadataView,
    ),
    RetainedSurfaceExecutionErrorV1,
> {
    let mut sources = Vec::new();
    let mut pagination = Vec::new();
    let mut temporal = SessionTemporalMetadataView::default();
    let mut omitted = node_ids.len().saturating_sub(max_results);
    for node_id in node_ids.iter().take(max_results) {
        let outcome = service
            .expand_lcm_admitted(
                context.request_context,
                context.cancellation_signal,
                LcmExpandServiceCommand::new(
                    provider,
                    session_id.clone(),
                    LcmExpandTarget::SummaryNode {
                        node_id: node_id.clone(),
                    },
                    RetrievalGrainV1::Summary,
                    LcmContentSlice {
                        offset: 0,
                        limit: context_max_tokens.min(MAX_CONTENT_LIMIT),
                    },
                    Some(max_results),
                    cursor.clone(),
                    SessionRetrievalStoreScope::Profile,
                ),
            )
            .await;
        let (expansion, incoming, retrieval) = match outcome {
            LcmExpandServiceOutcome::Complete {
                expansion,
                temporal,
                retrieval,
                ..
            } => (expansion, temporal, retrieval),
            LcmExpandServiceOutcome::Partial {
                expansion: Some(expansion),
                temporal,
                retrieval,
                ..
            } => (expansion, temporal, retrieval),
            terminal => return Err(expand_error(terminal)),
        };
        omitted = omitted.saturating_add(
            usize::try_from(retrieval.omitted())
                .map_err(|_| RetainedSurfaceExecutionErrorV1::Saturated)?,
        );
        for source in expansion.summary_sources {
            let kind = match &source.source_ref {
                LcmSourceRef::RawMessage { .. } => "raw_message",
                LcmSourceRef::SummaryNode { .. } => "summary_source",
            };
            pagination.push(LcmExpandQueryPagination {
                kind: kind.to_owned(),
                node_id: Some(node_id.clone()),
                source_ref: Some(source.source_ref),
                state: Some(source.state),
                next_content_offset: source.content_range.as_ref().and_then(|range| {
                    range
                        .truncated
                        .then_some(range.offset.saturating_add(range.returned_chars))
                }),
                has_more: source.content_truncated,
            });
            if source.state == HydrationStateV1::Available {
                sources.push((kind, Some(node_id.clone()), source.content));
            }
        }
        sources.push(("summary_node", Some(node_id.clone()), expansion.content));
        if !output::merge_temporal(&mut temporal, incoming) {
            return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
        }
    }
    let (mut response, dropped) =
        output::assemble_query(prompt, query, max_tokens, context_max_tokens, sources)?;
    response.context_pagination = pagination;
    omitted = omitted.saturating_add(dropped);
    let omitted = u64::try_from(omitted).map_err(|_| RetainedSurfaceExecutionErrorV1::Saturated)?;
    let status = if omitted == 0 {
        RetainedOutcomeStatusV1::Ok
    } else {
        RetainedOutcomeStatusV1::Partial
    };
    Ok((response, status, omitted, temporal))
}

/// The retrieval-side context budget for a request that asked for
/// `requested_tokens` of assembled context.
///
/// The admitted daemon retrieval service rejects any query whose
/// `ContextBudget::max_bytes` exceeds `APPLICATION_RETRIEVAL_MAX_BYTES`, and
/// that rejection is terminal (`BudgetExhausted` -> `Saturated`), not a partial
/// answer. A caller-supplied assembly budget therefore must not be forwarded to
/// retrieval unclamped: `context_max_tokens` defaults to 32_000 tokens, which
/// is 128_000 estimated bytes and twice the admitted ceiling, so every default
/// `lcm_expand_query` would be refused before it read a single message. The
/// assembly budget stays whole; only the retrieval window is bounded here.
fn admitted_context_budget(requested_tokens: usize) -> ContextBudget {
    let max_bytes = requested_tokens
        .saturating_mul(4)
        .min(ADMITTED_RETRIEVAL_BYTE_LIMIT);
    ContextBudget {
        max_bytes: max_bytes as u64,
        max_tokens: (max_bytes / 4) as u64,
        estimator_version: "words-v1".to_owned(),
    }
}

fn default_context_budget() -> ContextBudget {
    admitted_context_budget(ADMITTED_RETRIEVAL_BYTE_LIMIT / 4)
}

fn bounded_value(
    value: Option<u64>,
    default: usize,
    maximum: usize,
) -> Result<usize, RetainedSurfaceExecutionErrorV1> {
    let value = optional_usize(value)?.unwrap_or(default);
    if value == 0 || value > maximum {
        Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
    } else {
        Ok(value)
    }
}

fn bounded_limit(
    value: Option<u64>,
    default: usize,
) -> Result<usize, RetainedSurfaceExecutionErrorV1> {
    bounded_value(value, default, MAX_RESULTS)
}

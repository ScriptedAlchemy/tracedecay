//! Typed retained session-query mappers over mounted daemon authorities.

use tracedecay_application::RetainedSurfaceExecutionErrorV1;
use tracedecay_application::retained_surfaces::{
    CorrelationIndexV1, GitScopeV1, RetainedErrorV1, RetainedOutcomeStatusV1,
    SessionCorrelationHitV1, SessionGitRefV1, SessionGitRelationV1, SessionsForRequestV1,
    SessionsForResultV1, WorkflowAgentV1, WorkflowCoverageV1, WorkflowIndexCoverageV1,
    WorkflowQueryModeV1, WorkflowRunV1, WorkflowStatusV1, WorkflowsRequestV1, WorkflowsResultV1,
};
use tracedecay_sessions::runtime::git_correlation::{
    CommitEvidence, CommitRelation, CommitRelationFilter, GitCorrelationError, GitRefFilter,
    GitScopeFilter, SessionGitCorrelationHit, SessionsForQuery, SpanOverlapKind,
};
use tracedecay_sessions::{
    WorkflowAgent, WorkflowGitScope, WorkflowIndexHealth, WorkflowIndexReadPort,
    WorkflowIndexState, WorkflowIngestCoverage, WorkflowIngestReceipt, WorkflowRun,
    WorkflowRunDetail, WorkflowRunDetailOutcome, WorkflowRunDetailRequest, WorkflowRunListOutcome,
    WorkflowRunListRequest, WorkflowRunScope, WorkflowStatus,
};

use crate::global_db::RegisteredGlobalDb;
use crate::store::GlobalDbGitCorrelationStore;
use crate::timeutil::{SearchTimeBound, parse_search_time_filter_bound};
use crate::tracedecay::current_timestamp;

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

pub(crate) async fn sessions_for(
    database: Option<&RegisteredGlobalDb>,
    request: &SessionsForRequestV1,
) -> Result<SessionsForResultV1, RetainedSurfaceExecutionErrorV1> {
    let Some(database) = database else {
        return Ok(sessions_unavailable(
            "registered project session database is unavailable",
            None,
        ));
    };
    let kind = match request.git_ref {
        SessionGitRefV1::Branch => "branch",
        SessionGitRefV1::Worktree => "worktree",
        SessionGitRefV1::Commit => "commit",
    };
    let git_ref = GitRefFilter::parse(kind, &request.value).map_err(map_git_error)?;
    let since = time_filter(request.since.as_ref(), SearchTimeBound::Start)?;
    let until = time_filter(request.until.as_ref(), SearchTimeBound::End)?;
    if since.zip(until).is_some_and(|(since, until)| since > until) {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    let limit = limit(request.limit)?;
    let relation = match request.relation.unwrap_or(SessionGitRelationV1::Produced) {
        SessionGitRelationV1::Produced => CommitRelationFilter::Produced,
        SessionGitRelationV1::Observed => CommitRelationFilter::Observed,
        SessionGitRelationV1::All => CommitRelationFilter::All,
    };
    let query = SessionsForQuery {
        git_ref,
        since,
        until,
        limit,
    };
    let correlation = GlobalDbGitCorrelationStore::new(database);
    let index_health = correlation.correlation_index_health().await.ok();
    let results = match correlation
        .sessions_for_with_relation(&query, relation)
        .await
    {
        Ok(results) => results,
        Err(GitCorrelationError::Unavailable(message)) => {
            return Ok(sessions_unavailable(
                &message,
                Some("sessions.git-evidence.unavailable"),
            ));
        }
        Err(error) => return Err(map_git_error(error)),
    };
    let observed_fallback = if results.is_empty()
        && matches!(query.git_ref, GitRefFilter::Commit(_))
        && relation == CommitRelationFilter::Produced
    {
        match correlation
            .sessions_for_with_relation(&query, CommitRelationFilter::Observed)
            .await
        {
            Ok(observed) if !observed.is_empty() => Some(observed),
            Ok(_) | Err(GitCorrelationError::Unavailable(_)) => None,
            Err(error) => return Err(map_git_error(error)),
        }
    } else {
        None
    };
    let index_empty = index_health
        .as_ref()
        .is_none_or(|health| health.is_empty_for(&query.git_ref));
    let mut result = SessionsForResultV1 {
        status: RetainedOutcomeStatusV1::Ok,
        git_ref: Some(query.git_ref.kind().to_owned()),
        value: Some(query.git_ref.value().to_owned()),
        since,
        until,
        relation: Some(relation.as_str().to_owned()),
        count: results.len(),
        results: results.into_iter().map(correlation_hit).collect(),
        index_empty: Some(index_empty),
        index: index_health.map(|health| CorrelationIndexV1 {
            projection_available: health.projection_available,
            generation: health.generation,
            source_watermark: health.source_watermark,
            span_count: health.span_count,
            commit_count: health.commit_count,
            backfill_watermark: health.backfill_watermark,
        }),
        message: None,
        observed_count: None,
        observed_sessions: None,
        problem_code: None,
    };
    if result.results.is_empty() {
        if let Some(observed) = observed_fallback {
            let observed_count = observed.len();
            result.observed_count = Some(observed_count);
            result.observed_sessions = Some(observed.into_iter().map(correlation_hit).collect());
            result.message = Some(format!(
                "no producing sessions; {} session(s) observed this commit — pass relation=observed to list them",
                observed_count,
            ));
        } else {
            result.message = Some(if index_empty {
                if matches!(&query.git_ref, GitRefFilter::Commit(_)) {
                    "no commit evidence indexed yet — run `tracedecay sync` to ingest direct host/tool evidence; `tracedecay sessions git-sync` adds weaker historical overlap evidence".to_owned()
                } else {
                    "correlation index empty (no git spans recorded yet) — it will converge on the next daemon startup, or run `tracedecay sessions git-sync` to schedule it now".to_owned()
                }
            } else {
                "no sessions matched this git ref".to_owned()
            });
        }
    }
    Ok(result)
}

pub(crate) async fn workflows(
    workflow_index: Option<&dyn WorkflowIndexReadPort>,
    request: &WorkflowsRequestV1,
) -> Result<WorkflowsResultV1, RetainedSurfaceExecutionErrorV1> {
    let selectors = [
        request
            .run_id
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty()),
        request
            .session_id
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty()),
        request
            .branch
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
            || request
                .worktree
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty())
            || request
                .commit
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty()),
    ];
    if selectors.into_iter().filter(|selected| *selected).count() != 1 {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    let Some(port) = workflow_index else {
        return Ok(workflow_unavailable(
            WorkflowIndexState::AuthorityNotRetained,
        ));
    };
    let limit = limit(request.limit)?;
    let receipt = match port
        .health()
        .await
        .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?
    {
        WorkflowIndexHealth::Ready(receipt) => receipt,
        WorkflowIndexHealth::Unavailable(reason) => return Ok(workflow_unavailable(reason)),
    };
    let mut result = if let Some(run_id) = request
        .run_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        workflow_run_query(port, run_id, request.agent_label.as_deref(), limit).await?
    } else if let Some(session_id) = request
        .session_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        match port
            .runs(WorkflowRunListRequest {
                scope: WorkflowRunScope::Session {
                    session_id: session_id.to_owned(),
                },
                limit,
            })
            .await
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?
        {
            WorkflowRunListOutcome::Runs(runs) => workflow_list(
                WorkflowQueryModeV1::Session,
                Some(session_id.to_owned()),
                None,
                runs,
            ),
            WorkflowRunListOutcome::Unavailable(reason) => workflow_unavailable(reason),
        }
    } else {
        let filter = GitScopeFilter::from_args(
            request.branch.as_deref(),
            request.worktree.as_deref(),
            request.commit.as_deref(),
        )
        .map_err(map_git_error)?;
        match port
            .runs(WorkflowRunListRequest {
                scope: WorkflowRunScope::GitScope(WorkflowGitScope {
                    branch: filter.branch.clone(),
                    worktree: filter.worktree.clone(),
                    commit: filter.commit.clone(),
                }),
                limit,
            })
            .await
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?
        {
            WorkflowRunListOutcome::Runs(runs) => workflow_list(
                WorkflowQueryModeV1::GitScope,
                None,
                Some(GitScopeV1 {
                    branch: filter.branch,
                    worktree: filter.worktree,
                    commit: filter.commit,
                }),
                runs,
            ),
            WorkflowRunListOutcome::Unavailable(reason) => workflow_unavailable(reason),
        }
    };
    if matches!(
        result.status,
        RetainedOutcomeStatusV1::Ok | RetainedOutcomeStatusV1::Partial
    ) {
        apply_receipt(&mut result, receipt);
    }
    Ok(result)
}

fn sessions_unavailable(message: &str, problem_code: Option<&str>) -> SessionsForResultV1 {
    SessionsForResultV1 {
        count: 0,
        results: Vec::new(),
        status: RetainedOutcomeStatusV1::Unavailable,
        git_ref: None,
        index: None,
        index_empty: None,
        message: Some(message.to_owned()),
        observed_count: None,
        observed_sessions: None,
        problem_code: problem_code.map(str::to_owned),
        relation: None,
        since: None,
        until: None,
        value: None,
    }
}

fn map_git_error(error: GitCorrelationError) -> RetainedSurfaceExecutionErrorV1 {
    match error {
        GitCorrelationError::InvalidArgument(_) | GitCorrelationError::Contract(_) => {
            RetainedSurfaceExecutionErrorV1::InvalidRequest
        }
        GitCorrelationError::ProfileResetRequired { .. } => {
            RetainedSurfaceExecutionErrorV1::ProfileResetRequired
        }
        GitCorrelationError::Corrupt(_) => RetainedSurfaceExecutionErrorV1::ProjectResetRequired,
        GitCorrelationError::Cancelled => RetainedSurfaceExecutionErrorV1::Cancelled,
        GitCorrelationError::BudgetExhausted => RetainedSurfaceExecutionErrorV1::TimedOut,
        GitCorrelationError::Db(_) | GitCorrelationError::Unavailable(_) => {
            RetainedSurfaceExecutionErrorV1::Unavailable
        }
    }
}

fn limit(value: Option<u64>) -> Result<usize, RetainedSurfaceExecutionErrorV1> {
    let value = usize::try_from(value.unwrap_or(DEFAULT_LIMIT as u64))
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
    (1..=MAX_LIMIT)
        .contains(&value)
        .then_some(value)
        .ok_or(RetainedSurfaceExecutionErrorV1::InvalidRequest)
}

fn time_filter(
    value: Option<&tracedecay_application::retained_surfaces::RetainedTimeFilterV1>,
    bound: SearchTimeBound,
) -> Result<Option<i64>, RetainedSurfaceExecutionErrorV1> {
    match value {
        None => Ok(None),
        Some(tracedecay_application::retained_surfaces::RetainedTimeFilterV1::Micros(value)) => {
            i64::try_from(*value)
                .map(Some)
                .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)
        }
        Some(tracedecay_application::retained_surfaces::RetainedTimeFilterV1::Expression(
            value,
        )) => value
            .parse::<i64>()
            .ok()
            .filter(|value| *value >= 0)
            .or_else(|| parse_search_time_filter_bound(value, current_timestamp(), bound))
            .map(Some)
            .ok_or(RetainedSurfaceExecutionErrorV1::InvalidRequest),
    }
}

fn correlation_hit(hit: SessionGitCorrelationHit) -> SessionCorrelationHitV1 {
    SessionCorrelationHitV1 {
        provider: hit.provider,
        session_id: hit.session_id,
        branch: hit.branch,
        worktree: hit.worktree,
        first_ts: hit.first_ts,
        last_ts: hit.last_ts,
        event_count: hit.event_count,
        span_count: hit.span_count,
        sources: hit.sources,
        commit_sha: hit.commit_sha,
        committed_at: hit.committed_at,
        span_overlap_kind: hit.span_overlap_kind.map(|value| {
            match value {
                SpanOverlapKind::Direct => "direct",
                SpanOverlapKind::WithinSpan => "within_span",
                SpanOverlapKind::ExtendedWindow => "extended_window",
                SpanOverlapKind::Reflog => "reflog",
            }
            .to_owned()
        }),
        relation: hit.relation.map(|value| {
            match value {
                CommitRelation::Produced => "produced",
                CommitRelation::Observed => "observed",
            }
            .to_owned()
        }),
        evidence: hit.evidence.map(|value| {
            match value {
                CommitEvidence::ToolResult => "tool_result",
                CommitEvidence::HostEvent => "host_event",
                CommitEvidence::HeadObservation => "head_observation",
                CommitEvidence::ReflogOverlap => "reflog_overlap",
                CommitEvidence::TimeOverlap => "time_overlap",
            }
            .to_owned()
        }),
        confidence: hit.confidence,
        evidence_message_id: hit.evidence_message_id,
    }
}

async fn workflow_run_query(
    port: &dyn WorkflowIndexReadPort,
    run_id: &str,
    agent_label: Option<&str>,
    limit: usize,
) -> Result<WorkflowsResultV1, RetainedSurfaceExecutionErrorV1> {
    let outcome = match agent_label {
        Some(label) if !label.trim().is_empty() => {
            port.agent(run_id.to_owned(), label.to_owned()).await
        }
        Some(_) => return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest),
        None => {
            port.run(WorkflowRunDetailRequest {
                run_id: run_id.to_owned(),
                limit,
            })
            .await
        }
    }
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)?;
    let detail = match outcome {
        WorkflowRunDetailOutcome::Run(detail) => detail,
        WorkflowRunDetailOutcome::NotFound => return Ok(workflow_not_found(run_id)),
        WorkflowRunDetailOutcome::Unavailable(reason) => return Ok(workflow_unavailable(reason)),
    };
    let WorkflowRunDetail {
        run,
        agents,
        agent_count,
        agents_complete,
    } = *detail;
    match agent_label {
        Some(label) => {
            let agent = agents
                .iter()
                .find(|agent| agent.agent_label == label)
                .cloned();
            let lookup_complete = agent.is_some() || agents_complete;
            let mut result = empty_workflows(if lookup_complete {
                RetainedOutcomeStatusV1::Ok
            } else {
                RetainedOutcomeStatusV1::Partial
            });
            result.mode = Some(WorkflowQueryModeV1::Agent);
            result.run_id = Some(run_id.to_owned());
            result.agent_label = Some(label.to_owned());
            result.found = lookup_complete.then_some(agent.is_some());
            result.run = Some(workflow_run(run));
            result.agent = agent.map(workflow_agent);
            result.agent_count = Some(agent_count);
            result.agents_returned = Some(agents.len());
            result.lookup_complete = Some(lookup_complete);
            result.lookup_coverage = Some(if lookup_complete {
                WorkflowCoverageV1::Conclusive
            } else {
                WorkflowCoverageV1::BoundedPrefix
            });
            Ok(result)
        }
        None => {
            let mut result = empty_workflows(RetainedOutcomeStatusV1::Ok);
            result.mode = Some(WorkflowQueryModeV1::Run);
            result.run_id = Some(run_id.to_owned());
            result.found = Some(true);
            result.run = Some(workflow_run(run));
            result.agent_count = Some(agent_count);
            result.agents_returned = Some(agents.len());
            result.agents = Some(agents.into_iter().map(workflow_agent).collect());
            result.agents_complete = Some(agents_complete);
            result.agents_coverage = Some(if agents_complete {
                WorkflowCoverageV1::Complete
            } else {
                WorkflowCoverageV1::BoundedPrefix
            });
            Ok(result)
        }
    }
}

fn workflow_list(
    mode: WorkflowQueryModeV1,
    session_id: Option<String>,
    git_filter: Option<GitScopeV1>,
    runs: Vec<WorkflowRun>,
) -> WorkflowsResultV1 {
    let mut result = empty_workflows(RetainedOutcomeStatusV1::Ok);
    result.mode = Some(mode);
    result.session_id = session_id;
    result.git_filter = git_filter;
    result.count = Some(runs.len());
    result.runs = Some(runs.into_iter().map(workflow_run).collect());
    result
}

fn workflow_not_found(run_id: &str) -> WorkflowsResultV1 {
    let mut result = empty_workflows(RetainedOutcomeStatusV1::Ok);
    result.mode = Some(WorkflowQueryModeV1::Run);
    result.run_id = Some(run_id.to_owned());
    result.found = Some(false);
    result.runs = Some(Vec::new());
    result.count = Some(0);
    result
}

fn workflow_unavailable(reason: WorkflowIndexState) -> WorkflowsResultV1 {
    let status = match &reason {
        WorkflowIndexState::IngestionPartial(_) => RetainedOutcomeStatusV1::Stale,
        WorkflowIndexState::IngestionFailed(_) => RetainedOutcomeStatusV1::Error,
        WorkflowIndexState::AuthorityNotRetained
        | WorkflowIndexState::IndexNotBuilt
        | WorkflowIndexState::IngestionNotRun => RetainedOutcomeStatusV1::Unavailable,
    };
    let mut result = empty_workflows(status);
    result.message = Some(reason.message().to_owned());
    result.error = Some(RetainedErrorV1 {
        code: match &reason {
            WorkflowIndexState::IngestionPartial(_) => "workflow_index_stale",
            WorkflowIndexState::IngestionFailed(_) => "workflow_index_failed",
            WorkflowIndexState::AuthorityNotRetained
            | WorkflowIndexState::IndexNotBuilt
            | WorkflowIndexState::IngestionNotRun => "workflow_index_unavailable",
        }
        .to_owned(),
        message: reason.message().to_owned(),
        kind: None,
        maximum: None,
        observed: None,
        reason: Some(reason.as_str().to_owned()),
        retryable: Some(reason.is_retryable()),
    });
    if let Some(receipt) = reason.receipt() {
        apply_receipt(&mut result, receipt);
    }
    result
}

fn empty_workflows(status: RetainedOutcomeStatusV1) -> WorkflowsResultV1 {
    WorkflowsResultV1 {
        status,
        agent: None,
        agent_count: None,
        agent_label: None,
        agents: None,
        agents_complete: None,
        agents_coverage: None,
        agents_returned: None,
        count: None,
        error: None,
        found: None,
        git_filter: None,
        index_attempted_at: None,
        index_coverage: None,
        index_failed_runs: None,
        index_updated_at: None,
        index_watermark_mtime: None,
        lookup_complete: None,
        lookup_coverage: None,
        message: None,
        mode: None,
        run: None,
        run_id: None,
        runs: None,
        session_id: None,
    }
}

fn apply_receipt(result: &mut WorkflowsResultV1, receipt: WorkflowIngestReceipt) {
    result.index_attempted_at = Some(receipt.attempted_at);
    result.index_coverage = Some(match receipt.coverage {
        WorkflowIngestCoverage::Complete => WorkflowIndexCoverageV1::Complete,
        WorkflowIngestCoverage::Partial => WorkflowIndexCoverageV1::Partial,
        WorkflowIngestCoverage::Failed => WorkflowIndexCoverageV1::Failed,
    });
    result.index_failed_runs = Some(receipt.failed_runs);
    result.index_updated_at = Some(receipt.completed_at);
    result.index_watermark_mtime = Some(receipt.watermark_mtime);
}

fn workflow_run(run: WorkflowRun) -> WorkflowRunV1 {
    WorkflowRunV1 {
        run_id: run.run_id,
        parent_session_id: run.parent_session_id,
        name: run.name,
        description: run.description,
        phase_json: run.phase_json,
        status: workflow_status(run.status),
        started_ts: run.started_ts,
        ended_ts: run.ended_ts,
        result_summary: run.result_summary,
        agent_count: run.agent_count,
    }
}

fn workflow_agent(agent: WorkflowAgent) -> WorkflowAgentV1 {
    WorkflowAgentV1 {
        run_id: agent.run_id,
        agent_label: agent.agent_label,
        agent_id: agent.agent_id,
        phase: agent.phase,
        transcript_path: agent.transcript_path,
        agent_session_id: agent.agent_session_id,
        status: workflow_status(agent.status),
        model: agent.model,
        tokens: agent.tokens,
        started_ts: agent.started_ts,
        ended_ts: agent.ended_ts,
    }
}

const fn workflow_status(status: WorkflowStatus) -> WorkflowStatusV1 {
    match status {
        WorkflowStatus::Running => WorkflowStatusV1::Running,
        WorkflowStatus::Completed => WorkflowStatusV1::Completed,
        WorkflowStatus::Failed => WorkflowStatusV1::Failed,
        WorkflowStatus::Unknown => WorkflowStatusV1::Unknown,
    }
}

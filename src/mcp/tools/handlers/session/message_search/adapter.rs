use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalModeV1};

use super::super::lcm_args::{
    bool_arg, message_search_time_range, parse_git_scope_filter,
    parse_message_search_provider_scope, parse_message_search_scope, parse_session_message_type,
};
use super::super::sessions_for::render_message_search_md;
use super::contract::{
    SessionRetrievalCommand, SessionRetrievalFilters, SessionRetrievalPageView,
    SessionRetrievalProjectSelector, SessionRetrievalServiceOutcome, SessionRetrievalServicePort,
    SessionRetrievalStoreScope, SessionRetrievalUnavailable, SessionRetrievalWorkerBlocker,
    SessionRetrievalWorkerRetryClass, SessionTemporalMetadataView,
};
use crate::application::session::{
    SessionDataFreshness, SessionFreshnessPolicy, SessionRetrievalScope, SessionTemporalQuery,
};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::WorkflowScopeFilter;
use crate::mcp::tools::ToolResult;
use crate::mcp::tools::handlers::support::{argument_error, tool_json_with_md};
use crate::sessions::git_correlation::GitScopeFilter;
use crate::sessions::{
    ProviderScope, SessionMessageSearchResult, SessionMessageType, SessionSearchScope,
    SessionSearchTimeRange,
};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ranking::DiversityLimits;

pub(crate) struct MessageSearchRequest<'a> {
    pub(crate) query: &'a str,
    pub(crate) provider_scope: ProviderScope,
    pub(crate) requested_provider: Option<&'static str>,
    pub(crate) project_key: Option<&'a str>,
    pub(crate) parent_session_id: Option<&'a str>,
    pub(crate) workflow_run: Option<&'a str>,
    pub(crate) workflow_agent: Option<&'a str>,
    pub(crate) include_subagents: bool,
    pub(crate) catch_up: bool,
    pub(crate) cursor: Option<&'a str>,
    pub(crate) scope: SessionSearchScope,
    pub(crate) message_type: SessionMessageType,
    pub(crate) limit: usize,
    pub(crate) git_filter: GitScopeFilter,
    pub(crate) time_range: SessionSearchTimeRange,
    pub(crate) workflow_scope: Option<WorkflowScopeFilter>,
    /// When true, ignore FTS and list each session's latest goal
    /// (`kind = 'goal'`) instead. `query` is optional in this mode.
    pub(crate) goals: bool,
}

fn optional_message_search_string<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be a non-empty string")))
}

pub(crate) fn parse_message_search_request(args: &Value) -> Result<MessageSearchRequest<'_>> {
    let goals = bool_arg(args, "goals")?.unwrap_or(false);
    let query = match optional_message_search_string(args, "query")? {
        Some(query) => query,
        // In goals-listing mode the query is optional: the listing is not an
        // FTS search, so an absent query simply lists the most recent goals.
        None if goals => "",
        None => {
            return Err(TraceDecayError::Config {
                message: "missing required parameter: query".to_string(),
            });
        }
    };
    let provider_scope = parse_message_search_provider_scope(args)?;
    let workflow_run = optional_message_search_string(args, "workflow_run")?;
    let workflow_agent = optional_message_search_string(args, "workflow_agent")?;
    if workflow_agent.is_some() && workflow_run.is_none() {
        return Err(argument_error(
            "workflow_agent requires workflow_run to avoid broadening retrieval",
        ));
    }
    let include_subagents = bool_arg(args, "include_subagents")?.unwrap_or(true);
    let limit = match args.get("limit") {
        None => 10,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| argument_error("limit must be a non-negative integer"))?
            .clamp(1, 50) as usize,
    };
    let mut scope = parse_message_search_scope(args)?;
    if !include_subagents && matches!(scope, SessionSearchScope::SubagentsOnly) {
        return Err(argument_error(
            "include_subagents=false cannot be combined with scope=subagents_only",
        ));
    }
    if !include_subagents && matches!(scope, SessionSearchScope::All) {
        scope = SessionSearchScope::ParentsOnly;
    }
    Ok(MessageSearchRequest {
        query,
        provider_scope,
        requested_provider: provider_scope.provider_id(),
        project_key: optional_message_search_string(args, "project_key")?,
        parent_session_id: optional_message_search_string(args, "parent_session_id")?,
        workflow_run,
        workflow_agent,
        include_subagents,
        catch_up: bool_arg(args, "catch_up")?.unwrap_or(false),
        cursor: optional_message_search_string(args, "cursor")?,
        scope,
        message_type: parse_session_message_type(args)?,
        limit,
        git_filter: parse_git_scope_filter(args)?,
        time_range: message_search_time_range(args)?,
        workflow_scope: workflow_run.map(|run_id| WorkflowScopeFilter {
            run_id: run_id.to_string(),
            agent_label: workflow_agent.map(str::to_string),
        }),
        goals,
    })
}

fn base_message_search_payload(request: &MessageSearchRequest<'_>) -> Value {
    let mut payload = json!({
        "status": "ok",
        "outcome": "complete_zero",
        "provider": request.requested_provider.unwrap_or("all"),
        "requested_provider": request.requested_provider,
        "project_key": request.project_key,
        "parent_session_id": request.parent_session_id,
        "include_subagents": request.include_subagents,
        "catch_up": request.catch_up,
        "catch_up_performed": false,
        "catch_up_failures": [],
        "catch_up_provider": request.provider_scope.response_label(),
        "scope": request.scope.as_str(),
        "message_type": request.message_type.as_str(),
        "since": request.time_range.start_time,
        "until": request.time_range.end_time,
        "query": request.query,
        "goals": request.goals,
        "count": 0,
        "results": [],
        "refresh_required": false,
        "next_action": Value::Null,
    });
    if !request.git_filter.is_empty()
        && let Some(map) = payload.as_object_mut()
    {
        map.insert(
            "git_filter".to_string(),
            git_filter_value(&request.git_filter),
        );
        map.insert("git_filter_applied".to_string(), Value::Bool(true));
    }
    if request.workflow_scope.is_some()
        && let Some(map) = payload.as_object_mut()
    {
        map.insert(
            "workflow_run".to_string(),
            request
                .workflow_run
                .map_or(Value::Null, |run| Value::String(run.to_string())),
        );
        if let Some(label) = request.workflow_agent {
            map.insert(
                "workflow_agent".to_string(),
                Value::String(label.to_string()),
            );
        }
        map.insert("workflow_filter_applied".to_string(), Value::Bool(true));
        map.insert("workflow_run_parent_session".to_string(), Value::Null);
    }
    payload
}

fn git_filter_value(git_filter: &GitScopeFilter) -> Value {
    let mut value = serde_json::Map::new();
    if let Some(branch) = &git_filter.branch {
        value.insert("branch".to_string(), Value::String(branch.clone()));
    }
    if let Some(worktree) = &git_filter.worktree {
        value.insert("worktree".to_string(), Value::String(worktree.clone()));
    }
    if let Some(commit) = &git_filter.commit {
        value.insert("commit".to_string(), Value::String(commit.clone()));
    }
    Value::Object(value)
}

fn temporal_value(
    temporal: &SessionTemporalMetadataView,
    freshness: SessionDataFreshness,
) -> Value {
    let freshness = match freshness {
        SessionDataFreshness::Fresh => json!({ "state": "fresh" }),
        SessionDataFreshness::Stored { generation_lag } => {
            json!({ "state": "stored", "generation_lag": generation_lag })
        }
        SessionDataFreshness::Partial { generation_lag } => {
            json!({ "state": "partial", "generation_lag": generation_lag })
        }
    };
    let mut value = json!({
        "anchors": temporal.anchors,
        "watermarks": temporal.watermarks,
        "coverage": temporal.coverage,
        "cursor": temporal.cursor,
        "explanations": temporal.explanations,
        "freshness": freshness,
    });
    if !temporal.source_coverage.is_empty() {
        value["source_coverage"] = json!(temporal.source_coverage);
    }
    if !temporal.omissions.is_empty() {
        value["omissions"] = json!(temporal.omissions);
    }
    value
}

fn message_search_results_value(results: Vec<SessionMessageSearchResult>) -> Result<Value> {
    let mut values = Vec::with_capacity(results.len());
    for result in results {
        let score =
            serde_json::Number::from_f64(result.score).ok_or_else(|| TraceDecayError::Config {
                message: "session retrieval result score must be finite".to_string(),
            })?;
        let mut value = serde_json::Map::new();
        value.insert("session".to_string(), serde_json::to_value(result.session)?);
        value.insert("message".to_string(), serde_json::to_value(result.message)?);
        value.insert("score".to_string(), Value::Number(score));
        values.push(Value::Object(value));
    }
    Ok(Value::Array(values))
}

fn apply_page(
    payload: &mut Value,
    page: SessionRetrievalPageView,
    freshness: SessionDataFreshness,
) -> Result<()> {
    let SessionRetrievalPageView { results, temporal } = page;
    let map = payload
        .as_object_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: "message search payload must be an object".to_string(),
        })?;
    if let Some(root) = &temporal.authorized_root {
        map.insert(
            "selected_project_root".to_string(),
            Value::String(root.clone()),
        );
    }
    map.insert("count".to_string(), json!(results.len()));
    map.insert(
        "results".to_string(),
        message_search_results_value(results)?,
    );
    map.insert("temporal".to_string(), temporal_value(&temporal, freshness));
    Ok(())
}

fn apply_temporal(
    payload: &mut Value,
    temporal: &SessionTemporalMetadataView,
    freshness: SessionDataFreshness,
) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    if let Some(root) = &temporal.authorized_root {
        map.insert(
            "selected_project_root".to_string(),
            Value::String(root.clone()),
        );
    }
    map.insert("temporal".to_string(), temporal_value(temporal, freshness));
}

fn apply_refresh_guidance(payload: &mut Value, required: bool) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    map.insert("refresh_required".to_string(), Value::Bool(required));
    map.insert(
        "next_action".to_string(),
        if required {
            json!({
                "kind": "session_refresh",
                "tool": "tracedecay_session_refresh",
                "action": "begin",
                "reason": "the authorized session-temporal store does not satisfy the requested freshness precondition",
            })
        } else {
            Value::Null
        },
    );
}

fn apply_typed_error(payload: &mut Value, status: &str, code: &str, message: &str) {
    let Some(map) = payload.as_object_mut() else {
        return;
    };
    map.insert("status".to_string(), Value::String(status.to_string()));
    map.insert("outcome".to_string(), Value::String(status.to_string()));
    map.insert("message".to_string(), Value::String(message.to_string()));
    map.insert(
        "error".to_string(),
        json!({
            "code": code,
            "message": message,
            "retryable": false
        }),
    );
}

fn apply_unavailable(payload: &mut Value, unavailable: SessionRetrievalUnavailable) {
    apply_typed_error(
        payload,
        "unavailable",
        "session_retrieval_service_unavailable",
        "the authorized session retrieval service is unavailable",
    );
    payload["error"]["reason"] = json!(unavailable.reason.as_str());
    payload["error"]["retryable"] = json!(unavailable.reason.is_retryable());
    if let Some(worker) = unavailable.worker {
        payload["service_status"] = json!({
            "last_progress_at_unix_micros": worker.last_progress_at_unix_micros,
            "backlog": worker.backlog,
            "blocker": worker.blocker.map(SessionRetrievalWorkerBlocker::as_str),
            "retry_class": worker.retry_class.map(SessionRetrievalWorkerRetryClass::as_str),
        });
    }
}

fn render_service_outcome(
    request: &MessageSearchRequest<'_>,
    outcome: SessionRetrievalServiceOutcome,
) -> Result<Value> {
    let mut payload = base_message_search_payload(request);
    match outcome {
        SessionRetrievalServiceOutcome::Complete { page, freshness } => {
            payload["outcome"] = json!("complete");
            apply_page(&mut payload, page, freshness)?;
        }
        SessionRetrievalServiceOutcome::CompleteZero {
            temporal,
            freshness,
        } => {
            apply_temporal(&mut payload, &temporal, freshness);
        }
        SessionRetrievalServiceOutcome::Stale {
            temporal,
            freshness,
        } => {
            payload["status"] = json!("stale");
            payload["outcome"] = json!("stale");
            apply_temporal(&mut payload, &temporal, freshness);
            apply_refresh_guidance(&mut payload, request.catch_up);
        }
        SessionRetrievalServiceOutcome::Partial {
            page,
            freshness,
            omitted,
        } => {
            payload["status"] = json!("partial");
            payload["outcome"] = json!("partial");
            payload["omitted"] = json!(omitted);
            let refresh_required = request.catch_up
                && matches!(
                    freshness,
                    SessionDataFreshness::Stored { .. } | SessionDataFreshness::Partial { .. }
                );
            apply_page(&mut payload, page, freshness)?;
            apply_refresh_guidance(&mut payload, refresh_required);
        }
        SessionRetrievalServiceOutcome::WrongScope => apply_typed_error(
            &mut payload,
            "wrong_scope",
            "session_retrieval_wrong_scope",
            "the canonical session retrieval service does not own the requested root",
        ),
        SessionRetrievalServiceOutcome::Locked => apply_typed_error(
            &mut payload,
            "locked",
            "session_retrieval_locked",
            "the authorized session-temporal store is locked",
        ),
        SessionRetrievalServiceOutcome::Redacted => apply_typed_error(
            &mut payload,
            "redacted",
            "session_retrieval_redacted",
            "the requested session evidence is redacted",
        ),
        SessionRetrievalServiceOutcome::Deleted => apply_typed_error(
            &mut payload,
            "deleted",
            "session_retrieval_deleted",
            "the requested session evidence was deleted",
        ),
        SessionRetrievalServiceOutcome::Denied => apply_typed_error(
            &mut payload,
            "denied",
            "session_retrieval_denied",
            "session retrieval was denied",
        ),
        SessionRetrievalServiceOutcome::Unavailable(unavailable) => {
            apply_unavailable(&mut payload, unavailable);
        }
        SessionRetrievalServiceOutcome::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        } => {
            apply_typed_error(
                &mut payload,
                "cursor_manifest_limit_exceeded",
                "session_cursor_manifest_limit_exceeded",
                "session retrieval cursor manifest exceeded its canonical bound",
            );
            payload["error"]["kind"] = json!(kind);
            payload["error"]["observed"] = json!(observed);
            payload["error"]["maximum"] = json!(maximum);
        }
        SessionRetrievalServiceOutcome::BudgetExhausted => apply_typed_error(
            &mut payload,
            "budget_exhausted",
            "session_retrieval_budget_exhausted",
            "session retrieval exhausted its bounded work budget",
        ),
        SessionRetrievalServiceOutcome::Cancelled => apply_typed_error(
            &mut payload,
            "cancelled",
            "session_retrieval_cancelled",
            "session retrieval was cancelled",
        ),
    }
    Ok(payload)
}

fn optional_owned_string(value: Option<&Value>, name: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .map(Some)
        .ok_or_else(|| argument_error(format!("{name} must be a non-empty string")))
}

fn project_selector(args: &Value) -> Result<Option<SessionRetrievalProjectSelector>> {
    let nested = args.get("project_selector");
    let nested = nested
        .map(|value| {
            value
                .as_object()
                .ok_or_else(|| argument_error("project_selector must be an object"))
        })
        .transpose()?;
    let project_id = nested
        .and_then(|selector| selector.get("project_id"))
        .or_else(|| args.get("project_id"));
    let project_path = nested
        .and_then(|selector| {
            selector
                .get("path")
                .or_else(|| selector.get("project_path"))
        })
        .or_else(|| args.get("project_path"));
    let selector = SessionRetrievalProjectSelector {
        project_id: optional_owned_string(project_id, "project_id")?,
        project_path: optional_owned_string(project_path, "project_path")?,
    };
    if selector.project_id.is_none() && selector.project_path.is_none() {
        if nested.is_some()
            || args.get("project_id").is_some()
            || args.get("project_path").is_some()
        {
            return Err(argument_error(
                "project selector must include project_id or project_path",
            ));
        }
        return Ok(None);
    }
    Ok(Some(selector))
}

fn retrieval_command(
    request: &MessageSearchRequest<'_>,
    store_scope: SessionRetrievalStoreScope,
    project_selector: Option<SessionRetrievalProjectSelector>,
) -> Result<SessionRetrievalCommand> {
    let query = SessionTemporalQuery::new(
        SessionId::new("session.message-search.root").map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?,
        request.requested_provider.map(str::to_string),
        request.query,
        request.cursor.map(str::to_string),
        TemporalModeV1::Current,
        RetrievalGrainV1::LogicalMessage,
        request.limit,
        DiversityLimits::default(),
        ContextBudget {
            max_bytes: 64 * 1024,
            max_tokens: 16 * 1024,
            estimator_version: "words-v1".to_string(),
        },
    )
    .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })?
    .with_retrieval_scope(SessionRetrievalScope::AllSessionsInAuthorizedRoot)
    .with_freshness_policy(if request.catch_up {
        SessionFreshnessPolicy::RequireFresh
    } else {
        SessionFreshnessPolicy::AllowStored
    });
    let filters = SessionRetrievalFilters {
        project_key: request.project_key.map(str::to_string),
        parent_session_id: request.parent_session_id.map(str::to_string),
        source: None,
        include_summaries: false,
        scope: request.scope,
        message_type: request.message_type,
        roles: Vec::new(),
        time_range: request.time_range,
        git_filter: request.git_filter.clone(),
        workflow_scope: request.workflow_scope.clone(),
    };
    Ok(
        SessionRetrievalCommand::new(query, filters, request.goals, store_scope)
            .with_project_selector(project_selector),
    )
}

fn deferred_all_registered_payload(request: &MessageSearchRequest<'_>) -> Value {
    let mut payload = base_message_search_payload(request);
    apply_typed_error(
        &mut payload,
        "deferred",
        "session_retrieval_multi_root_deferred",
        "multi-root session retrieval is not implemented",
    );
    payload["project_scope"] = json!("all_registered");
    payload
}

pub(crate) fn render_temporal_message_search_md(payload: &Value) -> String {
    let mut markdown = render_message_search_md(payload);
    if let Some(coverage) = payload
        .get("temporal")
        .and_then(|temporal| temporal.get("coverage"))
    {
        let _ = write!(
            markdown,
            "\n- Coverage: visible {}, hidden {}, unknown {}, redacted {}\n",
            coverage["visible"].as_u64().unwrap_or_default(),
            coverage["hidden"].as_u64().unwrap_or_default(),
            coverage["unknown"].as_u64().unwrap_or_default(),
            coverage["redacted"].as_u64().unwrap_or_default(),
        );
    }
    if payload
        .get("refresh_required")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        markdown.push_str(
            "- Refresh required: run `tracedecay_session_refresh` with action `begin`.\n",
        );
    }
    if let Some(error) = payload.get("error") {
        let _ = writeln!(
            markdown,
            "- Problem: `{}` — {}",
            error["code"].as_str().unwrap_or("session_retrieval_error"),
            error["message"]
                .as_str()
                .unwrap_or("session retrieval failed"),
        );
        if let Some(reason) = error.get("reason").and_then(Value::as_str) {
            let _ = writeln!(markdown, "- Unavailable reason: `{reason}`");
        }
    }
    if let Some(status) = payload.get("service_status") {
        let last_progress = status
            .get("last_progress_at_unix_micros")
            .and_then(Value::as_i64)
            .map_or_else(|| "none".to_string(), |value| value.to_string());
        let _ = writeln!(
            markdown,
            "- Refresh worker: last progress {last_progress}, backlog {}, blocker `{}`, retry class `{}`",
            status["backlog"].as_u64().unwrap_or_default(),
            status["blocker"].as_str().unwrap_or("none"),
            status["retry_class"].as_str().unwrap_or("none"),
        );
    }
    markdown
}

pub(crate) async fn handle_message_search_with_service(
    project_root: Option<&Path>,
    store_scope: SessionRetrievalStoreScope,
    args: Value,
    service: Option<&dyn SessionRetrievalServicePort>,
) -> Result<ToolResult> {
    let request = parse_message_search_request(&args)?;
    let project_selector = project_selector(&args)?;
    let has_project_selector = project_selector.is_some();
    let project_scope = optional_message_search_string(&args, "project_scope")?;
    if let Some(project_scope) = project_scope {
        if project_scope != "all_registered" {
            return Err(argument_error(
                "project_scope must be omitted or all_registered",
            ));
        }
        if project_selector.is_some() {
            return Err(argument_error(
                "project_scope cannot be combined with project_id, project_path, or project_selector",
            ));
        }
        let payload = deferred_all_registered_payload(&request);
        return Ok(tool_json_with_md(project_root, &args, &payload, || {
            render_temporal_message_search_md(&payload)
        }));
    }
    if matches!(store_scope, SessionRetrievalStoreScope::Profile) && project_selector.is_some() {
        return Err(argument_error(
            "profile session storage cannot be combined with a project selector",
        ));
    }
    let command = retrieval_command(&request, store_scope, project_selector)?;
    let outcome = match service {
        Some(service) => service.execute(command).await,
        None => SessionRetrievalServiceOutcome::Unavailable(
            SessionRetrievalUnavailable::service_not_configured(),
        ),
    };
    let mut payload = render_service_outcome(&request, outcome)?;
    payload["store_scope"] = json!(store_scope.as_str());
    if matches!(store_scope, SessionRetrievalStoreScope::Project)
        && project_root.is_some()
        && !has_project_selector
    {
        payload["selected_project_root"] = json!(project_root);
    }
    Ok(tool_json_with_md(project_root, &args, &payload, || {
        render_temporal_message_search_md(&payload)
    }))
}

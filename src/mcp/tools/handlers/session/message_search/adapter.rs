use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Map, Value, json};
use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalModeV1};

use super::super::lcm_args::{
    bool_arg, message_search_time_range, parse_git_scope_filter,
    parse_message_search_provider_scope, parse_message_search_scope, parse_session_message_type,
};
use super::super::sessions_for::render_message_search_md;
use super::contract::{
    SessionRetrievalCommand, SessionRetrievalFilters, SessionRetrievalNextActionView,
    SessionRetrievalPageView, SessionRetrievalProjectSelector, SessionRetrievalServiceOutcome,
    SessionRetrievalServicePort, SessionRetrievalStoreScope, SessionRetrievalSweepOutcome,
    SessionRetrievalSweepPort, SessionRetrievalSweepRootView, SessionRetrievalUnavailable,
    SessionTemporalMetadataView,
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

fn payload_object_mut(payload: &mut Value) -> Result<&mut Map<String, Value>> {
    payload
        .as_object_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: "message search payload must be an object".to_string(),
        })
}

fn error_object_mut(payload: &mut Value) -> Result<&mut Map<String, Value>> {
    payload_object_mut(payload)?
        .get_mut("error")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| TraceDecayError::Config {
            message: "message search error payload must be an object".to_string(),
        })
}

fn base_message_search_payload(request: &MessageSearchRequest<'_>) -> Result<Value> {
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
    let map = payload_object_mut(&mut payload)?;
    if !request.git_filter.is_empty() {
        map.insert(
            "git_filter".to_string(),
            serde_json::to_value(&request.git_filter)?,
        );
        map.insert("git_filter_applied".to_string(), Value::Bool(true));
    }
    if request.workflow_scope.is_some() {
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
    Ok(payload)
}

fn temporal_value(
    temporal: &SessionTemporalMetadataView,
    freshness: SessionDataFreshness,
) -> Result<Value> {
    let freshness = match freshness {
        SessionDataFreshness::Fresh => json!({ "state": "fresh" }),
        SessionDataFreshness::Stored { generation_lag } => {
            json!({ "state": "stored", "generation_lag": generation_lag })
        }
        SessionDataFreshness::Partial { generation_lag } => {
            json!({ "state": "partial", "generation_lag": generation_lag })
        }
    };
    let mut value = serde_json::to_value(temporal)?;
    let map = payload_object_mut(&mut value)?;
    map.remove("authorized_root");
    map.insert("freshness".to_string(), freshness);
    Ok(value)
}

fn message_search_results_value(results: Vec<SessionMessageSearchResult>) -> Result<Value> {
    if results.iter().any(|result| !result.score.is_finite()) {
        return Err(TraceDecayError::Config {
            message: "session retrieval result score must be finite".to_string(),
        });
    }
    serde_json::to_value(results).map_err(Into::into)
}

fn apply_page(
    payload: &mut Value,
    page: SessionRetrievalPageView,
    freshness: SessionDataFreshness,
) -> Result<()> {
    let SessionRetrievalPageView { results, temporal } = page;
    let map = payload_object_mut(payload)?;
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
    map.insert(
        "temporal".to_string(),
        temporal_value(&temporal, freshness)?,
    );
    Ok(())
}

fn apply_temporal(
    payload: &mut Value,
    temporal: &SessionTemporalMetadataView,
    freshness: SessionDataFreshness,
) -> Result<()> {
    let map = payload_object_mut(payload)?;
    if let Some(root) = &temporal.authorized_root {
        map.insert(
            "selected_project_root".to_string(),
            Value::String(root.clone()),
        );
    }
    map.insert("temporal".to_string(), temporal_value(temporal, freshness)?);
    Ok(())
}

const fn refresh_next_action() -> SessionRetrievalNextActionView {
    SessionRetrievalNextActionView {
        kind: "session_refresh",
        tool: "tracedecay_session_refresh",
        action: "begin",
        reason: "the authorized session-temporal store does not satisfy the requested freshness precondition",
    }
}

fn apply_refresh_guidance(payload: &mut Value, required: bool) -> Result<()> {
    let map = payload_object_mut(payload)?;
    map.insert("refresh_required".to_string(), Value::Bool(required));
    map.insert(
        "next_action".to_string(),
        if required {
            serde_json::to_value(refresh_next_action())?
        } else {
            Value::Null
        },
    );
    Ok(())
}

fn apply_typed_error(payload: &mut Value, status: &str, code: &str, message: &str) -> Result<()> {
    let map = payload_object_mut(payload)?;
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
    Ok(())
}

fn apply_unavailable(payload: &mut Value, unavailable: SessionRetrievalUnavailable) -> Result<()> {
    apply_typed_error(
        payload,
        "unavailable",
        "session_retrieval_service_unavailable",
        "the authorized session retrieval service is unavailable",
    )?;
    let map = payload_object_mut(payload)?;
    map.insert("count".to_string(), Value::Null);
    map.insert("results".to_string(), Value::Null);
    let error = error_object_mut(payload)?;
    error.insert("reason".to_string(), json!(unavailable.reason.as_str()));
    error.insert(
        "retryable".to_string(),
        json!(unavailable.reason.is_retryable()),
    );
    if let Some(worker) = unavailable.worker {
        payload_object_mut(payload)?
            .insert("service_status".to_string(), serde_json::to_value(worker)?);
    }
    Ok(())
}

fn render_service_outcome(
    request: &MessageSearchRequest<'_>,
    outcome: SessionRetrievalServiceOutcome,
) -> Result<Value> {
    let mut payload = base_message_search_payload(request)?;
    match outcome {
        SessionRetrievalServiceOutcome::Complete { page, freshness } => {
            payload_object_mut(&mut payload)?.insert("outcome".to_string(), json!("complete"));
            apply_page(&mut payload, page, freshness)?;
        }
        SessionRetrievalServiceOutcome::CompleteZero {
            temporal,
            freshness,
        } => {
            apply_temporal(&mut payload, &temporal, freshness)?;
        }
        SessionRetrievalServiceOutcome::Stale {
            temporal,
            freshness,
        } => {
            let map = payload_object_mut(&mut payload)?;
            map.insert("status".to_string(), json!("stale"));
            map.insert("outcome".to_string(), json!("stale"));
            apply_temporal(&mut payload, &temporal, freshness)?;
            apply_refresh_guidance(&mut payload, request.catch_up)?;
        }
        SessionRetrievalServiceOutcome::Partial {
            page,
            freshness,
            omitted,
        } => {
            let map = payload_object_mut(&mut payload)?;
            map.insert("status".to_string(), json!("partial"));
            map.insert("outcome".to_string(), json!("partial"));
            map.insert("omitted".to_string(), json!(omitted));
            let refresh_required = request.catch_up
                && matches!(
                    freshness,
                    SessionDataFreshness::Stored { .. } | SessionDataFreshness::Partial { .. }
                );
            apply_page(&mut payload, page, freshness)?;
            apply_refresh_guidance(&mut payload, refresh_required)?;
        }
        SessionRetrievalServiceOutcome::WrongScope => apply_typed_error(
            &mut payload,
            "wrong_scope",
            "session_retrieval_wrong_scope",
            "the canonical session retrieval service does not own the requested root",
        )?,
        SessionRetrievalServiceOutcome::Locked => apply_typed_error(
            &mut payload,
            "locked",
            "session_retrieval_locked",
            "the authorized session-temporal store is locked",
        )?,
        SessionRetrievalServiceOutcome::Redacted => apply_typed_error(
            &mut payload,
            "redacted",
            "session_retrieval_redacted",
            "the requested session evidence is redacted",
        )?,
        SessionRetrievalServiceOutcome::Deleted => apply_typed_error(
            &mut payload,
            "deleted",
            "session_retrieval_deleted",
            "the requested session evidence was deleted",
        )?,
        SessionRetrievalServiceOutcome::Denied => apply_typed_error(
            &mut payload,
            "denied",
            "session_retrieval_denied",
            "session retrieval was denied",
        )?,
        SessionRetrievalServiceOutcome::Unavailable(unavailable) => {
            apply_unavailable(&mut payload, unavailable)?;
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
            )?;
            let error = error_object_mut(&mut payload)?;
            error.insert("kind".to_string(), json!(kind));
            error.insert("observed".to_string(), json!(observed));
            error.insert("maximum".to_string(), json!(maximum));
        }
        SessionRetrievalServiceOutcome::BudgetExhausted => apply_typed_error(
            &mut payload,
            "budget_exhausted",
            "session_retrieval_budget_exhausted",
            "session retrieval exhausted its bounded work budget",
        )?,
        SessionRetrievalServiceOutcome::Cancelled => apply_typed_error(
            &mut payload,
            "cancelled",
            "session_retrieval_cancelled",
            "session retrieval was cancelled",
        )?,
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

const fn service_outcome_label(outcome: &SessionRetrievalServiceOutcome) -> &'static str {
    match outcome {
        SessionRetrievalServiceOutcome::Complete { .. } => "complete",
        SessionRetrievalServiceOutcome::CompleteZero { .. } => "complete_zero",
        SessionRetrievalServiceOutcome::Stale { .. } => "stale",
        SessionRetrievalServiceOutcome::Partial { .. } => "partial",
        SessionRetrievalServiceOutcome::WrongScope => "wrong_scope",
        SessionRetrievalServiceOutcome::Locked => "locked",
        SessionRetrievalServiceOutcome::Redacted => "redacted",
        SessionRetrievalServiceOutcome::Deleted => "deleted",
        SessionRetrievalServiceOutcome::Denied => "denied",
        SessionRetrievalServiceOutcome::Unavailable(_) => "unavailable",
        SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { .. } => {
            "cursor_manifest_limit_exceeded"
        }
        SessionRetrievalServiceOutcome::BudgetExhausted => "budget_exhausted",
        SessionRetrievalServiceOutcome::Cancelled => "cancelled",
    }
}

const fn freshness_label(freshness: SessionDataFreshness) -> &'static str {
    match freshness {
        SessionDataFreshness::Fresh => "fresh",
        SessionDataFreshness::Stored { .. } => "stored",
        SessionDataFreshness::Partial { .. } => "partial",
    }
}

struct SweptResult {
    project_id: String,
    root: String,
    result: SessionMessageSearchResult,
}

fn sweep_root_entry(view: &SessionRetrievalSweepRootView) -> Value {
    let mut map = Map::new();
    map.insert("project_id".to_string(), json!(view.project_id));
    map.insert("root".to_string(), json!(view.root));
    map.insert(
        "status".to_string(),
        json!(service_outcome_label(&view.outcome)),
    );
    match &view.outcome {
        SessionRetrievalServiceOutcome::Complete { page, freshness } => {
            map.insert("count".to_string(), json!(page.results.len()));
            map.insert("freshness".to_string(), json!(freshness_label(*freshness)));
        }
        SessionRetrievalServiceOutcome::CompleteZero { freshness, .. }
        | SessionRetrievalServiceOutcome::Stale { freshness, .. } => {
            map.insert("count".to_string(), json!(0));
            map.insert("freshness".to_string(), json!(freshness_label(*freshness)));
        }
        SessionRetrievalServiceOutcome::Partial {
            page,
            freshness,
            omitted,
        } => {
            map.insert("count".to_string(), json!(page.results.len()));
            map.insert("freshness".to_string(), json!(freshness_label(*freshness)));
            map.insert("omitted".to_string(), json!(omitted));
        }
        SessionRetrievalServiceOutcome::Unavailable(unavailable) => {
            map.insert("reason".to_string(), json!(unavailable.reason.as_str()));
        }
        _ => {}
    }
    Value::Object(map)
}

/// Deterministic merge order for swept results: newest message first, then
/// score, then registered project identity, then message identity. Every key
/// is total, so equal pages always merge identically.
fn merged_sweep_order(a: &SweptResult, b: &SweptResult) -> std::cmp::Ordering {
    b.result
        .message
        .timestamp
        .cmp(&a.result.message.timestamp)
        .then_with(|| b.result.score.total_cmp(&a.result.score))
        .then_with(|| a.project_id.cmp(&b.project_id))
        .then_with(|| {
            a.result
                .message
                .message_id
                .cmp(&b.result.message.message_id)
        })
}

fn swept_result_value(swept: SweptResult) -> Result<Value> {
    if !swept.result.score.is_finite() {
        return Err(TraceDecayError::Config {
            message: "session retrieval result score must be finite".to_string(),
        });
    }
    let mut value = serde_json::to_value(&swept.result)?;
    let map = payload_object_mut(&mut value)?;
    map.insert("project_id".to_string(), Value::String(swept.project_id));
    map.insert("root".to_string(), Value::String(swept.root));
    Ok(value)
}

fn render_sweep_outcome(
    request: &MessageSearchRequest<'_>,
    outcome: SessionRetrievalSweepOutcome,
) -> Result<Value> {
    let mut payload = base_message_search_payload(request)?;
    payload_object_mut(&mut payload)?.insert("project_scope".to_string(), json!("all_registered"));
    match outcome {
        SessionRetrievalSweepOutcome::Complete {
            roots,
            skipped,
            registry_truncated,
        } => {
            let root_entries = roots.iter().map(sweep_root_entry).collect::<Vec<_>>();
            let mut merged = Vec::new();
            for view in roots {
                let page = match view.outcome {
                    SessionRetrievalServiceOutcome::Complete { page, .. }
                    | SessionRetrievalServiceOutcome::Partial { page, .. } => page,
                    _ => continue,
                };
                merged.extend(page.results.into_iter().map(|result| SweptResult {
                    project_id: view.project_id.clone(),
                    root: view.root.clone(),
                    result,
                }));
            }
            merged.sort_by(merged_sweep_order);
            merged.truncate(request.limit);
            let results = merged
                .into_iter()
                .map(swept_result_value)
                .collect::<Result<Vec<_>>>()?;
            let map = payload_object_mut(&mut payload)?;
            if !results.is_empty() {
                map.insert("outcome".to_string(), json!("complete"));
            }
            map.insert("count".to_string(), json!(results.len()));
            map.insert("results".to_string(), Value::Array(results));
            map.insert(
                "searched_project_count".to_string(),
                json!(root_entries.len()),
            );
            map.insert("skipped_project_count".to_string(), json!(skipped.len()));
            map.insert("roots".to_string(), json!(root_entries));
            map.insert("skipped".to_string(), serde_json::to_value(skipped)?);
            map.insert("registry_truncated".to_string(), json!(registry_truncated));
        }
        SessionRetrievalSweepOutcome::WrongScope => {
            apply_typed_error(
                &mut payload,
                "wrong_scope",
                "session_retrieval_sweep_wrong_scope",
                "the registered-project sweep serves only selector-free project-store commands",
            )?;
        }
        SessionRetrievalSweepOutcome::RegistryUnavailable => {
            apply_typed_error(
                &mut payload,
                "unavailable",
                "session_retrieval_registry_unavailable",
                "the project registry authority is unavailable",
            )?;
        }
    }
    Ok(payload)
}

fn sweep_unmounted_payload(request: &MessageSearchRequest<'_>) -> Result<Value> {
    let mut payload = base_message_search_payload(request)?;
    apply_typed_error(
        &mut payload,
        "unavailable",
        "session_retrieval_sweep_unavailable",
        "the registered-project sweep authority is not mounted on this server",
    )?;
    payload_object_mut(&mut payload)?.insert("project_scope".to_string(), json!("all_registered"));
    Ok(payload)
}

fn markdown_object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>> {
    value.as_object().ok_or_else(|| TraceDecayError::Config {
        message: format!("message search markdown requires {name} to be an object"),
    })
}

fn markdown_u64(value: &Map<String, Value>, field: &str, name: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("message search markdown requires {name} to be an unsigned integer"),
        })
}

fn markdown_string<'a>(value: &'a Map<String, Value>, field: &str, name: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("message search markdown requires {name} to be a string"),
        })
}

fn markdown_optional_string<'a>(
    value: &'a Map<String, Value>,
    field: &str,
    name: &str,
) -> Result<&'a str> {
    match value.get(field) {
        Some(Value::String(value)) => Ok(value),
        Some(Value::Null) => Ok("none"),
        _ => Err(TraceDecayError::Config {
            message: format!("message search markdown requires {name} to be a string or null"),
        }),
    }
}

pub(crate) fn render_temporal_message_search_md(payload: &Value) -> Result<String> {
    let mut markdown = render_message_search_md(payload);
    if let Some(temporal) = payload.get("temporal") {
        let temporal = markdown_object(temporal, "temporal")?;
        let coverage = temporal
            .get("coverage")
            .ok_or_else(|| TraceDecayError::Config {
                message: "message search markdown requires temporal.coverage".to_string(),
            })?;
        let coverage = markdown_object(coverage, "temporal.coverage")?;
        let visible = markdown_u64(coverage, "visible", "temporal.coverage.visible")?;
        let hidden = markdown_u64(coverage, "hidden", "temporal.coverage.hidden")?;
        let unknown = markdown_u64(coverage, "unknown", "temporal.coverage.unknown")?;
        let redacted = markdown_u64(coverage, "redacted", "temporal.coverage.redacted")?;
        let _ = writeln!(
            markdown,
            "\n- Coverage: visible {visible}, hidden {hidden}, unknown {unknown}, redacted {redacted}"
        );
    }
    let refresh_required = payload
        .get("refresh_required")
        .and_then(Value::as_bool)
        .ok_or_else(|| TraceDecayError::Config {
            message: "message search markdown requires refresh_required to be a boolean"
                .to_string(),
        })?;
    if refresh_required {
        markdown.push_str(
            "- Refresh required: run `tracedecay_session_refresh` with action `begin`.\n",
        );
    }
    if let Some(error) = payload.get("error") {
        let error = markdown_object(error, "error")?;
        let code = markdown_string(error, "code", "error.code")?;
        let message = markdown_string(error, "message", "error.message")?;
        let _ = writeln!(markdown, "- Problem: `{code}` — {message}");
        if let Some(reason) = error.get("reason") {
            let reason = reason.as_str().ok_or_else(|| TraceDecayError::Config {
                message: "message search markdown requires error.reason to be a string".to_string(),
            })?;
            let _ = writeln!(markdown, "- Unavailable reason: `{reason}`");
        }
    }
    if let Some(status) = payload.get("service_status") {
        let status = markdown_object(status, "service_status")?;
        let last_progress = match status.get("last_progress_at_unix_micros") {
            Some(Value::Null) => "none".to_string(),
            Some(value) => value.as_i64().map(|value| value.to_string()).ok_or_else(|| {
                TraceDecayError::Config {
                    message: "message search markdown requires service_status.last_progress_at_unix_micros to be an integer or null".to_string(),
                }
            })?,
            None => {
                return Err(TraceDecayError::Config {
                    message: "message search markdown requires service_status.last_progress_at_unix_micros".to_string(),
                });
            }
        };
        let backlog = markdown_u64(status, "backlog", "service_status.backlog")?;
        let blocker = markdown_optional_string(status, "blocker", "service_status.blocker")?;
        let retry_class =
            markdown_optional_string(status, "retry_class", "service_status.retry_class")?;
        let _ = writeln!(
            markdown,
            "- Refresh worker: last progress {last_progress}, backlog {backlog}, blocker `{blocker}`, retry class `{retry_class}`"
        );
    }
    Ok(markdown)
}

pub(crate) async fn handle_message_search_with_service(
    project_root: Option<&Path>,
    store_scope: SessionRetrievalStoreScope,
    args: Value,
    service: Option<&dyn SessionRetrievalServicePort>,
    sweep: Option<&dyn SessionRetrievalSweepPort>,
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
        if matches!(store_scope, SessionRetrievalStoreScope::Profile) {
            return Err(argument_error(
                "project_scope=all_registered requires project session storage",
            ));
        }
        if request.cursor.is_some() {
            return Err(argument_error(
                "project_scope=all_registered cannot resume a single-root cursor",
            ));
        }
        if request.catch_up {
            return Err(argument_error(
                "catch_up refresh is a single-root operation and cannot be combined with project_scope=all_registered",
            ));
        }
        let payload = match sweep {
            Some(sweep) => {
                let command =
                    retrieval_command(&request, SessionRetrievalStoreScope::Project, None)?;
                render_sweep_outcome(&request, sweep.execute_registered(command).await)?
            }
            None => sweep_unmounted_payload(&request)?,
        };
        let markdown = render_temporal_message_search_md(&payload)?;
        let semantic_error = payload.get("error").is_some_and(|error| !error.is_null());
        return Ok(
            tool_json_with_md(project_root, &args, &payload, move || markdown)
                .with_semantic_error(semantic_error),
        );
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
    {
        let map = payload_object_mut(&mut payload)?;
        map.insert("store_scope".to_string(), json!(store_scope.as_str()));
        if matches!(store_scope, SessionRetrievalStoreScope::Project)
            && project_root.is_some()
            && !has_project_selector
        {
            map.insert("selected_project_root".to_string(), json!(project_root));
        }
    }
    let markdown = render_temporal_message_search_md(&payload)?;
    let semantic_error = payload.get("error").is_some_and(|error| !error.is_null());
    Ok(
        tool_json_with_md(project_root, &args, &payload, move || markdown)
            .with_semantic_error(semantic_error),
    )
}

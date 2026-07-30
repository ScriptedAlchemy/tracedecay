#[cfg(test)]
use crate::automation::backend::{AgentTaskKind, AgentTaskRequest, run_agent_task_with_retry};
use serde::Serialize;
use tracedecay_domain::{
    HydrationStateV1, RetrievalGrainV1, SessionId, TemporalCoverageCountsV1, TemporalModeV1,
};
use tracedecay_sessions::lcm::contracts::{LcmDataFreshness, LcmRetrievalOutcome};

use super::lcm_args::*;
use super::lcm_compact::{
    MAX_LCM_EXPAND_QUERY_PROMPT_CHARS, MAX_LCM_EXPAND_QUERY_QUERY_CHARS,
    lcm_expand_query_tool_json, lcm_preflight_tool_json, lcm_response_handle_root, truncate_chars,
};
use super::lcm_storage::{LcmHandlerContext, LcmOpenMode, LcmStorageResolution, open_lcm_storage};
use super::live_projection::upsert_live_transcript_projection;
use super::message_search::{
    LcmDescribeServiceCommand, LcmDescribeServiceOutcome, LcmExpandServiceCommand,
    LcmExpandServiceOutcome, SessionRetrievalCommand, SessionRetrievalFilters,
    SessionRetrievalPageView, SessionRetrievalServiceOutcome, SessionRetrievalUnavailable,
    SessionTemporalMetadataView,
};
use super::*;
use crate::application::session::{
    SessionDataFreshness, SessionRetrievalScope, SessionTemporalQuery,
};
use crate::sessions::lcm::{
    LcmContentRange, LcmExpandQueryBudget, LcmExpandQueryContextBlock, LcmExpandQueryMatch,
    LcmExpandQueryPagination, LcmExpandQueryResponse, LcmExpandQuerySynthesisPrompt, LcmSourceRef,
};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ranking::DiversityLimits;

fn lcm_status_payload<T: Serialize>(
    provider: &str,
    session_id: Option<&str>,
    deep: bool,
    status: T,
) -> Value {
    json!({
        "status": "ok",
        "provider": provider,
        "session_id": session_id,
        "deep": deep,
        "lcm": status,
    })
}

pub(in super::super) async fn handle_lcm_status(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_or_all_arg(&args)?;
    let session_id = string_arg(&args, "session_id");
    let deep = bool_arg(&args, "deep")?.unwrap_or(false);
    let gc_config = lcm_gc_config(&args)?;
    let storage = match open_lcm_storage(context, &args, LcmOpenMode::ReadOnlyOrMissing).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let status = storage
        .db
        .lcm_status_with_options(provider, session_id, deep, &gc_config)
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        context.project_root,
        &args,
        &lcm_status_payload(provider, session_id, deep, status),
    ))
}

pub(in super::super) async fn handle_lcm_doctor(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = string_arg(&args, "session_id");
    let mode = lcm_doctor_mode(&args)?;
    let apply = bool_arg(&args, "apply")?.unwrap_or(false);
    let clean_apply_enabled = lcm_doctor_clean_apply_enabled(&args)?;
    let gc_apply_enabled = lcm_gc_apply_enabled(&args)?;
    if mode == "clean" && apply && !clean_apply_enabled {
        return Ok(tool_json(
            context.project_root,
            &args,
            &json!({
                "status": "denied",
                "provider": provider,
                "session_id": session_id,
                "mode": mode,
                "dry_run": false,
                "apply": true,
                "error": "destructive cleanup is disabled by default",
                "note": "set LCM_DOCTOR_CLEAN_APPLY_ENABLED=true only in trusted operator environments",
                "repairs": {
                    "planned_actions": [],
                    "applied_actions": [],
                    "backup": Value::Null,
                    "unsafe_actions_skipped": [
                        {
                            "kind": "clean_lcm_noise",
                            "safe": false,
                            "reason": "doctor_clean_apply_disabled"
                        }
                    ]
                }
            }),
        ));
    }
    if mode == "gc" && apply && !gc_apply_enabled {
        return Ok(tool_json(
            context.project_root,
            &args,
            &json!({
                "status": "denied",
                "provider": provider,
                "session_id": session_id,
                "mode": mode,
                "dry_run": false,
                "apply": true,
                "error": "payload GC apply is disabled by default",
                "note": "set LCM_GC_APPLY_ENABLED=true only in trusted operator environments",
                "repairs": {
                    "planned_actions": [],
                    "applied_actions": [],
                    "backup": Value::Null,
                    "unsafe_actions_skipped": [
                        {
                            "kind": "payload_gc",
                            "safe": false,
                            "reason": "lcm_gc_apply_disabled"
                        }
                    ]
                }
            }),
        ));
    }
    let clean_config = lcm_clean_config(&args)?;
    let gc_config = lcm_gc_config(&args)?;
    let open_mode = if matches!(mode, "repair" | "clean" | "gc") && apply {
        LcmOpenMode::Writable
    } else {
        LcmOpenMode::ReadOnlyExisting
    };
    let storage = match open_lcm_storage(context, &args, open_mode).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let mut payload = storage
        .db
        .lcm_doctor(provider, session_id, mode, apply, clean_config, gc_config)
        .await
        .map_err(lcm_error)?;
    if let Some(object) = payload.as_object_mut()
        && let Some(diagnostics) = object
            .get_mut("diagnostics")
            .and_then(serde_json::Value::as_object_mut)
    {
        diagnostics.insert(
            "ast_grep".to_string(),
            super::super::super::definitions::ast_grep_diagnostics_json(),
        );
    }
    Ok(tool_json(context.project_root, &args, &payload))
}

fn default_lcm_context_budget() -> ContextBudget {
    ContextBudget {
        max_bytes: 64 * 1024,
        max_tokens: 16 * 1024,
        estimator_version: "words-v1".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn lcm_retrieval_command(
    context: LcmHandlerContext<'_>,
    session_id: &str,
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
) -> Result<SessionRetrievalCommand> {
    let session_id =
        SessionId::new(session_id).map_err(|error| argument_error(error.to_string()))?;
    let query = SessionTemporalQuery::new(
        session_id,
        provider.map(str::to_string),
        query_text,
        cursor,
        temporal_mode,
        RetrievalGrainV1::Occurrence,
        limit,
        DiversityLimits::default(),
        context_budget,
    )
    .map_err(|error| argument_error(error.to_string()))?
    .with_retrieval_scope(retrieval_scope);
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
            git_filter: GitScopeFilter::default(),
            workflow_scope: None,
        },
        false,
        context.retrieval_store_scope,
    ))
}

fn lcm_temporal_fields(temporal: &SessionTemporalMetadataView) -> Value {
    json!({
        "anchors": temporal.anchors,
        "watermarks": temporal.watermarks,
        "authorized_root": temporal.authorized_root,
        "coverage": temporal.coverage,
        "source_coverage": temporal.source_coverage,
        "explanations": temporal.explanations,
        "omissions": temporal.omissions,
        "next_cursor": temporal.cursor,
    })
}

fn apply_lcm_temporal_fields(payload: &mut Value, temporal: &SessionTemporalMetadataView) {
    let fields = lcm_temporal_fields(temporal);
    // Callers only ever pass a JSON object payload.
    #[allow(clippy::expect_used)]
    let payload = payload.as_object_mut().expect("LCM payload object");
    for key in [
        "anchors",
        "watermarks",
        "authorized_root",
        "coverage",
        "source_coverage",
        "explanations",
        "omissions",
        "next_cursor",
    ] {
        payload.insert(key.to_string(), fields[key].clone());
    }
}

fn apply_lcm_retrieval_fields(payload: &mut Value, retrieval: LcmRetrievalOutcome) {
    let Some(payload) = payload.as_object_mut() else {
        return;
    };
    payload.insert("retrieval".to_string(), json!(retrieval));
    payload.insert("omitted".to_string(), json!(retrieval.omitted()));
}

const fn session_data_freshness(freshness: LcmDataFreshness) -> SessionDataFreshness {
    match freshness {
        LcmDataFreshness::Fresh => SessionDataFreshness::Fresh,
        LcmDataFreshness::Stored { generation_lag } => {
            SessionDataFreshness::Stored { generation_lag }
        }
        LcmDataFreshness::Partial { generation_lag } => {
            SessionDataFreshness::Partial { generation_lag }
        }
    }
}

const fn lcm_data_freshness(freshness: SessionDataFreshness) -> LcmDataFreshness {
    match freshness {
        SessionDataFreshness::Fresh => LcmDataFreshness::Fresh,
        SessionDataFreshness::Stored { generation_lag } => {
            LcmDataFreshness::Stored { generation_lag }
        }
        SessionDataFreshness::Partial { generation_lag } => {
            LcmDataFreshness::Partial { generation_lag }
        }
    }
}

fn apply_lcm_expand_query_input_truncation(
    payload: &mut Value,
    prompt_truncated: bool,
    query_truncated: bool,
) {
    if !prompt_truncated && !query_truncated {
        return;
    }
    let Some(payload) = payload.as_object_mut() else {
        return;
    };
    payload.insert("mcp_response_truncated".to_string(), json!(true));
    payload.insert("contract_truncated".to_string(), json!(true));
    payload.insert(
        "mcp_truncation_reason".to_string(),
        json!("expand-query prompt or query exceeded the MCP input bound"),
    );
    payload.insert(
        "prompt_truncated_for_mcp".to_string(),
        json!(prompt_truncated),
    );
    payload.insert(
        "query_truncated_for_mcp".to_string(),
        json!(query_truncated),
    );
    if let Some(synthesis_prompt) = payload
        .get_mut("synthesis_prompt")
        .and_then(Value::as_object_mut)
    {
        synthesis_prompt.insert(
            "user_prompt_truncated_for_mcp".to_string(),
            json!(prompt_truncated),
        );
    }
}

fn lcm_typed_outcome(
    project_root: Option<&Path>,
    args: &Value,
    legacy_key: &str,
    outcome: SessionRetrievalServiceOutcome,
) -> ToolResult {
    let outcome = match outcome {
        SessionRetrievalServiceOutcome::Partial {
            page,
            freshness,
            omitted,
        } => {
            let retrieval = LcmRetrievalOutcome::partial(lcm_data_freshness(freshness), omitted);
            let mut payload = json!({
                "status": "partial",
                "omitted": omitted,
                "retrieval": retrieval,
                "capped_sessions": {},
            });
            apply_lcm_temporal_fields(&mut payload, &page.temporal);
            payload[legacy_key] = json!([]);
            return tool_json(project_root, args, &payload);
        }
        SessionRetrievalServiceOutcome::Stale {
            temporal,
            freshness,
        } => {
            let retrieval = LcmRetrievalOutcome::stale(lcm_data_freshness(freshness));
            let mut payload = json!({
                "status": "stale",
                "omitted": 0,
                "retrieval": retrieval,
                "capped_sessions": {},
            });
            apply_lcm_temporal_fields(&mut payload, &temporal);
            payload[legacy_key] = json!([]);
            return tool_json(project_root, args, &payload);
        }
        outcome => outcome,
    };
    let unavailable = match &outcome {
        SessionRetrievalServiceOutcome::Unavailable(unavailable) => Some(*unavailable),
        _ => None,
    };
    let cursor_manifest_limit = match &outcome {
        SessionRetrievalServiceOutcome::CursorManifestLimitExceeded {
            kind,
            observed,
            maximum,
        } => Some((*kind, *observed, *maximum)),
        _ => None,
    };
    let (status, code, message) = match outcome {
        SessionRetrievalServiceOutcome::WrongScope => (
            "wrong_scope",
            "lcm_retrieval_wrong_scope",
            "the injected retrieval service does not own the requested session root",
        ),
        SessionRetrievalServiceOutcome::Locked => (
            "locked",
            "lcm_retrieval_locked",
            "the authorized session-temporal store is locked",
        ),
        SessionRetrievalServiceOutcome::Redacted => (
            "redacted",
            "lcm_retrieval_redacted",
            "the requested session evidence is redacted",
        ),
        SessionRetrievalServiceOutcome::Deleted => (
            "deleted",
            "lcm_retrieval_deleted",
            "the requested session evidence was deleted",
        ),
        SessionRetrievalServiceOutcome::Denied => (
            "denied",
            "lcm_cursor_denied",
            "session retrieval or the authenticated cursor was denied",
        ),
        SessionRetrievalServiceOutcome::Unavailable(_) => (
            "unavailable",
            "lcm_retrieval_service_unavailable",
            "the authorized session retrieval service is unavailable",
        ),
        SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { .. } => (
            "cursor_manifest_limit_exceeded",
            "lcm_cursor_manifest_limit_exceeded",
            "the canonical session cursor manifest exceeded its bounded limit",
        ),
        SessionRetrievalServiceOutcome::BudgetExhausted => (
            "budget_exhausted",
            "lcm_retrieval_budget_exhausted",
            "session retrieval exhausted its bounded work budget",
        ),
        SessionRetrievalServiceOutcome::Cancelled => (
            "cancelled",
            "lcm_retrieval_cancelled",
            "session retrieval was cancelled",
        ),
        _ => (
            "unavailable",
            "lcm_retrieval_invalid_outcome",
            "the retrieval service returned an invalid compatibility outcome",
        ),
    };
    let mut payload = json!({
        "status": status,
        "error": {"code": code, "message": message, "retryable": false},
        "anchors": [],
        "watermarks": {},
        "coverage": TemporalCoverageCountsV1::default(),
        "explanations": [],
        "next_cursor": Value::Null,
        "capped_sessions": {},
    });
    if let Some(unavailable) = unavailable {
        payload["error"]["reason"] = json!(unavailable.reason.as_str());
        payload["error"]["retryable"] = json!(unavailable.reason.is_retryable());
        if let Some(worker) = unavailable.worker {
            payload["service_status"] = json!({
                "last_progress_at_unix_micros": worker.last_progress_at_unix_micros,
                "backlog": worker.backlog,
                "blocker": worker.blocker.map(super::message_search::SessionRetrievalWorkerBlocker::as_str),
                "retry_class": worker.retry_class.map(super::message_search::SessionRetrievalWorkerRetryClass::as_str),
            });
        }
    }
    if let Some((kind, observed, maximum)) = cursor_manifest_limit {
        payload["error"]["kind"] = json!(kind);
        payload["error"]["observed"] = json!(observed);
        payload["error"]["maximum"] = json!(maximum);
    }
    payload[legacy_key] = json!([]);
    tool_json(project_root, args, &payload)
}

fn unsupported_lcm_filter(
    project_root: Option<&Path>,
    args: &Value,
    legacy_key: &str,
    filter: &str,
) -> ToolResult {
    let mut payload = json!({
        "status": "unsupported_filter",
        "error": {
            "code": format!("lcm_{filter}_filter_unsupported"),
            "message": format!("{filter} is not supported by canonical session retrieval"),
            "retryable": false,
        },
        "anchors": [],
        "watermarks": {},
        "coverage": TemporalCoverageCountsV1::default(),
        "explanations": [],
        "next_cursor": Value::Null,
        "capped_sessions": {},
    });
    payload[legacy_key] = json!([]);
    tool_json(project_root, args, &payload)
}

fn sliced_message(result: SessionMessageSearchResult, content_slice: LcmContentSlice) -> Value {
    let total_chars = result.message.text.chars().count();
    let offset = content_slice.offset.min(total_chars);
    let content: String = result
        .message
        .text
        .chars()
        .skip(offset)
        .take(content_slice.limit)
        .collect();
    let returned_chars = content.chars().count();
    json!({
        "provider": result.message.provider,
        "message_id": result.message.message_id,
        "session_id": result.message.session_id,
        "store_id": Value::Null,
        "role": result.message.role,
        "ordinal": result.message.ordinal,
        "timestamp": result.message.timestamp,
        "content": content,
        "content_range": {
            "offset": offset,
            "limit": content_slice.limit,
            "returned_chars": returned_chars,
            "total_chars": total_chars,
            "truncated": offset.saturating_add(returned_chars) < total_chars,
        },
        "content_hash": Value::Null,
        "storage_kind": "canonical_occurrence",
        "payload_ref": Value::Null,
        "legacy_source": false,
        "legacy_truncated": false,
        "metadata_json": result.message.metadata_json,
    })
}

pub(in super::super) async fn handle_lcm_load_session(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_or_all_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let (content_slice, content_limit_clamped_from) = lcm_load_content_slice(&args)?;
    let cursor = lcm_cursor_arg(&args)?;
    let roles = lcm_roles_arg(&args)?;
    let limit = bounded_usize_arg(&args, "limit", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(50);
    let command = lcm_retrieval_command(
        context,
        session_id,
        optional_search_provider_arg(&args)?,
        "",
        cursor,
        lcm_temporal_mode(&args, TemporalModeV1::Forensic)?,
        limit,
        default_lcm_context_budget(),
        SessionRetrievalScope::Session(
            SessionId::new(session_id).map_err(|error| argument_error(error.to_string()))?,
        ),
        SessionSearchScope::All,
        SessionMessageType::All,
        roles,
        SessionSearchTimeRange {
            start_time: non_negative_i64_arg_alias(&args, "start_time", "time_from")?,
            end_time: non_negative_i64_arg_alias(&args, "end_time", "time_to")?,
        },
        None,
        false,
    )?;
    let outcome = match context.retrieval_service {
        Some(service) => service.execute(command).await,
        None => SessionRetrievalServiceOutcome::Unavailable(
            SessionRetrievalUnavailable::service_not_configured(),
        ),
    };
    let (page, status, omitted) = match outcome {
        SessionRetrievalServiceOutcome::Complete { page, .. } => (Some(page), "ok", 0),
        SessionRetrievalServiceOutcome::CompleteZero { temporal, .. } => (
            Some(SessionRetrievalPageView {
                results: Vec::new(),
                temporal,
            }),
            "ok",
            0,
        ),
        SessionRetrievalServiceOutcome::Partial { page, omitted, .. } => {
            (Some(page), "partial", omitted)
        }
        SessionRetrievalServiceOutcome::Stale { temporal, .. } => (
            Some(SessionRetrievalPageView {
                results: Vec::new(),
                temporal,
            }),
            "stale",
            0,
        ),
        terminal => {
            return Ok(lcm_typed_outcome(
                context.project_root,
                &args,
                "messages",
                terminal,
            ));
        }
    };
    // The match above returns early for every terminal outcome, so a
    // non-terminal outcome always carries a page.
    #[allow(clippy::expect_used)]
    let page = page.expect("page exists for non-terminal LCM outcome");
    let messages = page
        .results
        .into_iter()
        .map(|result| sliced_message(result, content_slice))
        .collect::<Vec<_>>();
    let mut payload = json!({
        "status": status,
        "provider": provider,
        "session_id": session_id,
        "messages": messages,
        "content_limit": content_slice.limit,
        "omitted": omitted,
    });
    apply_lcm_temporal_fields(&mut payload, &page.temporal);
    if let Some(clamped_from) = content_limit_clamped_from
        && let Some(object) = payload.as_object_mut()
    {
        object.insert(
            "content_limit_clamped_from".to_string(),
            json!(clamped_from),
        );
    }
    Ok(tool_json(context.project_root, &args, &payload))
}

pub(in super::super) async fn handle_lcm_grep(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let query = required_string_arg(&args, "query")?;
    // Validate scope before opening storage so argument errors are reported
    // even when the sessions DB does not exist yet.
    let scope = parse_lcm_scope(&args)?;
    let relationship_scope = parse_lcm_relationship_scope(&args)?;
    let message_type = parse_session_message_type(&args)?;
    let provider = lcm_grep_provider_arg(&args)?;
    let git_filter = parse_git_scope_filter(&args)?;
    let include_summaries = bool_arg(&args, "include_summaries")?.unwrap_or(false);
    if !git_filter.is_empty() {
        return Ok(unsupported_lcm_filter(
            context.project_root,
            &args,
            "hits",
            "git",
        ));
    }
    let source = args
        .get("source")
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| argument_error("source must be a non-empty string"))
        })
        .transpose()?;
    if !matches!(parse_lcm_grep_sort(&args)?, LcmGrepSort::Relevance) {
        return Ok(unsupported_lcm_filter(
            context.project_root,
            &args,
            "hits",
            "sort",
        ));
    }
    let session_id = match scope {
        LcmScope::All => "session.lcm-grep.root",
        LcmScope::Current | LcmScope::Session => required_string_arg(&args, "session_id")?,
    };
    let retrieval_scope = match scope {
        LcmScope::All => SessionRetrievalScope::AllSessionsInAuthorizedRoot,
        LcmScope::Current | LcmScope::Session => SessionRetrievalScope::Session(
            SessionId::new(session_id).map_err(|error| argument_error(error.to_string()))?,
        ),
    };
    let command = lcm_retrieval_command(
        context,
        session_id,
        optional_search_provider_arg(&args)?,
        query,
        lcm_cursor_arg(&args)?,
        lcm_temporal_mode(&args, TemporalModeV1::Current)?,
        bounded_usize_arg(&args, "limit", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(10),
        default_lcm_context_budget(),
        retrieval_scope,
        relationship_scope,
        message_type,
        lcm_roles_arg(&args)?,
        message_search_time_range(&args)?,
        source,
        include_summaries,
    )?;
    let outcome = match context.retrieval_service {
        Some(service) => service.execute(command).await,
        None => SessionRetrievalServiceOutcome::Unavailable(
            SessionRetrievalUnavailable::service_not_configured(),
        ),
    };
    let (page, status, omitted) = match outcome {
        SessionRetrievalServiceOutcome::Complete { page, .. } => (page, "ok", 0),
        SessionRetrievalServiceOutcome::CompleteZero { temporal, .. } => (
            SessionRetrievalPageView {
                results: Vec::new(),
                temporal,
            },
            "ok",
            0,
        ),
        SessionRetrievalServiceOutcome::Partial { page, omitted, .. } => (page, "partial", omitted),
        SessionRetrievalServiceOutcome::Stale { temporal, .. } => (
            SessionRetrievalPageView {
                results: Vec::new(),
                temporal,
            },
            "stale",
            0,
        ),
        terminal => {
            return Ok(lcm_typed_outcome(
                context.project_root,
                &args,
                "hits",
                terminal,
            ));
        }
    };
    let hits = page
        .results
        .into_iter()
        .map(|result| {
            let (snippet, _) = truncate_chars(&result.message.text, DEFAULT_LCM_CONTENT_LIMIT);
            let is_summary = result.message.kind.as_deref() == Some("summary");
            let message_id = result.message.message_id;
            json!({
                "kind": if is_summary { "summary_node" } else { "raw_message" },
                "provider": result.message.provider,
                "session_id": result.message.session_id,
                "message_id": if is_summary {
                    Value::Null
                } else {
                    json!(&message_id)
                },
                "node_id": if is_summary {
                    json!(&message_id)
                } else {
                    Value::Null
                },
                "store_id": Value::Null,
                "role": if is_summary {
                    Value::Null
                } else {
                    json!(result.message.role)
                },
                "snippet": snippet,
                "score": result.score,
            })
        })
        .collect::<Vec<_>>();
    let mut payload = json!({
        "status": status,
        "provider": provider,
        "query": query,
        "count": hits.len(),
        "hits": hits,
        "sort": "relevance",
        "relationship_scope": string_arg(&args, "relationship_scope").unwrap_or("all"),
        "message_type": string_arg(&args, "message_type").unwrap_or("all"),
        "capped_sessions": {},
        "omitted": omitted,
    });
    apply_lcm_temporal_fields(&mut payload, &page.temporal);
    Ok(tool_json(context.project_root, &args, &payload))
}

pub(in super::super) async fn handle_lcm_describe(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let target = parse_lcm_describe_target(&args)?;
    let grain = match &target {
        LcmDescribeTarget::Session => RetrievalGrainV1::Session,
        LcmDescribeTarget::SummaryNode { .. } => RetrievalGrainV1::Summary,
        LcmDescribeTarget::ExternalPayload { .. } => RetrievalGrainV1::Occurrence,
    };
    let session_id =
        SessionId::new(session_id).map_err(|error| argument_error(error.to_string()))?;
    let outcome = match context.retrieval_service {
        Some(service) => {
            service
                .describe_lcm(LcmDescribeServiceCommand::new(
                    provider,
                    session_id.clone(),
                    target,
                    grain,
                    context.retrieval_store_scope,
                ))
                .await
        }
        None => LcmDescribeServiceOutcome::Unavailable(
            SessionRetrievalUnavailable::service_not_configured(),
        ),
    };
    match outcome {
        LcmDescribeServiceOutcome::Complete {
            description,
            temporal,
            grain,
            state,
            lineage,
            retrieval,
        } => {
            let mut payload = json!({
                "status": "ok",
                "provider": provider,
                "session_id": session_id.as_str(),
                "description": description,
                "grain": grain,
                "state": state,
                "lineage": lineage,
            });
            apply_lcm_temporal_fields(&mut payload, &temporal);
            apply_lcm_retrieval_fields(&mut payload, retrieval);
            Ok(tool_json(context.project_root, &args, &payload))
        }
        LcmDescribeServiceOutcome::Partial {
            description,
            temporal,
            grain,
            state,
            lineage,
            retrieval,
        } => {
            let mut payload = json!({
                "status": "partial",
                "provider": provider,
                "session_id": session_id.as_str(),
                "description": description,
                "grain": grain,
                "state": state,
                "lineage": lineage,
            });
            apply_lcm_temporal_fields(&mut payload, &temporal);
            apply_lcm_retrieval_fields(&mut payload, retrieval);
            Ok(tool_json(context.project_root, &args, &payload))
        }
        LcmDescribeServiceOutcome::Stale {
            temporal,
            retrieval,
        } => {
            let mut payload = json!({
                "status": "stale",
                "provider": provider,
                "session_id": session_id.as_str(),
                "description": Value::Null,
            });
            apply_lcm_temporal_fields(&mut payload, &temporal);
            apply_lcm_retrieval_fields(&mut payload, retrieval);
            Ok(tool_json(context.project_root, &args, &payload))
        }
        terminal => Ok(lcm_typed_outcome(
            context.project_root,
            &args,
            "description",
            describe_terminal_outcome(terminal),
        )),
    }
}

fn describe_terminal_outcome(outcome: LcmDescribeServiceOutcome) -> SessionRetrievalServiceOutcome {
    match outcome {
        LcmDescribeServiceOutcome::WrongScope => SessionRetrievalServiceOutcome::WrongScope,
        LcmDescribeServiceOutcome::Locked => SessionRetrievalServiceOutcome::Locked,
        LcmDescribeServiceOutcome::Redacted => SessionRetrievalServiceOutcome::Redacted,
        LcmDescribeServiceOutcome::Deleted => SessionRetrievalServiceOutcome::Deleted,
        LcmDescribeServiceOutcome::Denied => SessionRetrievalServiceOutcome::Denied,
        LcmDescribeServiceOutcome::Unavailable(unavailable) => {
            SessionRetrievalServiceOutcome::Unavailable(unavailable)
        }
        LcmDescribeServiceOutcome::Partial {
            temporal,
            retrieval,
            ..
        } => SessionRetrievalServiceOutcome::Partial {
            page: SessionRetrievalPageView {
                results: Vec::new(),
                temporal,
            },
            freshness: session_data_freshness(retrieval.freshness()),
            omitted: retrieval.omitted(),
        },
        LcmDescribeServiceOutcome::Stale {
            temporal,
            retrieval,
        } => SessionRetrievalServiceOutcome::Stale {
            temporal,
            freshness: session_data_freshness(retrieval.freshness()),
        },
        LcmDescribeServiceOutcome::BudgetExhausted => {
            SessionRetrievalServiceOutcome::BudgetExhausted
        }
        LcmDescribeServiceOutcome::Cancelled => SessionRetrievalServiceOutcome::Cancelled,
        LcmDescribeServiceOutcome::Complete { .. } => SessionRetrievalServiceOutcome::Unavailable(
            SessionRetrievalUnavailable::service_not_configured(),
        ),
    }
}

fn expand_terminal_outcome(outcome: LcmExpandServiceOutcome) -> SessionRetrievalServiceOutcome {
    match outcome {
        LcmExpandServiceOutcome::WrongScope => SessionRetrievalServiceOutcome::WrongScope,
        LcmExpandServiceOutcome::Locked => SessionRetrievalServiceOutcome::Locked,
        LcmExpandServiceOutcome::Redacted => SessionRetrievalServiceOutcome::Redacted,
        LcmExpandServiceOutcome::Deleted => SessionRetrievalServiceOutcome::Deleted,
        LcmExpandServiceOutcome::Denied => SessionRetrievalServiceOutcome::Denied,
        LcmExpandServiceOutcome::Unavailable(unavailable) => {
            SessionRetrievalServiceOutcome::Unavailable(unavailable)
        }
        LcmExpandServiceOutcome::Partial {
            temporal,
            retrieval,
            ..
        } => SessionRetrievalServiceOutcome::Partial {
            page: SessionRetrievalPageView {
                results: Vec::new(),
                temporal,
            },
            freshness: session_data_freshness(retrieval.freshness()),
            omitted: retrieval.omitted(),
        },
        LcmExpandServiceOutcome::Stale {
            temporal,
            retrieval,
        } => SessionRetrievalServiceOutcome::Stale {
            temporal,
            freshness: session_data_freshness(retrieval.freshness()),
        },
        LcmExpandServiceOutcome::BudgetExhausted => SessionRetrievalServiceOutcome::BudgetExhausted,
        LcmExpandServiceOutcome::Cancelled => SessionRetrievalServiceOutcome::Cancelled,
        LcmExpandServiceOutcome::Complete { .. } => SessionRetrievalServiceOutcome::Unavailable(
            SessionRetrievalUnavailable::service_not_configured(),
        ),
    }
}

pub(in super::super) async fn handle_lcm_expand(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let target = parse_lcm_expand_target(&args)?;
    if !matches!(target, LcmExpandTarget::SummaryNode { .. })
        && (args.get("source_offset").is_some()
            || args.get("source_limit").is_some()
            || args.get("cursor").is_some())
    {
        return Err(argument_error(
            "source_offset, source_limit, and cursor are valid only when target.kind is summary_node",
        ));
    }
    if args.get("cursor").is_some() && args.get("source_offset").is_some() {
        return Err(argument_error(
            "cursor cannot be combined with source_offset; use one continuation mechanism",
        ));
    }
    let grain = match &target {
        LcmExpandTarget::RawMessage { .. } | LcmExpandTarget::ExternalPayload { .. } => {
            RetrievalGrainV1::Occurrence
        }
        LcmExpandTarget::SummaryNode { .. } => RetrievalGrainV1::Summary,
    };
    let source_limit = if matches!(target, LcmExpandTarget::SummaryNode { .. }) {
        Some(bounded_usize_arg(&args, "source_limit", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(50))
    } else {
        None
    };
    let session_id =
        SessionId::new(session_id).map_err(|error| argument_error(error.to_string()))?;
    let outcome = match context.retrieval_service {
        Some(service) => {
            service
                .expand_lcm(LcmExpandServiceCommand::new(
                    provider,
                    session_id.clone(),
                    target,
                    grain,
                    lcm_content_slice(&args)?,
                    bounded_usize_arg(&args, "source_offset", 0, usize::MAX)?.unwrap_or(0),
                    source_limit,
                    lcm_cursor_arg(&args)?,
                    context.retrieval_store_scope,
                ))
                .await
        }
        None => LcmExpandServiceOutcome::Unavailable(
            SessionRetrievalUnavailable::service_not_configured(),
        ),
    };
    match outcome {
        LcmExpandServiceOutcome::Complete {
            expansion,
            temporal,
            grain,
            state,
            retrieval,
        } => {
            let mut payload = json!({
                "status": "ok",
                "provider": provider,
                "session_id": session_id.as_str(),
                "expansion": expansion,
                "grain": grain,
                "state": state,
            });
            apply_lcm_temporal_fields(&mut payload, &temporal);
            apply_lcm_retrieval_fields(&mut payload, retrieval);
            Ok(tool_json(context.project_root, &args, &payload))
        }
        LcmExpandServiceOutcome::Partial {
            expansion,
            temporal,
            grain,
            state,
            retrieval,
        } => {
            let mut payload = json!({
                "status": "partial",
                "provider": provider,
                "session_id": session_id.as_str(),
                "expansion": expansion,
                "grain": grain,
                "state": state,
            });
            apply_lcm_temporal_fields(&mut payload, &temporal);
            apply_lcm_retrieval_fields(&mut payload, retrieval);
            Ok(tool_json(context.project_root, &args, &payload))
        }
        LcmExpandServiceOutcome::Stale {
            temporal,
            retrieval,
        } => {
            let mut payload = json!({
                "status": "stale",
                "provider": provider,
                "session_id": session_id.as_str(),
                "expansion": Value::Null,
            });
            apply_lcm_temporal_fields(&mut payload, &temporal);
            apply_lcm_retrieval_fields(&mut payload, retrieval);
            Ok(tool_json(context.project_root, &args, &payload))
        }
        terminal => Ok(lcm_typed_outcome(
            context.project_root,
            &args,
            "expansion",
            expand_terminal_outcome(terminal),
        )),
    }
}

/// Core synthesis step, isolated from backend construction and config
/// resolution so it can be unit tested with a fake backend. Runs one bounded
/// backend call built from the response's synthesis prompt and, on success,
/// records the answer. Returns `true` when an answer was synthesized.
#[cfg(test)]
pub(super) async fn synthesize_expand_query_answer(
    response: &mut crate::sessions::lcm::LcmExpandQueryResponse,
    backend: &dyn crate::automation::backend::AgentTaskBackend,
    policy: &crate::automation::backend::BackendRetryPolicy,
) -> bool {
    if !response.needs_synthesis || response.context_blocks.is_empty() {
        return false;
    }
    let Some(synthesis_prompt) = response.synthesis_prompt.clone() else {
        return false;
    };
    let request = AgentTaskRequest::new(
        format!("lcm-expand-query-{}", current_timestamp()),
        AgentTaskKind::UserJob,
        synthesis_prompt.user,
        None,
        json!({ "system": synthesis_prompt.system }),
    );
    let Ok(task) = run_agent_task_with_retry(backend, &request, policy).await else {
        return false;
    };
    let answer = task.output_text.trim();
    if answer.is_empty() {
        return false;
    }
    response.answer = Some(answer.to_string());
    response.needs_synthesis = false;
    true
}

fn expand_query_response_from_sources(
    prompt: &str,
    query: Option<&str>,
    max_tokens: usize,
    context_max_tokens: usize,
    sources: Vec<(&'static str, Option<String>, String)>,
) -> (LcmExpandQueryResponse, usize) {
    let (prompt, _) = truncate_chars(prompt, MAX_LCM_EXPAND_QUERY_PROMPT_CHARS);
    let query = query.map(|query| {
        let (query, _) = truncate_chars(query, MAX_LCM_EXPAND_QUERY_QUERY_CHARS);
        query
    });
    let source_count = sources.len();
    let mut context =
        tracedecay_temporal_query::context::OrderedTextContextAssembler::new(context_max_tokens);
    let mut context_truncated = false;
    let mut matches = Vec::new();
    let mut context_blocks = Vec::new();
    let mut node_ids = Vec::new();
    for (kind, node_id, source_content) in sources {
        let admitted = context.admit(&source_content);
        let Some(content) = admitted.content else {
            context_truncated |= admitted.truncated;
            break;
        };
        context_truncated |= admitted.truncated;
        if let Some(node_id) = &node_id
            && !node_ids.contains(node_id)
        {
            node_ids.push(node_id.clone());
        }
        matches.push(LcmExpandQueryMatch {
            kind: kind.to_string(),
            node_id: node_id.clone(),
            store_id: None,
            snippet: content.clone(),
        });
        context_blocks.push(LcmExpandQueryContextBlock {
            kind: kind.to_string(),
            node_id,
            source_ref: None,
            content,
            content_range: LcmContentRange {
                offset: 0,
                limit: admitted.limit,
                returned_chars: admitted.returned_chars,
                total_chars: admitted.total_chars,
                truncated: admitted.truncated,
            },
            raw_message: None,
            summary_node: None,
        });
    }
    let needs_synthesis = !context_blocks.is_empty();
    let synthesis_prompt = needs_synthesis.then(|| LcmExpandQuerySynthesisPrompt {
        system: LCM_EXPAND_QUERY_SYNTHESIS_SYSTEM_PROMPT.to_string(),
        user: format!(
            "QUESTION:\n{prompt}\n\nEXPANDED CONTEXT:\n{}",
            serde_json::to_string(&context_blocks).unwrap_or_else(|_| "[]".to_string())
        ),
    });
    let dropped_sources = source_count.saturating_sub(context_blocks.len());
    (
        LcmExpandQueryResponse {
            answer: (!needs_synthesis)
                .then(|| "No matching LCM context found in the current session.".to_string()),
            needs_synthesis,
            prompt,
            query,
            synthesis_prompt,
            max_tokens,
            context_max_tokens,
            context_budget: LcmExpandQueryBudget {
                requested_max_chars: context_max_tokens,
                used_chars: context.used_chars(),
            },
            context_truncated,
            context_pagination: Vec::new(),
            node_ids,
            matches,
            context_blocks,
        },
        dropped_sources,
    )
}

fn merge_temporal_metadata(
    target: &mut SessionTemporalMetadataView,
    incoming: SessionTemporalMetadataView,
) -> bool {
    if target
        .authorized_root
        .as_ref()
        .zip(incoming.authorized_root.as_ref())
        .is_some_and(|(left, right)| left != right)
    {
        return false;
    }
    if target.authorized_root.is_none() {
        target.authorized_root = incoming.authorized_root;
    }
    for anchor in incoming.anchors {
        if !target.anchors.contains(&anchor) {
            target.anchors.push(anchor);
        }
    }
    target.watermarks.generation = target
        .watermarks
        .generation
        .max(incoming.watermarks.generation);
    target.watermarks.source = target.watermarks.source.max(incoming.watermarks.source);
    target.watermarks.projection = target
        .watermarks
        .projection
        .max(incoming.watermarks.projection);
    target.watermarks.index = target.watermarks.index.max(incoming.watermarks.index);
    target.watermarks.summary = target.watermarks.summary.max(incoming.watermarks.summary);
    target.coverage.visible = target
        .coverage
        .visible
        .saturating_add(incoming.coverage.visible);
    target.coverage.hidden = target
        .coverage
        .hidden
        .saturating_add(incoming.coverage.hidden);
    target.coverage.unknown = target
        .coverage
        .unknown
        .saturating_add(incoming.coverage.unknown);
    target.coverage.redacted = target
        .coverage
        .redacted
        .saturating_add(incoming.coverage.redacted);
    if incoming.cursor.is_some() {
        target.cursor = incoming.cursor;
    }
    for explanation in incoming.explanations {
        if !target.explanations.contains(&explanation) {
            target.explanations.push(explanation);
        }
    }
    for omission in incoming.omissions {
        if !target.omissions.contains(&omission) {
            target.omissions.push(omission);
        }
    }
    true
}

pub(in super::super) async fn handle_lcm_expand_query(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let (prompt, prompt_truncated) = truncate_chars(
        required_string_arg(&args, "prompt")?,
        MAX_LCM_EXPAND_QUERY_PROMPT_CHARS,
    );
    let (query, query_truncated) =
        optional_non_empty_string_arg(&args, "query")?.map_or((None, false), |query| {
            let (query, truncated) = truncate_chars(query, MAX_LCM_EXPAND_QUERY_QUERY_CHARS);
            (Some(query), truncated)
        });
    let max_results =
        bounded_usize_arg(&args, "max_results", 1, MAX_LCM_RESULT_LIMIT)?.unwrap_or(5);
    let max_tokens =
        bounded_usize_arg(&args, "max_tokens", 1, MAX_LCM_CONTENT_LIMIT)?.unwrap_or(2000);
    // `context_max_tokens` is the retrieval context budget (how much LCM
    // material is assembled before host synthesis). It is orthogonal to
    // `max_tokens` (the synthesis *output* budget): max_tokens ≤ 8 192
    // while context_max_tokens lives in [32 000, 65 536], so a clamp of
    // the form `max_tokens.clamp(32_000, 65_536)` always evaluates to
    // 32_000 — making max_tokens dead. The default is therefore a fixed
    // constant; pass `context_max_tokens` explicitly when a larger budget
    // is wanted.
    let context_max_tokens = bounded_usize_arg(
        &args,
        "context_max_tokens",
        1,
        MAX_LCM_EXPAND_QUERY_CONTEXT_LIMIT,
    )?
    .unwrap_or(DEFAULT_LCM_EXPAND_QUERY_CONTEXT_LIMIT);
    let cursor = optional_non_empty_string_arg(&args, "cursor")?.map(str::to_string);
    let request = LcmExpandQueryRequest {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        prompt,
        query,
        node_ids: string_only_array_arg(&args, "node_ids")?,
        max_results,
        max_tokens,
        context_max_tokens,
    };
    if cursor.is_some() && request.node_ids.len() > 1 {
        return Err(argument_error(
            "cursor continuation requires exactly one node_id",
        ));
    }
    if !request.node_ids.is_empty() {
        let Some(service) = context.retrieval_service else {
            return Ok(lcm_typed_outcome(
                context.project_root,
                &args,
                "context_blocks",
                SessionRetrievalServiceOutcome::Unavailable(
                    SessionRetrievalUnavailable::service_not_configured(),
                ),
            ));
        };
        let mut sources = Vec::new();
        let mut summary_provenance = Vec::new();
        let mut source_omitted = 0_usize;
        let mut temporal = SessionTemporalMetadataView::default();
        for node_id in request.node_ids.iter().take(request.max_results) {
            let outcome = service
                .expand_lcm(LcmExpandServiceCommand::new(
                    provider,
                    SessionId::new(session_id)
                        .map_err(|error| argument_error(error.to_string()))?,
                    LcmExpandTarget::SummaryNode {
                        node_id: node_id.clone(),
                    },
                    RetrievalGrainV1::Summary,
                    LcmContentSlice {
                        offset: 0,
                        limit: request.context_max_tokens.min(MAX_LCM_CONTENT_LIMIT),
                    },
                    0,
                    Some(request.max_results),
                    cursor.clone(),
                    context.retrieval_store_scope,
                ))
                .await;
            let outcome = match outcome {
                LcmExpandServiceOutcome::Partial {
                    expansion: Some(expansion),
                    temporal,
                    grain,
                    state: Some(state),
                    retrieval,
                } => {
                    source_omitted = source_omitted
                        .saturating_add(usize::try_from(retrieval.omitted()).unwrap_or(usize::MAX));
                    LcmExpandServiceOutcome::Complete {
                        expansion,
                        temporal,
                        grain,
                        state,
                        retrieval,
                    }
                }
                outcome => outcome,
            };
            match outcome {
                LcmExpandServiceOutcome::Complete {
                    expansion,
                    temporal: expansion_temporal,
                    ..
                } => {
                    for source in expansion.summary_sources {
                        let kind = match &source.source_ref {
                            LcmSourceRef::RawMessage { .. } => "raw_message",
                            LcmSourceRef::SummaryNode { .. } => "summary_source",
                        };
                        summary_provenance.push(LcmExpandQueryPagination {
                            kind: kind.to_string(),
                            node_id: Some(node_id.clone()),
                            source_ref: Some(source.source_ref.clone()),
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
                        } else {
                            source_omitted = source_omitted.saturating_add(1);
                        }
                    }
                    sources.push(("summary_node", Some(node_id.clone()), expansion.content));
                    if !merge_temporal_metadata(&mut temporal, expansion_temporal) {
                        return Ok(lcm_typed_outcome(
                            context.project_root,
                            &args,
                            "context_blocks",
                            SessionRetrievalServiceOutcome::WrongScope,
                        ));
                    }
                }
                terminal => {
                    return Ok(lcm_typed_outcome(
                        context.project_root,
                        &args,
                        "context_blocks",
                        expand_terminal_outcome(terminal),
                    ));
                }
            }
        }
        let (mut response, budget_omitted) = expand_query_response_from_sources(
            &request.prompt,
            request.query.as_deref(),
            request.max_tokens,
            request.context_max_tokens,
            sources,
        );
        response.context_pagination = summary_provenance;
        let mut payload =
            serde_json::to_value(response).map_err(|err| TraceDecayError::Config {
                message: format!("failed to serialize expand-query response: {err}"),
            })?;
        if let Some(object) = payload.as_object_mut() {
            let omitted = request
                .node_ids
                .len()
                .saturating_sub(request.max_results)
                .saturating_add(budget_omitted)
                .saturating_add(source_omitted);
            object.insert(
                "status".to_string(),
                json!(if omitted == 0 { "ok" } else { "partial" }),
            );
            object.insert("omitted".to_string(), json!(omitted));
            object.insert("provider".to_string(), json!(provider));
            object.insert("session_id".to_string(), json!(session_id));
        }
        apply_lcm_expand_query_input_truncation(&mut payload, prompt_truncated, query_truncated);
        apply_lcm_temporal_fields(&mut payload, &temporal);
        return Ok(lcm_expand_query_tool_json(
            context.project_root,
            &args,
            &payload,
        ));
    }
    let query = request.query.as_deref();
    let retrieval_query = query.unwrap_or(&request.prompt);
    let outcome = match context.retrieval_service {
        Some(service) => {
            let command = lcm_retrieval_command(
                context,
                session_id,
                Some(provider),
                retrieval_query,
                cursor,
                TemporalModeV1::Current,
                request.max_results,
                ContextBudget {
                    max_bytes: u64::try_from(request.context_max_tokens.saturating_mul(4))
                        .unwrap_or(u64::MAX),
                    max_tokens: u64::try_from(request.context_max_tokens).unwrap_or(u64::MAX),
                    estimator_version: "words-v1".to_string(),
                },
                SessionRetrievalScope::Session(
                    SessionId::new(session_id)
                        .map_err(|error| argument_error(error.to_string()))?,
                ),
                SessionSearchScope::All,
                SessionMessageType::All,
                Vec::new(),
                SessionSearchTimeRange::default(),
                None,
                false,
            )?;
            service.execute(command).await
        }
        None => SessionRetrievalServiceOutcome::Unavailable(
            SessionRetrievalUnavailable::service_not_configured(),
        ),
    };
    let (results, temporal, status, service_omitted) = match outcome {
        SessionRetrievalServiceOutcome::Complete { page, .. } => {
            (page.results, page.temporal, "ok", 0)
        }
        SessionRetrievalServiceOutcome::CompleteZero { temporal, .. } => {
            (Vec::new(), temporal, "ok", 0)
        }
        SessionRetrievalServiceOutcome::Partial { page, omitted, .. } => {
            (page.results, page.temporal, "partial", omitted)
        }
        SessionRetrievalServiceOutcome::Stale { temporal, .. } => {
            (Vec::new(), temporal, "stale", 0)
        }
        terminal => {
            return Ok(lcm_typed_outcome(
                context.project_root,
                &args,
                "context_blocks",
                terminal,
            ));
        }
    };
    let sources = results
        .into_iter()
        .map(|result| ("raw_message", None, result.message.text))
        .collect();
    let (response, budget_omitted) = expand_query_response_from_sources(
        &request.prompt,
        query,
        request.max_tokens,
        request.context_max_tokens,
        sources,
    );
    let mut payload = serde_json::to_value(response).map_err(|err| TraceDecayError::Config {
        message: format!("failed to serialize expand-query response: {err}"),
    })?;
    if let Some(object) = payload.as_object_mut() {
        let omitted = service_omitted.saturating_add(budget_omitted as u64);
        object.insert(
            "status".to_string(),
            json!(if status == "ok" && omitted > 0 {
                "partial"
            } else {
                status
            }),
        );
        object.insert("omitted".to_string(), json!(omitted));
        object.insert("provider".to_string(), json!(provider));
        object.insert("session_id".to_string(), json!(session_id));
    }
    apply_lcm_expand_query_input_truncation(&mut payload, prompt_truncated, query_truncated);
    apply_lcm_temporal_fields(&mut payload, &temporal);
    Ok(lcm_expand_query_tool_json(
        context.project_root,
        &args,
        &payload,
    ))
}

pub(in super::super) async fn handle_lcm_session_boundary(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = match open_lcm_storage(context, &args, LcmOpenMode::Writable).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let response = storage
        .db
        .lcm_session_boundary(LcmSessionBoundaryRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            old_session_id: string_arg(&args, "old_session_id").map(str::to_string),
            boundary_reason: string_arg(&args, "boundary_reason").map(str::to_string),
            bound_session_id: string_arg(&args, "bound_session_id").map(str::to_string),
            boundary_skip_at: None,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        context.project_root,
        &args,
        &json!({
            "status": response.status,
            "provider": provider,
            "session_id": session_id,
            "recorded": response.recorded,
            "reason": response.reason,
        }),
    ))
}

pub(in super::super) async fn handle_lcm_preflight(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let storage = match open_lcm_storage(context, &args, LcmOpenMode::Writable).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let messages = messages_arg(&args)?;
    let response = storage
        .db
        .lcm_preflight(LcmPreflightRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            messages: messages.clone(),
            current_tokens: non_negative_i64_arg(&args, "current_tokens")?,
            threshold_tokens: non_negative_i64_arg(&args, "threshold_tokens")?,
            max_assembly_tokens: non_negative_i64_arg(&args, "max_assembly_tokens")?,
            leaf_chunk_tokens: non_negative_i64_arg(&args, "leaf_chunk_tokens")?,
            max_source_messages: bounded_usize_arg(&args, "max_source_messages", 1, usize::MAX)?,
            summary_fan_in: bounded_usize_arg(&args, "summary_fan_in", 2, usize::MAX)?,
            incremental_max_depth: signed_i64_arg(&args, "incremental_max_depth")?,
            fresh_tail_count: bounded_usize_arg(&args, "fresh_tail_count", 0, usize::MAX)?,
            dynamic_leaf_chunk_enabled: bool_arg(&args, "dynamic_leaf_chunk_enabled")?,
            dynamic_leaf_chunk_max: non_negative_i64_arg(&args, "dynamic_leaf_chunk_max")?,
            context_length: non_negative_i64_arg(&args, "context_length")?,
            reserve_tokens_floor: non_negative_i64_arg(&args, "reserve_tokens_floor")?,
            ignore_session_patterns: string_array_arg(&args, "ignore_session_patterns")?,
            stateless_session_patterns: string_array_arg(&args, "stateless_session_patterns")?,
            ignore_message_patterns: string_array_arg(&args, "ignore_message_patterns")?,
        })
        .await
        .map_err(lcm_error)?;
    if bool_arg(&args, "transcript_projection")? == Some(true) {
        upsert_live_transcript_projection(
            &storage.db,
            context.project_root,
            provider,
            session_id,
            &messages,
        )
        .await?;
    }
    Ok(lcm_preflight_tool_json(
        context.project_root,
        &args,
        &json!({
            "status": response.status,
            "provider": provider,
            "session_id": session_id,
            "should_compress": response.should_compress,
            "reason": response.reason,
            "replay_messages": response.replay_messages,
        }),
    ))
}

pub(in super::super) async fn handle_lcm_compress(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let response_handle_root = lcm_response_handle_root(context.project_root, &args);
    let storage = match open_lcm_storage(context, &args, LcmOpenMode::Writable).await {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let response = storage
        .db
        .lcm_compress(LcmCompressionRequest {
            provider: provider.to_string(),
            session_id: session_id.to_string(),
            messages: messages_arg(&args)?,
            current_tokens: non_negative_i64_arg(&args, "current_tokens")?,
            focus_topic: string_arg(&args, "focus_topic").map(str::to_string),
            ignore_session_patterns: string_array_arg(&args, "ignore_session_patterns")?,
            stateless_session_patterns: string_array_arg(&args, "stateless_session_patterns")?,
            ignore_message_patterns: string_array_arg(&args, "ignore_message_patterns")?,
            expected_current_frontier_store_id: non_negative_i64_arg(
                &args,
                "expected_current_frontier_store_id",
            )?,
            threshold_tokens: non_negative_i64_arg(&args, "threshold_tokens")?,
            max_assembly_tokens: non_negative_i64_arg(&args, "max_assembly_tokens")?,
            leaf_chunk_tokens: non_negative_i64_arg(&args, "leaf_chunk_tokens")?,
            max_source_messages: bounded_usize_arg(&args, "max_source_messages", 1, usize::MAX)?,
            summary_fan_in: bounded_usize_arg(&args, "summary_fan_in", 2, usize::MAX)?,
            incremental_max_depth: signed_i64_arg(&args, "incremental_max_depth")?,
            fresh_tail_count: bounded_usize_arg(&args, "fresh_tail_count", 0, usize::MAX)?,
            dynamic_leaf_chunk_enabled: bool_arg(&args, "dynamic_leaf_chunk_enabled")?,
            dynamic_leaf_chunk_max: non_negative_i64_arg(&args, "dynamic_leaf_chunk_max")?,
            context_length: non_negative_i64_arg(&args, "context_length")?,
            reserve_tokens_floor: non_negative_i64_arg(&args, "reserve_tokens_floor")?,
            summarizer: summarizer_arg(&args)?,
        })
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        response_handle_root.as_deref(),
        &args,
        &json!({
            "status": response.status,
            "provider": provider,
            "session_id": session_id,
            "reason": response.reason,
            "summary_nodes_created": response.summary_nodes_created,
            "summary_nodes": response.summary_nodes,
            "replay_messages": response.replay_messages,
            "replay_token_estimate": response.replay_token_estimate,
            "replay_over_budget": response.replay_over_budget,
            "compression_attempts": response.compression_attempts,
            "fallback_used": response.fallback_used,
            "context_recovery_hint": response.context_recovery_hint,
            "retry_status": response.retry_status,
            "frontier": response.frontier,
            "summary_request": response.summary_request,
        }),
    ))
}

#[cfg(test)]
mod compatibility_tests {
    use std::path::Path;
    use std::sync::Mutex;

    use tempfile::TempDir;
    use tracedecay_domain::{
        HydrationStateV1, RetrievalAnchorId, RetrievalGrainV1, SessionId, SessionSourceCoverageV1,
        SessionSourceFrontierV1, SessionSourceIdV1, SessionTemporalCoverageRequestV1,
        TemporalCoverageCountsV1, TemporalModeV1, UtcMicros,
    };

    use super::super::message_search::{
        LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
        LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
        SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalPageView,
        SessionRetrievalServiceFuture, SessionRetrievalServiceOutcome, SessionRetrievalServicePort,
        SessionRetrievalUnavailable, SessionTemporalMetadataView, SessionTemporalWatermarksView,
    };
    use super::*;
    use crate::application::session::{SessionDataFreshness, SessionRetrievalScope};
    use crate::sessions::lcm::{LcmContentRange, LcmDescribeResponse, LcmExpandResponse};

    struct RecordingService {
        commands: Mutex<Vec<SessionRetrievalCommand>>,
        outcome: SessionRetrievalServiceOutcome,
        describe_commands: Mutex<Vec<LcmDescribeServiceCommand>>,
        describe_outcome: Mutex<LcmDescribeServiceOutcome>,
        expand_commands: Mutex<Vec<LcmExpandServiceCommand>>,
        expand_outcome: Mutex<LcmExpandServiceOutcome>,
    }

    impl RecordingService {
        fn new(outcome: SessionRetrievalServiceOutcome) -> Self {
            Self {
                commands: Mutex::new(Vec::new()),
                outcome,
                describe_commands: Mutex::new(Vec::new()),
                describe_outcome: Mutex::new(LcmDescribeServiceOutcome::Unavailable(
                    SessionRetrievalUnavailable::service_not_configured(),
                )),
                expand_commands: Mutex::new(Vec::new()),
                expand_outcome: Mutex::new(LcmExpandServiceOutcome::Unavailable(
                    SessionRetrievalUnavailable::service_not_configured(),
                )),
            }
        }

        fn command(&self) -> SessionRetrievalCommand {
            self.commands.lock().unwrap().last().unwrap().clone()
        }

        fn calls(&self) -> usize {
            self.commands.lock().unwrap().len()
        }

        fn set_describe_outcome(&self, outcome: LcmDescribeServiceOutcome) {
            *self.describe_outcome.lock().unwrap() = outcome;
        }

        fn describe_command(&self) -> LcmDescribeServiceCommand {
            self.describe_commands
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .clone()
        }

        fn set_expand_outcome(&self, outcome: LcmExpandServiceOutcome) {
            *self.expand_outcome.lock().unwrap() = outcome;
        }

        fn expand_command(&self) -> LcmExpandServiceCommand {
            self.expand_commands.lock().unwrap().last().unwrap().clone()
        }

        fn expand_calls(&self) -> usize {
            self.expand_commands.lock().unwrap().len()
        }
    }

    impl SessionRetrievalServicePort for RecordingService {
        fn execute(&self, command: SessionRetrievalCommand) -> SessionRetrievalServiceFuture<'_> {
            self.commands.lock().unwrap().push(command);
            let outcome = self.outcome.clone();
            Box::pin(async move { outcome })
        }

        fn describe_lcm(&self, command: LcmDescribeServiceCommand) -> LcmDescribeServiceFuture<'_> {
            self.describe_commands.lock().unwrap().push(command);
            let outcome = self.describe_outcome.lock().unwrap().clone();
            Box::pin(async move { outcome })
        }

        fn expand_lcm(&self, command: LcmExpandServiceCommand) -> LcmExpandServiceFuture<'_> {
            self.expand_commands.lock().unwrap().push(command);
            let outcome = self.expand_outcome.lock().unwrap().clone();
            Box::pin(async move { outcome })
        }
    }

    fn temporal(cursor: Option<&str>) -> SessionTemporalMetadataView {
        SessionTemporalMetadataView {
            anchors: vec![RetrievalAnchorId::new("anchor.compatibility.1").unwrap()],
            watermarks: SessionTemporalWatermarksView {
                generation: 9,
                source: 8,
                projection: 7,
                index: 6,
                summary: 5,
            },
            coverage: TemporalCoverageCountsV1 {
                visible: 1,
                hidden: 0,
                unknown: 0,
                redacted: 0,
            },
            source_coverage: vec![
                SessionSourceCoverageV1::from_frontiers(
                    SessionSourceIdV1::new("claude").unwrap(),
                    SessionSourceFrontierV1::new(9),
                    SessionSourceFrontierV1::new(9),
                    SessionSourceFrontierV1::new(9),
                    SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current),
                )
                .unwrap(),
            ],
            cursor: cursor.map(str::to_string),
            explanations: vec![SessionRetrievalExplanationView {
                anchor: RetrievalAnchorId::new("anchor.compatibility.1").unwrap(),
                summary: "exact canonical occurrence".to_string(),
            }],
            omissions: Vec::new(),
            authorized_root: Some("/project".to_string()),
        }
    }

    fn result(text: &str, role: &str) -> SessionMessageSearchResult {
        SessionMessageSearchResult {
            session: SessionRecord {
                provider: "claude".to_string(),
                session_id: "session-exact".to_string(),
                project_key: "project".to_string(),
                project_path: "/project".to_string(),
                title: None,
                started_at: Some(10),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            },
            message: SessionMessageRecord {
                provider: "claude".to_string(),
                message_id: "message-1".to_string(),
                session_id: "session-exact".to_string(),
                role: role.to_string(),
                timestamp: Some(20),
                ordinal: 3,
                text: text.to_string(),
                kind: None,
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            },
            score: 0.875,
        }
    }

    fn summary_result(text: &str, node_id: &str) -> SessionMessageSearchResult {
        let mut result = result(text, "summary");
        result.message.message_id = node_id.to_string();
        result.message.kind = Some("summary".to_string());
        result
    }

    fn complete(text: &str, role: &str, cursor: Option<&str>) -> SessionRetrievalServiceOutcome {
        SessionRetrievalServiceOutcome::Complete {
            page: SessionRetrievalPageView {
                results: vec![result(text, role)],
                temporal: temporal(cursor),
            },
            freshness: SessionDataFreshness::Fresh,
        }
    }

    fn payload(result: ToolResult) -> Value {
        serde_json::from_str(
            result.value["content"][0]["text"]
                .as_str()
                .expect("JSON tool result text"),
        )
        .expect("valid JSON tool result")
    }

    #[tokio::test]
    async fn load_maps_exact_forensic_occurrence_and_preserves_legacy_keys() {
        let service = RecordingService::new(complete("a😀界bc", "assistant", Some("opaque-next")));
        let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
        let response = handle_lcm_load_session(
            context,
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "roles": ["assistant"],
                "start_time": 10,
                "end_time": 30,
                "cursor": "opaque-current",
                "limit": 7,
                "content_offset": 1,
                "content_limit": 2,
                "format": "json"
            }),
        )
        .await
        .unwrap();

        let command = service.command();
        assert_eq!(
            command.query().retrieval_scope(),
            &SessionRetrievalScope::Session(SessionId::new("session-exact").unwrap())
        );
        assert_eq!(command.query().provider(), Some("claude"));
        assert_eq!(command.query().query(), "");
        assert_eq!(command.query().cursor(), Some("opaque-current"));
        assert_eq!(command.query().temporal_mode(), TemporalModeV1::Forensic);
        assert_eq!(command.query().grain(), RetrievalGrainV1::Occurrence);
        assert_eq!(command.query().limit(), 7);
        assert_eq!(command.filters().roles, ["assistant"]);
        assert_eq!(command.filters().time_range.start_time, Some(10));
        assert_eq!(command.filters().time_range.end_time, Some(30));

        let response = payload(response);
        assert_eq!(response["messages"][0]["content"], "😀界");
        assert_eq!(response["messages"][0]["content_range"]["offset"], 1);
        assert_eq!(
            response["messages"][0]["content_range"]["returned_chars"],
            2
        );
        assert_eq!(response["messages"][0]["content_range"]["total_chars"], 5);
        assert_eq!(response["next_cursor"], "opaque-next");
        assert!(response["anchors"].is_array());
        assert!(response["watermarks"].is_object());
        assert_eq!(response["watermarks"]["generation"], 9);
        assert!(response["coverage"].is_object());
        assert!(response["explanations"].is_array());
    }

    #[tokio::test]
    async fn load_preserves_the_kernel_page_order_bound_to_its_cursor() {
        let first = result("kernel-first", "assistant");
        let mut second = result("kernel-second", "assistant");
        second.message.message_id = "message-2".to_string();
        second.message.timestamp = Some(30);
        second.message.ordinal = 4;
        let service = RecordingService::new(SessionRetrievalServiceOutcome::Complete {
            page: SessionRetrievalPageView {
                results: vec![first, second],
                temporal: temporal(Some("opaque-next")),
            },
            freshness: SessionDataFreshness::Fresh,
        });
        let response = payload(
            handle_lcm_load_session(
                LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
                json!({
                    "session_id": "session-exact",
                    "cursor": "opaque-current",
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );

        assert_eq!(response["messages"][0]["content"], "kernel-first");
        assert_eq!(response["messages"][1]["content"], "kernel-second");
        assert_eq!(response["next_cursor"], "opaque-next");
    }

    #[tokio::test]
    async fn grep_preserves_exact_phrase_cjk_emoji_and_maps_exact_session_filters() {
        let query = "\"exact phrase\" 精确 😀";
        let service = RecordingService::new(complete(query, "user", Some("grep-next")));
        let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
        let response = handle_lcm_grep(
            context,
            json!({
                "query": query,
                "provider": "claude",
                "scope": "session",
                "session_id": "session-exact",
                "relationship_scope": "parents_only",
                "message_type": "direct_user",
                "role": "user",
                "cursor": "grep-current",
                "include_summaries": false,
                "sort": "relevance",
                "format": "json"
            }),
        )
        .await
        .unwrap();

        let command = service.command();
        assert_eq!(command.query().query(), query);
        assert_eq!(command.query().cursor(), Some("grep-current"));
        assert_eq!(command.query().temporal_mode(), TemporalModeV1::Current);
        assert_eq!(command.query().grain(), RetrievalGrainV1::Occurrence);
        assert_eq!(
            command.query().retrieval_scope(),
            &SessionRetrievalScope::Session(SessionId::new("session-exact").unwrap())
        );
        assert_eq!(command.filters().scope, SessionSearchScope::ParentsOnly);
        assert_eq!(
            command.filters().message_type,
            SessionMessageType::DirectUser
        );
        assert_eq!(command.filters().roles, ["user"]);

        let response = payload(response);
        assert_eq!(response["hits"][0]["snippet"], query);
        assert_eq!(response["capped_sessions"], json!({}));
        assert_eq!(response["next_cursor"], "grep-next");
    }

    #[tokio::test]
    async fn grep_binds_summary_source_as_of_and_renders_stable_summary_hits() {
        let service = RecordingService::new(SessionRetrievalServiceOutcome::Complete {
            page: SessionRetrievalPageView {
                results: vec![summary_result(
                    "current canonical summary",
                    "summary-successor",
                )],
                temporal: temporal(Some("summary-next")),
            },
            freshness: SessionDataFreshness::Fresh,
        });
        let response = payload(
            handle_lcm_grep(
                LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
                json!({
                    "query": "canonical summary",
                    "provider": "claude",
                    "scope": "session",
                    "session_id": "session-exact",
                    "include_summaries": true,
                    "source": "claude",
                    "temporal_mode": "as_of",
                    "as_of_micros": 1234,
                    "cursor": "summary-current",
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );

        let command = service.command();
        assert_eq!(
            command.query().temporal_mode(),
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(1234)
            }
        );
        assert_eq!(command.query().cursor(), Some("summary-current"));
        assert_eq!(command.filters().source.as_deref(), Some("claude"));
        assert!(command.filters().include_summaries);
        assert_eq!(
            command.query().semantic_filter().source.as_deref(),
            Some("claude")
        );
        assert!(command.query().semantic_filter().include_summaries);

        assert_eq!(response["hits"][0]["kind"], "summary_node");
        assert_eq!(response["hits"][0]["node_id"], "summary-successor");
        assert!(response["hits"][0]["message_id"].is_null());
        assert!(response["hits"][0]["role"].is_null());
        assert_eq!(response["next_cursor"], "summary-next");
        assert_eq!(response["anchors"][0], "anchor.compatibility.1");
    }

    #[tokio::test]
    async fn unsupported_filters_are_typed_and_never_call_the_service() {
        for args in [
            json!({"query": "x", "branch": "main", "include_summaries": false, "format": "json"}),
            json!({"query": "x", "sort": "recency", "include_summaries": false, "format": "json"}),
        ] {
            let service = RecordingService::new(complete("unused", "user", None));
            let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
            let response = payload(handle_lcm_grep(context, args).await.unwrap());
            assert_eq!(response["status"], "unsupported_filter");
            assert!(
                response["error"]["code"]
                    .as_str()
                    .unwrap()
                    .starts_with("lcm_")
            );
            assert_eq!(service.calls(), 0);
        }
    }

    #[tokio::test]
    async fn malformed_unsupported_filters_are_rejected_without_broadening() {
        for (args, field) in [
            (
                json!({"query": "x", "include_summaries": "yes", "format": "json"}),
                "include_summaries",
            ),
            (
                json!({"query": "x", "source": 7, "format": "json"}),
                "source",
            ),
            (
                json!({"query": "x", "sort": false, "format": "json"}),
                "sort",
            ),
            (
                json!({"query": "x", "provider": 7, "format": "json"}),
                "provider",
            ),
            (json!({"query": "x", "role": 7, "format": "json"}), "role"),
            (
                json!({"query": "x", "temporal_mode": false, "format": "json"}),
                "temporal_mode",
            ),
            (
                json!({"query": "x", "branch": 7, "format": "json"}),
                "branch",
            ),
        ] {
            let service = RecordingService::new(complete("unused", "user", None));
            let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
            let error = handle_lcm_grep(context, args).await.unwrap_err();
            assert!(error.to_string().contains(field), "{error}");
            assert_eq!(service.calls(), 0);
        }
    }

    #[tokio::test]
    async fn malformed_expand_query_selectors_never_call_the_service() {
        for (args, field) in [
            (
                json!({
                    "provider": "claude",
                    "session_id": "session-exact",
                    "prompt": "question",
                    "query": false,
                    "format": "json"
                }),
                "query",
            ),
            (
                json!({
                    "provider": "claude",
                    "session_id": "session-exact",
                    "prompt": "question",
                    "node_ids": [7],
                    "format": "json"
                }),
                "node_ids",
            ),
        ] {
            let service = RecordingService::new(complete("unused", "user", None));
            let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
            let error = handle_lcm_expand_query(context, args).await.unwrap_err();
            assert!(error.to_string().contains(field), "{error}");
            assert_eq!(service.calls(), 0);
            assert_eq!(service.expand_calls(), 0);
        }
    }

    #[tokio::test]
    async fn malformed_doctor_controls_are_rejected_before_storage_open() {
        for (args, field) in [
            (
                json!({"provider": "claude", "mode": false, "format": "json"}),
                "mode",
            ),
            (
                json!({"provider": "claude", "apply": "yes", "format": "json"}),
                "apply",
            ),
        ] {
            let error = handle_lcm_doctor(
                LcmHandlerContext::user(Path::new("/missing"), None, None),
                args,
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains(field), "{error}");
        }
    }

    #[tokio::test]
    async fn cursor_failures_and_legacy_numeric_cursor_are_typed_without_db_fallback() {
        let denied = RecordingService::new(SessionRetrievalServiceOutcome::Denied);
        let denied_context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&denied));
        let denied_response = payload(
            handle_lcm_grep(
                denied_context,
                json!({
                    "query": "tampered",
                    "cursor": "tampered.cursor",
                    "include_summaries": false,
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );
        assert_eq!(denied_response["status"], "denied");

        let drifted = RecordingService::new(SessionRetrievalServiceOutcome::WrongScope);
        let drifted_context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&drifted));
        let drifted_response = payload(
            handle_lcm_grep(
                drifted_context,
                json!({
                    "query": "drifted",
                    "cursor": "opaque.other-root",
                    "include_summaries": false,
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );
        assert_eq!(drifted_response["status"], "wrong_scope");

        let service = RecordingService::new(complete("compat", "assistant", Some("opaque-next")));
        let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
        let error = handle_lcm_load_session(
            context,
            json!({
                "session_id": "session-exact",
                "after_store_id": 7,
                "format": "json"
            }),
        )
        .await
        .expect_err("legacy offset pagination must be rejected");
        assert!(
            error
                .to_string()
                .contains("after_store_id is no longer supported")
        );
        assert_eq!(service.calls(), 0);

        let missing_path = Path::new("/definitely/missing/tracedecay-sessions.db");
        let missing_context = LcmHandlerContext::user(missing_path, None, None);
        let missing = payload(
            handle_lcm_load_session(
                missing_context,
                json!({"session_id": "session-exact", "format": "json"}),
            )
            .await
            .unwrap(),
        );
        assert_eq!(missing["status"], "unavailable");
        assert!(!missing_path.exists());
    }

    #[tokio::test]
    async fn grep_missing_profile_store_is_unavailable_without_db_fallback() {
        let temp = TempDir::new().unwrap();
        let missing_path = temp.path().join("sessions.db");
        let response = payload(
            handle_lcm_grep(
                LcmHandlerContext::user(&missing_path, None, None),
                json!({"query": "anything", "format": "json"}),
            )
            .await
            .unwrap(),
        );

        assert_eq!(response["status"], "unavailable");
        assert_eq!(
            response["error"]["code"],
            "lcm_retrieval_service_unavailable"
        );
        assert_eq!(response["hits"], json!([]));
        assert!(!missing_path.exists());
    }

    #[tokio::test]
    async fn project_read_alias_without_service_never_probes_the_store_path() {
        let temp = TempDir::new().unwrap();
        let missing_path = temp.path().join("sessions.db");
        let response = payload(
            handle_lcm_load_session(
                LcmHandlerContext::project_for_test(temp.path(), &missing_path, None),
                json!({"session_id": "session-exact", "format": "json"}),
            )
            .await
            .unwrap(),
        );

        assert_eq!(response["status"], "unavailable");
        assert_eq!(
            response["error"]["code"],
            "lcm_retrieval_service_unavailable"
        );
        assert!(!missing_path.exists());
    }

    #[tokio::test]
    async fn describe_maps_summary_target_to_typed_service_and_adds_temporal_metadata() {
        let service = RecordingService::new(complete("unused", "assistant", None));
        service.set_describe_outcome(LcmDescribeServiceOutcome::Complete {
            description: LcmDescribeResponse {
                target: "summary_node".to_string(),
                provider: "claude".to_string(),
                session_id: "session-exact".to_string(),
                raw_message_count: 2,
                summary_node_count: 1,
                external_payload_count: 0,
                first_store_id: Some(1),
                last_store_id: Some(2),
                raw_messages: Vec::new(),
                summary_nodes: Vec::new(),
                summary_node: None,
                external_payload: None,
            },
            temporal: temporal(None),
            grain: RetrievalGrainV1::Summary,
            state: HydrationStateV1::Available,
            lineage: Vec::new(),
        });
        let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));

        let response = payload(
            handle_lcm_describe(
                context,
                json!({
                    "provider": "claude",
                    "session_id": "session-exact",
                    "target": {"kind": "summary_node", "node_id": "summary-1"},
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );

        let command = service.describe_command();
        assert_eq!(command.provider(), "claude");
        assert_eq!(command.session_id().as_str(), "session-exact");
        assert_eq!(command.grain(), RetrievalGrainV1::Summary);
        assert!(matches!(
            command.target(),
            LcmDescribeTarget::SummaryNode { node_id } if node_id == "summary-1"
        ));
        assert_eq!(response["description"]["raw_message_count"], 2);
        assert_eq!(response["description"]["summary_node_count"], 1);
        assert_eq!(response["grain"], "summary");
        assert_eq!(response["state"], "available");
        assert_eq!(response["anchors"][0], "anchor.compatibility.1");
        assert_eq!(response["watermarks"]["generation"], 9);
        assert_eq!(response["coverage"]["visible"], 1);
        assert_eq!(response["source_coverage"][0]["source_id"], "claude");
        assert_eq!(
            response["source_coverage"][0]["reason"]["kind"],
            "caught_up"
        );
        assert!(response["lineage"].is_array());
    }

    #[tokio::test]
    async fn expand_maps_raw_alias_and_preserves_bounded_legacy_expansion() {
        let service = RecordingService::new(complete("unused", "assistant", None));
        service.set_expand_outcome(LcmExpandServiceOutcome::Complete {
            expansion: LcmExpandResponse {
                kind: "raw_message".to_string(),
                content: "😀界".to_string(),
                content_range: LcmContentRange {
                    offset: 1,
                    limit: 2,
                    returned_chars: 2,
                    total_chars: 5,
                    truncated: true,
                },
                raw_message: None,
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: None,
                from_current_session: Some(false),
                externalized_note: None,
                source_pagination: None,
            },
            temporal: temporal(Some("opaque-next")),
            grain: RetrievalGrainV1::Occurrence,
            state: HydrationStateV1::Available,
        });
        let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));

        let response = payload(
            handle_lcm_expand(
                context,
                json!({
                    "provider": "claude",
                    "session_id": "session-exact",
                    "target": {"kind": "raw_message", "store_id": 41},
                    "content_offset": 1,
                    "content_limit": 2,
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );

        let command = service.expand_command();
        assert_eq!(command.provider(), "claude");
        assert_eq!(command.session_id().as_str(), "session-exact");
        assert_eq!(command.grain(), RetrievalGrainV1::Occurrence);
        assert_eq!(command.content_slice().offset, 1);
        assert_eq!(command.content_slice().limit, 2);
        assert_eq!(command.source_offset(), 0);
        assert_eq!(command.source_limit(), None);
        assert_eq!(command.cursor(), None);
        assert!(matches!(
            command.target(),
            LcmExpandTarget::RawMessage { store_id: 41 }
        ));
        assert_eq!(response["expansion"]["content"], "😀界");
        assert_eq!(response["expansion"]["from_current_session"], false);
        assert_eq!(response["grain"], "occurrence");
        assert_eq!(response["state"], "available");
        assert_eq!(response["next_cursor"], "opaque-next");
        assert_eq!(response["source_coverage"][0]["source_id"], "claude");
    }

    #[tokio::test]
    async fn describe_and_expand_without_service_never_probe_legacy_storage() {
        let missing_path = Path::new("/definitely/missing/tracedecay-lcm-authority.db");

        let describe = payload(
            handle_lcm_describe(
                LcmHandlerContext::user(missing_path, None, None),
                json!({
                    "provider": "claude",
                    "session_id": "session-exact",
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );
        assert_eq!(describe["status"], "unavailable");
        assert_eq!(
            describe["error"]["code"],
            "lcm_retrieval_service_unavailable"
        );
        assert_eq!(describe["description"], json!([]));

        let expand = payload(
            handle_lcm_expand(
                LcmHandlerContext::user(missing_path, None, None),
                json!({
                    "provider": "claude",
                    "session_id": "session-exact",
                    "target": {"kind": "raw_message", "store_id": 41},
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );
        assert_eq!(expand["status"], "unavailable");
        assert_eq!(expand["error"]["code"], "lcm_retrieval_service_unavailable");
        assert_eq!(expand["expansion"], json!([]));
        assert!(!missing_path.exists());
    }

    #[tokio::test]
    async fn expand_query_translates_search_through_the_retrieval_service() {
        let service = RecordingService::new(complete(
            "canonical context only",
            "assistant",
            Some("expand-query-next"),
        ));
        let response = payload(
            handle_lcm_expand_query(
                LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
                json!({
                    "provider": "claude",
                    "session_id": "session-exact",
                    "prompt": "What did we decide?",
                    "query": "decision",
                    "max_results": 3,
                    "max_tokens": 512,
                    "context_max_tokens": 4096,
                    "cursor": "expand-query-current",
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );

        let command = service.command();
        assert_eq!(
            command.query().retrieval_scope(),
            &SessionRetrievalScope::Session(SessionId::new("session-exact").unwrap())
        );
        assert_eq!(command.query().provider(), Some("claude"));
        assert_eq!(command.query().query(), "decision");
        assert_eq!(command.query().grain(), RetrievalGrainV1::Occurrence);
        assert_eq!(command.query().limit(), 3);
        assert_eq!(command.query().context_budget().max_tokens, 4096);
        assert_eq!(command.query().cursor(), Some("expand-query-current"));
        assert_eq!(service.calls(), 1);
        assert_eq!(response["status"], "ok");
        assert_eq!(response["needs_synthesis"], true);
        assert_eq!(
            response["context_blocks"][0]["content"],
            "canonical context only"
        );
        assert_eq!(response["next_cursor"], "expand-query-next");
        assert_eq!(response["source_coverage"][0]["source_id"], "claude");
    }

    #[tokio::test]
    async fn expand_query_translates_node_ids_through_summary_expansion() {
        let service = RecordingService::new(complete("unused", "assistant", None));
        service.set_expand_outcome(LcmExpandServiceOutcome::Complete {
            expansion: LcmExpandResponse {
                kind: "summary_node".to_string(),
                content: "canonical summary context".to_string(),
                content_range: LcmContentRange {
                    offset: 0,
                    limit: 4096,
                    returned_chars: 25,
                    total_chars: 25,
                    truncated: false,
                },
                raw_message: None,
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: None,
                from_current_session: Some(true),
                externalized_note: None,
                source_pagination: None,
            },
            temporal: temporal(None),
            grain: RetrievalGrainV1::Summary,
            state: HydrationStateV1::Available,
        });
        let response = payload(
            handle_lcm_expand_query(
                LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
                json!({
                    "provider": "claude",
                    "session_id": "session-exact",
                    "prompt": "What did we decide?",
                    "node_ids": ["summary-1", "summary-2"],
                    "max_results": 1,
                    "context_max_tokens": 4096,
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );

        assert_eq!(service.calls(), 0);
        assert_eq!(service.expand_calls(), 1);
        assert!(matches!(
            service.expand_command().target(),
            LcmExpandTarget::SummaryNode { node_id } if node_id == "summary-1"
        ));
        assert_eq!(response["status"], "partial");
        assert_eq!(response["omitted"], 1);
        assert_eq!(response["node_ids"], json!(["summary-1"]));
        assert_eq!(
            response["context_blocks"][0]["content"],
            "canonical summary context"
        );
    }

    #[tokio::test]
    async fn expand_query_omits_typed_unavailable_summary_sources() {
        let service = RecordingService::new(complete("unused", "assistant", None));
        let source =
            |store_id, state, content: &str| crate::sessions::lcm::LcmExpandedSummarySource {
                source_ref: LcmSourceRef::RawMessage { store_id },
                state,
                content: content.to_string(),
                content_range: (state == HydrationStateV1::Available).then_some(LcmContentRange {
                    offset: 0,
                    limit: 4096,
                    returned_chars: content.chars().count() as u64,
                    total_chars: content.chars().count() as u64,
                    truncated: false,
                }),
                content_truncated: false,
                raw_message: None,
                summary_node: None,
            };
        service.set_expand_outcome(LcmExpandServiceOutcome::Complete {
            expansion: LcmExpandResponse {
                kind: "summary_node".to_string(),
                content: "canonical summary".to_string(),
                content_range: LcmContentRange {
                    offset: 0,
                    limit: 4096,
                    returned_chars: 17,
                    total_chars: 17,
                    truncated: false,
                },
                raw_message: None,
                summary_node: None,
                summary_sources: vec![
                    source(1, HydrationStateV1::Available, "visible source"),
                    source(2, HydrationStateV1::Redacted, ""),
                    source(3, HydrationStateV1::Unauthorized, ""),
                    source(4, HydrationStateV1::Deleted, ""),
                ],
                payload_ref: None,
                from_current_session: None,
                externalized_note: None,
                source_pagination: None,
            },
            temporal: temporal(None),
            grain: RetrievalGrainV1::Summary,
            state: HydrationStateV1::Available,
        });

        let direct = payload(
            handle_lcm_expand(
                LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
                json!({
                    "provider": "claude",
                    "session_id": "session-exact",
                    "target": {"kind": "summary_node", "node_id": "summary-1"},
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );
        assert_eq!(direct["status"], "partial", "{direct}");
        assert_eq!(direct["omitted"], 3, "{direct}");
        assert_eq!(
            direct["expansion"]["summary_sources"][1]["state"],
            "redacted"
        );
        assert_eq!(
            direct["expansion"]["summary_sources"][2]["state"],
            "unauthorized"
        );
        assert_eq!(
            direct["expansion"]["summary_sources"][3]["state"],
            "deleted"
        );

        let response = payload(
            handle_lcm_expand_query(
                LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
                json!({
                    "provider": "claude",
                    "session_id": "session-exact",
                    "prompt": "Recover visible context",
                    "node_ids": ["summary-1"],
                    "max_results": 4,
                    "context_max_tokens": 4096,
                    "format": "json"
                }),
            )
            .await
            .unwrap(),
        );

        assert_eq!(response["status"], "partial", "{response}");
        assert_eq!(response["omitted"], 3, "{response}");
        assert!(
            response["context_blocks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|block| block["content"] == "visible source"),
            "{response}"
        );
        assert!(
            response["context_blocks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|block| !block["content"].as_str().unwrap_or_default().is_empty()),
            "{response}"
        );
        assert_eq!(
            response["context_pagination"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|entry| entry["state"].as_str())
                .collect::<Vec<_>>(),
            vec!["available", "redacted", "unauthorized", "deleted"]
        );
    }

    #[tokio::test]
    async fn expand_query_forwards_single_node_cursor_to_canonical_expansion() {
        let service = RecordingService::new(complete("unused", "assistant", None));
        service.set_expand_outcome(LcmExpandServiceOutcome::Complete {
            expansion: LcmExpandResponse {
                kind: "summary_node".to_string(),
                content: "continued context".to_string(),
                content_range: LcmContentRange {
                    offset: 0,
                    limit: 4096,
                    returned_chars: 17,
                    total_chars: 17,
                    truncated: false,
                },
                raw_message: None,
                summary_node: None,
                summary_sources: Vec::new(),
                payload_ref: None,
                from_current_session: Some(true),
                externalized_note: None,
                source_pagination: None,
            },
            temporal: temporal(None),
            grain: RetrievalGrainV1::Summary,
            state: HydrationStateV1::Available,
        });

        handle_lcm_expand_query(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "prompt": "Continue",
                "node_ids": ["summary-1"],
                "cursor": "expand-query-node-current",
                "format": "json"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            service.expand_command().cursor(),
            Some("expand-query-node-current")
        );
    }

    #[test]
    fn expand_query_response_bounds_oversized_prompt_and_query_before_synthesis() {
        let prompt = "p".repeat(3_000);
        let query = "q".repeat(2_000);
        let (response, _) = expand_query_response_from_sources(
            &prompt,
            Some(&query),
            128,
            128,
            vec![("raw_message", None, "bounded context".to_string())],
        );

        assert_eq!(response.prompt.chars().count(), 2_048);
        assert_eq!(response.query.as_deref().unwrap().chars().count(), 1_024);
        let synthesis = response.synthesis_prompt.expect("response needs synthesis");
        assert!(synthesis.user.contains(&response.prompt));
        assert!(!synthesis.user.contains(&prompt));
    }

    #[test]
    fn status_envelope_preserves_exact_json_and_markdown_rendering() {
        let status = json!({
            "raw_message_count": 12,
            "payload": {"externalized_count": 2}
        });
        let expected = json!({
            "status": "ok",
            "provider": "all",
            "session_id": "session-1",
            "deep": true,
            "lcm": status,
        });
        let value = lcm_status_payload("all", Some("session-1"), true, status);
        assert_eq!(value, expected);

        let json_result = tool_json(None, &json!({"format": "json"}), &value);
        assert_eq!(payload(json_result), expected);

        let markdown_result = tool_json(None, &json!({"format": "markdown"}), &value);
        let markdown = markdown_result.value["content"][0]["text"]
            .as_str()
            .expect("markdown tool result text");
        assert_eq!(markdown, crate::mcp::tools::render::generic_md(&expected));
    }
}

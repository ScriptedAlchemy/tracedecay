use tracedecay_domain::{SessionId, TemporalModeV1};

use super::super::lcm_args::*;
use super::super::lcm_compact::truncate_chars;
use super::super::lcm_storage::LcmHandlerContext;
use super::super::message_search::{
    SessionRetrievalPageView, SessionRetrievalServiceOutcome, SessionRetrievalUnavailable,
};
use super::super::*;
use crate::application::session::SessionRetrievalScope;

use super::shared::{
    apply_lcm_temporal_fields, default_lcm_context_budget, lcm_retrieval_command,
    lcm_typed_outcome, sliced_message, unsupported_lcm_filter,
};

pub(in crate::mcp::tools::handlers) async fn handle_lcm_load_session(
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
        GitScopeFilter::default(),
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

pub(in crate::mcp::tools::handlers) async fn handle_lcm_grep(
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
        git_filter,
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

#[cfg(test)]
mod tests;

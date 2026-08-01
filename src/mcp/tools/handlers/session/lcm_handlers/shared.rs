use serde::Serialize;
use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalCoverageCountsV1, TemporalModeV1};
use tracedecay_sessions::lcm::contracts::{LcmDataFreshness, LcmRetrievalOutcome};

use super::super::lcm_storage::LcmHandlerContext;
use super::super::message_search::{
    SessionRetrievalCommand, SessionRetrievalFilters, SessionRetrievalServiceOutcome,
    SessionTemporalMetadataView,
};
use super::super::*;
use crate::application::session::{
    SessionDataFreshness, SessionRetrievalScope, SessionTemporalQuery,
};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ranking::DiversityLimits;

pub(super) fn lcm_status_payload<T: Serialize>(
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

pub(super) fn default_lcm_context_budget() -> ContextBudget {
    ContextBudget {
        max_bytes: 64 * 1024,
        max_tokens: 16 * 1024,
        estimator_version: "words-v1".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lcm_retrieval_command(
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
    git_filter: GitScopeFilter,
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
            git_filter,
            workflow_scope: None,
        },
        false,
        context.retrieval_store_scope,
    ))
}

pub(super) fn lcm_temporal_fields(temporal: &SessionTemporalMetadataView) -> Value {
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

pub(super) fn apply_lcm_temporal_fields(
    payload: &mut Value,
    temporal: &SessionTemporalMetadataView,
) {
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

pub(super) fn apply_lcm_retrieval_fields(payload: &mut Value, retrieval: LcmRetrievalOutcome) {
    let Some(payload) = payload.as_object_mut() else {
        return;
    };
    payload.insert("retrieval".to_string(), json!(retrieval));
    payload.insert("omitted".to_string(), json!(retrieval.omitted()));
}

pub(super) const fn session_data_freshness(freshness: LcmDataFreshness) -> SessionDataFreshness {
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

pub(super) const fn lcm_data_freshness(freshness: SessionDataFreshness) -> LcmDataFreshness {
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

pub(super) fn apply_lcm_expand_query_input_truncation(
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

pub(super) fn lcm_typed_outcome(
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
                "blocker": worker.blocker.map(super::super::message_search::SessionRetrievalWorkerBlocker::as_str),
                "retry_class": worker.retry_class.map(super::super::message_search::SessionRetrievalWorkerRetryClass::as_str),
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

pub(super) fn unsupported_lcm_filter(
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

pub(super) fn sliced_message(
    result: SessionMessageSearchResult,
    content_slice: LcmContentSlice,
) -> Value {
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

#[cfg(test)]
mod tests;

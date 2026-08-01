#[cfg(test)]
use crate::automation::backend::{AgentTaskKind, AgentTaskRequest, run_agent_task_with_retry};
use tracedecay_domain::{HydrationStateV1, RetrievalGrainV1, SessionId, TemporalModeV1};

use super::super::lcm_args::*;
use super::super::lcm_compact::{
    MAX_LCM_EXPAND_QUERY_PROMPT_CHARS, MAX_LCM_EXPAND_QUERY_QUERY_CHARS,
    lcm_expand_query_tool_json, truncate_chars,
};
use super::super::lcm_storage::LcmHandlerContext;
use super::super::message_search::{
    LcmExpandServiceCommand, LcmExpandServiceOutcome, SessionRetrievalServiceOutcome,
    SessionRetrievalUnavailable, SessionTemporalMetadataView,
};
use super::super::*;
use crate::application::session::SessionRetrievalScope;
use crate::sessions::lcm::{
    LcmContentRange, LcmExpandQueryBudget, LcmExpandQueryContextBlock, LcmExpandQueryMatch,
    LcmExpandQueryPagination, LcmExpandQueryResponse, LcmExpandQuerySynthesisPrompt, LcmSourceRef,
};
use tracedecay_temporal_query::context::ContextBudget;

use super::expansion::expand_terminal_outcome;
use super::shared::{
    apply_lcm_expand_query_input_truncation, apply_lcm_temporal_fields, lcm_retrieval_command,
    lcm_typed_outcome,
};

/// Core synthesis step, isolated from backend construction and config
/// resolution so it can be unit tested with a fake backend. Runs one bounded
/// backend call built from the response's synthesis prompt and, on success,
/// records the answer. Returns `true` when an answer was synthesized.
#[cfg(test)]
pub(in crate::mcp::tools::handlers::session) async fn synthesize_expand_query_answer(
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

pub(in crate::mcp::tools::handlers) async fn handle_lcm_expand_query(
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
                GitScopeFilter::default(),
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

#[cfg(test)]
mod tests;

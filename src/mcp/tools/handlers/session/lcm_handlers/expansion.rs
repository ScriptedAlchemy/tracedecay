use tracedecay_domain::{RetrievalGrainV1, SessionId};

use super::super::lcm_args::*;
use super::super::lcm_storage::LcmHandlerContext;
use super::super::message_search::{
    LcmDescribeServiceCommand, LcmDescribeServiceOutcome, LcmExpandServiceCommand,
    LcmExpandServiceOutcome, SessionRetrievalPageView, SessionRetrievalServiceOutcome,
    SessionRetrievalUnavailable,
};
use super::super::*;

use super::shared::{
    apply_lcm_retrieval_fields, apply_lcm_temporal_fields, lcm_typed_outcome,
    session_data_freshness,
};

pub(in crate::mcp::tools::handlers) async fn handle_lcm_describe(
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

pub(super) fn expand_terminal_outcome(
    outcome: LcmExpandServiceOutcome,
) -> SessionRetrievalServiceOutcome {
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

pub(in crate::mcp::tools::handlers) async fn handle_lcm_expand(
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

#[cfg(test)]
mod tests;

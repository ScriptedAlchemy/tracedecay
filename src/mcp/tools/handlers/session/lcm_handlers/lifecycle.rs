use super::super::lcm_args::*;
use super::super::lcm_compact::{lcm_preflight_tool_json, lcm_response_handle_root};
use super::super::lcm_storage::{
    LcmHandlerContext, LcmOpenMode, LcmStorageResolution, open_lcm_storage,
};
use super::super::live_projection::upsert_live_transcript_projection;
use super::super::*;

pub(in crate::mcp::tools::handlers) async fn handle_lcm_session_boundary(
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

pub(in crate::mcp::tools::handlers) async fn handle_lcm_preflight(
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

pub(in crate::mcp::tools::handlers) async fn handle_lcm_compress(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    let response_handle_root = lcm_response_handle_root(context.project_root, &args);
    let summarizer_advisory = summarizer_pressure_advisory(&args)?;
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
            "summarizer_advisory": summarizer_advisory,
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

use super::super::lcm_args::*;
use super::super::lcm_compact::lcm_preflight_tool_json;
use super::super::lcm_storage::{LcmHandlerContext, LcmStorageResolution, resolve_lcm_storage};
use super::super::*;

pub(in crate::mcp::tools::handlers) async fn handle_lcm_preflight(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = required_string_arg(&args, "session_id")?;
    if args.get("transcript_projection").is_some() {
        return Err(argument_error(
            "transcript_projection is only accepted by daemon hook_runtime lcm_preflight",
        ));
    }
    let storage = match resolve_lcm_storage(context, &args) {
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

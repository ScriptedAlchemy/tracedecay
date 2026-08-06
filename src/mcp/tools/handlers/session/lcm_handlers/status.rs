use super::super::lcm_args::*;
use super::super::lcm_storage::{LcmHandlerContext, LcmStorageResolution, resolve_lcm_storage};
use super::super::*;

use super::shared::lcm_status_payload;

pub(in crate::mcp::tools::handlers) async fn handle_lcm_status(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = provider_or_all_arg(&args)?;
    let session_id = string_arg(&args, "session_id");
    let deep = bool_arg(&args, "deep")?.unwrap_or(false);
    if args.get("gc_config").is_some() {
        return Err(argument_error(
            "unknown parameter `gc_config` for read-only LCM status",
        ));
    }
    let storage = match resolve_lcm_storage(context, &args) {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let status = storage
        .db
        .lcm_status_with_options(provider, session_id, deep, &Default::default())
        .await
        .map_err(lcm_error)?;
    Ok(tool_json(
        context.project_root,
        &args,
        &lcm_status_payload(provider, session_id, deep, status),
    ))
}

pub(in crate::mcp::tools::handlers) async fn handle_lcm_doctor(
    context: LcmHandlerContext<'_>,
    args: Value,
) -> Result<ToolResult> {
    let provider = required_specific_provider_arg(&args)?;
    let session_id = string_arg(&args, "session_id");
    for removed in [
        "apply",
        "doctor_clean_apply_enabled",
        "lcm_gc_apply_enabled",
        "gc_config",
        "ignore_session_patterns",
        "stateless_session_patterns",
        "ignore_message_patterns",
    ] {
        if args.get(removed).is_some() {
            return Err(argument_error(format!(
                "unknown parameter `{removed}` for read-only LCM doctor"
            )));
        }
    }
    let mode = lcm_doctor_mode(&args)?;
    let storage = match resolve_lcm_storage(context, &args) {
        LcmStorageResolution::Available(storage) => storage,
        LcmStorageResolution::Unavailable(result) => return Ok(result),
    };
    let mut payload = storage
        .db
        .lcm_doctor(provider, session_id, mode)
        .await
        .map_err(lcm_error)?;
    if let Some(object) = payload.as_object_mut()
        && let Some(diagnostics) = object
            .get_mut("diagnostics")
            .and_then(serde_json::Value::as_object_mut)
    {
        diagnostics.insert(
            "ast_grep".to_string(),
            super::super::super::super::definitions::ast_grep_diagnostics_json(),
        );
    }
    Ok(tool_json(context.project_root, &args, &payload))
}

#[cfg(test)]
mod tests;

use super::super::lcm_args::*;
use super::super::lcm_storage::{
    LcmHandlerContext, LcmOpenMode, LcmStorageResolution, open_lcm_storage,
};
use super::super::*;

use super::shared::lcm_status_payload;

pub(in crate::mcp::tools::handlers) async fn handle_lcm_status(
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

pub(in crate::mcp::tools::handlers) async fn handle_lcm_doctor(
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
            super::super::super::super::definitions::ast_grep_diagnostics_json(),
        );
    }
    Ok(tool_json(context.project_root, &args, &payload))
}

#[cfg(test)]
mod tests;

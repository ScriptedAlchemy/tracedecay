use crate::application::host_admission::{HostAdmissionOutcome, SharedHostAdmissionBroker};
use crate::automation::config_error;
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::tools::ToolResult;
use crate::tracedecay::TraceDecay;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

use super::SessionAuthorities;
use super::support::tool_json;

mod admission;
mod context_scout;
mod envelope;
mod errors;
mod hermes;
mod ingest;

#[cfg(test)]
mod entry_tests;
#[cfg(test)]
mod test_support;

pub(crate) use admission::{
    HookV2AdmissionOutcomeV1, admit_hook_v2_envelope, hook_v2_pending_work_envelopes,
};
pub(crate) use errors::structured_hook_error_data;
pub(crate) use hermes::replay_projectless_hermes_host_admission;

use admission::hook_v2_admit;
use context_scout::{
    ContextScoutReadSurfaceV1, hook_v2_cancel, hook_v2_delivery_receipt, hook_v2_feedback,
    hook_v2_feedback_notice_delivery, hook_v2_scout_prepare, hook_v2_scout_read, hook_v2_status,
};
use errors::map_host_admission_outcome;
use hermes::{hermes_receipt, user_review};
use ingest::{accounting_receipt, codex_compact, cursor_compact, ingest_transcript};

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error(format!("missing required parameter `{key}`")))
}

pub async fn handle_hook_runtime(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
    accounting_db: Option<&RegisteredGlobalDb>,
    session_authorities: SessionAuthorities<'_>,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let output = match action {
        "reset_counter" => {
            cg.reset_local_counter().await?;
            json!({ "action": action, "reset": true })
        }
        "accounting_receipt" => accounting_receipt(cg, accounting_db).await?,
        "hook_v2_admit" | "hook_v2_guidance_lookup" => {
            hook_v2_admit(cg, &args, action, required_project_db(session_authorities)?).await?
        }
        "hook_v2_scout_prepare" => hook_v2_scout_prepare(cg, &args).await?,
        "hook_v2_delivery_receipt" => hook_v2_delivery_receipt(cg, &args).await?,
        "hook_v2_feedback_notice_delivery" => hook_v2_feedback_notice_delivery(cg, &args).await?,
        "hook_v2_feedback" => hook_v2_feedback(cg, &args).await?,
        "hook_v2_cancel" => hook_v2_cancel(cg, &args).await?,
        "hook_v2_status" => hook_v2_status(cg, &args).await?,
        "opencode_lsp_updated" => {
            opencode_lsp_updated(cg, &args, required_project_db(session_authorities)?).await?
        }
        action if ContextScoutReadSurfaceV1::from_action(action).is_some() => {
            hook_v2_scout_read(cg, &args, action).await?
        }
        "ingest_transcript" => {
            if args.get("user_scope").and_then(Value::as_bool) == Some(true) {
                return Err(config_error(
                    "user transcript ingest requires projectless daemon routing",
                ));
            }
            ingest_transcript(Some(cg), &args, None, global_db, session_authorities).await?
        }
        "user_review" | "hermes_receipt" => {
            return Err(config_error(format!(
                "hook action `{action}` requires projectless daemon routing"
            )));
        }
        "codex_compact" => codex_compact(cg, &args, session_authorities).await?,
        "cursor_compact" => cursor_compact(cg, &args, session_authorities).await?,
        other => {
            return Err(config_error(format!(
                "unknown hook runtime action: {other}"
            )));
        }
    };
    Ok(tool_json(Some(cg.project_root()), &args, &output))
}

async fn opencode_lsp_updated(
    cg: &TraceDecay,
    args: &Value,
    project_sessions: &RegisteredGlobalDb,
) -> Result<Value> {
    let event = args
        .get("event")
        .ok_or_else(|| config_error("missing required parameter `event`"))?;
    let payload = serde_json::to_vec(event)
        .map_err(|error| config_error(format!("invalid OpenCode LSP event: {error}")))?;
    tracedecay_hooks::decode_opencode_lsp_event(&payload)
        .map_err(|error| config_error(format!("invalid OpenCode LSP event: {error}")))?;
    crate::application::event_lane::publish(
        project_sessions,
        crate::application::event_lane::ActivityFamilyV1::Hook,
        cg.project_root(),
        None,
        1,
        Some("opencode_lsp_updated"),
    )
    .await;
    Ok(json!({
        "action": "opencode_lsp_updated",
        "status": "accepted",
    }))
}

pub(crate) async fn handle_projectless_hook_runtime(
    args: Value,
    profile_root: &Path,
    session_runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    global_db: &RegisteredGlobalDb,
    session_authorities: SessionAuthorities<'_>,
    host_admission_broker: std::result::Result<&SharedHostAdmissionBroker, HostAdmissionOutcome>,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    if !projectless_action_allowed(action, &args) {
        return Err(config_error(format!(
            "projectless hook runtime action `{action}` is forbidden"
        )));
    }
    let output = match action {
        "ingest_transcript" => {
            ingest_transcript(
                None,
                &args,
                Some(profile_root),
                Some(global_db),
                session_authorities,
            )
            .await?
        }
        "user_review" => user_review(&args, profile_root, &session_runtime_registry).await?,
        "hermes_receipt" => {
            let host_admission_broker =
                host_admission_broker.map_err(map_host_admission_outcome)?;
            hermes_receipt(
                &args,
                profile_root,
                Some(&session_runtime_registry),
                required_user_db(session_authorities)?,
                host_admission_broker,
            )
            .await?
        }
        _ => unreachable!("projectless hook action validated above"),
    };
    Ok(tool_json(None, &args, &output))
}

fn projectless_action_allowed(action: &str, args: &Value) -> bool {
    matches!(action, "user_review" | "hermes_receipt")
        || (action == "ingest_transcript"
            && args.get("user_scope").and_then(Value::as_bool) == Some(true))
}

fn required_value(args: &Value, key: &str) -> Result<Value> {
    args.get(key)
        .cloned()
        .ok_or_else(|| config_error(format!("missing required field `{key}`")))
}

fn required_project_db(authorities: SessionAuthorities<'_>) -> Result<&RegisteredGlobalDb> {
    authorities
        .project
        .map(AsRef::as_ref)
        .ok_or_else(|| config_error("daemon project session database is unavailable"))
}

fn required_user_db(authorities: SessionAuthorities<'_>) -> Result<&RegisteredGlobalDb> {
    authorities
        .user
        .map(AsRef::as_ref)
        .ok_or_else(|| config_error("daemon user session database is unavailable"))
}

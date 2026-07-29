use std::path::Path;

use serde_json::{Value, json};
use tracedecay_store::CompatibilityFeedbackRepairProgressV1;

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::tools::ToolResult;
use crate::memory::types::FeedbackRequest;
use crate::tracedecay::TraceDecay;

use super::super::rendered_tool_json;
use super::super::support::project_selector_present;
use super::args::{fact_id, feedback_action, requests_user_memory, required_str};
use super::fact_store::handle_fact_store_for_target;
use super::{
    config_error, memory_application, memory_application_error, memory_operation_context,
    open_target_memory_db, open_user_memory_target,
};

pub(super) fn feedback_history_repair_payload(
    progress: CompatibilityFeedbackRepairProgressV1,
) -> Value {
    let state = match progress {
        CompatibilityFeedbackRepairProgressV1::Unknown => "unknown",
        CompatibilityFeedbackRepairProgressV1::NotRequired => "not_required",
        CompatibilityFeedbackRepairProgressV1::Complete { .. } => "complete",
        CompatibilityFeedbackRepairProgressV1::Incomplete { .. } => "incomplete",
    };
    json!({
        "state": state,
        "processed": progress.processed(),
        "remaining": progress.remaining(),
    })
}

pub(in crate::mcp::tools::handlers) async fn handle_memory_status(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<ToolResult> {
    let target_memory = open_target_memory_db(cg, &args, global_db).await?;
    let status = memory_application(&target_memory)?
        .memory_status_with_repair_v1()
        .await
        .map_err(memory_application_error)?;
    let value = json!({
        "status": "ok",
        "memory": status.status,
        "feedback_history_repair": feedback_history_repair_payload(status.feedback_history_repair),
    });
    Ok(rendered_tool_json(
        (!target_memory.user_scope).then_some(target_memory.project_root.as_path()),
        &args,
        &value,
    ))
}

pub(crate) async fn handle_user_memory_tool(
    tool_name: &str,
    args: Value,
    registry: &DaemonSessionRuntimeRegistryV1,
    profile_root: &Path,
) -> Result<ToolResult> {
    if !requests_user_memory(&args) {
        return Err(config_error(
            "projectless memory dispatch requires memory_scope=user",
        ));
    }
    let target_memory = open_user_memory_target(registry, profile_root).await?;
    match tool_name {
        "tracedecay_fact_store" => {
            required_str(&args, "action")?;
            if project_selector_present(&args, &["project_path"]) {
                return Err(config_error(
                    "memory_scope=user cannot be combined with a project selector",
                ));
            }
            handle_fact_store_for_target(args, false, target_memory).await
        }
        "tracedecay_fact_feedback" => {
            let note = args
                .get("note")
                .or_else(|| args.get("reason"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let request = FeedbackRequest {
                fact_id: fact_id(&args)?,
                action: feedback_action(&args)?,
                source: args
                    .get("source")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                note,
            };
            let result = memory_application(&target_memory)?
                .record_fact_feedback_v1(
                    request,
                    memory_operation_context(&args, &target_memory, "feedback")?,
                )
                .await
                .map_err(memory_application_error)?;
            Ok(rendered_tool_json(
                None,
                &args,
                &json!({ "status": "recorded", "feedback": result }),
            ))
        }
        "tracedecay_memory_status" => {
            let status = memory_application(&target_memory)?
                .memory_status_with_repair_v1()
                .await
                .map_err(memory_application_error)?;
            Ok(rendered_tool_json(
                None,
                &args,
                &json!({
                    "status": "ok",
                    "memory": status.status,
                    "feedback_history_repair": feedback_history_repair_payload(status.feedback_history_repair),
                }),
            ))
        }
        other => Err(config_error(format!("{other} is not a user-memory tool"))),
    }
}

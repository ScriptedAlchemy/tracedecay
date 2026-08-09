use serde_json::{Value, json};
use tracedecay_store::ProjectMemoryFeedbackRepairProgressV1;

use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::tools::ToolResult;
use crate::tracedecay::TraceDecay;

use super::super::support::tool_json;
use super::{memory_application, memory_application_error, open_target_memory_db};

pub(super) fn feedback_history_repair_payload(
    progress: ProjectMemoryFeedbackRepairProgressV1,
) -> Value {
    let state = match progress {
        ProjectMemoryFeedbackRepairProgressV1::Unknown => "unknown",
        ProjectMemoryFeedbackRepairProgressV1::NotRequired => "not_required",
        ProjectMemoryFeedbackRepairProgressV1::Complete { .. } => "complete",
        ProjectMemoryFeedbackRepairProgressV1::Incomplete { .. } => "incomplete",
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
        .memory_status_with_repair()
        .await
        .map_err(memory_application_error)?;
    let value = json!({
        "status": "ok",
        "memory": status.status,
        "feedback_history_repair": feedback_history_repair_payload(status.feedback_history_repair),
    });
    Ok(tool_json(
        (!target_memory.user_scope).then_some(target_memory.project_root.as_path()),
        &args,
        &value,
    ))
}

use serde_json::{Value, json};

use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::tools::ToolResult;
use crate::memory::types::FeedbackRequest;
use crate::tracedecay::TraceDecay;

use super::super::support::{project_selector_present, tool_json};
use super::args::{fact_id, feedback_action};
use super::fact_store::controlled_memory_application;
use super::{
    config_error, memory_application_error, memory_operation_context, open_target_memory_db,
};

pub(in crate::mcp::tools::handlers) async fn handle_fact_feedback(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    if project_selector_present(&args, &["project_path"]) {
        return Err(config_error(
            "cross-project fact_feedback writes are not supported; omit project_selector to write the active project",
        ));
    }
    let note = args
        .get("note")
        .or_else(|| args.get("reason"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let target_memory = open_target_memory_db(cg, &args, global_db).await?;
    let request = FeedbackRequest {
        fact_id: fact_id(&args)?,
        action: feedback_action(&args)?,
        source: args
            .get("source")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        note,
    };
    let memory = controlled_memory_application(&target_memory, cancellation)?;
    if memory
        .get_fact(request.fact_id)
        .await
        .map_err(memory_application_error)?
        .is_none()
    {
        return Err(config_error(format!("fact {} not found", request.fact_id)));
    }
    let result = memory
        .record_fact_feedback(
            request,
            memory_operation_context(&args, &target_memory, "feedback")?,
        )
        .await
        .map_err(memory_application_error)?;
    let value = json!({ "status": "recorded", "feedback": result });
    Ok(tool_json(
        (!target_memory.user_scope).then_some(target_memory.project_root.as_path()),
        &args,
        &value,
    ))
}

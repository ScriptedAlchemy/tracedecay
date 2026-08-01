use serde_json::{Value, json};

use crate::errors::Result;
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::tools::ToolResult;
use crate::memory::types::FeedbackRequest;
use crate::tracedecay::TraceDecay;

use super::super::support::{project_selector_present, tool_json};
use super::args::{fact_id, feedback_action};
use super::{
    config_error, memory_application, memory_application_error, memory_operation_context,
    open_target_memory_db, refresh_target_memory_digest, with_memory_deadline,
};

pub(in crate::mcp::tools::handlers) async fn handle_fact_feedback(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<ToolResult> {
    if project_selector_present(&args, &["project_path"]) {
        return Err(config_error(
            "cross-project fact_feedback writes are not supported; omit project_selector to write the active project",
        ));
    }
    // Feedback shares the unbounded-await shape of the add path (a lane saw it
    // hang on a nonexistent id), so bound the whole store-touching operation on
    // one deadline rather than let it pin the transport open.
    with_memory_deadline("fact_feedback", async move {
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
        let memory = memory_application(&target_memory)?;
        let result = memory
            .record_fact_feedback_v1(
                request,
                memory_operation_context(&args, &target_memory, "feedback")?,
            )
            .await
            .map_err(memory_application_error)?;
        if !target_memory.user_scope {
            refresh_target_memory_digest(&memory, &target_memory).await;
        }
        let value = json!({ "status": "recorded", "feedback": result });
        Ok(tool_json(
            (!target_memory.user_scope).then_some(target_memory.project_root.as_path()),
            &args,
            &value,
        ))
    })
    .await
}

//! MCP adapter for the canonical Work application owner.
//!
//! Work owns a typed HTTP envelope already. MCP builds that exact request and
//! returns the owner's envelope as JSON content, so request decoding, binding
//! lookup, cancellation policy, result contracts, and failure taxonomy cannot
//! drift between the two transports. The composition root supplies the
//! daemon-owned invoke; this crate never imports that owner.

use std::future::Future;

use serde_json::Value;
use tracedecay_api::{HttpApplicationControls, WorkHttpRequest, WorkOperation};
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{CancellationSignal, Deadline, RequestId};
use tracedecay_daemon_protocol::invocation_now_micros;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_tool_catalog::OperationId;

use crate::ToolResult;
use crate::handlers::support::unknown_tool_error;
use crate::text_tool_result;

fn json_result(value: &Value) -> ToolResult {
    text_tool_result(&value.to_string(), Vec::new())
}

#[hotpath::measure(future = true, label = "mcp.work.total")]
pub async fn handle_work<Invoke, InvokeFuture>(
    tool_name: &str,
    mut body: Value,
    invoke: Option<Invoke>,
    protocol_request_id: Option<RequestId>,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<ToolResult>
where
    Invoke: FnOnce(WorkHttpRequest) -> InvokeFuture,
    InvokeFuture: Future<Output = Result<Value>>,
{
    let (operation, request_id, controls) = hotpath::measure_block!("mcp.work.request_build", {
        let operation =
            work_operation_for_tool(tool_name).ok_or_else(|| unknown_tool_error(tool_name))?;
        let request_id = protocol_request_id.map_or_else(mint_request_id, Ok)?;
        let controls = work_controls(
            operation,
            &request_id,
            protocol_deadline,
            protocol_cancellation,
        )?;
        if let Some(object) = body.as_object_mut() {
            // MCP presentation and request-correlation fields never belong to a
            // typed Work request body.
            object.remove("format");
            object.remove("__mcp_request_id");
        }
        (operation, request_id, controls)
    });
    let Some(invoke) = invoke else {
        return Err(TraceDecayError::project_route(
            "work.daemon_unavailable",
            true,
            "The Work daemon invocation owner is unavailable",
        ));
    };
    let payload = hotpath::future!(
        invoke(WorkHttpRequest {
            operation,
            request_id,
            controls,
            body,
        }),
        label = "mcp.work.invoke"
    )
    .await?;
    hotpath::measure_block!("mcp.work.result_assemble", {
        let result = json_result(&payload);
        Ok(
            if payload.get("kind").and_then(Value::as_str) == Some("problem") {
                result.with_semantic_error(true)
            } else {
                result.with_semantic_error(false)
            },
        )
    })
}

pub fn work_operation_for_tool(tool_name: &str) -> Option<WorkOperation> {
    let key = tool_name.strip_prefix("tracedecay_work_")?;
    WorkOperation::ALL
        .into_iter()
        .find(|operation| operation.operation_key() == key)
}

fn mint_request_id() -> Result<RequestId> {
    mint_global_request_id(GlobalRequestSurface::McpFallback).map_err(|_| TraceDecayError::Config {
        message: "could not allocate a Work request id".to_owned(),
    })
}

fn work_controls(
    operation: WorkOperation,
    request_id: &RequestId,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<HttpApplicationControls> {
    let operation_id = OperationId::new(operation.operation_id()).map_err(|error| {
        TraceDecayError::project_route(
            "work.operation_identity_unavailable",
            false,
            format!("The canonical Work operation identity is invalid: {error}"),
        )
    })?;
    let binding = tracedecay_contracts::work_executable_binding(&operation_id)
        .map_err(|error| {
            TraceDecayError::project_route(
                "work.catalog_unavailable",
                false,
                format!("The canonical Work catalog is unavailable: {error}"),
            )
        })?
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "work.binding_unavailable",
                false,
                "The canonical Work operation is not advertised by this build",
            )
        })?;
    let maximum_micros = i64::try_from(
        std::time::Duration::from_millis(binding.deadline().maximum_millis()).as_micros(),
    )
    .map_err(|_| {
        TraceDecayError::project_route(
            "work.deadline_unavailable",
            false,
            "The canonical Work deadline exceeds the domain clock",
        )
    })?;
    let maximum_deadline = UtcMicros(invocation_now_micros().0.saturating_add(maximum_micros));
    let deadline = protocol_deadline
        .filter(|deadline| deadline.expires_at <= maximum_deadline)
        .map_or_else(|| Deadline::new(maximum_deadline), Ok)
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    let cancellation = protocol_cancellation
        .map_or_else(
            || CancellationSignal::active(format!("cancellation.{}", request_id.as_str())),
            Ok,
        )
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    Ok(HttpApplicationControls {
        deadline,
        cancellation,
    })
}

#[cfg(test)]
mod tests {
    use super::work_operation_for_tool;

    #[test]
    fn maps_every_canonical_work_operation_without_a_second_name_list() {
        for operation in tracedecay_api::WorkOperation::ALL {
            let name = format!("tracedecay_work_{}", operation.operation_key());
            assert_eq!(work_operation_for_tool(&name), Some(operation));
        }
        assert_eq!(work_operation_for_tool("tracedecay_work_missing"), None);
        for retired in [
            "tracedecay_work_snapshot",
            "tracedecay_work_delta",
            "tracedecay_work_replan_dependencies",
            "tracedecay_work_accept_task",
        ] {
            assert_eq!(work_operation_for_tool(retired), None, "{retired}");
        }
    }
}

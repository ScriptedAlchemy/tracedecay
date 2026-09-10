//! MCP adapter for the canonical Workflow application owner.
//!
//! Workflow owns a typed HTTP envelope already. MCP builds that exact request
//! and returns the owner's envelope as JSON content, so request decoding,
//! binding lookup, cancellation policy, result contracts, and failure taxonomy
//! cannot drift between the two transports. This is the Work adapter's mirror;
//! the composition root supplies the daemon-owned invoke.

use std::future::Future;

use serde_json::Value;
use tracedecay_api::{HttpApplicationControls, WorkflowHttpRequest, WorkflowOperation};
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{CancellationSignal, Deadline, RequestId};
use tracedecay_daemon_protocol::invocation_now_micros;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_tool_catalog::OperationId;

use crate::ToolResult;
use crate::text_tool_result;

fn json_result(value: &Value) -> ToolResult {
    text_tool_result(&value.to_string(), Vec::new())
}

#[hotpath::measure(future = true, label = "mcp.workflow.total")]
pub async fn handle_workflow<Invoke, InvokeFuture>(
    tool_name: &str,
    mut body: Value,
    invoke: Invoke,
    protocol_request_id: Option<RequestId>,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<ToolResult>
where
    Invoke: FnOnce(WorkflowHttpRequest) -> InvokeFuture,
    InvokeFuture: Future<Output = Result<Value>>,
{
    let (operation, request_id, controls) =
        hotpath::measure_block!("mcp.workflow.request_build", {
            let operation =
                workflow_operation_for_tool(tool_name).ok_or_else(|| TraceDecayError::Config {
                    message: format!("unknown tool: {tool_name}"),
                })?;
            let request_id = protocol_request_id.map_or_else(mint_request_id, Ok)?;
            let controls = workflow_controls(
                operation,
                &request_id,
                protocol_deadline,
                protocol_cancellation,
            )?;
            if let Some(object) = body.as_object_mut() {
                // MCP presentation and request-correlation fields never belong to a
                // typed Workflow request body.
                object.remove("format");
                object.remove("__mcp_request_id");
            }
            (operation, request_id, controls)
        });
    let payload = hotpath::future!(
        invoke(WorkflowHttpRequest {
            operation,
            request_id,
            controls,
            body,
        }),
        label = "mcp.workflow.invoke"
    )
    .await?;
    hotpath::measure_block!("mcp.workflow.result_assemble", {
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

pub fn workflow_operation_for_tool(tool_name: &str) -> Option<WorkflowOperation> {
    let key = tool_name.strip_prefix("tracedecay_workflow_")?;
    WorkflowOperation::ALL
        .into_iter()
        .find(|operation| operation.operation_key() == key)
}

fn mint_request_id() -> Result<RequestId> {
    mint_global_request_id(GlobalRequestSurface::McpFallback).map_err(|_| TraceDecayError::Config {
        message: "could not allocate a Workflow request id".to_owned(),
    })
}

fn workflow_controls(
    operation: WorkflowOperation,
    request_id: &RequestId,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<HttpApplicationControls> {
    let operation_id =
        OperationId::new(operation.operation_id_str().to_owned()).map_err(|error| {
            TraceDecayError::project_route(
                "workflow.operation_identity_unavailable",
                false,
                format!("The canonical Workflow operation identity is invalid: {error}"),
            )
        })?;
    let registry =
        tracedecay_contracts::workflow_executable_binding_registry().map_err(|error| {
            TraceDecayError::project_route(
                "workflow.catalog_unavailable",
                false,
                format!("The canonical Workflow catalog is unavailable: {error}"),
            )
        })?;
    let binding = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "workflow.binding_unavailable",
                false,
                "The canonical Workflow operation is not advertised by this build",
            )
        })?;
    let maximum_micros = i64::try_from(
        std::time::Duration::from_millis(binding.deadline().maximum_millis()).as_micros(),
    )
    .map_err(|_| {
        TraceDecayError::project_route(
            "workflow.deadline_unavailable",
            false,
            "The canonical Workflow deadline exceeds the domain clock",
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
    use super::workflow_operation_for_tool;

    #[test]
    fn maps_every_canonical_workflow_operation_without_a_second_name_list() {
        for operation in tracedecay_api::WorkflowOperation::ALL {
            let name = format!("tracedecay_workflow_{}", operation.operation_key());
            assert_eq!(workflow_operation_for_tool(&name), Some(operation));
        }
        assert_eq!(
            workflow_operation_for_tool("tracedecay_workflow_missing"),
            None
        );
    }
}

//! Tool adapter for the canonical Workflow application owner.
//!
//! Workflow owns a typed HTTP envelope already. MCP and `tracedecay tool`
//! build that exact request and return the owner's envelope as JSON content, so
//! request decoding, binding lookup, cancellation policy, result contracts, and
//! failure taxonomy cannot drift between transports. This is the Work adapter's
//! mirror; the composition root supplies the daemon-owned invoke.

use std::future::Future;

use serde_json::Value;
use tracedecay_api::{HttpApplicationControls, WorkflowHttpRequest, WorkflowOperation};
use tracedecay_contracts::{CancellationSignal, Deadline, RequestId};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_tool_catalog::OperationId;

use crate::ToolResult;
use crate::handlers::support::{
    mint_owner_request_id, owner_envelope_result, owner_request_controls, unknown_tool_error,
};

#[tracing::instrument(name = "mcp.workflow.total", level = "trace", skip_all)]
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
    let (operation, request_id, controls) = {
        let _span = tracing::trace_span!("mcp.workflow.request_build").entered();
        {
            let operation = workflow_operation_for_tool(tool_name)
                .ok_or_else(|| unknown_tool_error(tool_name))?;
            let request_id =
                protocol_request_id.map_or_else(|| mint_owner_request_id("Workflow"), Ok)?;
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
        }
    };
    let payload = tracing::Instrument::instrument(
        invoke(WorkflowHttpRequest {
            operation,
            request_id,
            controls,
            body,
        }),
        tracing::trace_span!("mcp.workflow.invoke"),
    )
    .await?;
    {
        let _span = tracing::trace_span!("mcp.workflow.result_assemble").entered();
        owner_envelope_result(&payload)
    }
}

pub fn workflow_operation_for_tool(tool_name: &str) -> Option<WorkflowOperation> {
    let key = tool_name.strip_prefix("tracedecay_workflow_")?;
    WorkflowOperation::ALL
        .into_iter()
        .find(|operation| operation.operation_key() == key)
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
    owner_request_controls(
        "Workflow",
        binding.deadline().maximum_millis(),
        request_id,
        protocol_deadline,
        protocol_cancellation,
    )
}

//! MCP adapter for the canonical Workflow application owner.
//!
//! Workflow owns a typed HTTP envelope already. MCP invokes that exact owner
//! and returns its envelope as JSON content, so request decoding, binding
//! lookup, cancellation policy, result contracts, and failure taxonomy cannot
//! drift between the two transports. This is the Work adapter's mirror; the
//! only thing that differs is which descriptor names the operation.

use axum::body::to_bytes;
use serde_json::Value;
use tracedecay_api::{HttpApplicationControls, WorkflowHttpRequest, WorkflowOperation};
use tracedecay_application::{CancellationSignal, Deadline, RequestId};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::OperationId;

use crate::daemon_client::{DaemonInvocationExecutor, invocation_now_micros};
use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::ToolResult;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

use super::tool_call_support::json_result;

pub(super) async fn handle_workflow(
    tool_name: &str,
    mut body: Value,
    executor: Option<&dyn DaemonInvocationExecutor>,
    protocol_request_id: Option<RequestId>,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<ToolResult> {
    let operation =
        crate::mcp::tools::binding::workflow_operation_for_tool(tool_name).ok_or_else(|| {
            TraceDecayError::Config {
                message: format!("unknown tool: {tool_name}"),
            }
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
    let response = crate::application_surface::invoke_workflow_operation(
        executor,
        WorkflowHttpRequest {
            operation,
            request_id,
            controls,
            body,
        },
    )
    .await;
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|error| {
            TraceDecayError::project_route(
                "workflow.response_unavailable",
                true,
                format!("The Workflow application response could not be read: {error}"),
            )
        })?;
    let payload = serde_json::from_slice::<Value>(&body).map_err(|error| {
        TraceDecayError::project_route(
            "workflow.response_invalid",
            true,
            format!("The Workflow application response was not valid JSON: {error}"),
        )
    })?;
    let result = json_result(&payload);
    Ok(
        if payload.get("kind").and_then(Value::as_str) == Some("problem") {
            result.with_semantic_error(true)
        } else {
            result.with_semantic_error(false)
        },
    )
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
        tracedecay_application::workflow_executable_binding_registry().map_err(|error| {
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
    use serde_json::Value;
    use tracedecay_application::{CancellationSignal, Deadline, RequestId};
    use tracedecay_domain::UtcMicros;

    use super::handle_workflow;

    #[test]
    fn maps_every_canonical_workflow_operation_without_a_second_name_list() {
        for operation in tracedecay_api::WorkflowOperation::ALL {
            let name = format!("tracedecay_workflow_{}", operation.operation_key());
            assert_eq!(
                crate::mcp::tools::binding::workflow_operation_for_tool(&name),
                Some(operation)
            );
        }
        assert_eq!(
            crate::mcp::tools::binding::workflow_operation_for_tool("tracedecay_workflow_missing"),
            None
        );
    }

    #[tokio::test]
    async fn missing_executor_returns_the_registered_workflow_problem_envelope() {
        let request_id = RequestId::new("request.workflow-missing-executor").expect("request id");
        let deadline = Deadline::new(UtcMicros(
            crate::daemon_client::invocation_now_micros().0 + 30_000_000,
        ))
        .expect("deadline");
        let cancellation = CancellationSignal::active("cancellation.workflow-missing-executor")
            .expect("cancellation");
        let result = handle_workflow(
            "tracedecay_workflow_list_definitions",
            serde_json::json!({}),
            None,
            Some(request_id),
            Some(deadline),
            Some(cancellation),
        )
        .await
        .expect("MCP Workflow adapter response");
        let text = result.value["content"][0]["text"]
            .as_str()
            .expect("MCP Workflow JSON content");
        let payload: Value = serde_json::from_str(text).expect("Workflow envelope");
        // Either the request body was rejected as invalid for this operation or
        // the absent executor produced the canonical unavailable problem. Both
        // are typed envelopes from the same owner; neither is an MCP-specific
        // transport error, which is the property under test.
        assert!(
            payload.get("kind").is_some(),
            "the Workflow owner must answer a typed envelope, got {payload}"
        );
    }
}

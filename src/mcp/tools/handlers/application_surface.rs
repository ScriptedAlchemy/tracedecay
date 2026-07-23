use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tracedecay_application::{ApplicationProblemKind, CancellationSignal, Deadline, RequestId};

use super::rendered_tool_json;
use crate::application_surface::{
    ApplicationSurfaceInvocationResult, ApplicationSurfaceOperation,
    parse_application_surface_request,
};
use crate::daemon_client::{DaemonInvocationClient, RequestedOutputFormat};
use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::dispatch::{
    resolve_mcp_application_surface, resolve_mcp_application_surface_with_controls,
};
use crate::tracedecay::{TraceDecay, current_timestamp};

static NEXT_SURFACE_REQUEST: AtomicU64 = AtomicU64::new(1);

fn request_id() -> Result<RequestId> {
    let sequence = NEXT_SURFACE_REQUEST.fetch_add(1, Ordering::Relaxed);
    RequestId::new(format!("request.mcp.{}.{}", current_timestamp(), sequence)).map_err(|_| {
        TraceDecayError::Config {
            message: "could not allocate an application surface request id".to_owned(),
        }
    })
}

pub(super) async fn handle_application_surface(
    cg: &TraceDecay,
    operation: ApplicationSurfaceOperation,
    args: &Value,
    client: Option<&DaemonInvocationClient>,
    protocol_request_id: Option<RequestId>,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<crate::mcp::tools::ToolResult> {
    let requested_format = if super::super::render::wants_json(args) {
        RequestedOutputFormat::Json
    } else {
        RequestedOutputFormat::Markdown
    };
    let mut request_args = args.clone();
    if let Some(object) = request_args.as_object_mut() {
        object.remove("__mcp_request_id");
    }
    let render_args = request_args.clone();
    if let Some(object) = request_args.as_object_mut() {
        // `format` selects the rendered output only; it is not part of any
        // reviewed surface schema and must not reach schema validation.
        object.remove("format");
    }
    let request_id = protocol_request_id.unwrap_or(request_id()?);
    let request = match parse_application_surface_request(operation, request_args) {
        Ok(request) => request,
        Err(error) => {
            crate::application_surface::observe_surface_argument_rejection(
                client,
                tracedecay_tool_catalog::BindingSurface::Mcp,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Err(TraceDecayError::Config {
                message: error.to_string(),
            });
        }
    };
    let result = match (protocol_deadline, protocol_cancellation) {
        (Some(deadline), Some(cancellation)) => {
            resolve_mcp_application_surface_with_controls(
                operation,
                request_id,
                request,
                requested_format,
                deadline,
                cancellation,
                client,
            )
            .await
        }
        _ => {
            resolve_mcp_application_surface(
                operation,
                request_id,
                request,
                requested_format,
                client,
            )
            .await
        }
    }
    .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })?;

    render_result(cg, &render_args, result)
}

fn render_result(
    cg: &TraceDecay,
    args: &Value,
    result: ApplicationSurfaceInvocationResult,
) -> Result<crate::mcp::tools::ToolResult> {
    match result.result {
        Ok(application) => {
            let value = serde_json::to_value(application)?;
            Ok(rendered_tool_json(Some(cg.project_root()), args, &value))
        }
        Err(problem) => {
            let failure_message = match problem.problem.kind() {
                ApplicationProblemKind::NotFoundOrNotAuthorized => {
                    "application surface was not found or is not authorized"
                }
                ApplicationProblemKind::Unavailable => "application surface unavailable",
                _ => "application surface request failed",
            };
            let value = serde_json::to_value(problem)?;
            Ok(rendered_tool_json(Some(cg.project_root()), args, &value)
                .with_semantic_error(true)
                .with_failure_message(failure_message))
        }
    }
}

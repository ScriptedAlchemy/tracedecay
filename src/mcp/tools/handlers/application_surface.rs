use serde_json::Value;
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblemKind, ApplicationResult, CancellationSignal, Deadline,
    InvocationTarget, OperationTermination, RequestId,
};
use tracedecay_tool_catalog::BindingId;

use crate::application_output::view::CanonicalHumanView;
use crate::application_surface::{
    ApplicationSurfaceInvocationResult, ApplicationSurfaceOperation, NormalizedApplicationToolArgs,
    parse_application_surface_request,
};
use crate::daemon_client::{DaemonInvocationExecutor, RequestedOutputFormat};
use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::dispatch::resolve_mcp_application_surface_with_controls_for_target;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use crate::tracedecay::TraceDecay;

fn request_id() -> Result<RequestId> {
    mint_global_request_id(GlobalRequestSurface::McpFallback).map_err(|_| TraceDecayError::Config {
        message: "could not allocate an application surface request id".to_owned(),
    })
}

fn complete_protocol_controls(
    request_id: &RequestId,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<(Deadline, CancellationSignal)> {
    match (deadline, cancellation) {
        (Some(deadline), Some(cancellation)) => Ok((deadline, cancellation)),
        _ => Err(TraceDecayError::mcp_tool_dispatch(
            "tool_dispatch_control_missing",
            crate::mcp::server::McpToolDispatchStage::QueueAdmission.as_str(),
            false,
            format!(
                "application surface request '{}' requires its admitted deadline and cancellation control",
                request_id.as_str()
            ),
        )),
    }
}

pub(super) async fn handle_application_surface(
    cg: &TraceDecay,
    operation: ApplicationSurfaceOperation,
    normalized: NormalizedApplicationToolArgs,
    executor: Option<&dyn DaemonInvocationExecutor>,
    target: InvocationTarget,
    protocol_request_id: Option<RequestId>,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<crate::mcp::tools::ToolResult> {
    let NormalizedApplicationToolArgs {
        request: request_args,
        requested_format,
    } = normalized;
    let request_id = protocol_request_id.unwrap_or(request_id()?);
    let request = match parse_application_surface_request(operation, request_args) {
        Ok(request) => request,
        Err(error) => {
            crate::application_surface::observe_surface_argument_rejection(
                executor,
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
    let (deadline, cancellation) =
        complete_protocol_controls(&request_id, protocol_deadline, protocol_cancellation)?;
    let result = resolve_mcp_application_surface_with_controls_for_target(
        operation,
        request_id,
        request,
        requested_format,
        deadline,
        cancellation,
        target,
        executor,
    )
    .await
    .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })?;

    render_result(cg, result)
}

fn render_result(
    cg: &TraceDecay,
    result: ApplicationSurfaceInvocationResult,
) -> Result<crate::mcp::tools::ToolResult> {
    let terminal = application_terminal(&result.result);
    let (value, failure_message) = match &result.result {
        Ok(application) => (
            serde_json::to_value(application)?,
            match terminal {
                crate::mcp::server::McpToolCallTerminal::Unavailable => {
                    Some("application surface unavailable")
                }
                crate::mcp::server::McpToolCallTerminal::Cancelled => {
                    Some("application surface cancelled")
                }
                crate::mcp::server::McpToolCallTerminal::DeadlineExceeded => {
                    Some("application surface timed out")
                }
                crate::mcp::server::McpToolCallTerminal::Failed => {
                    Some("application surface failed")
                }
                _ => None,
            },
        ),
        Err(problem) => {
            let failure_message = match problem.problem.kind() {
                ApplicationProblemKind::NotFoundOrNotAuthorized => {
                    "application surface was not found or is not authorized"
                }
                ApplicationProblemKind::Unavailable => "application surface unavailable",
                _ => "application surface request failed",
            };
            (serde_json::to_value(problem)?, Some(failure_message))
        }
    };
    let markdown = match result.requested_format {
        RequestedOutputFormat::Json => None,
        RequestedOutputFormat::Markdown => Some(render_canonical_markdown(
            result.operation.as_str(),
            &result.binding_id,
            &result.result,
        )?),
    };
    let text = super::super::render::finalize_with_format(
        Some(cg.project_root()),
        result.requested_format,
        &value,
        || markdown.unwrap_or_default(),
    );
    let mut rendered = super::text_tool_result(&text);
    let Some(result_object) = rendered.value.as_object_mut() else {
        return Err(TraceDecayError::Config {
            message: "application surface MCP result did not materialize as an object".to_owned(),
        });
    };
    let meta = result_object
        .entry("_meta")
        .or_insert_with(|| serde_json::json!({}));
    let Some(meta) = meta.as_object_mut() else {
        return Err(TraceDecayError::Config {
            message: "application surface MCP result metadata is not an object".to_owned(),
        });
    };
    meta.insert(
        crate::mcp::server::APPLICATION_TERMINAL_KEY.to_owned(),
        Value::String(terminal.as_str().to_owned()),
    );
    Ok(match failure_message {
        Some(failure_message) => rendered
            .with_semantic_error(true)
            .with_failure_message(failure_message),
        None => rendered,
    })
}

fn application_terminal(
    result: &ApplicationResult<Value>,
) -> crate::mcp::server::McpToolCallTerminal {
    let termination = match result {
        Ok(application) => match &application.outcome {
            ApplicationOutcome::Evidence(evidence) => evidence.execution.termination,
            ApplicationOutcome::Preview(preview) => preview.execution.termination,
            ApplicationOutcome::Effect(effect) => effect.execution.termination,
        },
        Err(problem) => {
            return match problem.problem.kind() {
                ApplicationProblemKind::InvalidRequest
                | ApplicationProblemKind::NotFoundOrNotAuthorized => {
                    crate::mcp::server::McpToolCallTerminal::Denied
                }
                ApplicationProblemKind::Unavailable | ApplicationProblemKind::Saturated => {
                    crate::mcp::server::McpToolCallTerminal::Unavailable
                }
                ApplicationProblemKind::Cancelled => {
                    crate::mcp::server::McpToolCallTerminal::Cancelled
                }
                ApplicationProblemKind::TimedOut => {
                    crate::mcp::server::McpToolCallTerminal::DeadlineExceeded
                }
                ApplicationProblemKind::Conflict
                | ApplicationProblemKind::Stale
                | ApplicationProblemKind::Unsupported => {
                    crate::mcp::server::McpToolCallTerminal::Failed
                }
            };
        }
    };
    match termination {
        OperationTermination::Completed | OperationTermination::Partial => {
            crate::mcp::server::McpToolCallTerminal::Completed
        }
        OperationTermination::Cancelled => crate::mcp::server::McpToolCallTerminal::Cancelled,
        OperationTermination::TimedOut => crate::mcp::server::McpToolCallTerminal::DeadlineExceeded,
        OperationTermination::Unavailable => crate::mcp::server::McpToolCallTerminal::Unavailable,
        OperationTermination::Failed | OperationTermination::EffectUnknown => {
            crate::mcp::server::McpToolCallTerminal::Failed
        }
    }
}

fn render_canonical_markdown(
    operation: &str,
    binding_id: &BindingId,
    result: &ApplicationResult<Value>,
) -> serde_json::Result<String> {
    let view = CanonicalHumanView::from_application_result(operation, binding_id, result)?;
    Ok(crate::application_output::markdown::render(view)
        .as_str()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tracedecay_application::{
        ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult, CancellationSignal,
        Deadline, RequestId, ResultContractRef, SafeDiagnostic,
    };
    use tracedecay_domain::UtcMicros;
    use tracedecay_tool_catalog::{BindingId, SchemaId};

    use super::{application_terminal, complete_protocol_controls, render_canonical_markdown};

    #[test]
    fn rejects_a_supplied_deadline_without_admitted_cancellation() {
        let request_id = RequestId::new("request.mcp.controls.deadline").unwrap();
        let deadline = Deadline::new(UtcMicros(91)).unwrap();

        let error = complete_protocol_controls(&request_id, Some(deadline), None).unwrap_err();
        assert!(error.to_string().contains("tool_dispatch_control_missing"));
    }

    #[test]
    fn rejects_live_cancellation_without_its_admitted_deadline() {
        let request_id = RequestId::new("request.mcp.controls.cancellation").unwrap();
        let cancellation = CancellationSignal::active("cancel.protocol.exact").unwrap();
        let observer = cancellation.clone();

        let error = complete_protocol_controls(&request_id, None, Some(cancellation)).unwrap_err();

        assert!(error.to_string().contains("tool_dispatch_control_missing"));
        assert_eq!(
            observer.context().token_id.as_str(),
            "cancel.protocol.exact"
        );
        assert!(!observer.is_cancelled());
    }

    #[test]
    fn rejects_dispatch_without_admitted_protocol_controls() {
        let request_id = RequestId::new("request.mcp.controls.default").unwrap();
        let error = complete_protocol_controls(&request_id, None, None).unwrap_err();
        assert!(error.to_string().contains("tool_dispatch_control_missing"));
    }

    #[test]
    fn canonical_problem_markdown_matches_the_cli_contract() {
        let result: ApplicationResult<Value> = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(SchemaId::new("schema.test.result").unwrap(), 3).unwrap(),
            RequestId::new("request.mcp.golden").unwrap(),
            ApplicationProblem::unavailable(
                SafeDiagnostic::new(
                    "daemon_unavailable",
                    "The owning TraceDecay daemon is unavailable",
                )
                .unwrap(),
            ),
        ));

        let rendered = render_canonical_markdown(
            "feedback_list",
            &BindingId::new("binding.mcp.feedback-list.v1").unwrap(),
            &result,
        )
        .unwrap();

        assert_eq!(
            application_terminal(&result),
            crate::mcp::server::McpToolCallTerminal::Unavailable
        );
        assert!(rendered.starts_with("## feedback\\_list\n"));
        assert!(rendered.contains("\n- Operation: `feedback_list`"));
        assert!(rendered.contains("\n- Binding: `binding.mcp.feedback-list.v1`"));
        assert!(rendered.contains("\n- Status: `problem`"));
        assert!(rendered.contains("\n- Problem: `daemon_unavailable`"));
        assert!(rendered.contains("\n- Retry: `after_delay`"));
        assert!(!rendered.contains("### contract"));
    }
}

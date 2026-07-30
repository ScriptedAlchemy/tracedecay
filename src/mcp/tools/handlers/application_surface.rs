use serde_json::Value;
use tracedecay_application::{
    ApplicationProblemKind, ApplicationResult, CancellationSignal, Deadline, InvocationTarget,
    RequestId,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::BindingId;

use crate::application_output::view::CanonicalHumanView;
use crate::application_surface::{
    ApplicationSurfaceInvocationResult, ApplicationSurfaceOperation, NormalizedApplicationToolArgs,
    parse_application_surface_request,
};
use crate::daemon_client::{
    DaemonInvocationExecutor, InvocationCancellationPolicy, RequestedOutputFormat,
};
use crate::errors::{Result, TraceDecayError};
use crate::mcp::tools::dispatch::{
    resolve_mcp_application_surface_for_target,
    resolve_mcp_application_surface_with_controls_for_target,
};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use crate::tracedecay::{TraceDecay, current_timestamp};

const DEFAULT_SURFACE_DEADLINE_MICROS: i64 = 30_000_000;
pub(super) const MULTI_ROOT_TOOL_NAMES: [&str; 3] = [
    "tracedecay_multi_root_scope_set_read",
    "tracedecay_multi_root_scope_set_compare_and_swap",
    "tracedecay_multi_root_execute",
];

fn request_id() -> Result<RequestId> {
    mint_global_request_id(GlobalRequestSurface::McpFallback).map_err(|_| TraceDecayError::Config {
        message: "could not allocate an application surface request id".to_owned(),
    })
}

fn complete_protocol_controls(
    request_id: &RequestId,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<Option<(Deadline, CancellationSignal)>> {
    match (deadline, cancellation) {
        (None, None) => Ok(None),
        (deadline, cancellation) => {
            // Match the canonical application dispatch's 30-second default
            // when protocol cancellation exists without a deadline.
            let deadline = match deadline {
                Some(deadline) => deadline,
                None => Deadline::new(UtcMicros(
                    current_timestamp()
                        .saturating_mul(1_000_000)
                        .saturating_add(DEFAULT_SURFACE_DEADLINE_MICROS),
                ))
                .map_err(|error| TraceDecayError::Config {
                    message: error.to_string(),
                })?,
            };
            let cancellation = match cancellation {
                Some(cancellation) => cancellation,
                None => CancellationSignal::active(format!("cancellation.{}", request_id.as_str()))
                    .map_err(|error| TraceDecayError::Config {
                        message: error.to_string(),
                    })?,
            };
            Ok(Some((deadline, cancellation)))
        }
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
    let controls =
        complete_protocol_controls(&request_id, protocol_deadline, protocol_cancellation)?;
    let result = match controls {
        Some((deadline, cancellation)) => {
            resolve_mcp_application_surface_with_controls_for_target(
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
        }
        None => {
            resolve_mcp_application_surface_for_target(
                operation,
                request_id,
                request,
                requested_format,
                target,
                executor,
            )
            .await
        }
    }
    .map_err(|error| TraceDecayError::Config {
        message: error.to_string(),
    })?;

    render_result(cg, result)
}

pub(super) async fn handle_multi_root(
    tool_name: &str,
    args: Value,
    executor: Option<&dyn DaemonInvocationExecutor>,
    protocol_request_id: Option<RequestId>,
    protocol_deadline: Option<Deadline>,
    protocol_cancellation: Option<CancellationSignal>,
) -> Result<crate::mcp::tools::ToolResult> {
    let executor = executor.ok_or_else(|| TraceDecayError::Config {
        message: "multi-root daemon authority is unavailable".to_owned(),
    })?;
    let request_id = protocol_request_id.unwrap_or(request_id()?);
    let (deadline, cancellation) = complete_protocol_controls(
        &request_id,
        protocol_deadline.or_else(|| {
            Deadline::new(UtcMicros(
                current_timestamp()
                    .saturating_mul(1_000_000)
                    .saturating_add(DEFAULT_SURFACE_DEADLINE_MICROS),
            ))
            .ok()
        }),
        protocol_cancellation,
    )?
    .ok_or_else(|| TraceDecayError::Config {
        message: "multi-root invocation controls are unavailable".to_owned(),
    })?;
    let observed_at = UtcMicros(current_timestamp().saturating_mul(1_000_000));
    let invocation = match tool_name {
        "tracedecay_multi_root_scope_set_read" => {
            let request = serde_json::from_value(args)?;
            crate::daemon::DaemonInvocationRequest::multi_root_scope_set_read(
                request_id.as_str(),
                request,
                observed_at,
                deadline.clone(),
                cancellation.context(),
            )
        }
        "tracedecay_multi_root_scope_set_compare_and_swap" => {
            let request = serde_json::from_value(args)?;
            crate::daemon::DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
                request_id.as_str(),
                request,
                observed_at,
                deadline.clone(),
                cancellation.context(),
            )
        }
        "tracedecay_multi_root_execute" => {
            let request = serde_json::from_value(args)?;
            crate::daemon::DaemonInvocationRequest::multi_root_execute(
                request_id.as_str(),
                request,
                observed_at,
                deadline.clone(),
                cancellation.context(),
            )
        }
        _ => {
            return Err(TraceDecayError::Config {
                message: format!("unknown multi-root tool: {tool_name}"),
            });
        }
    };
    let response = executor
        .invoke_controlled(
            invocation,
            deadline,
            cancellation,
            if tool_name == "tracedecay_multi_root_scope_set_compare_and_swap" {
                InvocationCancellationPolicy::AuthoritativeEffect
            } else {
                InvocationCancellationPolicy::ReadOnly
            },
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("multi-root invocation failed: {error:?}"),
        })?;
    let (value, failure) = match response.outcome {
        crate::daemon::DaemonInvocationOutcome::MultiRootScopeSetRead { scope, outcome } => {
            (
                serde_json::json!({"request_id": request_id, "scope": scope, "outcome": outcome}),
                None,
            )
        }
        crate::daemon::DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap {
            scope,
            outcome,
        } => (
            serde_json::json!({"request_id": request_id, "scope": scope, "outcome": outcome}),
            None,
        ),
        crate::daemon::DaemonInvocationOutcome::MultiRootQueryPage { scope, outcome } => {
            (
                serde_json::json!({"request_id": request_id, "scope": scope, "outcome": outcome}),
                None,
            )
        }
        crate::daemon::DaemonInvocationOutcome::Problem { problem } => {
            let (status, reason) = match problem {
                crate::daemon::DaemonInvocationProblem::InvalidRequest
                | crate::daemon::DaemonInvocationProblem::UnsupportedRevision => {
                    ("denied", "invalid_request")
                }
                crate::daemon::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                    ("denied", "not_found_or_not_authorized")
                }
                crate::daemon::DaemonInvocationProblem::Unavailable => {
                    ("unavailable", "authority_unavailable")
                }
            };
            (
                serde_json::json!({
                    "request_id": request_id,
                    "status": status,
                    "reason": reason,
                }),
                Some(format!("multi-root invocation {status}: {reason}")),
            )
        }
        _ => {
            return Err(TraceDecayError::Config {
                message: "multi-root daemon returned a mismatched outcome".to_owned(),
            });
        }
    };
    let text = serde_json::to_string_pretty(&value)?;
    let result = crate::mcp::tools::ToolResult::new(
        serde_json::json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": value,
        }),
        Vec::new(),
    );
    Ok(match failure {
        Some(message) => result
            .with_semantic_error(true)
            .with_failure_message(message),
        None => result.with_semantic_error(false),
    })
}

fn render_result(
    cg: &TraceDecay,
    result: ApplicationSurfaceInvocationResult,
) -> Result<crate::mcp::tools::ToolResult> {
    let (value, failure_message) = match &result.result {
        Ok(application) => (serde_json::to_value(application)?, None),
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
    let rendered = super::text_tool_result(&text);
    Ok(match failure_message {
        Some(failure_message) => rendered
            .with_semantic_error(true)
            .with_failure_message(failure_message),
        None => rendered,
    })
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

    use super::{complete_protocol_controls, render_canonical_markdown};

    #[test]
    fn preserves_a_supplied_deadline_when_cancellation_is_missing() {
        let request_id = RequestId::new("request.mcp.controls.deadline").unwrap();
        let deadline = Deadline::new(UtcMicros(91)).unwrap();

        let (actual_deadline, cancellation) =
            complete_protocol_controls(&request_id, Some(deadline.clone()), None)
                .unwrap()
                .unwrap();

        assert_eq!(actual_deadline, deadline);
        assert_eq!(
            cancellation.context().token_id.as_str(),
            "cancellation.request.mcp.controls.deadline"
        );
    }

    #[test]
    fn preserves_a_supplied_live_cancellation_when_deadline_is_missing() {
        let request_id = RequestId::new("request.mcp.controls.cancellation").unwrap();
        let cancellation = CancellationSignal::active("cancel.protocol.exact").unwrap();
        let observer = cancellation.clone();

        let (_deadline, actual_cancellation) =
            complete_protocol_controls(&request_id, None, Some(cancellation))
                .unwrap()
                .unwrap();
        actual_cancellation.cancel(UtcMicros(41));

        assert_eq!(
            observer.context().token_id.as_str(),
            "cancel.protocol.exact"
        );
        assert!(observer.is_cancelled());
    }

    #[test]
    fn leaves_default_controls_to_the_canonical_dispatch_when_both_are_missing() {
        let request_id = RequestId::new("request.mcp.controls.default").unwrap();
        assert!(
            complete_protocol_controls(&request_id, None, None)
                .unwrap()
                .is_none()
        );
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

        assert!(rendered.starts_with("## feedback\\_list\n"));
        assert!(rendered.contains("\n- Operation: `feedback_list`"));
        assert!(rendered.contains("\n- Binding: `binding.mcp.feedback-list.v1`"));
        assert!(rendered.contains("\n- Status: `problem`"));
        assert!(rendered.contains("\n- Problem: `daemon_unavailable`"));
        assert!(rendered.contains("\n- Retry: `after_delay`"));
        assert!(!rendered.contains("### contract"));
    }
}

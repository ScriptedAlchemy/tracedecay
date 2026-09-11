use serde::Deserialize;
use serde_json::Value;
use tracedecay_contracts::{
    ApplicationOutcome, ApplicationProblemKind, ApplicationResult, CancellationSignal, Deadline,
    InvocationTarget, RequestId, RetainedSurfaceOperation,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingId};

use crate::mcp::tools::dispatch::{
    resolve_mcp_application_surface_for_target,
    resolve_mcp_application_surface_with_controls_for_target,
};
use crate::tracedecay::TraceDecay;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceInvocationResult, ApplicationToolRequest, parse_application_surface_request,
};
use tracedecay_daemon_protocol::{DaemonInvocationExecutor, RequestedOutputFormat};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::application_output::view::CanonicalHumanView;

pub(super) fn request_id() -> Result<RequestId> {
    mint_global_request_id(GlobalRequestSurface::McpFallback).map_err(|_| TraceDecayError::Config {
        message: "could not allocate an application surface request id".to_owned(),
    })
}

pub(super) fn complete_protocol_controls(
    operation: ApplicationSurfaceOperation,
    request_id: &RequestId,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<Option<(Deadline, CancellationSignal)>> {
    complete_protocol_controls_for_tool(
        operation.mcp_tool_name(),
        request_id,
        deadline,
        cancellation,
    )
}

pub(super) fn complete_retained_protocol_controls(
    operation: RetainedSurfaceOperation,
    request_id: &RequestId,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<Option<(Deadline, CancellationSignal)>> {
    let binding = super::retained_catalog::retained_mcp_binding(operation)?;
    let ceiling = std::time::Duration::from_millis(binding.maximum_millis());
    complete_protocol_controls_with_ceiling(ceiling, request_id, deadline, cancellation)
}

fn complete_protocol_controls_for_tool(
    tool_name: &str,
    request_id: &RequestId,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<Option<(Deadline, CancellationSignal)>> {
    let ceiling = crate::mcp::tools::binding::canonical_tool_dispatch_ceiling(tool_name).map_err(
        |error| TraceDecayError::Config {
            message: format!("could not resolve application surface deadline: {error}"),
        },
    )?;
    complete_protocol_controls_with_ceiling(ceiling, request_id, deadline, cancellation)
}

fn complete_protocol_controls_with_ceiling(
    ceiling: std::time::Duration,
    request_id: &RequestId,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<Option<(Deadline, CancellationSignal)>> {
    let ceiling_micros =
        i64::try_from(ceiling.as_micros()).map_err(|_| TraceDecayError::Config {
            message: "application surface deadline exceeds the domain clock".to_owned(),
        })?;
    let maximum_deadline_at = UtcMicros(
        tracedecay_contracts::clock::now_micros()
            .0
            .saturating_add(ceiling_micros),
    );
    let effective_deadline_at = deadline
        .as_ref()
        .map(|deadline| deadline.expires_at)
        .filter(|expires_at| *expires_at <= maximum_deadline_at)
        .unwrap_or(maximum_deadline_at);
    let deadline =
        Deadline::new(effective_deadline_at).map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    let cancellation = match cancellation {
        Some(cancellation) => cancellation,
        None => CancellationSignal::active(format!("cancellation.{}", request_id.as_str()))
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?,
    };
    Ok(Some((deadline, cancellation)))
}

#[hotpath::measure(future = true, label = "mcp.application.surface.total")]
pub(super) async fn handle_application_surface(
    cg: &TraceDecay,
    operation: ApplicationSurfaceOperation,
    normalized: ApplicationToolRequest,
    executor: Option<&dyn DaemonInvocationExecutor>,
    target: InvocationTarget,
    protocol_request_id: Option<RequestId>,
    request_controls: tracedecay_mcp::RequestControls<'_>,
) -> Result<tracedecay_mcp::ToolResult> {
    let ApplicationToolRequest {
        request: request_args,
        requested_format,
    } = normalized;
    let request_id = protocol_request_id.unwrap_or(request_id()?);
    let request = match parse_application_surface_request(operation, request_args) {
        Ok(request) => request,
        Err(error) => {
            tracedecay_daemon_service::application_surface::observe_surface_argument_rejection(
                executor,
                tracedecay_tool_catalog::BindingSurface::Mcp,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Err(TraceDecayError::project_route(
                "application_surface_invalid_request",
                false,
                error.to_string(),
            ));
        }
    };
    let controls = complete_protocol_controls(
        operation,
        &request_id,
        request_controls.deadline.cloned(),
        request_controls.cancellation.cloned(),
    )?;
    let result = match controls {
        Some((deadline, cancellation)) => {
            hotpath::future!(
                resolve_mcp_application_surface_with_controls_for_target(
                    operation,
                    request_id,
                    request,
                    requested_format,
                    deadline,
                    cancellation,
                    target,
                    executor,
                ),
                label = "mcp.application.surface.resolve"
            )
            .await
        }
        None => {
            hotpath::future!(
                resolve_mcp_application_surface_for_target(
                    operation,
                    request_id,
                    request,
                    requested_format,
                    target,
                    executor,
                ),
                label = "mcp.application.surface.resolve"
            )
            .await
        }
    }
    .map_err(application_surface_dispatch_error)?;

    let served_stale = served_stale_code_graph_read(&result)?;
    let mut rendered = render_result(cg, result)?;
    if let Some(served) = served_stale.as_ref() {
        super::append_code_graph_freshness(&mut rendered, served);
    }
    Ok(rendered)
}

#[derive(Deserialize)]
struct CodeGraphFreshnessMarker {
    generation: tracedecay_domain::CodeGenerationId,
    freshness: tracedecay_graph_query::CodeGraphReadFreshnessV1,
}

fn served_stale_code_graph_read(
    result: &ApplicationSurfaceInvocationResult,
) -> Result<Option<super::ServedStaleCodeGraphReadV1>> {
    if !matches!(
        result.operation,
        ApplicationSurfaceOperation::CodeSymbolSearch
            | ApplicationSurfaceOperation::CodeSignatureSearch
            | ApplicationSurfaceOperation::CodeImplementations
            | ApplicationSurfaceOperation::CodeTypeHierarchy
            | ApplicationSurfaceOperation::CodeCallers
            | ApplicationSurfaceOperation::CodeCallees
    ) {
        return Ok(None);
    }
    let Ok(envelope) = &result.result else {
        return Ok(None);
    };
    let ApplicationOutcome::Evidence(evidence) = &envelope.outcome else {
        return Ok(None);
    };
    let Some(payload) = evidence.payload.as_ref() else {
        return Ok(None);
    };
    let marker: CodeGraphFreshnessMarker = serde_json::from_value(payload.clone())?;
    let tracedecay_graph_query::CodeGraphReadFreshnessV1::LastCompleteStale {
        sealed_at,
        rebuild_in_flight,
    } = marker.freshness
    else {
        return Ok(None);
    };
    Ok(Some(super::ServedStaleCodeGraphReadV1 {
        generation: marker.generation.as_str().to_owned(),
        sealed_at,
        rebuild_in_flight,
    }))
}

/// Map surface-resolution failures to typed reason codes so MCP clients see
/// truthful unavailable/denied states instead of an untyped internal error.
fn application_surface_dispatch_error(
    error: tracedecay_daemon_protocol::ApplicationSurfaceAdapterError,
) -> TraceDecayError {
    use tracedecay_daemon_protocol::ApplicationSurfaceAdapterError as AdapterError;
    let (reason_code, retryable) = match &error {
        AdapterError::DaemonUnavailable => ("application_surface_unavailable", true),
        // Keep the transport's own reason code (`daemon_connect_down` /
        // `daemon_connect_saturated`) so every dispatch surface names the
        // dead-daemon state identically.
        AdapterError::DaemonUnreachable { reason_code, .. } => {
            return TraceDecayError::project_route(reason_code.clone(), true, error.to_string());
        }
        AdapterError::UnknownOrNotAuthorized => {
            ("application_surface_not_found_or_not_authorized", false)
        }
        AdapterError::InvalidRequestHandle | AdapterError::InvalidSurfaceRequest => {
            ("application_surface_invalid_request", false)
        }
        AdapterError::Catalog(_)
        | AdapterError::Contract(_)
        | AdapterError::Identifier(_)
        | AdapterError::CatalogValidation(_) => ("application_surface_catalog_invalid", false),
    };
    TraceDecayError::project_route(reason_code, retryable, error.to_string())
}

fn render_result(
    cg: &TraceDecay,
    result: ApplicationSurfaceInvocationResult,
) -> Result<tracedecay_mcp::ToolResult> {
    render_result_for_root(Some(cg.project_root()), result)
}

fn render_result_for_root(
    project_root: Option<&std::path::Path>,
    result: ApplicationSurfaceInvocationResult,
) -> Result<tracedecay_mcp::ToolResult> {
    render_result_parts(
        project_root,
        result.operation.as_str(),
        &result.binding_id,
        &result.result,
        result.requested_format,
    )
}

fn render_result_parts(
    project_root: Option<&std::path::Path>,
    operation: &str,
    binding_id: &BindingId,
    result: &ApplicationResult<Value>,
    requested_format: RequestedOutputFormat,
) -> Result<tracedecay_mcp::ToolResult> {
    let (value, failure_message) = match result {
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
    let markdown = match requested_format {
        RequestedOutputFormat::Json => None,
        RequestedOutputFormat::Markdown => {
            Some(render_canonical_markdown(operation, binding_id, result)?)
        }
    };
    let text = tracedecay_mcp::tools::render::finalize_with_format(
        project_root,
        requested_format,
        &value,
        || markdown.unwrap_or_default(),
    );
    let mut rendered = super::text_tool_result(&text);
    if let Err(problem) = result {
        // Keep the typed problem machine-readable in every presentation
        // format: markdown rendering alone would strand it in prose that
        // clients cannot classify. The whole record travels, not a
        // kind/code summary — the parts a caller must *act* on are the
        // legal actions, the retry directive, and, for an admitted partial
        // effect, the committed receipt. Publishing only kind/code left the
        // one instruction that matters ("reconcile this committed effect")
        // readable by humans and invisible to every client.
        if let Some(object) = rendered.value.as_object_mut() {
            object.insert(
                "problem".to_string(),
                serde_json::to_value(problem.problem.as_ref())?,
            );
        }
    }
    Ok(match failure_message {
        Some(failure_message) => rendered
            .with_semantic_error(true)
            .with_failure_message(failure_message),
        None => rendered,
    })
}

pub(super) fn render_retained_result(
    project_root: Option<&std::path::Path>,
    operation: RetainedSurfaceOperation,
    binding_id: &BindingId,
    result: ApplicationResult<tracedecay_contracts::retained_surfaces::RetainedSurfaceResultV1>,
    requested_format: RequestedOutputFormat,
) -> Result<tracedecay_mcp::ToolResult> {
    let result = tracedecay_daemon_service::application_surface::retained::result_value(result)
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid retained application result: {error}"),
        })?;
    render_result_parts(
        project_root,
        operation.as_str(),
        binding_id,
        &result,
        requested_format,
    )
}

fn render_canonical_markdown(
    operation: &str,
    binding_id: &BindingId,
    result: &ApplicationResult<Value>,
) -> serde_json::Result<String> {
    let view = CanonicalHumanView::from_application_result(operation, binding_id, result)?;
    Ok(tracedecay_mcp::application_output::markdown::render(view)
        .as_str()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use tracedecay_contracts::{CancellationSignal, Deadline, RequestId};
    use tracedecay_domain::UtcMicros;

    use super::complete_protocol_controls;
    use tracedecay_tool_catalog::ApplicationSurfaceOperation;

    #[test]
    fn preserves_a_supplied_deadline_when_cancellation_is_missing() {
        let request_id = RequestId::new("request.mcp.controls.deadline").unwrap();
        let deadline = Deadline::new(UtcMicros(91)).unwrap();

        let (actual_deadline, cancellation) = complete_protocol_controls(
            ApplicationSurfaceOperation::ConfigurationSet,
            &request_id,
            Some(deadline.clone()),
            None,
        )
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

        let (_deadline, actual_cancellation) = complete_protocol_controls(
            ApplicationSurfaceOperation::ConfigurationSet,
            &request_id,
            None,
            Some(cancellation),
        )
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
    fn derives_default_deadline_from_the_exact_catalog_capability() {
        let request_id = RequestId::new("request.mcp.controls.default").unwrap();
        let before = tracedecay_contracts::clock::now_micros();
        let (deadline, _) = complete_protocol_controls(
            ApplicationSurfaceOperation::ConfigurationSet,
            &request_id,
            None,
            None,
        )
        .unwrap()
        .unwrap();
        let after = tracedecay_contracts::clock::now_micros();
        assert!(
            deadline.expires_at.0 >= before.0.saturating_add(15_000_000)
                && deadline.expires_at.0 <= after.0.saturating_add(15_000_000),
            "configuration_set must inherit its exact 15-second catalog ceiling"
        );
    }
}

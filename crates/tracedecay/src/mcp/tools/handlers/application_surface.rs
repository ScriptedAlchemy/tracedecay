use serde_json::Value;
use tracedecay_contracts::{
    ApplicationOutcome, ApplicationProblemKind, ApplicationResult, CancellationSignal, Deadline,
    InvocationTarget, RequestId, RetainedSurfaceOperation,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingId};

use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceInvocationResult, ApplicationToolRequest, parse_application_surface_request,
};
use tracedecay_daemon_protocol::{DaemonInvocationExecutor, RequestedOutputFormat};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::application_output::view::CanonicalHumanView;
use tracedecay_mcp::tools::dispatch::{
    resolve_mcp_application_surface_for_target,
    resolve_mcp_application_surface_with_controls_for_target,
};
use tracedecay_mcp::tools::response_trailers::ResponseTrailer;
use tracedecay_project::project::TraceDecay;

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
    let ceiling = tracedecay_mcp::tools::binding::canonical_tool_dispatch_ceiling(tool_name)
        .map_err(|error| TraceDecayError::Config {
            message: format!("could not resolve application surface deadline: {error}"),
        })?;
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
    render_application_surface_result(Some(&cg.store_layout().response_handle_root), &result)
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
        AdapterError::InvalidRequestHandle | AdapterError::InvalidSurfaceRequest { .. } => {
            ("application_surface_invalid_request", false)
        }
        AdapterError::Catalog(_)
        | AdapterError::Contract(_)
        | AdapterError::Identifier(_)
        | AdapterError::CatalogValidation(_) => ("application_surface_catalog_invalid", false),
    };
    TraceDecayError::project_route(reason_code, retryable, error.to_string())
}

/// Render one settled application-surface call as its tool result. MCP and
/// the `tracedecay tool` CLI both print this.
pub fn render_application_surface_result(
    response_handle_root: Option<&std::path::Path>,
    result: &ApplicationSurfaceInvocationResult,
) -> Result<tracedecay_mcp::ToolResult> {
    render_result_parts(
        response_handle_root,
        result.operation.as_str(),
        &result.binding_id,
        &result.result,
        result.requested_format,
    )
}

fn render_result_parts(
    response_handle_root: Option<&std::path::Path>,
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
        response_handle_root,
        requested_format,
        &value,
        || markdown.unwrap_or_default(),
    );
    let mut rendered = super::text_tool_result(&text);
    match result {
        Ok(envelope) => ResponseTrailer {
            touched_files: &envelope.touched_files,
            code_graph: envelope.code_graph.as_ref(),
            cost: envelope.cost.as_ref(),
        }
        .attach(&mut rendered),
        // Keep the typed problem machine-readable in every presentation
        // format: markdown rendering alone would strand it in prose that
        // clients cannot classify. The whole record travels, not a
        // kind/code summary, the parts a caller must *act* on are the
        // legal actions, the retry directive, and, for an admitted partial
        // effect, the committed receipt. Publishing only kind/code left the
        // one instruction that matters ("reconcile this committed effect")
        // readable by humans and invisible to every client.
        Err(problem) => {
            if let Some(object) = rendered.value.as_object_mut() {
                object.insert(
                    "problem".to_string(),
                    serde_json::to_value(problem.problem.as_ref())?,
                );
            }
        }
    }
    Ok(match failure_message {
        Some(failure_message) => rendered
            .with_semantic_error(true)
            .with_failure_message(failure_message),
        None => rendered,
    })
}

/// A settled retained tool call, before rendering.
pub struct RetainedSurfaceExecution {
    pub operation: ApplicationSurfaceOperation,
    pub binding_id: BindingId,
    pub requested_format: RequestedOutputFormat,
    pub result: ApplicationResult<Value>,
}

/// Run one retained memory, session, or workflow tool on `surface` and render
/// its tool result exactly as the retained tools always have.
#[allow(clippy::too_many_arguments)]
#[hotpath::measure(future = true, label = "mcp.retained.total")]
pub async fn run_retained_surface_tool(
    response_handle_root: Option<&std::path::Path>,
    surface: tracedecay_tool_catalog::BindingSurface,
    operation: ApplicationSurfaceOperation,
    args: Value,
    executor: Option<&dyn DaemonInvocationExecutor>,
    protocol_request_id: Option<RequestId>,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<tracedecay_mcp::ToolResult> {
    let execution = execute_retained_surface_tool(
        surface,
        operation,
        args,
        executor,
        protocol_request_id,
        deadline,
        cancellation,
    )
    .await?;
    render_retained_execution(response_handle_root, &execution)
}

/// Render a settled retained tool call.
pub fn render_retained_execution(
    response_handle_root: Option<&std::path::Path>,
    execution: &RetainedSurfaceExecution,
) -> Result<tracedecay_mcp::ToolResult> {
    hotpath::measure_block!(
        "mcp.retained.render",
        render_result_parts(
            response_handle_root,
            execution.operation.as_str(),
            &execution.binding_id,
            &execution.result,
            execution.requested_format,
        )
    )
}

/// Which store a retained tool call addresses.
///
/// `memory_scope: "user"` and a session refresh's `scope.kind: "profile"` are
/// part of the canonical request. `storage_scope` is the LCM and
/// message-search transport selector; it never reaches the request body.
pub fn retained_tool_target(
    operation: RetainedSurfaceOperation,
    args: &Value,
) -> Result<InvocationTarget> {
    use RetainedSurfaceOperation as Op;
    if let Some(storage_scope) = args.get("storage_scope") {
        if !matches!(
            operation,
            Op::LcmStatus
                | Op::LcmDoctor
                | Op::LcmLoadSession
                | Op::LcmGrep
                | Op::LcmDescribe
                | Op::LcmExpand
                | Op::LcmExpandQuery
                | Op::MessageSearch
        ) {
            return Err(TraceDecayError::Config {
                message: format!(
                    "unknown parameter `storage_scope` for `tracedecay_{}`",
                    operation.as_str()
                ),
            });
        }
        return match storage_scope.as_str() {
            Some("user") => Ok(InvocationTarget::Profile),
            Some("project") => Ok(InvocationTarget::CurrentProject),
            _ => Err(TraceDecayError::Config {
                message: "storage_scope must be one of project, user".to_owned(),
            }),
        };
    }
    let profile = match operation {
        Op::FactStoreAdd
        | Op::FactStoreSearch
        | Op::FactStoreProbe
        | Op::FactStoreRelated
        | Op::FactStoreReason
        | Op::FactStoreContradict
        | Op::FactStoreGet
        | Op::FactStoreUpdate
        | Op::FactStoreRemove
        | Op::FactStoreSupersede
        | Op::FactStoreList
        | Op::FactFeedback
        | Op::MemoryStatus => args.get("memory_scope").and_then(Value::as_str) == Some("user"),
        Op::SessionRefreshBegin | Op::SessionRefreshStatus | Op::SessionRefreshCancel => {
            args.pointer("/scope/kind").and_then(Value::as_str) == Some("profile")
        }
        Op::FactStoreCurate
        | Op::MessageSearch
        | Op::SessionsFor
        | Op::Workflows
        | Op::LcmStatus
        | Op::LcmDoctor
        | Op::LcmLoadSession
        | Op::LcmGrep
        | Op::LcmDescribe
        | Op::LcmExpand
        | Op::LcmExpandQuery => false,
    };
    Ok(if profile {
        InvocationTarget::Profile
    } else {
        InvocationTarget::CurrentProject
    })
}

/// Decode, dispatch, and settle one retained tool call. `Err` is an argument
/// or transport failure the caller reports as-is.
#[allow(clippy::too_many_arguments)]
pub async fn execute_retained_surface_tool(
    surface: tracedecay_tool_catalog::BindingSurface,
    operation: ApplicationSurfaceOperation,
    mut args: Value,
    executor: Option<&dyn DaemonInvocationExecutor>,
    protocol_request_id: Option<RequestId>,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<RetainedSurfaceExecution> {
    let tool_name = operation.mcp_tool_name();
    let retained = RetainedSurfaceOperation::from_application(operation)
        .ok_or_else(|| super::unknown_tool_error(tool_name))?;
    let target = retained_tool_target(retained, &args)?;
    if let Some(arguments) = args.as_object_mut() {
        arguments.remove("storage_scope");
    }
    let normalized =
        tracedecay_daemon_protocol::separate_application_tool_request(args).map_err(|error| {
            TraceDecayError::Config {
                message: error.to_string(),
            }
        })?;
    let requested_format = normalized.requested_format;
    let request = hotpath::measure_block!(
        "mcp.retained.decode",
        tracedecay_daemon_protocol::decode_retained_request(retained, normalized.request)
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("invalid retained application request for {tool_name}: {error}"),
    })?;
    let request_id = match protocol_request_id {
        Some(request_id) => request_id,
        None => self::request_id()?,
    };
    let (deadline, cancellation) =
        complete_retained_protocol_controls(retained, &request_id, deadline, cancellation)?
            .ok_or_else(|| {
                TraceDecayError::project_route(
                    "retained_application_controls_unavailable",
                    true,
                    "retained application protocol controls are unavailable",
                )
            })?;
    // The executor belongs to the selected project's server, and the retained
    // daemon payload carries no resolved scope: the target is that project or
    // the authenticated profile.
    let mut dispatched = tracedecay_daemon_service::application_surface::resolve_application_surface_dispatch_with_controls(
        surface,
        operation,
        request_id.clone(),
        tracedecay_daemon_protocol::ApplicationSurfaceRequest::Retained(request),
        tracedecay_contracts::PageRequest::first(10).map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?,
        Some(deadline),
        cancellation,
        requested_format,
    )
    .map_err(application_surface_dispatch_error)?;
    dispatched.invocation.invocation.scope = target;
    let binding_id = dispatched.invocation.binding_id.clone();
    let result_contract =
        tracedecay_contracts::ResultContractRef::from_schema(&dispatched.invocation.result_schema);
    let unavailable = |code: String, message: String| {
        tracedecay_contracts::ApplicationProblemEnvelope::new(
            result_contract.clone(),
            request_id.clone(),
            tracedecay_contracts::ApplicationProblem::unavailable(
                tracedecay_contracts::SafeDiagnostic { code, message },
            ),
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid retained application problem envelope: {error}"),
        })
    };
    let result = match executor {
        None => Err(unavailable(
            "application.transport.unavailable".to_owned(),
            "The daemon retained application transport is unavailable".to_owned(),
        )?),
        Some(executor) => match hotpath::future!(
            tracedecay_daemon_service::application_surface::execute_application_surface(
                operation,
                dispatched,
                Some(executor),
            ),
            label = "mcp.retained.invoke"
        )
        .await
        {
            Ok(result) => result.result,
            Err(
                tracedecay_daemon_protocol::ApplicationSurfaceAdapterError::DaemonUnreachable {
                    reason_code,
                    detail,
                },
            ) => Err(unavailable(reason_code, detail)?),
            Err(error) => return Err(application_surface_dispatch_error(error)),
        },
    };
    Ok(RetainedSurfaceExecution {
        operation,
        binding_id,
        requested_format,
        result,
    })
}

/// Invoke one graph-tool operation through the project's graph-tool owner and
/// return its typed result. A refusal comes back as the handler's own error
/// kind, so every surface reports the failure it always reported.
#[allow(clippy::too_many_arguments)]
#[hotpath::measure(future = true, label = "mcp.graph_tool.total")]
pub async fn execute_graph_tool_surface(
    surface: tracedecay_tool_catalog::BindingSurface,
    operation: ApplicationSurfaceOperation,
    args: Value,
    executor: Option<&dyn DaemonInvocationExecutor>,
    protocol_request_id: Option<RequestId>,
    deadline: Option<Deadline>,
    cancellation: Option<CancellationSignal>,
) -> Result<tracedecay_contracts::graph_tool::GraphToolCompletionV1> {
    let request = parse_application_surface_request(operation, args).map_err(|error| {
        TraceDecayError::Config {
            message: match error {
                tracedecay_daemon_protocol::ApplicationSurfaceAdapterError::InvalidSurfaceRequest {
                    detail,
                } => detail,
                error => error.to_string(),
            },
        }
    })?;
    let request_id = match protocol_request_id {
        Some(request_id) => request_id,
        None => self::request_id()?,
    };
    let (deadline, cancellation) =
        complete_protocol_controls(operation, &request_id, deadline, cancellation)?.ok_or_else(
            || {
                TraceDecayError::project_route(
                    "application_surface_controls_unavailable",
                    true,
                    "graph-tool protocol controls are unavailable",
                )
            },
        )?;
    let dispatched = tracedecay_daemon_service::application_surface::resolve_application_surface_dispatch_with_controls(
        surface,
        operation,
        request_id,
        request,
        tracedecay_contracts::PageRequest::first(10).map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?,
        Some(deadline),
        cancellation,
        RequestedOutputFormat::Json,
    )
    .map_err(application_surface_dispatch_error)?;
    let result = tracedecay_daemon_service::application_surface::execute_application_surface(
        operation, dispatched, executor,
    )
    .await
    .map_err(application_surface_dispatch_error)?
    .result;
    let envelope = result.map_err(|problem| graph_tool_problem_error(&problem.problem))?;
    let ApplicationOutcome::Result(value) = envelope.outcome else {
        return Err(TraceDecayError::project_route(
            "application_surface_invalid_response",
            false,
            format!(
                "{} returned a non-result outcome",
                operation.mcp_tool_name()
            ),
        ));
    };
    let result =
        tracedecay_contracts::graph_tool::GraphToolResultV1::from_result_value(operation, value)
            .map_err(|error| {
                TraceDecayError::project_route(
                    "application_surface_invalid_response",
                    false,
                    format!(
                        "{} returned an invalid result: {error}",
                        operation.mcp_tool_name()
                    ),
                )
            })?;
    Ok(tracedecay_contracts::graph_tool::GraphToolCompletionV1 {
        result,
        touched_files: envelope.touched_files,
        code_graph: envelope.code_graph,
        analytics: envelope.analytics,
        cost: envelope.cost,
    })
}

/// The graph-tool owner reports handler argument errors as invalid requests
/// and every other refusal under its own reason code.
fn graph_tool_problem_error(
    problem: &tracedecay_contracts::ApplicationProblemRecord,
) -> TraceDecayError {
    let message = problem.diagnostic.as_ref().map_or_else(
        || problem.message.clone(),
        |diagnostic| diagnostic.message.clone(),
    );
    match problem.kind {
        ApplicationProblemKind::InvalidRequest => TraceDecayError::Config { message },
        _ => TraceDecayError::project_route(problem.code.clone(), problem.retryable, message),
    }
}

/// The owner-side counterpart of [`graph_tool_problem_error`].
pub(crate) fn graph_tool_error_problem(
    error: &TraceDecayError,
) -> tracedecay_contracts::ApplicationProblem {
    match error {
        TraceDecayError::Config { message } => {
            tracedecay_contracts::ApplicationProblem::invalid_request_without_action(
                "application.surface.invalid_request",
                message.clone(),
            )
        }
        TraceDecayError::ProjectRoute {
            reason_code,
            retryable,
            detail,
        } => graph_tool_unavailable(reason_code, *retryable, detail),
        error => graph_tool_unavailable("graph_tool.failed", false, &error.to_string()),
    }
}

fn graph_tool_unavailable(
    code: &str,
    retryable: bool,
    message: &str,
) -> tracedecay_contracts::ApplicationProblem {
    let diagnostic = tracedecay_contracts::SafeDiagnostic {
        code: code.to_owned(),
        message: message.to_owned(),
    };
    if retryable {
        return tracedecay_contracts::ApplicationProblem::unavailable(diagnostic);
    }
    tracedecay_contracts::ApplicationProblem::Unavailable {
        classification: tracedecay_contracts::ApplicationUnavailableClassV1::Authority,
        diagnostic,
        retry: tracedecay_contracts::RetryDirective::Never,
        legal_actions: Vec::new(),
    }
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
    use serde_json::json;
    use tracedecay_contracts::retrieval::{CodeGraphReadFreshnessV1, ServedCodeGraphGenerationV1};
    use tracedecay_contracts::{
        ApplicationEnvelope, ApplicationOutcome, CancellationSignal, Deadline, RequestId,
        ResolvedScope, ResultContractRef,
    };
    use tracedecay_daemon_protocol::RequestedOutputFormat;
    use tracedecay_domain::{ProjectId, RepositoryId, UtcMicros, WorktreeId};
    use tracedecay_mcp::tools::response_trailers::account_tool_result;
    use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingId, SchemaId};

    use super::{complete_protocol_controls, render_result_parts};

    #[test]
    fn retained_calls_target_the_profile_only_through_their_canonical_selector() {
        use super::retained_tool_target;
        use serde_json::json;
        use tracedecay_contracts::{InvocationTarget, RetainedSurfaceOperation as Op};

        for (operation, arguments, expected) in [
            (
                Op::LcmGrep,
                json!({"storage_scope": "user"}),
                InvocationTarget::Profile,
            ),
            (
                Op::MessageSearch,
                json!({"storage_scope": "project"}),
                InvocationTarget::CurrentProject,
            ),
            (
                Op::FactStoreList,
                json!({"memory_scope": "user"}),
                InvocationTarget::Profile,
            ),
            (
                Op::FactStoreList,
                json!({"memory_scope": "project"}),
                InvocationTarget::CurrentProject,
            ),
            (
                Op::LcmGrep,
                json!({"memory_scope": "user"}),
                InvocationTarget::CurrentProject,
            ),
            (
                Op::SessionRefreshBegin,
                json!({"scope": {"kind": "profile"}}),
                InvocationTarget::Profile,
            ),
            (
                Op::SessionRefreshBegin,
                json!({"scope": {"kind": "project"}}),
                InvocationTarget::CurrentProject,
            ),
            (Op::Workflows, json!({}), InvocationTarget::CurrentProject),
        ] {
            assert_eq!(
                retained_tool_target(operation, &arguments).unwrap(),
                expected,
                "{operation:?} {arguments}"
            );
        }
        for (operation, arguments, message) in [
            (
                Op::MemoryStatus,
                json!({"memory_scope": "user", "storage_scope": "user"}),
                "unknown parameter `storage_scope` for `tracedecay_memory_status`",
            ),
            (
                Op::LcmDoctor,
                json!({"storage_scope": "hermes_profile"}),
                "storage_scope must be one of project, user",
            ),
        ] {
            let error = retained_tool_target(operation, &arguments).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }
    }

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

    fn texts(result: &tracedecay_mcp::ToolResult) -> Vec<String> {
        result.value["content"]
            .as_array()
            .expect("content blocks")
            .iter()
            .map(|block| block["text"].as_str().expect("text block").to_owned())
            .collect()
    }

    /// The shared renderer takes the stale trailer and the touched files from
    /// the envelope alone, so any typed tool gains them in either format.
    #[test]
    fn envelope_freshness_and_files_render_the_trailer_and_footer_in_both_formats() {
        let root = tempfile::tempdir().expect("project");
        std::fs::create_dir_all(root.path().join("src")).expect("src");
        std::fs::write(root.path().join("src/lib.rs"), "a".repeat(800)).expect("source");
        let envelope = ApplicationEnvelope {
            contract: ResultContractRef::new(SchemaId::new("schema.test.result").unwrap(), 1)
                .unwrap(),
            request_id: RequestId::new("request.mcp.render.trailers").unwrap(),
            scope: ResolvedScope::new(
                ProjectId::new("project.mcp.render").unwrap(),
                RepositoryId::new("repository.mcp.render").unwrap(),
                WorktreeId::new("worktree.mcp.render").unwrap(),
                None,
            )
            .unwrap(),
            outcome: ApplicationOutcome::Result(json!({"items": ["src/lib.rs::answer"]})),
            touched_files: vec!["src/lib.rs".to_owned()],
            code_graph: Some(ServedCodeGraphGenerationV1 {
                generation: "generation.render.stale.1".to_owned(),
                freshness: CodeGraphReadFreshnessV1::LastCompleteStale {
                    sealed_at: UtcMicros(10),
                    rebuild_in_flight: true,
                },
            }),
            analytics: None,
            cost: None,
        };
        let binding = BindingId::new("binding.mcp.code-callers.v1").unwrap();
        for format in [RequestedOutputFormat::Markdown, RequestedOutputFormat::Json] {
            let mut rendered = render_result_parts(
                Some(root.path()),
                "code_callers",
                &binding,
                &Ok(envelope.clone()),
                format,
            )
            .expect("rendered");
            assert_eq!(rendered.touched_files, vec!["src/lib.rs".to_owned()]);
            account_tool_result(Some(root.path()), &mut rendered);
            let blocks = texts(&rendered);
            assert_eq!(blocks.len(), 3, "{format:?}: {blocks:?}");
            assert!(
                blocks[1].starts_with(
                    "\ncode_graph_freshness: stale, serving the last complete generation \
                     generation.render.stale.1 (sealed "
                ),
                "{format:?}: {blocks:?}"
            );
            let after = (blocks[0].len() + blocks[1].len()) / 4;
            assert_eq!(
                blocks[2],
                format!("\ntracedecay_metrics: before=200 after={after}"),
                "{format:?}"
            );
        }
    }
}

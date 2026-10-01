use serde_json::Value;
use tracedecay_contracts::{
    ApplicationOutcome, ApplicationResult, CancellationSignal, Deadline, InvocationTarget,
    RequestId, RetainedSurfaceOperation,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingId};

use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceInvocationResult, ApplicationToolRequest,
    parse_application_surface_request,
};
use tracedecay_daemon_protocol::{DaemonInvocationExecutor, RequestedOutputFormat};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::application_output::tool_result::{
    ApplicationRefusal, render_application_result,
};
use tracedecay_mcp::tools::dispatch::{
    resolve_mcp_application_surface_for_target,
    resolve_mcp_application_surface_with_controls_for_target,
};

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
    let ceiling =
        tracedecay_daemon_service::application_surface::application_operation_deadline_ceiling(
            operation,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("could not resolve application surface deadline: {error}"),
        })?;
    complete_protocol_controls_with_ceiling(ceiling, request_id, deadline, cancellation)
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

/// Run one application-surface tool through `executor` and render its result,
/// spilling oversized payloads under `response_handle_root` when one is given.
#[hotpath::measure(future = true, label = "mcp.application.surface.total")]
pub async fn handle_application_surface(
    response_handle_root: Option<&std::path::Path>,
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
            return Err(error.into_trace_decay_error());
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
    .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?;
    render_application_surface_result(response_handle_root, &result)
}

/// Render one settled application-surface call as its tool result. MCP and
/// the `tracedecay tool` CLI both print this.
pub fn render_application_surface_result(
    response_handle_root: Option<&std::path::Path>,
    result: &ApplicationSurfaceInvocationResult,
) -> Result<tracedecay_mcp::ToolResult> {
    render_application_result(
        response_handle_root,
        result.operation.as_str(),
        &result.binding_id,
        &result.result,
        result.requested_format,
    )
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
        render_application_result(
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
            return Err(ApplicationSurfaceAdapterError::invalid_request(format!(
                "unknown parameter `storage_scope` for `tracedecay_{}`",
                operation.as_str()
            ))
            .into_trace_decay_error());
        }
        return match storage_scope.as_str() {
            Some("user") => Ok(InvocationTarget::Profile),
            Some("project") => Ok(InvocationTarget::CurrentProject),
            _ => Err(ApplicationSurfaceAdapterError::invalid_request(
                "storage_scope must be one of project, user",
            )
            .into_trace_decay_error()),
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
    let normalized = tracedecay_daemon_protocol::separate_application_tool_request(args)
        .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?;
    let requested_format = normalized.requested_format;
    let request = hotpath::measure_block!(
        "mcp.retained.decode",
        tracedecay_daemon_protocol::decode_retained_request(retained, normalized.request)
    )
    .map_err(|error| {
        ApplicationSurfaceAdapterError::invalid_request(error).into_trace_decay_error()
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
    .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?;
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
            Err(ApplicationSurfaceAdapterError::DaemonUnreachable {
                reason_code,
                detail,
            }) => Err(unavailable(reason_code, detail)?),
            Err(error) => return Err(error.into_trace_decay_error()),
        },
    };
    Ok(RetainedSurfaceExecution {
        operation,
        binding_id,
        requested_format,
        result,
    })
}

/// A settled graph-tool call: the typed completion, or the owner's refusal.
pub type GraphToolOutcome = std::result::Result<
    tracedecay_contracts::graph_tool::GraphToolCompletionV1,
    ApplicationRefusal,
>;

/// Invoke one graph-tool operation through the project's graph-tool owner, or
/// one profile-owner request through the daemon's profile owner, and return
/// its typed result. Every refusal keeps the owner's whole problem record for
/// the surface to render.
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
) -> Result<GraphToolOutcome> {
    let profile_owner_request = args
        .as_object()
        .is_some_and(|arguments| operation.is_profile_owner_request(arguments));
    let request = parse_application_surface_request(operation, args)
        .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?;
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
    let mut dispatched = tracedecay_daemon_service::application_surface::resolve_application_surface_dispatch_with_controls(
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
    .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?;
    // A request that names no project, only the profile, is the daemon's
    // profile owner's whatever project this caller's executor serves.
    if profile_owner_request {
        dispatched.invocation.invocation.scope = InvocationTarget::Profile;
    }
    let binding_id = dispatched.invocation.binding_id.clone();
    let result = tracedecay_daemon_service::application_surface::execute_application_surface(
        operation, dispatched, executor,
    )
    .await
    .map_err(ApplicationSurfaceAdapterError::into_trace_decay_error)?
    .result;
    settle_graph_tool_result(operation, binding_id, result)
}

fn settle_graph_tool_result(
    operation: ApplicationSurfaceOperation,
    binding_id: BindingId,
    result: ApplicationResult<Value>,
) -> Result<GraphToolOutcome> {
    let envelope = match result {
        Ok(envelope) => envelope,
        Err(problem) => {
            return Ok(Err(ApplicationRefusal {
                operation,
                binding_id,
                problem,
            }));
        }
    };
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
    Ok(Ok(
        tracedecay_contracts::graph_tool::GraphToolCompletionV1 {
            result,
            touched_files: envelope.touched_files,
            code_graph: envelope.code_graph,
            analytics: envelope.analytics,
            cost: envelope.cost,
        },
    ))
}

/// A route refusal that settled before any owner answered `tool_name` (an
/// unreachable daemon, a refused argument, an unknown tool), rendered as the
/// one tool-result shape every route answers: `isError` with the typed
/// record at `structuredContent.problem`. A tool with a binding on `surface`
/// refuses under its own result contract; a name no binding owns refuses
/// under the adapter's. `None` when `error` is not a route refusal.
pub fn render_settled_route_refusal(
    surface: tracedecay_tool_catalog::BindingSurface,
    tool_name: &str,
    request_id: RequestId,
    error: &TraceDecayError,
    args: &Value,
) -> Option<Result<tracedecay_mcp::ToolResult>> {
    error.project_route_context()?;
    let problem = graph_tool_error_problem(error);
    let bound = ApplicationSurfaceOperation::from_tool_name(tool_name).and_then(|operation| {
        tracedecay_daemon_service::application_surface::settled_tool_refusal(
            surface,
            tool_name,
            request_id.clone(),
            problem.clone(),
        )
        .map(|(binding_id, problem)| ApplicationRefusal {
            operation,
            binding_id,
            problem,
        })
    });
    let rendered = match bound {
        Some(refusal) => refusal.render(None, args),
        None => unbound_refusal(request_id, problem),
    };
    Some(rendered.map(|mut rendered| {
        tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut rendered);
        rendered
    }))
}

fn unbound_refusal(
    request_id: RequestId,
    problem: tracedecay_contracts::ApplicationProblem,
) -> Result<tracedecay_mcp::ToolResult> {
    let envelope = tracedecay_api::adapter_problem(request_id, problem).map_err(|error| {
        TraceDecayError::Config {
            message: format!("the adapter refusal violated its problem contract: {error}"),
        }
    })?;
    tracedecay_mcp::application_output::tool_result::problem_tool_result(
        &serde_json::to_string(&envelope)?,
        &envelope.problem,
    )
}

/// The graph-tool owner reports handler argument errors as invalid requests,
/// a typed route detail, a persisted-shape refusal, or a lock that missed its
/// deadline as that detail, a route refusal as unavailable under its own
/// reason code, and any other handler failure as an internal execution
/// failure.
pub(crate) fn graph_tool_error_problem(
    error: &TraceDecayError,
) -> tracedecay_contracts::ApplicationProblem {
    if let Some(detail) = error.project_route_typed_detail() {
        return tracedecay_contracts::ApplicationProblem::from_detail(detail.clone());
    }
    if let Some(detail) = tracedecay_mcp::reset_required_detail(error) {
        return tracedecay_contracts::ApplicationProblem::from_detail(detail);
    }
    if let Some(detail) =
        tracedecay_contracts::ApplicationProblemDetailV1::from_lock_deadline(error)
    {
        return tracedecay_contracts::ApplicationProblem::from_detail(detail);
    }
    // A hook's admission authority names its own reason and retry verdict;
    // they travel as the problem's code and retry directive.
    if let Some((reason_code, retryable, detail)) = error.hook_runtime_context() {
        return graph_tool_unavailable(reason_code, retryable, detail);
    }
    match error {
        TraceDecayError::Config { message } => {
            tracedecay_contracts::ApplicationProblem::invalid_request_without_action(
                "application.surface.invalid_request",
                safe_diagnostic_message(message),
            )
        }
        TraceDecayError::ProjectRoute {
            reason_code,
            detail,
            ..
        } if reason_code == tracedecay_domain::CURSOR_PARAMETER_CHANGED_CODE
            || reason_code == tracedecay_domain::CURSOR_INVALID_CODE =>
        {
            tracedecay_contracts::ApplicationProblem::cursor_refusal(
                tracedecay_contracts::SafeDiagnostic {
                    code: reason_code.clone(),
                    message: detail.clone(),
                },
            )
        }
        TraceDecayError::ProjectRoute {
            reason_code,
            retryable,
            detail,
            ..
        } => match (!*retryable)
            .then(|| tracedecay_mcp::tool_errors::project_route_problem_kind(reason_code))
            .flatten()
        {
            // A retryable route refusal is a transient state the caller
            // rides out, whatever reason code it carries.
            Some("invalid_request") => {
                tracedecay_contracts::ApplicationProblem::invalid_request_without_action(
                    reason_code.clone(),
                    safe_diagnostic_message(detail),
                )
            }
            Some("denied") => {
                tracedecay_contracts::ApplicationProblem::not_found_or_not_authorized(
                    tracedecay_contracts::RetryDirective::Never,
                )
            }
            _ => graph_tool_unavailable(reason_code, *retryable, detail),
        },
        error => tracedecay_contracts::ApplicationProblem::ExecutionFailed {
            classification: tracedecay_contracts::ApplicationExecutionFailureClassV1::Permanent,
            diagnostic: tracedecay_contracts::SafeDiagnostic {
                code: "graph_tool.failed".to_owned(),
                message: safe_diagnostic_message(&error.to_string()),
            },
            retry: tracedecay_contracts::RetryDirective::Never,
            legal_actions: vec![tracedecay_contracts::LegalAction::ContactAdministrator],
        },
    }
}

/// A problem's diagnostic is one bounded line, but handler errors can span
/// lines (a regex parse error draws a caret diagram), so whitespace and control
/// characters are folded to single spaces and the text is bounded before it
/// crosses the owner boundary.
fn safe_diagnostic_message(message: &str) -> String {
    const MAX_SAFE_DIAGNOSTIC_BYTES: usize = 512;
    let folded = message
        .split(|character: char| character.is_whitespace() || character.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if folded.len() <= MAX_SAFE_DIAGNOSTIC_BYTES {
        return folded;
    }
    let mut end = MAX_SAFE_DIAGNOSTIC_BYTES;
    while !folded.is_char_boundary(end) {
        end -= 1;
    }
    folded[..end].trim_end().to_owned()
}

fn graph_tool_unavailable(
    code: &str,
    retryable: bool,
    message: &str,
) -> tracedecay_contracts::ApplicationProblem {
    let diagnostic = tracedecay_contracts::SafeDiagnostic {
        code: code.to_owned(),
        message: safe_diagnostic_message(message),
    };
    if retryable {
        return tracedecay_contracts::ApplicationProblem::unavailable(diagnostic);
    }
    tracedecay_contracts::ApplicationProblem::Unavailable {
        classification: tracedecay_contracts::ApplicationUnavailableClassV1::Authority,
        diagnostic,
        retry: tracedecay_contracts::RetryDirective::Never,
        legal_actions: Vec::new(),
        detail: None,
    }
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
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_domain::{ProjectId, RepositoryId, UtcMicros, WorktreeId};
    use tracedecay_mcp::tools::response_trailers::account_tool_result;
    use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingId, SchemaId};

    use super::{complete_protocol_controls, render_application_result, settle_graph_tool_result};

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
                "application surface request does not match its reviewed schema: unknown \
                 parameter `storage_scope` for `tracedecay_memory_status`",
            ),
            (
                Op::LcmDoctor,
                json!({"storage_scope": "hermes_profile"}),
                "application surface request does not match its reviewed schema: \
                 storage_scope must be one of project, user",
            ),
        ] {
            let error = retained_tool_target(operation, &arguments).unwrap_err();
            assert_eq!(
                error.project_route_context(),
                Some(("application_surface_invalid_request", false, message)),
                "{error}"
            );
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
            let mut rendered = render_application_result(
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

    #[test]
    fn graph_tool_adapter_preserves_a_code_only_problem_record() {
        let operation = ApplicationSurfaceOperation::Signature;
        let binding = BindingId::new("binding.mcp.signature.v1").unwrap();
        let problem = tracedecay_contracts::ApplicationProblemEnvelope::new(
            ResultContractRef::new(SchemaId::new("schema.test.graph-problem.v1").unwrap(), 1)
                .unwrap(),
            RequestId::new("request.mcp.graph-problem").unwrap(),
            tracedecay_contracts::ApplicationProblem::invalid_request_without_action(
                "application.surface.invalid_request",
                "the graph request is invalid",
            ),
        )
        .unwrap();
        let expected = serde_json::to_value(problem.problem.as_ref()).unwrap();

        let refusal = settle_graph_tool_result(operation, binding.clone(), Err(problem))
            .unwrap()
            .expect_err("the typed problem must remain a refusal record");

        assert_eq!(refusal.operation, operation);
        assert_eq!(refusal.binding_id, binding);
        assert_eq!(
            serde_json::to_value(refusal.problem.problem.as_ref()).unwrap(),
            expected
        );
    }

    /// Every handler error the owner maps carries a kind of its own, so the
    /// surface renders the whole record as an `isError` result.
    #[test]
    fn every_graph_tool_handler_error_renders_as_a_kinded_problem() {
        let operation = ApplicationSurfaceOperation::Signature;
        let binding = BindingId::new("binding.mcp.signature.v1").unwrap();
        for (error, kind, code, retry, legal_actions) in [
            (
                TraceDecayError::Config {
                    message: "invalid arguments for tracedecay_signature: missing field `symbol`"
                        .to_owned(),
                },
                "invalid_request",
                "application.surface.invalid_request",
                "never",
                json!([]),
            ),
            (
                TraceDecayError::project_route(
                    "code-graph-unavailable",
                    true,
                    "the verified code graph is not ready",
                ),
                "unavailable",
                "code-graph-unavailable",
                "after_delay",
                json!(["retry"]),
            ),
            (
                TraceDecayError::reset_required(
                    "project registry",
                    "table 'code_projects' has an incompatible number of columns",
                ),
                "reset_required",
                "application.reset-required",
                "never",
                json!(["reset"]),
            ),
            (
                TraceDecayError::Io(std::io::Error::other("generation file vanished")),
                "execution_failed",
                "graph_tool.failed",
                "never",
                json!(["contact_administrator"]),
            ),
            (
                TraceDecayError::hook_runtime_with_status(
                    "observation_cursor_conflict",
                    true,
                    "Claude observation store operation failed",
                    "backpressured",
                ),
                "unavailable",
                "observation_cursor_conflict",
                "after_delay",
                json!(["retry"]),
            ),
            (
                TraceDecayError::hook_runtime_with_status(
                    "unknown_provider",
                    false,
                    "transcript provider is unsupported",
                    "unknown",
                ),
                "unavailable",
                "unknown_provider",
                "never",
                json!([]),
            ),
        ] {
            let problem = tracedecay_contracts::ApplicationProblemEnvelope::new(
                ResultContractRef::new(SchemaId::new("schema.test.graph-problem.v1").unwrap(), 1)
                    .unwrap(),
                RequestId::new("request.mcp.graph-problem").unwrap(),
                super::graph_tool_error_problem(&error),
            )
            .unwrap();
            let refusal = settle_graph_tool_result(operation, binding.clone(), Err(problem))
                .unwrap()
                .expect_err("a handler error is a refusal record");
            let mut rendered = refusal.render(None, &json!({"format": "json"})).unwrap();
            tracedecay_mcp::tool_errors::mark_semantic_tool_error(&mut rendered);

            assert_eq!(rendered.value["isError"], true, "{error}");
            let problem = &rendered.value["structuredContent"]["problem"];
            assert_eq!(problem["kind"], kind, "{error}");
            assert_eq!(problem["code"], code, "{error}");
            assert_eq!(problem["retry"], retry, "{error}");
            assert_eq!(problem["legal_actions"], legal_actions, "{error}");
        }
    }
}

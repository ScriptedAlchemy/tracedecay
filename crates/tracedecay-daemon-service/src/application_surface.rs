//! Shared transport adapter contracts for the first callable application surfaces.
//!
//! The adapters resolve catalog bindings and preserve canonical application
//! problem envelopes. They do not open stores, run queries, or bypass the
//! daemon-owned Git transaction authority.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::response::Response;
use serde_json::Value;
use tracedecay_api::{
    HttpApplicationInvocationFuture, HttpApplicationRequest, is_http_application_operation_exposed,
};
use tracedecay_application::operation_stream::OperationEventAuthority;
use tracedecay_contracts::catalog_composition::application_catalog_snapshot;
pub use tracedecay_contracts::git::{GitApplySurfaceRequest, GitPreviewSurfaceRequest};
use tracedecay_contracts::{
    ApplicationOutcome, ApplicationProblemKind, Deadline, Omission, OmissionReason,
    OperationTermination, PageRequest, RequestId, configuration_surface_catalog_contribution,
};
pub use tracedecay_contracts::{
    CallableCodeSurfaceMeta, CallableCodeSurfaceRequest, CodeCalleesSurfaceRequest,
    CodeCallersSurfaceRequest, CodeExactOccurrenceSurfaceRequest, CodeFacetSurfaceRequest,
    CodeImplementationsSurfaceRequest, CodeNavigationSurfaceRequest,
    CodePhraseSearchSurfaceRequest, CodeSignatureSearchSurfaceRequest,
    CodeSymbolSearchSurfaceRequest, CodeTimelineSurfaceRequest, CodeTypeHierarchySurfaceRequest,
    NativeIntegrationSurfaceRequest, PrimitiveCodeSurfaceRequest,
};
pub use tracedecay_daemon_protocol::GitReadSurfaceRequest;
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, CatalogBindingResolver, RequestedOutputFormat,
    parse_application_surface_request,
};
use tracedecay_domain::{ProjectId, ScopeOutcome, ScopePartialReasonV1, ScopeUnavailableReasonV1};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingSurface, ExecutableBindingRegistryV1, OperationId,
    RouteExposureV1,
};

mod catalog;
mod configuration_wire;
mod dispatch;
mod feedback_observation;
mod handoff;
mod multi_root_http;
mod operation_events;
mod problems;
mod registered_http;
mod request_control;
pub mod retained;
mod work;
mod workflow;

use catalog::resolve_application_binding;
pub use catalog::{
    application_operation_deadline_ceiling, application_surface_binding_catalog_ref,
    application_surface_catalog, application_surface_catalog_ref,
};
use configuration_wire::{
    CONFIGURATION_WIRE_OPERATIONS, configuration_binding_has_schema, is_configuration_operation,
};
use dispatch::invoke_application_adapter_request;
pub use dispatch::{
    application_surface_dispatch_input_with_controls, execute_application_surface,
    parse_http_application_surface_request, resolve_application_surface_dispatch,
    resolve_application_surface_dispatch_with_controls, resolve_dashboard_application_surface,
    resolve_http_application_surface, resolve_http_application_surface_dispatch,
};
pub use feedback_observation::observe_surface_argument_rejection;
use handoff::router_with_executor as handoff_application_router_with_executor;
use multi_root_http::router_with_executor as multi_root_application_router_with_executor;
use operation_events::http_operation_event_router;
pub use problems::{map_dispatch_error, settled_tool_refusal};
pub(crate) use registered_http::registered_executor_unavailable;
use request_control::application_http_context;
pub use workflow::invoke_workflow_operation;
use workflow::router_with_executor as workflow_application_router_with_executor;

pub(super) fn require_public_catalog_route(
    registry: &ExecutableBindingRegistryV1,
    operation_id: &OperationId,
    expected_route: &str,
) -> Result<(), ApplicationSurfaceAdapterError> {
    let Some(binding) = registry
        .get(operation_id)
        .and_then(|availability| availability.binding())
    else {
        return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
    };
    let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
        return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
    };
    if route_path != expected_route {
        return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
    }
    Ok(())
}

const DEFAULT_DEADLINE_MICROS: i64 = 30_000_000;
const APPLICATION_PROTOCOL_REVISION: u32 = 1;
const HTTP_DEADLINE_HEADER: &str = "x-tracedecay-deadline-micros";

#[tracing::instrument(
    name = "application_surface.invoker_assemble",
    level = "trace",
    skip_all
)]
fn application_invoker_for_surface(
    executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
    surface: BindingSurface,
    required_operations: &[ApplicationSurfaceOperation],
) -> Result<
    impl Fn(HttpApplicationRequest) -> HttpApplicationInvocationFuture + Clone + Send + Sync + 'static,
    ApplicationSurfaceAdapterError,
> {
    let catalog = Arc::clone(application_catalog_snapshot()?);
    let resolver = CatalogBindingResolver::new(&catalog);
    let configuration_schemas = (surface == BindingSurface::Http
        || required_operations
            .iter()
            .copied()
            .any(is_configuration_operation))
    .then(configuration_surface_catalog_contribution)
    .transpose()?;
    // The HTTP mount is the whole canonical operation family by definition, so
    // it validates the authority's own list and ignores the caller's; every
    // other surface validates exactly the operations its caller declared.
    let operations: &[ApplicationSurfaceOperation] = if surface == BindingSurface::Http {
        &ApplicationSurfaceOperation::ALL
    } else {
        required_operations
    };
    for &operation in operations {
        // Only the HTTP enumeration walks operations the mount is not meant to
        // publish; a caller-supplied list is required exactly as it was given.
        if surface == BindingSurface::Http && !is_http_application_operation_exposed(operation)? {
            continue;
        }
        let Some(binding) = resolve_application_binding(&resolver, surface, operation) else {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        };
        if is_configuration_operation(operation)
            && configuration_schemas.as_ref().is_none_or(|schemas| {
                !configuration_binding_has_schema(&catalog, schemas, &binding.binding_id)
            })
        {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        }
    }
    Ok(move |request| -> HttpApplicationInvocationFuture {
        let executor = Arc::clone(&executor);
        let catalog = Arc::clone(&catalog);
        Box::pin(async move {
            invoke_application_adapter_request(request, surface, executor.as_ref(), &catalog).await
        })
    })
}

#[tracing::instrument(
    name = "application_surface.multi_root.invoke",
    level = "trace",
    skip_all
)]
pub async fn invoke_multi_root_surface_request(
    executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    page: PageRequest,
    deadline: Deadline,
    cancellation: tracedecay_contracts::CancellationSignal,
    body: Value,
) -> Result<ScopeOutcome<Value>, ApplicationSurfaceAdapterError> {
    let request = parse_application_surface_request(operation, body)?;
    let dispatched = resolve_application_surface_dispatch_with_controls(
        BindingSurface::Http,
        operation,
        request_id,
        request,
        page,
        Some(deadline),
        cancellation,
        RequestedOutputFormat::Json,
    )?;
    let response =
        execute_application_surface(operation, dispatched, Some(executor.as_ref())).await?;
    let envelope = match response.result {
        Ok(envelope) => envelope,
        Err(problem) if problem.problem.kind == ApplicationProblemKind::NotFoundOrNotAuthorized => {
            return Ok(ScopeOutcome::Denied);
        }
        Err(_) => {
            return Ok(ScopeOutcome::Unavailable {
                reason: ScopeUnavailableReasonV1::AuthorityUnavailable,
            });
        }
    };
    let ApplicationOutcome::Evidence(packet) = envelope.outcome else {
        return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
    };
    let Some(payload) = packet.payload else {
        return Ok(ScopeOutcome::Unavailable {
            reason: ScopeUnavailableReasonV1::AuthorityUnavailable,
        });
    };
    Ok(match packet.execution.termination {
        OperationTermination::Completed => ScopeOutcome::Exact(payload),
        OperationTermination::Partial => ScopeOutcome::Partial {
            value: payload,
            reason: multi_root_partial_reason(&packet.omissions),
        },
        OperationTermination::Cancelled
        | OperationTermination::TimedOut
        | OperationTermination::Failed
        | OperationTermination::Unavailable
        | OperationTermination::EffectUnknown => ScopeOutcome::Unavailable {
            reason: ScopeUnavailableReasonV1::AuthorityUnavailable,
        },
    })
}

fn multi_root_partial_reason(omissions: &[Omission]) -> ScopePartialReasonV1 {
    if omissions
        .iter()
        .any(|omission| omission.reason == OmissionReason::Budget)
    {
        ScopePartialReasonV1::BudgetExceeded
    } else if omissions
        .iter()
        .any(|omission| omission.reason == OmissionReason::Stale)
    {
        ScopePartialReasonV1::Stale
    } else if omissions
        .iter()
        .any(|omission| omission.reason == OmissionReason::Unavailable)
    {
        ScopePartialReasonV1::RootUnavailable
    } else {
        ScopePartialReasonV1::Incomplete
    }
}

fn work_application_router_with_executor(
    executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    work::router_with_executor(executor)
}

/// Invoke the Work owner shared by the HTTP router and the MCP adapter.
///
/// The caller supplies transport-normalized controls; typed Work decoding,
/// registry binding resolution, cancellation policy, and canonical result
/// encoding remain here so transports cannot grow their own Work dispatcher.
pub async fn invoke_work_operation(
    executor: &dyn tracedecay_daemon_protocol::DaemonInvocationExecutor,
    request: tracedecay_api::WorkHttpRequest,
) -> Response {
    work::invoke_work_operation(Some(executor), request).await
}

const DASHBOARD_FEEDBACK_OPERATIONS: [ApplicationSurfaceOperation; 3] = [
    ApplicationSurfaceOperation::FeedbackGet,
    ApplicationSurfaceOperation::FeedbackExpand,
    ApplicationSurfaceOperation::FeedbackList,
];

pub fn http_application_router(
    client: tracedecay_daemon_protocol::DaemonInvocationClient,
    operation_events: OperationEventAuthority,
    active_project_id: ProjectId,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    http_application_router_with_executor(Arc::new(client), operation_events, active_project_id)
}

#[tracing::instrument(name = "application_surface.http.router", level = "trace", skip_all)]
pub fn http_application_router_with_executor(
    executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
    operation_events: OperationEventAuthority,
    active_project_id: ProjectId,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    assemble_http_application_router(executor, operation_events, active_project_id)
}

/// Complete HTTP application routes without the process HTTP-server layer.
///
/// Dashboard nests this under `/api/application` and applies one Axum layer
/// after the full dashboard router is assembled.
pub fn assemble_http_application_router(
    executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
    operation_events: OperationEventAuthority,
    active_project_id: ProjectId,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    let cancellations = Arc::new(Mutex::new(BTreeMap::new()));
    let event_executor = Arc::clone(&executor);
    let work_router = work_application_router_with_executor(Arc::clone(&executor))?;
    let workflow_router = workflow_application_router_with_executor(Arc::clone(&executor))?;
    let handoff_router = handoff_application_router_with_executor(Arc::clone(&executor))?;
    let multi_root_router = multi_root_application_router_with_executor(Arc::clone(&executor))?;
    Ok(
        tracedecay_api::application_router(application_invoker_for_surface(
            executor,
            BindingSurface::Http,
            &ApplicationSurfaceOperation::ALL,
        )?)
        .merge(work_router)
        .merge(workflow_router)
        .merge(handoff_router)
        .merge(multi_root_router)
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&cancellations),
            application_http_context,
        ))
        .merge(http_operation_event_router(
            operation_events,
            active_project_id,
            cancellations,
            Some(event_executor),
        )),
    )
}

/// Build the dashboard's public Work mount.
///
/// It is the core route subset of the application Work router: the same
/// descriptor, the same owner, the same dispatch and problem taxonomy. The
/// attempt-runtime routes are simply not registered here, so the lease protocol
/// is unreachable from the dashboard rather than merely undocumented.
#[tracing::instrument(
    name = "application_surface.http.dashboard_work_router",
    level = "trace",
    skip_all
)]
pub fn dashboard_work_application_router_with_executor(
    executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    let cancellations = Arc::new(Mutex::new(BTreeMap::new()));
    Ok(
        work::dashboard_router_with_executor(executor)?.layer(
            axum::middleware::from_fn_with_state(cancellations, application_http_context),
        ),
    )
}

#[tracing::instrument(
    name = "application_surface.http.dashboard_configuration_router",
    level = "trace",
    skip_all
)]
pub fn dashboard_configuration_application_router_with_executor(
    executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    let cancellations = Arc::new(Mutex::new(BTreeMap::new()));
    Ok(
        tracedecay_api::configuration_application_router(application_invoker_for_surface(
            executor,
            BindingSurface::Dashboard,
            &CONFIGURATION_WIRE_OPERATIONS,
        )?)
        .layer(axum::middleware::from_fn_with_state(
            cancellations,
            application_http_context,
        )),
    )
}

#[tracing::instrument(
    name = "application_surface.http.dashboard_feedback_router",
    level = "trace",
    skip_all
)]
pub fn dashboard_feedback_application_router_with_executor(
    executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    let cancellations = Arc::new(Mutex::new(BTreeMap::new()));
    let invoker = application_invoker_for_surface(
        executor,
        BindingSurface::Dashboard,
        &DASHBOARD_FEEDBACK_OPERATIONS,
    )?;
    Ok(tracedecay_api::feedback_application_router(invoker).layer(
        axum::middleware::from_fn_with_state(cancellations, application_http_context),
    ))
}

#[cfg(test)]
mod tests;

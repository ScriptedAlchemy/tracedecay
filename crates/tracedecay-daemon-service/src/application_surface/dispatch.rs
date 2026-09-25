//! Application-surface request resolution and canonical dispatch through the daemon executor.

use std::collections::BTreeSet;

use serde_json::Value;
use tracedecay_api::{
    CanonicalInvocationResult, HttpApplicationInvocationFuture, HttpApplicationRequest,
};
use tracedecay_contracts::catalog_composition::ApplicationCatalogComposition;
use tracedecay_contracts::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, ApplicationProblem,
    ApplicationProblemEnvelope, CancellationSignal, Deadline, PageRequest, RequestId,
    ResultContractRef, SafeDiagnostic,
};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceInvocationResult, ApplicationSurfaceRequest,
    BindingResolution, CatalogBindingResolver, DispatchInput, DispatchedInvocation,
    InvocationControls, RequestedOutputFormat, ScopeSelector, parse_application_surface_request,
    resolve_dispatch,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingSurface, CatalogSnapshotV1, ProfileId, SurfaceOperationName,
};

use super::catalog::{
    application_negotiated_features, application_surface_catalog_ref, resolve_application_binding,
    validate_current_application_binding,
};
use super::configuration_wire::validate_application_outcome;
use super::feedback_observation::observe_surface_argument_rejection;
use super::problems::{
    current_micros, http_adapter_problem, invocation_contract_problem, map_dispatch_error,
};
use super::{
    APPLICATION_PROTOCOL_REVISION, CatalogBoundHttpApplicationRequest,
    HttpApplicationCatalogDispatcher,
};

pub fn application_surface_dispatch_input_with_controls(
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    page: PageRequest,
    deadline: Option<Deadline>,
    cancellation: CancellationSignal,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchInput<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    if !request.matches(operation) {
        return Err(ApplicationSurfaceAdapterError::invalid_request(
            "request body does not belong to the addressed operation",
        ));
    }
    Ok(DispatchInput {
        request_id,
        binding: BindingResolution {
            profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?,
            operation: SurfaceOperationName::new(operation.name_for_surface(surface))?,
            protocol_revision: APPLICATION_PROTOCOL_REVISION,
            negotiated_features: application_negotiated_features(),
        },
        request,
        controls: InvocationControls {
            scope: ScopeSelector::CurrentProject,
            page,
            deadline,
            cancellation,
            requested_format,
        },
    })
}

#[hotpath::measure(label = "application_surface.execute")]
pub async fn execute_application_surface(
    operation: ApplicationSurfaceOperation,
    dispatched: DispatchedInvocation<ApplicationSurfaceRequest>,
    executor: Option<&dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    validate_current_application_binding(operation, &dispatched)?;
    let result_contract = ResultContractRef::from_schema(&dispatched.invocation.result_schema);
    let binding_id = dispatched.invocation.binding_id.clone();
    let request_id = dispatched.request_id;
    let surface = dispatched.surface;
    let (invocation, requested_format) = dispatched.invocation.into_application_invocation();
    let observed_at = current_micros()?;
    let (
        deadline_ceiling_micros,
        cancellation_contract,
        terminal_states,
        receipt_contract,
        reconciliation_contract,
    ) = hotpath::measure_block!("application_surface.execute.catalog", {
        let catalog = application_surface_catalog_ref()?;
        let capability = catalog
            .capabilities()
            .find(|capability| capability.binding_ids().contains(&binding_id))
            .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
        (
            i64::try_from(capability.deadline().maximum_millis())
                .map_err(ApplicationSurfaceAdapterError::invalid_request)?
                .saturating_mul(1_000),
            capability.cancellation().clone(),
            capability.terminal_states().clone(),
            capability.receipt(),
            capability.reconciliation(),
        )
    });
    let maximum_deadline_at = UtcMicros(observed_at.0.saturating_add(deadline_ceiling_micros));
    let effective_deadline_at = invocation
        .deadline
        .as_ref()
        .map(|deadline| deadline.expires_at)
        .filter(|expires_at| *expires_at <= maximum_deadline_at)
        .unwrap_or(maximum_deadline_at);
    let deadline = Deadline::new(effective_deadline_at)?;
    let payload = invocation.request.into_invocation_payload()?;
    let Some(executor) = executor else {
        return Ok(ApplicationSurfaceInvocationResult {
            operation,
            binding_id,
            result: Err(ApplicationProblemEnvelope::new(
                result_contract,
                request_id,
                ApplicationProblem::unavailable(SafeDiagnostic::new(
                    "application.transport.unavailable",
                    "The daemon application transport is unavailable",
                )?),
            )?),
            requested_format,
        });
    };
    let binding = tracedecay_contracts::ApplicationInvocationBinding::new(
        binding_id.clone(),
        surface,
        SurfaceOperationName::new(operation.name_for_surface(surface))?,
        result_contract.clone(),
        invocation.page,
    )?;
    let context = tracedecay_contracts::ApplicationInvocationContext::new(
        request_id.clone(),
        invocation.scope,
        deadline,
        invocation.cancellation,
    )?;
    let request = tracedecay_contracts::ApplicationRequest::surface(binding, payload)?;
    let invocation = tracedecay_contracts::ApplicationInvocation::new(context, request)?;
    let result = match hotpath::future!(
        tracedecay_contracts::ApplicationInvocationExecutor::invoke(executor, invocation),
        label = "application_surface.execute.invoke"
    )
    .await
    {
        Ok(response) => match response
            .envelope()
            .filter(|envelope| {
                validate_application_outcome(
                    operation,
                    &envelope.outcome,
                    &cancellation_contract,
                    &terminal_states,
                    receipt_contract,
                    reconciliation_contract,
                )
            })
            .cloned()
        {
            Some(envelope) => Ok(envelope),
            None => Err(ApplicationProblemEnvelope::new(
                result_contract.clone(),
                request_id.clone(),
                ApplicationProblem::unavailable(SafeDiagnostic {
                    code: "application.surface.invalid_response".to_owned(),
                    message: "The daemon returned an invalid application response".to_owned(),
                }),
            )?),
        },
        // An unreachable daemon never saw the request: it is a dispatch
        // failure, not a retryable problem envelope. Wrapping it made every
        // CLI surface re-dispatch (and re-pay the connect grace) until its
        // deadline, 128 s against a dead socket.
        Err(tracedecay_contracts::InvocationError::Unreachable {
            reason_code,
            detail,
        }) => {
            return Err(ApplicationSurfaceAdapterError::DaemonUnreachable {
                reason_code,
                detail,
            });
        }
        Err(error) => Err(ApplicationProblemEnvelope::new(
            result_contract,
            request_id,
            invocation_contract_problem(error)?,
        )?),
    };
    Ok(ApplicationSurfaceInvocationResult {
        operation,
        binding_id,
        result,
        requested_format,
    })
}

#[hotpath::measure(label = "application_surface.resolve.http", future = true)]
pub async fn resolve_http_application_surface(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    executor: Option<&dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    let dispatched = match resolve_http_application_surface_dispatch(
        operation,
        request_id.clone(),
        request,
        requested_format,
    ) {
        Ok(dispatched) => dispatched,
        Err(error) => {
            observe_surface_argument_rejection(
                executor,
                BindingSurface::Http,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    execute_application_surface(operation, dispatched, executor).await
}

/// Resolve a dashboard action through the same catalog entry and daemon-owned
/// application handler as CLI, MCP, and HTTP. Dashboard adapters may shape
/// presentation responses around this result, but they do not own mutation
/// validation, authorization, CAS, receipts, or rollback semantics.
#[hotpath::measure(label = "application_surface.resolve.dashboard", future = true)]
pub async fn resolve_dashboard_application_surface(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    executor: Option<&dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    let dispatched = resolve_application_surface_dispatch(
        BindingSurface::Dashboard,
        operation,
        request_id,
        request,
        requested_format,
    )?;
    execute_application_surface(operation, dispatched, executor).await
}

pub fn resolve_http_application_surface_dispatch(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchedInvocation<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    resolve_application_surface_dispatch(
        BindingSurface::Http,
        operation,
        request_id,
        request,
        requested_format,
    )
}

pub fn resolve_application_surface_dispatch(
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchedInvocation<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    let cancellation = CancellationSignal::active(format!("cancellation.{}", request_id.as_str()))?;
    resolve_application_surface_dispatch_with_controls(
        surface,
        operation,
        request_id,
        request,
        PageRequest::first(
            tracedecay_contracts::application_operation_default_page_size(operation),
        )?,
        None,
        cancellation,
        requested_format,
    )
}

#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "application_surface.dispatch")]
pub fn resolve_application_surface_dispatch_with_controls(
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    page: PageRequest,
    deadline: Option<Deadline>,
    cancellation: CancellationSignal,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchedInvocation<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    let catalog = application_surface_catalog_ref()?;
    let resolver = CatalogBindingResolver::new(catalog);
    let input = application_surface_dispatch_input_with_controls(
        surface,
        operation,
        request_id,
        request,
        page,
        deadline,
        cancellation,
        requested_format,
    )?;
    let dispatched = resolve_dispatch(&resolver, surface, input).map_err(map_dispatch_error)?;
    Ok(dispatched)
}

pub(super) fn invoke_catalog_bound_application_request(
    request: HttpApplicationRequest,
    surface: BindingSurface,
    composition: &ApplicationCatalogComposition<HttpApplicationCatalogDispatcher>,
) -> HttpApplicationInvocationFuture {
    let profile_id = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)
        .unwrap_or_else(|_| panic!("the application profile id is static"));
    let operation_name = SurfaceOperationName::new(request.operation.name_for_surface(surface))
        .unwrap_or_else(|_| panic!("the application operation name is static"));
    let capability = composition
        .snapshot()
        .resolve_binding(&profile_id, surface, &operation_name, 1, &BTreeSet::new())
        .unwrap_or_else(|| {
            panic!("surface bindings are validated before the application router is mounted")
        });
    let handler = composition
        .handler(capability.use_case_id())
        .unwrap_or_else(|| panic!("catalog composition validates every callable handler"));
    handler.invoke(CatalogBoundHttpApplicationRequest {
        capability_id: capability.capability_id().clone(),
        use_case_id: capability.use_case_id().clone(),
        surface,
        request,
    })
}

#[hotpath::measure(label = "application_surface.adapter.invoke", future = true)]
pub(super) async fn invoke_application_adapter_request(
    request: HttpApplicationRequest,
    surface: BindingSurface,
    executor: &dyn tracedecay_daemon_protocol::DaemonInvocationExecutor,
    catalog: &CatalogSnapshotV1,
) -> std::result::Result<CanonicalInvocationResult<Value>, ApplicationContractError> {
    let operation = request.operation;
    let resolver = CatalogBindingResolver::new(catalog);
    let binding = resolve_application_binding(&resolver, surface, operation).unwrap_or_else(|| {
        panic!("surface bindings are validated before the application router is mounted")
    });
    let binding_id = binding.binding_id;
    let result_contract = ResultContractRef::from_schema(&binding.result_schema);
    let request_id = request.request_id;
    let application_request =
        match parse_http_application_surface_request(operation, request.body, &request.page) {
            Ok(request) => request,
            Err(error) => {
                observe_surface_argument_rejection(
                    Some(executor),
                    surface,
                    operation,
                    &request_id,
                    &error,
                )
                .await;
                return Ok(CanonicalInvocationResult::new(
                    binding_id,
                    Err(http_adapter_problem(result_contract, request_id, error)?),
                ));
            }
        };
    let input = match application_surface_dispatch_input_with_controls(
        surface,
        operation,
        request_id.clone(),
        application_request,
        request.page,
        request.deadline,
        request.cancellation,
        RequestedOutputFormat::Json,
    ) {
        Ok(input) => input,
        Err(error) => {
            observe_surface_argument_rejection(
                Some(executor),
                surface,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Ok(CanonicalInvocationResult::new(
                binding_id,
                Err(http_adapter_problem(result_contract, request_id, error)?),
            ));
        }
    };
    let dispatched = match resolve_dispatch(&resolver, surface, input) {
        Ok(dispatched) => dispatched,
        Err(error) => {
            let error = map_dispatch_error(error);
            observe_surface_argument_rejection(
                Some(executor),
                surface,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Ok(CanonicalInvocationResult::new(
                binding_id,
                Err(http_adapter_problem(result_contract, request_id, error)?),
            ));
        }
    };
    Ok(
        match execute_application_surface(operation, dispatched, Some(executor)).await {
            Ok(result) => CanonicalInvocationResult::new(result.binding_id, result.result),
            Err(error) => CanonicalInvocationResult::new(
                binding_id,
                Err(http_adapter_problem(result_contract, request_id, error)?),
            ),
        },
    )
}

/// Where an HTTP page request lands inside an operation's request body.
///
/// The projection follows the request family the operation decodes into in
/// [`parse_application_surface_request`], which is what decides where the page
/// controls are readable at all: the callable- and primitive-code families
/// decode into requests carrying a [`CallableCodeSurfaceMeta`], so a
/// continuation cursor rides in `meta`; the diagnostics read decodes into a
/// request whose page controls are plain body fields; nothing else takes page
/// input from the transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HttpPageProjection {
    /// The continuation cursor is written into the request's `meta` object.
    MetaCursor,
    /// The page size and cursor are written as top-level body fields.
    BodyPageControls,
    /// The operation accepts no page input.
    Unpaged,
}

/// The single authority for how an operation receives an HTTP page request.
///
/// [`apply_http_page_to_surface_body`] asks this and nothing else, so the
/// cursor-carrying family and the diagnostics body-field case are stated in one
/// place instead of a boolean allowlist plus a stray operation comparison.
pub(super) fn http_page_projection(operation: ApplicationSurfaceOperation) -> HttpPageProjection {
    match operation {
        ApplicationSurfaceOperation::CodeExactOccurrence
        | ApplicationSurfaceOperation::CodePhraseSearch
        | ApplicationSurfaceOperation::CodeSymbolSearch
        | ApplicationSurfaceOperation::CodeSignatureSearch
        | ApplicationSurfaceOperation::CodeImplementations
        | ApplicationSurfaceOperation::CodeTypeHierarchy
        | ApplicationSurfaceOperation::CodeCallers
        | ApplicationSurfaceOperation::CodeCallees
        | ApplicationSurfaceOperation::CodeFacets
        | ApplicationSurfaceOperation::CodeTimeline
        | ApplicationSurfaceOperation::CodeDeclaration
        | ApplicationSurfaceOperation::CodeTypeDefinition
        | ApplicationSurfaceOperation::CodeReferences => HttpPageProjection::MetaCursor,
        ApplicationSurfaceOperation::DiagnosticsRead => HttpPageProjection::BodyPageControls,
        _ => HttpPageProjection::Unpaged,
    }
}

/// Decodes one HTTP request body into the canonical surface request.
///
/// HTTP is the one transport that carries page controls outside the body (the
/// `page_size` / `cursor` query), so they are projected into the body first;
/// CLI and MCP argument objects already carry them and reach
/// [`parse_application_surface_request`] directly. Every transport therefore
/// decodes into the same [`ApplicationSurfaceRequest`].
pub fn parse_http_application_surface_request(
    operation: ApplicationSurfaceOperation,
    body: Value,
    page: &PageRequest,
) -> Result<ApplicationSurfaceRequest, ApplicationSurfaceAdapterError> {
    parse_application_surface_request(
        operation,
        apply_http_page_to_surface_body(operation, body, page),
    )
}

pub(super) fn apply_http_page_to_surface_body(
    operation: ApplicationSurfaceOperation,
    mut body: Value,
    page: &PageRequest,
) -> Value {
    match http_page_projection(operation) {
        HttpPageProjection::MetaCursor => {
            if let Some(meta) = body.get_mut("meta").and_then(Value::as_object_mut)
                && let Some(cursor) = page.cursor.as_ref()
            {
                meta.insert("cursor".to_owned(), Value::from(cursor.as_str()));
            }
        }
        HttpPageProjection::BodyPageControls => {
            if let Some(object) = body.as_object_mut() {
                object.insert(
                    "maximum_diagnostics".to_owned(),
                    Value::from(page.page_size),
                );
                object.insert(
                    "cursor".to_owned(),
                    page.cursor
                        .as_ref()
                        .map_or(Value::Null, |cursor| Value::from(cursor.as_str())),
                );
            }
        }
        HttpPageProjection::Unpaged => {}
    }
    body
}

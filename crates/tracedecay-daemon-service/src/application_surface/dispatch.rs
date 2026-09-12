//! Application-surface request resolution and canonical dispatch through the daemon executor.

use std::collections::BTreeSet;

use serde_json::Value;
use tracedecay_api::{
    CanonicalInvocationResult, HttpApplicationInvocationFuture, HttpApplicationRequest,
};
use tracedecay_contracts::catalog_composition::ApplicationCatalogComposition;
use tracedecay_contracts::feedback::observations::{FeedbackOutcomeV1, FeedbackSourceEventV1};
use tracedecay_contracts::retrieval::PrimitiveRequest;
use tracedecay_contracts::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, ApplicationEnvelope,
    ApplicationProblem, ApplicationProblemEnvelope, CancellationSignal, Deadline, PageRequest,
    RequestId, ResultContractRef, SafeDiagnostic,
};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceInvocationResult, ApplicationSurfaceRequest,
    BindingResolution, CatalogBindingResolver, DaemonInvocationError, DispatchInput,
    DispatchedInvocation, InvocationCancellationPolicy, InvocationControls, RequestedOutputFormat,
    ScopeSelector, parse_application_surface_request, resolve_dispatch,
};
use tracedecay_domain::{UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingSurface, CatalogSnapshotV1, ProfileId, SurfaceOperationName,
};

use super::catalog::{
    application_negotiated_features, application_surface_catalog_ref, resolve_application_binding,
    validate_current_application_binding,
};
use super::configuration_wire::{
    configuration_invocation_payload, is_configuration_operation, validate_application_outcome,
};
use super::feedback_observation::{
    feedback_delivery_route, feedback_surface_is_observable, feedback_surface_operation,
    observe_surface_argument_rejection,
};
use super::problems::{
    current_micros, http_adapter_problem, invocation_contract_problem, invocation_problem,
    map_dispatch_error,
};
use super::{
    APPLICATION_PROTOCOL_REVISION, CatalogBoundHttpApplicationRequest,
    HttpApplicationCatalogDispatcher, retained,
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
        return Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest);
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
    let delivery_route = feedback_delivery_route(dispatched.surface);
    let (invocation, requested_format) = dispatched.invocation.into_application_invocation();
    let observed_at = current_micros()?;
    let (
        deadline_ceiling_micros,
        cancellation_contract,
        terminal_states,
        receipt_contract,
        reconciliation_contract,
        catalog_effect,
    ) = hotpath::measure_block!("application_surface.execute.catalog", {
        let catalog = application_surface_catalog_ref()?;
        let capability = catalog
            .capabilities()
            .find(|capability| capability.binding_ids().contains(&binding_id))
            .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
        (
            i64::try_from(capability.deadline().maximum_millis())
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?
                .saturating_mul(1_000),
            capability.cancellation().clone(),
            capability.terminal_states().clone(),
            capability.receipt(),
            capability.reconciliation(),
            capability.effect().is_effect(),
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
    let cancellation = invocation.cancellation;
    let cancellation_context = cancellation.context();
    let resolved_scope = match &invocation.scope {
        tracedecay_contracts::InvocationTarget::CurrentProject => None,
        tracedecay_contracts::InvocationTarget::Resolved(scope) => Some(scope.clone()),
    };
    let request_deadline = deadline.clone();
    let migrated_payload = match (&operation, &invocation.request) {
        (
            ApplicationSurfaceOperation::ConfigurationGet
            | ApplicationSurfaceOperation::ConfigurationSet
            | ApplicationSurfaceOperation::ConfigurationUnset
            | ApplicationSurfaceOperation::ConfigurationBatch,
            ApplicationSurfaceRequest::Configuration(request),
        ) => Some(configuration_invocation_payload(request)?),
        (
            ApplicationSurfaceOperation::FeedbackGet,
            ApplicationSurfaceRequest::Feedback(request),
        ) => Some(
            serde_json::to_value(request)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?,
        ),
        _ => None,
    };
    if let Some(payload) = migrated_payload {
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
            cancellation,
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
            // Same dispatch-failure contract as the non-migrated arm below: an
            // unreachable daemon never saw the request, so it is an error, not
            // a retryable problem envelope.
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
        return Ok(ApplicationSurfaceInvocationResult {
            operation,
            binding_id,
            result,
            requested_format,
        });
    }
    let request = hotpath::measure_block!("application_surface.execute.request_build", {
        match invocation.request {
            ApplicationSurfaceRequest::GitRead(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::git_read(
                    request_id.as_str(),
                    operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::GitPreview(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::git_preview(
                    request_id.as_str(),
                    request,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::GitApply(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::git_apply(
                    request_id.as_str(),
                    request,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::GitHubStackSignalExpand(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::github_stack_signal_expand(
                    request_id.as_str(),
                    request,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::NativeIntegration(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::native_integration(
                    request_id.as_str(),
                    operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::Feedback(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::feedback(
                    request_id.as_str(),
                    operation,
                    request.request_handle,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::FeedbackAdvisoryCycle(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::feedback_advisory_cycle(
                    request_id.as_str(),
                    request.document_uri,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::TestResults(_) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::primitive(
                    request_id.as_str(),
                    operation,
                    PrimitiveRequest::RecentTestResults(invocation.page),
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::CallableCode(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::callable_code(
                    request_id.as_str(),
                    operation,
                    request,
                    invocation.page,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::PrimitiveCode(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::primitive_code(
                    request_id.as_str(),
                    operation,
                    request,
                    invocation.page,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::Primitive(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::primitive(
                    request_id.as_str(),
                    operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::ObservatoryRead(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::observatory_read(
                    request_id.as_str(),
                    request,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
                .with_resolved_scope(resolved_scope)
            }
            ApplicationSurfaceRequest::Configuration(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::configuration(
                    request_id.as_str(),
                    operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::ContextScout(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::context_scout(
                    request_id.as_str(),
                    operation,
                    request,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
            ApplicationSurfaceRequest::Retained(request) => {
                tracedecay_daemon_protocol::DaemonInvocationRequest::retained_application(
                    request_id.as_str(),
                    request,
                    observed_at,
                    deadline,
                    cancellation_context,
                )
            }
        }
        .with_delivery_route(delivery_route)
    });
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
    let policy = if (is_configuration_operation(operation) && catalog_effect)
        || matches!(
            operation,
            ApplicationSurfaceOperation::GitApply
                | ApplicationSurfaceOperation::NativeIntegrationApprove
                | ApplicationSurfaceOperation::NativeIntegrationApply
                | ApplicationSurfaceOperation::NativeIntegrationCancel
                | ApplicationSurfaceOperation::ContextScoutPause
                | ApplicationSurfaceOperation::ContextScoutResume
                | ApplicationSurfaceOperation::ContextScoutCancel
                | ApplicationSurfaceOperation::ContextScoutClaim
                | ApplicationSurfaceOperation::ContextScoutDelivery
                | ApplicationSurfaceOperation::ContextScoutFeedback
        ) {
        InvocationCancellationPolicy::AuthoritativeEffect
    } else {
        InvocationCancellationPolicy::ReadOnly
    };
    let response = hotpath::future!(
        executor.invoke_controlled(request, request_deadline, cancellation, policy),
        label = "application_surface.execute.invoke"
    )
    .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            // An unreachable daemon is a dispatch failure, not an answer:
            // wrapping it in a retryable problem envelope made every CLI
            // surface re-dispatch (and re-pay the connect grace) until its
            // deadline — 128 s against a dead socket — while sibling
            // compatibility tools failed typed in one grace. The feedback
            // observation below rides the same dead transport, so it is
            // skipped too: it would pay one more full connect grace to
            // observe that the daemon it reports to is down.
            if let DaemonInvocationError::Unreachable {
                reason_code,
                detail,
            } = error
            {
                return Err(ApplicationSurfaceAdapterError::DaemonUnreachable {
                    reason_code,
                    detail,
                });
            }
            if feedback_surface_is_observable(operation)
                && let Ok(subject_digest) = canonical_sha256(&(
                    "tracedecay.feedback.transport-observation.v1",
                    request_id.as_str(),
                    operation.as_str(),
                    delivery_route,
                ))
                && let Ok(observed_at) = current_micros()
            {
                let event = match &error {
                    DaemonInvocationError::Cancelled { .. } => {
                        FeedbackSourceEventV1::Cancellation {
                            operation: feedback_surface_operation(operation),
                            outcome: FeedbackOutcomeV1::Cancelled,
                        }
                    }
                    DaemonInvocationError::TimedOut { .. } => FeedbackSourceEventV1::Cancellation {
                        operation: feedback_surface_operation(operation),
                        outcome: FeedbackOutcomeV1::TimedOut,
                    },
                    DaemonInvocationError::Unavailable
                    | DaemonInvocationError::Unreachable { .. } => {
                        FeedbackSourceEventV1::Delivery {
                            operation: feedback_surface_operation(operation),
                            route: delivery_route,
                            outcome: FeedbackOutcomeV1::Unavailable,
                            item_count: 0,
                            duration_micros: None,
                        }
                    }
                };
                let _ = executor
                    .observe_feedback(subject_digest, observed_at, event)
                    .await;
            }
            return Ok(ApplicationSurfaceInvocationResult {
                operation,
                binding_id,
                result: Err(ApplicationProblemEnvelope::new(
                    result_contract,
                    request_id,
                    error.into_application_problem(),
                )?),
                requested_format,
            });
        }
    };
    let result = hotpath::measure_block!("application_surface.execute.assemble", {
        match response.outcome {
            tracedecay_daemon_protocol::DaemonInvocationOutcome::GitRead { scope, result } => {
                Ok(ApplicationEnvelope::evidence(
                    result_contract.clone(),
                    request_id.clone(),
                    scope,
                    result.into_application(),
                ))
            }
            tracedecay_daemon_protocol::DaemonInvocationOutcome::GitPreview { scope, preview } => {
                Ok(ApplicationEnvelope::preview(
                    result_contract.clone(),
                    request_id.clone(),
                    scope,
                    preview.into_application_result()?,
                ))
            }
            tracedecay_daemon_protocol::DaemonInvocationOutcome::GitApply { scope, effect } => {
                Ok(ApplicationEnvelope::effect(
                    result_contract.clone(),
                    request_id.clone(),
                    scope,
                    effect.into_application_result()?,
                ))
            }
            tracedecay_daemon_protocol::DaemonInvocationOutcome::Feedback { scope, result }
            | tracedecay_daemon_protocol::DaemonInvocationOutcome::Primitive { scope, result }
            | tracedecay_daemon_protocol::DaemonInvocationOutcome::ObservatoryRead {
                scope,
                result,
            } => Ok(ApplicationEnvelope::evidence(
                result_contract.clone(),
                request_id.clone(),
                scope,
                result.into_application(),
            )),
            tracedecay_daemon_protocol::DaemonInvocationOutcome::CallableCode { scope, result } => {
                Ok(ApplicationEnvelope::evidence(
                    result_contract.clone(),
                    request_id.clone(),
                    scope,
                    result.into_application(),
                ))
            }
            tracedecay_daemon_protocol::DaemonInvocationOutcome::Configuration {
                scope,
                outcome,
            } => {
                if validate_application_outcome(
                    operation,
                    &outcome,
                    &cancellation_contract,
                    &terminal_states,
                    receipt_contract,
                    reconciliation_contract,
                ) {
                    Ok(ApplicationEnvelope {
                        contract: result_contract.clone(),
                        request_id: request_id.clone(),
                        scope,
                        outcome,
                    })
                } else {
                    Err(ApplicationProblemEnvelope::new(
                        result_contract.clone(),
                        request_id.clone(),
                        ApplicationProblem::unavailable(SafeDiagnostic::new(
                            "application.surface.invalid_configuration_response",
                            "The daemon returned a configuration result that did not match its wire contract",
                        )?),
                    )?)
                }
            }
            tracedecay_daemon_protocol::DaemonInvocationOutcome::GitHubStackSignalExpand {
                scope,
                outcome,
            }
            | tracedecay_daemon_protocol::DaemonInvocationOutcome::NativeIntegration {
                scope,
                outcome,
            }
            | tracedecay_daemon_protocol::DaemonInvocationOutcome::ContextScout {
                scope,
                outcome,
            } => Ok(ApplicationEnvelope {
                contract: result_contract.clone(),
                request_id: request_id.clone(),
                scope,
                outcome,
            }),
            tracedecay_daemon_protocol::DaemonInvocationOutcome::RetainedApplication {
                scope,
                outcome,
            } => Ok(ApplicationEnvelope {
                contract: result_contract.clone(),
                request_id: request_id.clone(),
                scope,
                outcome: retained::outcome_value(outcome)?,
            }),
            tracedecay_daemon_protocol::DaemonInvocationOutcome::ApplicationProblem { problem } => {
                Err(ApplicationProblemEnvelope::new(
                    result_contract.clone(),
                    request_id.clone(),
                    problem,
                )?)
            }
            tracedecay_daemon_protocol::DaemonInvocationOutcome::Problem { problem } => {
                Err(ApplicationProblemEnvelope::new(
                    result_contract.clone(),
                    request_id.clone(),
                    invocation_problem(problem)?,
                )?)
            }
            _ => Err(ApplicationProblemEnvelope::new(
                result_contract.clone(),
                request_id.clone(),
                ApplicationProblem::unavailable(SafeDiagnostic::new(
                    "application.surface.invalid_response",
                    "The daemon returned an invalid application response",
                )?),
            )?),
        }
    });

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

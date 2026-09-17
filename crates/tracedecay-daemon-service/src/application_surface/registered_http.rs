use axum::response::Response;
use serde::Serialize;
use tracedecay_api::{
    CanonicalInvocationResult, HandoffOperation, HttpApplicationControls, WorkOperation,
    WorkflowOperation,
};
use tracedecay_contracts::{
    ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, LegalAction,
    ProblemOwningLayer, RequestId, ResultContractRef, RetryDirective, SafeDiagnostic,
};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, DaemonInvocationError, InvocationCancellationPolicy,
};
use tracedecay_tool_catalog::RouteExposureV1;

use super::problems::{application_contract_error_response, registered_adapter_unavailable};

pub(crate) trait RegisteredHttpOperation: Copy {
    fn operation_id(self) -> String;
    fn is_read_only(self) -> bool;
    fn problem_family(self) -> &'static str;
    fn display_family(self) -> &'static str;
    fn application_problem_is_bound(
        self,
        _request_id: &RequestId,
        scope: Option<&tracedecay_contracts::ResolvedScope>,
        _problem: &ApplicationProblem,
    ) -> bool {
        scope.is_none()
    }
    fn registry(
        self,
    ) -> Result<
        std::borrow::Cow<'static, tracedecay_tool_catalog::ExecutableBindingRegistryV1>,
        tracedecay_daemon_protocol::ApplicationSurfaceAdapterError,
    >;
}

#[hotpath::measure(label = "application_surface.registered.validate_outcome")]
pub(super) fn validated_daemon_outcome<O>(
    operation: O,
    request_id: &RequestId,
    response: Result<tracedecay_daemon_protocol::DaemonInvocationResponse, DaemonInvocationError>,
) -> Result<tracedecay_daemon_protocol::DaemonInvocationOutcome, ApplicationProblem>
where
    O: RegisteredHttpOperation,
{
    let problem_code = |suffix: &str| format!("{}.{}", operation.problem_family(), suffix);
    let family = operation.display_family();
    match response {
        Ok(response)
            if response.protocol == tracedecay_daemon_protocol::DAEMON_INVOCATION_PROTOCOL
                && response.revision == tracedecay_daemon_protocol::DAEMON_INVOCATION_REVISION
                && response.request_id == request_id.as_str() =>
        {
            let problem_is_bound = match &response.outcome {
                tracedecay_daemon_protocol::DaemonInvocationOutcome::ApplicationProblem { problem } => {
                    operation.application_problem_is_bound(request_id, None, problem)
                }
                tracedecay_daemon_protocol::DaemonInvocationOutcome::RetainedApplicationProblem {
                    scope,
                    problem,
                } => operation.application_problem_is_bound(request_id, Some(scope), problem),
                _ => true,
            };
            if !problem_is_bound {
                return Err(ApplicationProblem::unavailable(SafeDiagnostic {
                    code: problem_code("invalid_terminal"),
                    message: format!("The {family} daemon returned an unbound terminal"),
                }));
            }
            Ok(response.outcome)
        }
        Ok(_) => Err(ApplicationProblem::unavailable(SafeDiagnostic {
            code: problem_code("invalid_envelope"),
            message: format!("The {family} daemon returned an invalid response envelope"),
        })),
        Err(DaemonInvocationError::Cancelled { stage }) => Err(ApplicationProblem::Cancelled {
            stage,
            retry: tracedecay_contracts::RetryDirective::Never,
            legal_actions: Vec::new(),
        }),
        Err(DaemonInvocationError::TimedOut { stage }) => Err(ApplicationProblem::TimedOut {
            stage,
            retry: tracedecay_contracts::RetryDirective::Never,
            legal_actions: Vec::new(),
        }),
        Err(DaemonInvocationError::Unavailable) => {
            Err(ApplicationProblem::unavailable(SafeDiagnostic {
                code: problem_code("transport_unavailable"),
                message: format!("The {family} application transport is unavailable"),
            }))
        }
        // Registered HTTP handlers run inside the daemon process, so a
        // connect-phase failure cannot occur here; the projection still keeps
        // the connect diagnostic truthful for completeness.
        Err(DaemonInvocationError::Unreachable {
            reason_code,
            detail,
        }) => Err(ApplicationProblem::unavailable(SafeDiagnostic {
            code: reason_code,
            message: detail,
        })),
    }
}

#[cfg(test)]
#[path = "retained_http_identity_tests.rs"]
mod tests;

/// Dispatch one registered operation and encode its canonical result.
///
/// Core and attempt operations differ only in which daemon payload carries them
/// and which outcome they answer with, so both arrive here: one binding lookup,
/// one cancellation policy, one problem taxonomy.
impl RegisteredHttpOperation for WorkOperation {
    fn operation_id(self) -> String {
        WorkOperation::operation_id_str(self).to_owned()
    }

    fn is_read_only(self) -> bool {
        WorkOperation::is_read_only(self)
    }

    fn problem_family(self) -> &'static str {
        "work"
    }

    fn display_family(self) -> &'static str {
        "Work"
    }

    fn registry(
        self,
    ) -> Result<
        std::borrow::Cow<'static, tracedecay_tool_catalog::ExecutableBindingRegistryV1>,
        ApplicationSurfaceAdapterError,
    > {
        tracedecay_contracts::work_executable_binding_registry()
            .map(std::borrow::Cow::Borrowed)
            .map_err(ApplicationSurfaceAdapterError::CatalogValidation)
    }
}

impl RegisteredHttpOperation for WorkflowOperation {
    fn operation_id(self) -> String {
        WorkflowOperation::operation_id_str(self).to_owned()
    }

    fn is_read_only(self) -> bool {
        false
    }

    fn problem_family(self) -> &'static str {
        "workflow"
    }

    fn display_family(self) -> &'static str {
        "Workflow"
    }

    fn registry(
        self,
    ) -> Result<
        std::borrow::Cow<'static, tracedecay_tool_catalog::ExecutableBindingRegistryV1>,
        ApplicationSurfaceAdapterError,
    > {
        tracedecay_contracts::workflow_executable_binding_registry()
            .map(std::borrow::Cow::Borrowed)
            .map_err(ApplicationSurfaceAdapterError::CatalogValidation)
    }
}

impl RegisteredHttpOperation for HandoffOperation {
    fn operation_id(self) -> String {
        HandoffOperation::operation_id_str(self).to_owned()
    }

    fn is_read_only(self) -> bool {
        // Not a blanket `false` any more: enumeration reads the grant store
        // without issuing or consuming anything, and treating it as a mutation
        // here would deny a safe read the retry and replay handling a read is
        // entitled to.
        HandoffOperation::is_read_only(self)
    }

    fn problem_family(self) -> &'static str {
        "handoff"
    }

    fn display_family(self) -> &'static str {
        "handoff-open"
    }

    fn registry(
        self,
    ) -> Result<
        std::borrow::Cow<'static, tracedecay_tool_catalog::ExecutableBindingRegistryV1>,
        ApplicationSurfaceAdapterError,
    > {
        tracedecay_contracts::handoff_executable_binding_registry()
            .map(std::borrow::Cow::Owned)
            .map_err(ApplicationSurfaceAdapterError::CatalogValidation)
    }
}

/// Return the same typed result-contract envelope used after daemon dispatch
/// when a caller has no authenticated daemon executor to invoke.
///
/// MCP can be constructed before the daemon route is attached. That state is
/// still an application failure of the named family, not an MCP tool-resolution
/// failure, so the response must retain the operation's registered result
/// schema and canonical runtime problem taxonomy.
pub(crate) fn registered_executor_unavailable<T, O>(operation: O, request_id: RequestId) -> Response
where
    T: Serialize,
    O: RegisteredHttpOperation,
{
    let problem_code = |suffix: &str| format!("{}.{}", operation.problem_family(), suffix);
    let family = operation.display_family();
    let registry = match operation.registry() {
        Ok(registry) => registry,
        Err(_) => {
            return registered_adapter_unavailable(
                request_id,
                &problem_code("catalog_unavailable"),
                &format!("The {family} capability catalog is unavailable"),
            );
        }
    };
    let operation_id = match tracedecay_tool_catalog::OperationId::new(operation.operation_id()) {
        Ok(operation_id) => operation_id,
        Err(_) => {
            return registered_adapter_unavailable(
                request_id,
                &problem_code("operation_identity_unavailable"),
                &format!("The {family} operation identity is unavailable"),
            );
        }
    };
    let Some(binding) = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
    else {
        return registered_adapter_unavailable(
            request_id,
            &problem_code("binding_unavailable"),
            &format!("The {family} operation is not advertised by this build"),
        );
    };
    let RouteExposureV1::Public { binding_id, .. } = binding.exposure() else {
        return registered_adapter_unavailable(
            request_id,
            &problem_code("route_unavailable"),
            &format!("The {family} operation binding carries no public route"),
        );
    };
    let result_contract = match ResultContractRef::new(
        binding.result_schema().schema_ref().schema_id().clone(),
        binding.result_schema().schema_ref().revision(),
    ) {
        Ok(contract) => contract,
        Err(_) => {
            return registered_adapter_unavailable(
                request_id,
                &problem_code("result_contract_unavailable"),
                &format!("The {family} operation result contract is unavailable"),
            );
        }
    };
    let problem = match ApplicationProblemEnvelope::new(
        result_contract,
        request_id,
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: problem_code("transport_unavailable"),
            message: format!("The {family} application transport is unavailable"),
        }),
    ) {
        Ok(problem) => problem.with_owning_layer(ProblemOwningLayer::Runtime),
        Err(error) => return application_contract_error_response(error),
    };
    CanonicalInvocationResult::<T>::new(binding_id.clone(), Err(problem)).into_http_response()
}

#[hotpath::measure(label = "application_surface.registered.invoke")]
pub(super) async fn invoke_registered_http<T, O>(
    executor: &dyn tracedecay_daemon_protocol::DaemonInvocationExecutor,
    operation: O,
    request_id: RequestId,
    controls: HttpApplicationControls,
    invocation: tracedecay_daemon_protocol::DaemonInvocationRequest,
    select_outcome: impl FnOnce(
        tracedecay_daemon_protocol::DaemonInvocationOutcome,
    ) -> Option<(
        tracedecay_contracts::ResolvedScope,
        tracedecay_contracts::ApplicationOutcome<T>,
    )>,
) -> Response
where
    T: Serialize,
    O: RegisteredHttpOperation,
{
    let problem_code = |suffix: &str| format!("{}.{}", operation.problem_family(), suffix);
    let family = operation.display_family();
    let registry = match operation.registry() {
        Ok(registry) => registry,
        Err(_) => {
            return registered_adapter_unavailable(
                request_id,
                &problem_code("catalog_unavailable"),
                &format!("The {family} capability catalog is unavailable"),
            );
        }
    };
    let operation_id = match tracedecay_tool_catalog::OperationId::new(operation.operation_id()) {
        Ok(operation_id) => operation_id,
        Err(_) => {
            return registered_adapter_unavailable(
                request_id,
                &problem_code("operation_identity_unavailable"),
                &format!("The {family} operation identity is unavailable"),
            );
        }
    };
    let Some(binding) = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
    else {
        return registered_adapter_unavailable(
            request_id,
            &problem_code("binding_unavailable"),
            &format!("The {family} operation is not advertised by this build"),
        );
    };
    let RouteExposureV1::Public { binding_id, .. } = binding.exposure() else {
        return registered_adapter_unavailable(
            request_id,
            &problem_code("route_unavailable"),
            &format!("The {family} operation binding carries no public route"),
        );
    };
    let result_contract = match ResultContractRef::new(
        binding.result_schema().schema_ref().schema_id().clone(),
        binding.result_schema().schema_ref().revision(),
    ) {
        Ok(contract) => contract,
        Err(_) => {
            return registered_adapter_unavailable(
                request_id,
                &problem_code("result_contract_unavailable"),
                &format!("The {family} operation result contract is unavailable"),
            );
        }
    };
    let binding_id = binding_id.clone();
    let policy = if operation.is_read_only() {
        InvocationCancellationPolicy::ReadOnly
    } else {
        InvocationCancellationPolicy::AuthoritativeEffect
    };
    let response = hotpath::future!(
        executor.invoke_controlled(invocation, controls.deadline, controls.cancellation, policy),
        label = "application_surface.registered.dispatch"
    )
    .await;
    let outcome = hotpath::measure_block!(
        "application_surface.registered.assemble",
        validated_daemon_outcome(operation, &request_id, response)
    );
    let owning_layer = match &outcome {
        Ok(
            tracedecay_daemon_protocol::DaemonInvocationOutcome::ApplicationProblem { .. }
            | tracedecay_daemon_protocol::DaemonInvocationOutcome::RetainedApplicationProblem {
                ..
            },
        ) => ProblemOwningLayer::Application,
        _ => ProblemOwningLayer::Runtime,
    };
    let problem = match outcome {
        Ok(outcome) => match outcome {
            tracedecay_daemon_protocol::DaemonInvocationOutcome::ApplicationProblem { problem } => {
                problem
            }
            tracedecay_daemon_protocol::DaemonInvocationOutcome::RetainedApplicationProblem {
                problem,
                ..
            } => problem,
            tracedecay_daemon_protocol::DaemonInvocationOutcome::Problem { problem } => match problem {
                tracedecay_daemon_protocol::DaemonInvocationProblem::InvalidRequest
                | tracedecay_daemon_protocol::DaemonInvocationProblem::UnsupportedRevision => {
                    ApplicationProblem::InvalidRequest {
                        diagnostic: SafeDiagnostic {
                            code: problem_code("invalid_request"),
                            message: format!("The {family} application request is invalid"),
                        },
                        retry: RetryDirective::Never,
                        legal_actions: vec![LegalAction::CorrectRequest],
                    }
                }
                tracedecay_daemon_protocol::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
                }
                tracedecay_daemon_protocol::DaemonInvocationProblem::ResetRequired => {
                    ApplicationProblem::reset_required(SafeDiagnostic {
                        code: problem_code("reset_required"),
                        message: format!("The {family} store requires an explicit reset"),
                    })
                }
                tracedecay_daemon_protocol::DaemonInvocationProblem::ApplicationContractViolation => {
                    ApplicationProblem::unavailable(SafeDiagnostic {
                        code: problem_code("application_contract_violation"),
                        message: format!(
                            "The {family} application result violated its canonical contract"
                        ),
                    })
                }
                tracedecay_daemon_protocol::DaemonInvocationProblem::Unavailable => {
                    ApplicationProblem::unavailable(SafeDiagnostic {
                        code: problem_code("unavailable"),
                        message: format!("The {family} application runtime is unavailable"),
                    })
                }
            },
            outcome => match select_outcome(outcome) {
                Some((scope, outcome)) => {
                    return CanonicalInvocationResult::new(
                        binding_id,
                        Ok(ApplicationEnvelope {
                            contract: result_contract,
                            request_id,
                            scope,
                            outcome,
                        }),
                    )
                    .into_http_response();
                }
                None => ApplicationProblem::unavailable(SafeDiagnostic {
                    code: problem_code("protocol_unavailable"),
                    message: format!("The {family} application protocol is unavailable"),
                }),
            },
        },
        Err(problem) => problem,
    };
    let problem = match ApplicationProblemEnvelope::new(result_contract, request_id, problem) {
        Ok(problem) => problem.with_owning_layer(owning_layer),
        Err(error) => return application_contract_error_response(error),
    };
    CanonicalInvocationResult::<T>::new(binding_id, Err(problem)).into_http_response()
}

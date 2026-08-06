//! HTTP ownership for canonical multi-root scope-set operations.
//!
//! The API crate owns route decoding. This owner performs no scope discovery:
//! it forwards the typed request to the authenticated daemon invocation owner
//! and preserves the daemon's application envelope and problem taxonomy.

use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;
use tracedecay_api::{
    CanonicalInvocationResult, HttpApplicationControls, MultiRootApplicationOwner,
    MultiRootHttpOperation, MultiRootHttpRequest, MultiRootInvocationFuture,
};
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope,
    LegalAction, ProblemOwningLayer, RequestId, ResultContractRef, RetryDirective, SafeDiagnostic,
};
use tracedecay_tool_catalog::{BindingId, SchemaId};

use crate::daemon_client::{
    DaemonInvocationExecutor, InvocationCancellationPolicy, invocation_now_micros,
};
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
    DaemonInvocationResponse,
};

#[derive(Clone)]
pub(crate) struct MultiRootExecutorOwner {
    executor: Arc<dyn DaemonInvocationExecutor>,
}

impl MultiRootExecutorOwner {
    pub(crate) fn new(executor: Arc<dyn DaemonInvocationExecutor>) -> Self {
        Self { executor }
    }
}

impl MultiRootApplicationOwner for MultiRootExecutorOwner {
    fn invoke_multi_root(&self, request: MultiRootHttpRequest) -> MultiRootInvocationFuture {
        let executor = Arc::clone(&self.executor);
        Box::pin(async move { invoke_multi_root_http(executor, request).await })
    }
}

async fn invoke_multi_root_http(
    executor: Arc<dyn DaemonInvocationExecutor>,
    request: MultiRootHttpRequest,
) -> Response {
    let MultiRootHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;
    let observed_at = invocation_now_micros();
    match operation {
        MultiRootHttpOperation::ScopeSetRead => {
            let Ok(request) = serde_json::from_value(body) else {
                return invalid_request(request_id, operation);
            };
            let invocation = DaemonInvocationRequest::multi_root_scope_set_read(
                request_id.as_str(),
                request,
                observed_at,
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_typed(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                InvocationCancellationPolicy::ReadOnly,
                |outcome| match outcome {
                    DaemonInvocationOutcome::MultiRootScopeSetRead { scope, outcome } => {
                        Some((scope, outcome))
                    }
                    _ => None,
                },
            )
            .await
        }
        MultiRootHttpOperation::ScopeSetCompareAndSwap => {
            let Ok(request) = serde_json::from_value(body) else {
                return invalid_request(request_id, operation);
            };
            let invocation = DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
                request_id.as_str(),
                request,
                observed_at,
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_typed(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                InvocationCancellationPolicy::AuthoritativeEffect,
                |outcome| match outcome {
                    DaemonInvocationOutcome::MultiRootScopeSetCompareAndSwap { scope, outcome } => {
                        Some((scope, outcome))
                    }
                    _ => None,
                },
            )
            .await
        }
        MultiRootHttpOperation::Execute => {
            let Ok(request) = serde_json::from_value(body) else {
                return invalid_request(request_id, operation);
            };
            let invocation = DaemonInvocationRequest::multi_root_execute(
                request_id.as_str(),
                request,
                observed_at,
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_typed(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                InvocationCancellationPolicy::ReadOnly,
                |outcome| match outcome {
                    DaemonInvocationOutcome::MultiRootQueryPage { scope, outcome } => {
                        Some((scope, outcome))
                    }
                    _ => None,
                },
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn invoke_typed<T>(
    executor: Arc<dyn DaemonInvocationExecutor>,
    operation: MultiRootHttpOperation,
    request_id: RequestId,
    controls: HttpApplicationControls,
    invocation: DaemonInvocationRequest,
    cancellation_policy: InvocationCancellationPolicy,
    select: fn(
        DaemonInvocationOutcome,
    ) -> Option<(tracedecay_application::ResolvedScope, ApplicationOutcome<T>)>,
) -> Response
where
    T: Serialize,
{
    let binding_id = match binding_id(operation) {
        Ok(binding_id) => binding_id,
        Err(response) => return response,
    };
    let contract = match result_contract(operation) {
        Ok(contract) => contract,
        Err(response) => return response,
    };
    let response = executor
        .invoke_controlled(
            invocation,
            controls.deadline,
            controls.cancellation,
            cancellation_policy,
        )
        .await;
    let problem = match response {
        Ok(DaemonInvocationResponse { outcome, .. }) => match outcome {
            DaemonInvocationOutcome::ApplicationProblem { problem } => problem,
            DaemonInvocationOutcome::Problem { problem } => daemon_problem(problem),
            outcome => match select(outcome) {
                Some((scope, outcome)) => {
                    return CanonicalInvocationResult::new(
                        binding_id,
                        Ok(ApplicationEnvelope {
                            contract,
                            request_id,
                            scope,
                            outcome,
                        }),
                    )
                    .into_http_response();
                }
                None => unavailable("multi_root.protocol_unavailable"),
            },
        },
        Err(error) => error.into_application_problem(),
    };
    CanonicalInvocationResult::<T>::new(
        binding_id,
        Err(
            ApplicationProblemEnvelope::new(contract, request_id, problem)
                .with_owning_layer(ProblemOwningLayer::Runtime),
        ),
    )
    .into_http_response()
}

fn invalid_request(request_id: RequestId, operation: MultiRootHttpOperation) -> Response {
    let binding_id = match binding_id(operation) {
        Ok(binding_id) => binding_id,
        Err(response) => return response,
    };
    let contract = match result_contract(operation) {
        Ok(contract) => contract,
        Err(response) => return response,
    };
    let problem = ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "multi_root.invalid_request".to_owned(),
            message: "The multi-root application request is invalid".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    };
    CanonicalInvocationResult::<Value>::new(
        binding_id,
        Err(
            ApplicationProblemEnvelope::new(contract, request_id, problem)
                .with_owning_layer(ProblemOwningLayer::Adapter),
        ),
    )
    .into_http_response()
}

fn daemon_problem(problem: DaemonInvocationProblem) -> ApplicationProblem {
    match problem {
        DaemonInvocationProblem::InvalidRequest | DaemonInvocationProblem::UnsupportedRevision => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic {
                    code: "multi_root.invalid_request".to_owned(),
                    message: "The multi-root application request is invalid".to_owned(),
                },
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::CorrectRequest],
            }
        }
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        DaemonInvocationProblem::Unavailable => unavailable("multi_root.unavailable"),
    }
}

fn unavailable(code: &'static str) -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: code.to_owned(),
        message: "The multi-root application runtime is unavailable".to_owned(),
    })
}

fn binding_id(operation: MultiRootHttpOperation) -> Result<BindingId, Response> {
    BindingId::new(format!(
        "binding.http.{}.v1",
        operation.operation_id().trim_start_matches("operation.")
    ))
    .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response())
}

fn result_contract(operation: MultiRootHttpOperation) -> Result<ResultContractRef, Response> {
    let suffix = match operation {
        MultiRootHttpOperation::ScopeSetRead => "scope-set-read",
        MultiRootHttpOperation::ScopeSetCompareAndSwap => "scope-set-compare-and-swap",
        MultiRootHttpOperation::Execute => "execute",
    };
    let schema = SchemaId::new(format!("schema.tracedecay.multi-root.{suffix}-result.v1"))
        .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response())?;
    ResultContractRef::new(schema, 1)
        .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response())
}

//! HTTP ownership for the canonical multi-root scope-set operations.
//!
//! The API crate owns route decoding. This owner performs no scope discovery of
//! its own: it forwards the typed request to the authenticated daemon
//! invocation owner and lets the shared registered-HTTP path project the
//! application envelope and the problem taxonomy, exactly as Work, Workflow and
//! Handoff do. The catalog is the single statement of which routes exist, so
//! the mount refuses to come up unless the catalog advertises every multi-root
//! operation at the path this build answers on.

use std::sync::Arc;

use axum::response::Response;
use tracedecay_api::MultiRootHttpOperation;
use tracedecay_application::multi_root::{
    MultiRootApplicationOperation, multi_root_executable_binding_registry,
};
use tracedecay_application::{
    ApplicationProblem, AuthorizedScopeSet, LegalAction, MultiRootExecuteRequestV1,
    MultiRootQueryPageV1, MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasResultV1,
    MultiRootScopeSetReadRequestV1, RequestId, RetryDirective, SafeDiagnostic,
};
use tracedecay_tool_catalog::RouteExposureV1;

use super::{ApplicationSurfaceAdapterError, RegisteredHttpOperation, invoke_registered_http};
use crate::daemon_client::DaemonInvocationExecutor;
use crate::daemon_contract::{DaemonInvocationOutcome, DaemonInvocationRequest};

pub(super) fn router_with_executor(
    executor: Arc<dyn DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_catalog_bindings()?;
    Ok(tracedecay_api::multi_root_application_router(
        MultiRootExecutorOwner { executor },
    ))
}

/// Refuse to mount multi-root unless the catalog advertises every operation at
/// exactly the path this build answers on.
pub(super) fn validate_catalog_bindings() -> Result<(), ApplicationSurfaceAdapterError> {
    let registry = multi_root_executable_binding_registry()
        .map_err(ApplicationSurfaceAdapterError::CatalogValidation)?;
    for operation in MultiRootApplicationOperation::ALL {
        let operation_id =
            tracedecay_tool_catalog::OperationId::new(operation.operation_id().to_owned())
                .map_err(ApplicationSurfaceAdapterError::Identifier)?;
        let Some(binding) = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
        else {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        };
        let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        };
        if route_path != operation.application_route_path() {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        }
    }
    Ok(())
}

impl RegisteredHttpOperation for MultiRootHttpOperation {
    fn operation_id(self) -> String {
        MultiRootHttpOperation::operation_id(self).to_owned()
    }

    fn is_read_only(self) -> bool {
        match self {
            Self::ScopeSetRead | Self::Execute => true,
            Self::ScopeSetCompareAndSwap => false,
        }
    }

    fn problem_family(self) -> &'static str {
        "multi_root"
    }

    fn display_family(self) -> &'static str {
        "Multi-root"
    }

    fn registry(
        self,
    ) -> Result<tracedecay_tool_catalog::ExecutableBindingRegistryV1, ApplicationSurfaceAdapterError>
    {
        multi_root_executable_binding_registry()
            .map_err(ApplicationSurfaceAdapterError::CatalogValidation)
    }
}

#[derive(Clone)]
struct MultiRootExecutorOwner {
    executor: Arc<dyn DaemonInvocationExecutor>,
}

impl tracedecay_api::MultiRootApplicationOwner for MultiRootExecutorOwner {
    fn invoke_multi_root(
        &self,
        request: tracedecay_api::MultiRootHttpRequest,
    ) -> tracedecay_api::MultiRootInvocationFuture {
        Box::pin(invoke_operation(Arc::clone(&self.executor), request))
    }
}

async fn invoke_operation(
    executor: Arc<dyn DaemonInvocationExecutor>,
    request: tracedecay_api::MultiRootHttpRequest,
) -> Response {
    let tracedecay_api::MultiRootHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;
    let observed_at = crate::daemon_client::invocation_now_micros();
    match operation {
        MultiRootHttpOperation::ScopeSetRead => {
            let Ok(decoded) = serde_json::from_value::<MultiRootScopeSetReadRequestV1>(body) else {
                return invalid_request_response(request_id);
            };
            let invocation = DaemonInvocationRequest::multi_root_scope_set_read(
                request_id.as_str(),
                decoded,
                observed_at,
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<Option<AuthorizedScopeSet>, _>(
                executor.as_ref(),
                operation,
                request_id,
                controls,
                invocation,
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
            let Ok(decoded) = serde_json::from_value::<MultiRootScopeSetCasRequestV1>(body) else {
                return invalid_request_response(request_id);
            };
            let invocation = DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
                request_id.as_str(),
                decoded,
                observed_at,
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<MultiRootScopeSetCasResultV1, _>(
                executor.as_ref(),
                operation,
                request_id,
                controls,
                invocation,
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
            let Ok(decoded) = serde_json::from_value::<MultiRootExecuteRequestV1>(body) else {
                return invalid_request_response(request_id);
            };
            let invocation = DaemonInvocationRequest::multi_root_execute(
                request_id.as_str(),
                decoded,
                observed_at,
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<MultiRootQueryPageV1<serde_json::Value>, _>(
                executor.as_ref(),
                operation,
                request_id,
                controls,
                invocation,
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

fn invalid_request_response(request_id: RequestId) -> Response {
    let diagnostic = SafeDiagnostic {
        code: "multi_root.invalid_request".to_owned(),
        message: "The multi-root application request is invalid".to_owned(),
    };
    tracedecay_api::adapter_problem_response(
        request_id,
        ApplicationProblem::InvalidRequest {
            diagnostic,
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;
    use tracedecay_domain::ProjectId;

    use crate::application_surface::http_application_router_with_executor;
    use tracedecay_usecases::operation_stream::OperationEventAuthority;

    /// Records the daemon operation every mounted multi-root route reaches,
    /// then refuses it. The refusal is the point: it proves the HTTP path
    /// crossed into the controlled daemon invocation owner rather than being
    /// answered in-process or not being mounted at all.
    #[derive(Default)]
    struct RecordingMultiRootExecutor {
        operations: Mutex<Vec<crate::daemon_contract::DaemonInvocationOperation>>,
    }

    impl tracedecay_application::ApplicationInvocationExecutor for RecordingMultiRootExecutor {
        fn invoke(
            &self,
            _invocation: tracedecay_application::ApplicationInvocation,
        ) -> tracedecay_application::ApplicationInvocationFuture<
            '_,
            std::result::Result<
                tracedecay_application::ApplicationResponse,
                tracedecay_application::InvocationError,
            >,
        > {
            Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
        }
    }

    impl crate::daemon_client::DaemonInvocationExecutor for RecordingMultiRootExecutor {
        fn invoke_controlled(
            &self,
            request: crate::daemon_contract::DaemonInvocationRequest,
            _deadline: tracedecay_application::Deadline,
            _cancellation: tracedecay_application::CancellationSignal,
            _policy: crate::daemon_client::InvocationCancellationPolicy,
        ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
            '_,
            std::result::Result<
                crate::daemon_contract::DaemonInvocationResponse,
                crate::daemon_client::DaemonInvocationError,
            >,
        > {
            self.operations
                .lock()
                .expect("recorded daemon operations")
                .push(request.operation());
            Box::pin(async { Err(crate::daemon_client::DaemonInvocationError::Unavailable) })
        }

        fn observe_feedback(
            &self,
            _subject_digest: tracedecay_domain::ManifestDigest,
            _observed_at: tracedecay_domain::UtcMicros,
            _event: tracedecay_usecases::feedback::observations::FeedbackSourceEventV1,
        ) -> crate::daemon_client::DaemonInvocationExecutorFuture<'_, crate::errors::Result<()>>
        {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn catalog_advertises_every_mounted_multi_root_route() {
        super::validate_catalog_bindings().expect("multi-root catalog bindings");
    }

    #[tokio::test]
    async fn production_http_router_mounts_all_multi_root_operations() {
        let executor = Arc::new(RecordingMultiRootExecutor::default());
        let application_executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor> =
            executor.clone();
        let router = http_application_router_with_executor(
            application_executor,
            OperationEventAuthority::default(),
            ProjectId::new("project.http-multi-root").expect("project"),
        )
        .expect("HTTP application router");
        let requests = [
            (
                "/multi-root/scope-set/read",
                json!({"scope_set_id": "scope-set.http"}),
            ),
            (
                "/multi-root/scope-set/compare-and-swap",
                json!({
                    "scope_set_id": "scope-set.http",
                    "expected_revision": null,
                    "roots": [{"project_id": "project.http-root", "root": "/project/http-root"}]
                }),
            ),
            (
                "/multi-root/execute",
                json!({
                    "scope_set_id": "scope-set.http",
                    "scope_set_revision": 1,
                    "scope_set_digest": format!("sha256:{}", "a".repeat(64)),
                    "operation": {"kind": "query", "request": {}},
                    "page": 0,
                    "continuation": null
                }),
            ),
        ];

        for (path, body) in requests {
            let response = router
                .clone()
                .oneshot(
                    Request::post(path)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .expect("HTTP request"),
                )
                .await
                .expect("HTTP response");
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{path} must reach the controlled daemon invocation owner"
            );
        }

        assert_eq!(
            *executor
                .operations
                .lock()
                .expect("recorded daemon operations"),
            vec![
                crate::daemon_contract::DaemonInvocationOperation::MultiRootScopeSetRead,
                crate::daemon_contract::DaemonInvocationOperation::MultiRootScopeSetCompareAndSwap,
                crate::daemon_contract::DaemonInvocationOperation::MultiRootExecute,
            ]
        );
    }
}

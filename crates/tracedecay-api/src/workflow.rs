//! Canonical public HTTP adapter for daemon-owned Workflow execution.

use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde_json::Value;
use tracedecay_application::{
    ApplicationProblem, RequestId, RetryDirective, TaskHandoffGrantV1, TaskHandoffIssueRequestV1,
    TaskHandoffRedeemRequestV1, TaskHandoffRedeemedV1, WorkflowActivationV1,
    WorkflowDefinitionActivateRequestV1, WorkflowDefinitionRegisterRequestV1,
    WorkflowExecutionTruthV1, WorkflowFanOutRequestV1,
};
use tracedecay_domain::WorkflowDefinitionV1;

use crate::http::{
    HttpApplicationControls, MAX_HTTP_APPLICATION_BODY_BYTES, adapter_problem,
    application_problem_response, invalid_request_response,
};

fn schema_name<T: JsonSchema>() -> Cow<'static, str> {
    T::schema_name()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkflowOperation {
    RegisterDefinition,
    ActivateDefinition,
    ExecuteFanOut,
    HandoffIssue,
    HandoffRedeem,
}

impl WorkflowOperation {
    pub const ALL: [Self; 5] = [
        Self::RegisterDefinition,
        Self::ActivateDefinition,
        Self::ExecuteFanOut,
        Self::HandoffIssue,
        Self::HandoffRedeem,
    ];

    pub const fn operation_id_str(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "operation.workflow.register_definition",
            Self::ActivateDefinition => "operation.workflow.activate_definition",
            Self::ExecuteFanOut => "operation.workflow.execute_fan_out",
            Self::HandoffIssue => "operation.workflow.handoff_issue",
            Self::HandoffRedeem => "operation.workflow.handoff_redeem",
        }
    }

    pub const fn route_segment(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "register-definition",
            Self::ActivateDefinition => "activate-definition",
            Self::ExecuteFanOut => "execute-fan-out",
            Self::HandoffIssue => "handoff-issue",
            Self::HandoffRedeem => "handoff-redeem",
        }
    }

    pub const fn route_path(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "/workflow/register-definition",
            Self::ActivateDefinition => "/workflow/activate-definition",
            Self::ExecuteFanOut => "/workflow/execute-fan-out",
            Self::HandoffIssue => "/workflow/handoff-issue",
            Self::HandoffRedeem => "/workflow/handoff-redeem",
        }
    }

    pub const fn application_route_path(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "/application/workflow/register-definition",
            Self::ActivateDefinition => "/application/workflow/activate-definition",
            Self::ExecuteFanOut => "/application/workflow/execute-fan-out",
            Self::HandoffIssue => "/application/workflow/handoff-issue",
            Self::HandoffRedeem => "/application/workflow/handoff-redeem",
        }
    }

    pub fn request_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::RegisterDefinition => schema_name::<WorkflowDefinitionRegisterRequestV1>(),
            Self::ActivateDefinition => schema_name::<WorkflowDefinitionActivateRequestV1>(),
            Self::ExecuteFanOut => schema_name::<WorkflowFanOutRequestV1>(),
            Self::HandoffIssue => schema_name::<TaskHandoffIssueRequestV1>(),
            Self::HandoffRedeem => schema_name::<TaskHandoffRedeemRequestV1>(),
        }
    }

    pub fn result_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::RegisterDefinition => schema_name::<WorkflowDefinitionV1>(),
            Self::ActivateDefinition => schema_name::<WorkflowActivationV1>(),
            Self::ExecuteFanOut => schema_name::<WorkflowExecutionTruthV1>(),
            Self::HandoffIssue => schema_name::<TaskHandoffGrantV1>(),
            Self::HandoffRedeem => schema_name::<TaskHandoffRedeemedV1>(),
        }
    }

    fn parse(segment: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|operation| operation.route_segment() == segment)
    }
}

#[derive(Clone, Debug)]
pub struct WorkflowHttpRequest {
    pub operation: WorkflowOperation,
    pub request_id: RequestId,
    pub controls: HttpApplicationControls,
    pub body: Value,
}

pub type WorkflowInvocationFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

pub trait WorkflowApplicationOwner: Clone + Send + Sync + 'static {
    fn invoke_workflow(&self, request: WorkflowHttpRequest) -> WorkflowInvocationFuture;
}

impl<F, Fut> WorkflowApplicationOwner for F
where
    F: Fn(WorkflowHttpRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn invoke_workflow(&self, request: WorkflowHttpRequest) -> WorkflowInvocationFuture {
        Box::pin((self)(request))
    }
}

pub fn workflow_application_router<O>(owner: O) -> Router
where
    O: WorkflowApplicationOwner,
{
    Router::new()
        .route("/workflow/{operation}", post(operation::<O>))
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owner)
}

async fn operation<O>(
    Path(segment): Path<String>,
    State(owner): State<O>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: WorkflowApplicationOwner,
{
    let Some(operation) = WorkflowOperation::parse(&segment) else {
        return application_problem_response(adapter_problem(
            request_id,
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        ));
    };
    let Ok(Json(body)) = body else {
        return invalid_request_response(
            request_id,
            "workflow.invalid_body",
            "The Workflow request body is invalid or exceeds the configured limit",
        );
    };
    owner
        .invoke_workflow(WorkflowHttpRequest {
            operation,
            request_id,
            controls,
            body,
        })
        .await
}

pub fn workflow_invalid_request_response(request_id: RequestId) -> Response {
    invalid_request_response(
        request_id,
        "workflow.invalid_request",
        "The Workflow application request is invalid",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::extract::Extension;
    use axum::http::{Request, StatusCode};
    use axum::response::IntoResponse;
    use tower::ServiceExt;
    use tracedecay_application::{CancellationSignal, Deadline, RequestId};
    use tracedecay_domain::UtcMicros;

    use super::{HttpApplicationControls, WorkflowOperation, workflow_application_router};

    #[test]
    fn descriptor_exposes_every_daemon_owned_operation() {
        assert_eq!(WorkflowOperation::ALL.len(), 5);
        assert_eq!(
            WorkflowOperation::ALL
                .into_iter()
                .map(|operation| (
                    operation.operation_id_str(),
                    operation.application_route_path()
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "operation.workflow.register_definition",
                    "/application/workflow/register-definition",
                ),
                (
                    "operation.workflow.activate_definition",
                    "/application/workflow/activate-definition",
                ),
                (
                    "operation.workflow.execute_fan_out",
                    "/application/workflow/execute-fan-out",
                ),
                (
                    "operation.workflow.handoff_issue",
                    "/application/workflow/handoff-issue",
                ),
                (
                    "operation.workflow.handoff_redeem",
                    "/application/workflow/handoff-redeem",
                ),
            ]
        );
        for operation in WorkflowOperation::ALL {
            assert_eq!(
                operation.application_route_path(),
                format!("/application{}", operation.route_path())
            );
            assert_eq!(
                operation.route_segment(),
                operation
                    .route_path()
                    .rsplit('/')
                    .next()
                    .expect("workflow route has a terminal segment")
            );
        }
    }

    #[tokio::test]
    async fn router_dispatches_every_descriptor_to_the_application_owner() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let owner_observed = Arc::clone(&observed);
        let app = workflow_application_router(move |request: super::WorkflowHttpRequest| {
            let observed = Arc::clone(&owner_observed);
            async move {
                observed.lock().unwrap().push(request.operation);
                StatusCode::NO_CONTENT.into_response()
            }
        })
        .layer(Extension(
            RequestId::new("request.workflow.route-test").unwrap(),
        ))
        .layer(Extension(HttpApplicationControls {
            deadline: Deadline::new(UtcMicros(10_000)).unwrap(),
            cancellation: CancellationSignal::active("cancel.workflow.route-test").unwrap(),
        }));

        for operation in WorkflowOperation::ALL {
            let response = app
                .clone()
                .oneshot(
                    Request::post(operation.route_path())
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }

        assert_eq!(*observed.lock().unwrap(), WorkflowOperation::ALL);
    }
}

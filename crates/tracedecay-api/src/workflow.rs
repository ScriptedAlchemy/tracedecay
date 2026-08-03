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
    ApplicationProblem, RequestId, RetryDirective, TaskHandoffGrant, TaskHandoffIssueRequest,
    TaskHandoffRedeemRequest, TaskHandoffRedeemed, WorkflowActivation,
    WorkflowDefinitionActivateRequest, WorkflowDefinitionRegisterRequest, WorkflowFanOutRequest,
};
use tracedecay_domain::{WorkflowDefinition, WorkflowRunProjection};

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
            Self::RegisterDefinition => schema_name::<WorkflowDefinitionRegisterRequest>(),
            Self::ActivateDefinition => schema_name::<WorkflowDefinitionActivateRequest>(),
            Self::ExecuteFanOut => schema_name::<WorkflowFanOutRequest>(),
            Self::HandoffIssue => schema_name::<TaskHandoffIssueRequest>(),
            Self::HandoffRedeem => schema_name::<TaskHandoffRedeemRequest>(),
        }
    }

    pub fn result_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::RegisterDefinition => schema_name::<WorkflowDefinition>(),
            Self::ActivateDefinition => schema_name::<WorkflowActivation>(),
            Self::ExecuteFanOut => schema_name::<WorkflowRunProjection>(),
            Self::HandoffIssue => schema_name::<TaskHandoffGrant>(),
            Self::HandoffRedeem => schema_name::<TaskHandoffRedeemed>(),
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
    fn descriptor_derives_route_and_catalog_identity() {
        for operation in WorkflowOperation::ALL {
            assert_eq!(
                operation.application_route_path(),
                format!("/application{}", operation.route_path())
            );
            assert!(
                operation
                    .operation_id_str()
                    .starts_with("operation.workflow.")
            );
        }
    }
}

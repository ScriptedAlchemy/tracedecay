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
    ApplicationProblem, RequestId, RetryDirective, WorkflowExecutionTruthV1,
    WorkflowFanOutRequestV1,
};

use crate::http::{
    HttpApplicationControls, MAX_HTTP_APPLICATION_BODY_BYTES, adapter_problem,
    application_problem_response, invalid_request_response,
};

fn schema_name<T: JsonSchema>() -> Cow<'static, str> {
    T::schema_name()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WorkflowOperation {
    ExecuteFanOut,
}

impl WorkflowOperation {
    pub const ALL: [Self; 1] = [Self::ExecuteFanOut];

    pub const fn operation_id_str(self) -> &'static str {
        match self {
            Self::ExecuteFanOut => "operation.workflow.execute_fan_out",
        }
    }

    pub const fn route_segment(self) -> &'static str {
        match self {
            Self::ExecuteFanOut => "execute-fan-out",
        }
    }

    pub const fn route_path(self) -> &'static str {
        match self {
            Self::ExecuteFanOut => "/workflow/execute-fan-out",
        }
    }

    pub const fn application_route_path(self) -> &'static str {
        match self {
            Self::ExecuteFanOut => "/application/workflow/execute-fan-out",
        }
    }

    pub fn request_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::ExecuteFanOut => schema_name::<WorkflowFanOutRequestV1>(),
        }
    }

    pub fn result_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::ExecuteFanOut => schema_name::<WorkflowExecutionTruthV1>(),
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
    use super::WorkflowOperation;

    #[test]
    fn descriptor_derives_route_and_catalog_identity() {
        let operation = WorkflowOperation::ExecuteFanOut;
        assert_eq!(
            operation.application_route_path(),
            format!("/application{}", operation.route_path())
        );
        assert_eq!(
            operation.operation_id_str(),
            "operation.workflow.execute_fan_out"
        );
        assert_eq!(operation.route_segment(), "execute-fan-out");
    }
}

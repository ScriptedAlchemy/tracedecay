//! Canonical public HTTP adapter for daemon-owned handoff opens.

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
    ApplicationProblem, IssueTaskHandoffRequestV1, IssueTaskHandoffResultV1,
    OpenInvestigationHandoffRequestV1, OpenInvestigationHandoffResultV1, OpenTaskHandoffRequestV1,
    OpenTaskHandoffResultV1, RequestId, RetryDirective,
};

use crate::http::{
    HttpApplicationControls, MAX_HTTP_APPLICATION_BODY_BYTES, adapter_problem_response,
    invalid_request_response,
};

fn schema_name<T: JsonSchema>() -> Cow<'static, str> {
    T::schema_name()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HandoffOperation {
    IssueTaskHandoff,
    OpenInvestigationHandoff,
    OpenTaskHandoff,
}

impl HandoffOperation {
    pub const ALL: [Self; 3] = [
        Self::IssueTaskHandoff,
        Self::OpenInvestigationHandoff,
        Self::OpenTaskHandoff,
    ];

    pub const fn operation_id_str(self) -> &'static str {
        match self {
            Self::IssueTaskHandoff => "operation.handoff.issue_task_handoff",
            Self::OpenInvestigationHandoff => "operation.handoff.open_investigation_handoff",
            Self::OpenTaskHandoff => "operation.handoff.open_task_handoff",
        }
    }

    pub const fn route_segment(self) -> &'static str {
        match self {
            Self::IssueTaskHandoff => "issue-task",
            Self::OpenInvestigationHandoff => "open-investigation",
            Self::OpenTaskHandoff => "open-task",
        }
    }

    pub const fn route_path(self) -> &'static str {
        match self {
            Self::IssueTaskHandoff => "/handoff/issue-task",
            Self::OpenInvestigationHandoff => "/handoff/open-investigation",
            Self::OpenTaskHandoff => "/handoff/open-task",
        }
    }

    pub const fn application_route_path(self) -> &'static str {
        match self {
            Self::IssueTaskHandoff => "/application/handoff/issue-task",
            Self::OpenInvestigationHandoff => "/application/handoff/open-investigation",
            Self::OpenTaskHandoff => "/application/handoff/open-task",
        }
    }

    pub fn request_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::IssueTaskHandoff => schema_name::<IssueTaskHandoffRequestV1>(),
            Self::OpenInvestigationHandoff => schema_name::<OpenInvestigationHandoffRequestV1>(),
            Self::OpenTaskHandoff => schema_name::<OpenTaskHandoffRequestV1>(),
        }
    }

    pub fn result_schema_name(self) -> Cow<'static, str> {
        match self {
            Self::IssueTaskHandoff => schema_name::<IssueTaskHandoffResultV1>(),
            Self::OpenInvestigationHandoff => schema_name::<OpenInvestigationHandoffResultV1>(),
            Self::OpenTaskHandoff => schema_name::<OpenTaskHandoffResultV1>(),
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
pub struct HandoffHttpRequest {
    pub operation: HandoffOperation,
    pub request_id: RequestId,
    pub controls: HttpApplicationControls,
    pub body: Value,
}

pub type HandoffInvocationFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

pub trait HandoffApplicationOwner: Clone + Send + Sync + 'static {
    fn invoke_handoff(&self, request: HandoffHttpRequest) -> HandoffInvocationFuture;
}

impl<F, Fut> HandoffApplicationOwner for F
where
    F: Fn(HandoffHttpRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn invoke_handoff(&self, request: HandoffHttpRequest) -> HandoffInvocationFuture {
        Box::pin((self)(request))
    }
}

pub fn handoff_application_router<O>(owner: O) -> Router
where
    O: HandoffApplicationOwner,
{
    Router::new()
        .route("/handoff/{operation}", post(operation::<O>))
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
    O: HandoffApplicationOwner,
{
    let Some(operation) = HandoffOperation::parse(&segment) else {
        return adapter_problem_response(
            request_id,
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        );
    };
    let Ok(Json(body)) = body else {
        return invalid_request_response(
            request_id,
            "handoff.invalid_body",
            "The handoff-open request body is invalid or exceeds the configured limit",
        );
    };
    owner
        .invoke_handoff(HandoffHttpRequest {
            operation,
            request_id,
            controls,
            body,
        })
        .await
}

pub fn handoff_invalid_request_response(request_id: RequestId) -> Response {
    invalid_request_response(
        request_id,
        "handoff.invalid_request",
        "The handoff-open application request is invalid",
    )
}

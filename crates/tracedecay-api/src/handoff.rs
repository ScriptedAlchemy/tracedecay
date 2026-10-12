//! Canonical public HTTP adapter for daemon-owned handoff opens.

use std::future::Future;
use std::pin::Pin;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;
use tracedecay_contracts::RequestId;

use crate::http::{
    HttpApplicationControls, MAX_HTTP_APPLICATION_BODY_BYTES, dispatch_json_operation,
    invalid_request_response,
};

mounted_operations! {
    pub enum HandoffOperation;
    id_prefix = "operation.handoff.",
    route_prefix = "handoff",
    IssueTaskHandoff: "issue_task_handoff", "issue-task";
    ListTaskHandoffs: "list_task_handoffs", "list-task";
    OpenInvestigationHandoff: "open_investigation_handoff", "open-investigation";
    OpenTaskHandoff: "open_task_handoff", "open-task";
}

impl HandoffOperation {
    /// True for operations that only read the grant store.
    ///
    /// The three token operations issue or consume a grant. Enumeration only
    /// looks, and the surface must know that so it does not treat a read as a
    /// mutation for retry and replay purposes.
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ListTaskHandoffs)
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
    dispatch_json_operation(
        HandoffOperation::parse(&segment),
        request_id,
        body,
        (
            "handoff.invalid_body",
            "The handoff-open request body is invalid or exceeds the configured limit",
        ),
        |operation, request_id, body| HandoffHttpRequest {
            operation,
            request_id,
            controls,
            body,
        },
        |request| owner.invoke_handoff(request),
    )
    .await
}

pub fn handoff_invalid_request_response(request_id: RequestId) -> Response {
    invalid_request_response(
        request_id,
        "handoff.invalid_request",
        "The handoff-open application request is invalid",
    )
}

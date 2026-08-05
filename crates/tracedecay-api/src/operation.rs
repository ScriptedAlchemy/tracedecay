//! Typed HTTP routes for subscribing to and explicitly cancelling operations.
//!
//! The owning application/runtime port supplies authorization, retention,
//! replay, gaps, and cancellation semantics. This adapter only validates
//! bounded transport inputs and frames canonical application results.

use std::future::Future;
use std::pin::Pin;

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Extension, Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::Value;
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope, ApplicationProblemKind,
    OperationCancelOutcome, OperationEventSubscription, RequestContext, RequestId, ResumeToken,
    RetryDirective, SafeDiagnostic, StreamEvent,
};

use crate::http::{adapter_problem, application_problem_response, invalid_request_response};
use crate::{CanonicalInvocationResult, HttpApplicationControls, sse_response};

/// Default maximum number of retained events requested when opening a stream.
pub const DEFAULT_OPERATION_EVENT_PAGE_SIZE: u16 = 256;
/// Largest retained-event page an HTTP client may request.
pub const MAX_OPERATION_EVENT_PAGE_SIZE: u16 = 256;
const MAX_OPERATION_EVENT_SEQUENCE: u64 = i64::MAX as u64;
const MAX_OPERATION_CANCEL_BODY_BYTES: usize = 1_024;

/// Exact authenticated input forwarded when an operation stream is opened.
#[derive(Clone, Debug)]
pub struct OperationEventRequest {
    pub context: RequestContext,
    pub controls: HttpApplicationControls,
    pub operation_id: RequestId,
    pub next_sequence: u64,
    pub resume_token: Option<ResumeToken>,
    pub max_events: u16,
}

/// Exact authenticated input forwarded for explicit operation cancellation.
#[derive(Clone, Debug)]
pub struct OperationCancelRequest {
    pub context: RequestContext,
    pub controls: HttpApplicationControls,
    pub operation_id: RequestId,
}

const fn operation_cancel_http_status(outcome: OperationCancelOutcome) -> axum::http::StatusCode {
    match outcome {
        OperationCancelOutcome::Requested => axum::http::StatusCode::ACCEPTED,
        OperationCancelOutcome::AlreadyRequested | OperationCancelOutcome::AlreadyTerminal => {
            axum::http::StatusCode::OK
        }
    }
}

/// Dynamically dispatched canonical event stream owned outside this adapter.
pub type OperationEventStream = Pin<Box<dyn Stream<Item = StreamEvent<Value>> + Send + 'static>>;

/// Owner future for an authenticated operation-event subscription.
pub type OperationEventFuture = Pin<
    Box<
        dyn Future<
                Output = Result<
                    OperationEventSubscription<OperationEventStream>,
                    ApplicationProblemEnvelope,
                >,
            > + Send
            + 'static,
    >,
>;
/// Owner future for an explicit typed cancellation result.
pub type OperationCancelFuture = Pin<
    Box<dyn Future<Output = CanonicalInvocationResult<OperationCancelOutcome>> + Send + 'static>,
>;

/// Application/runtime authority adapted by the operation HTTP routes.
pub trait OperationEventOwner: Clone + Send + Sync + 'static {
    fn operation_events(&self, request: OperationEventRequest) -> OperationEventFuture;

    fn cancel_operation(&self, request: OperationCancelRequest) -> OperationCancelFuture;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationEventQuery {
    #[serde(default)]
    next_sequence: u64,
    #[serde(default)]
    resume_token: Option<ResumeToken>,
    #[serde(default = "default_operation_event_page_size")]
    max_events: u16,
}

const fn default_operation_event_page_size() -> u16 {
    DEFAULT_OPERATION_EVENT_PAGE_SIZE
}

/// Build the operation-event and explicit-cancellation router.
pub fn operation_event_router<O>(owner: O) -> Router
where
    O: OperationEventOwner,
{
    Router::new()
        .route(
            "/operations/{operation_id}/events",
            get(operation_events::<O>),
        )
        .route(
            "/operations/{operation_id}/cancel",
            post(cancel_operation::<O>),
        )
        .layer(DefaultBodyLimit::max(MAX_OPERATION_CANCEL_BODY_BYTES))
        .with_state(owner)
}

async fn operation_events<O>(
    State(owner): State<O>,
    Path(operation_id): Path<String>,
    Extension(context): Extension<RequestContext>,
    Extension(controls): Extension<HttpApplicationControls>,
    query: Result<Query<OperationEventQuery>, QueryRejection>,
) -> Response
where
    O: OperationEventOwner,
{
    let request_id = context.request_id().clone();
    let operation_id = match RequestId::new(operation_id) {
        Ok(operation_id) => operation_id,
        Err(_) => return concealed_operation_response(request_id),
    };
    let Query(query) = match query {
        Ok(query) => query,
        Err(_) => {
            return invalid_request_response(
                request_id,
                "operation_event.invalid_query",
                "The operation-event query is invalid",
            );
        }
    };
    if query.max_events == 0
        || query.max_events > MAX_OPERATION_EVENT_PAGE_SIZE
        || query.next_sequence > MAX_OPERATION_EVENT_SEQUENCE
    {
        return invalid_request_response(
            request_id,
            "operation_event.invalid_page",
            "The operation-event page is outside the supported bounds",
        );
    }

    match owner
        .operation_events(OperationEventRequest {
            context,
            controls,
            operation_id,
            next_sequence: query.next_sequence,
            resume_token: query.resume_token,
            max_events: query.max_events,
        })
        .await
    {
        Ok(subscription) => {
            let (correlation_id, frontier, events) = subscription.into_parts();
            sse_response(correlation_id, frontier, events).into_response()
        }
        Err(problem)
            if problem.problem.kind() == ApplicationProblemKind::NotFoundOrNotAuthorized =>
        {
            concealed_operation_response(request_id)
        }
        Err(problem) => application_problem_response(problem),
    }
}

async fn cancel_operation<O>(
    State(owner): State<O>,
    Path(operation_id): Path<String>,
    Extension(context): Extension<RequestContext>,
    Extension(controls): Extension<HttpApplicationControls>,
    body: Result<Bytes, BytesRejection>,
) -> Response
where
    O: OperationEventOwner,
{
    let request_id = context.request_id().clone();
    let operation_id = match RequestId::new(operation_id) {
        Ok(operation_id) => operation_id,
        Err(_) => return concealed_operation_response(request_id),
    };
    if !matches!(body, Ok(ref body) if body.is_empty()) {
        return invalid_request_response(
            request_id,
            "operation_cancel.invalid_body",
            "The operation cancellation request body must be empty",
        );
    }
    let result = owner
        .cancel_operation(OperationCancelRequest {
            context,
            controls,
            operation_id,
        })
        .await;
    if matches!(
        &result.result,
        Err(problem)
            if problem.problem.kind() == ApplicationProblemKind::NotFoundOrNotAuthorized
    ) {
        return concealed_operation_response(request_id);
    }
    let status = match &result.result {
        Ok(application) => match &application.outcome {
            ApplicationOutcome::Evidence(packet) => packet.payload.as_ref(),
            ApplicationOutcome::Preview(preview) => preview.payload.as_ref(),
            ApplicationOutcome::Effect(effect) => effect.payload.as_ref(),
        }
        .copied()
        .map(operation_cancel_http_status),
        Err(_) => None,
    };
    match status {
        Some(status) => (status, Json(result.into_http_json())).into_response(),
        None if result.result.is_err() => result.into_http_response(),
        None => invalid_cancellation_outcome_response(request_id),
    }
}

fn concealed_operation_response(request_id: RequestId) -> Response {
    application_problem_response(adapter_problem(
        request_id,
        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
    ))
}

fn invalid_cancellation_outcome_response(request_id: RequestId) -> Response {
    application_problem_response(adapter_problem(
        request_id,
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "operation_cancel.invalid_outcome".to_owned(),
            message: "The operation cancellation authority returned no typed outcome".to_owned(),
        }),
    ))
}

#[cfg(test)]
#[path = "operation_tests.rs"]
mod tests;

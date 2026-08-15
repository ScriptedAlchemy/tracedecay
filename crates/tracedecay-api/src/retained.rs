//! Canonical public HTTP adapter for retained application operations.

use std::future::Future;
use std::pin::Pin;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;
use tracedecay_application::RequestId;
use tracedecay_application::retained_surfaces::RetainedSurfaceOperation;

use crate::http::{
    HttpApplicationControls, MAX_HTTP_APPLICATION_BODY_BYTES, invalid_request_response,
};

pub fn retained_operation_id(operation: RetainedSurfaceOperation) -> String {
    format!("operation.application.{}", operation.as_str())
}

pub fn retained_route_path(operation: RetainedSurfaceOperation) -> String {
    format!("/retained/{}", operation.as_str())
}

pub fn retained_application_route_path(operation: RetainedSurfaceOperation) -> String {
    format!("/application{}", retained_route_path(operation))
}

#[derive(Clone, Debug)]
pub struct RetainedHttpRequest {
    pub operation: RetainedSurfaceOperation,
    pub request_id: RequestId,
    pub controls: HttpApplicationControls,
    pub body: Value,
}

pub type RetainedInvocationFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

pub trait RetainedApplicationOwner: Clone + Send + Sync + 'static {
    fn invoke_retained(&self, request: RetainedHttpRequest) -> RetainedInvocationFuture;
}

impl<F, Fut> RetainedApplicationOwner for F
where
    F: Fn(RetainedHttpRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn invoke_retained(&self, request: RetainedHttpRequest) -> RetainedInvocationFuture {
        Box::pin((self)(request))
    }
}

/// Registers one explicit `POST` route per callable retained operation, so
/// the routing table itself is the per-binding mount authority: a callable
/// operation's path answers method-mismatch (`405`) probes, and an unknown or
/// non-callable segment answers the router's own `404` instead of a handler's
/// concealed problem envelope.
pub fn retained_application_router<O>(owner: O) -> Router
where
    O: RetainedApplicationOwner,
{
    let mut router = Router::new();
    for operation in RetainedSurfaceOperation::CALLABLE {
        router = router.route(
            &retained_route_path(operation),
            post(
                move |State(owner): State<O>,
                      Extension(request_id): Extension<RequestId>,
                      Extension(controls): Extension<HttpApplicationControls>,
                      body: Result<Json<Value>, JsonRejection>| {
                    invoke(operation, owner, request_id, controls, body)
                },
            ),
        );
    }
    router
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owner)
}

async fn invoke<O>(
    operation: RetainedSurfaceOperation,
    owner: O,
    request_id: RequestId,
    controls: HttpApplicationControls,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: RetainedApplicationOwner,
{
    let Ok(Json(body)) = body else {
        return invalid_request_response(
            request_id,
            "retained.invalid_body",
            "The retained application request body is invalid or exceeds the configured limit",
        );
    };
    owner
        .invoke_retained(RetainedHttpRequest {
            operation,
            request_id,
            controls,
            body,
        })
        .await
}

pub fn retained_invalid_request_response(request_id: RequestId) -> Response {
    invalid_request_response(
        request_id,
        "retained.invalid_request",
        "The retained application request is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callable_operations_have_canonical_route_and_operation_identity() {
        for operation in RetainedSurfaceOperation::CALLABLE {
            assert_eq!(
                retained_operation_id(operation),
                format!("operation.application.{}", operation.as_str())
            );
            assert_eq!(
                retained_application_route_path(operation),
                format!("/application/retained/{}", operation.as_str())
            );
        }
    }

    #[test]
    fn broad_translator_names_are_not_callable_routes() {
        assert!(!RetainedSurfaceOperation::SessionRefresh.is_callable());
    }
}

//! Canonical multi-root HTTP routes.

use std::future::Future;
use std::pin::Pin;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;
use tracedecay_application::RequestId;

use crate::http::{
    HttpApplicationControls, MAX_HTTP_APPLICATION_BODY_BYTES, invalid_request_response,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MultiRootHttpOperation {
    ScopeSetRead,
    ScopeSetCompareAndSwap,
    Execute,
}

impl MultiRootHttpOperation {
    pub const ALL: [Self; 3] = [
        Self::ScopeSetRead,
        Self::ScopeSetCompareAndSwap,
        Self::Execute,
    ];

    pub const fn operation_id(self) -> &'static str {
        match self {
            Self::ScopeSetRead => "operation.multi_root.scope_set_read",
            Self::ScopeSetCompareAndSwap => "operation.multi_root.scope_set_compare_and_swap",
            Self::Execute => "operation.multi_root.execute",
        }
    }

    pub const fn application_route_path(self) -> &'static str {
        match self {
            Self::ScopeSetRead => "/application/multi-root/scope-set/read",
            Self::ScopeSetCompareAndSwap => "/application/multi-root/scope-set/compare-and-swap",
            Self::Execute => "/application/multi-root/execute",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MultiRootHttpRequest {
    pub operation: MultiRootHttpOperation,
    pub request_id: RequestId,
    pub controls: HttpApplicationControls,
    pub body: Value,
}

pub type MultiRootInvocationFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

pub trait MultiRootApplicationOwner: Clone + Send + Sync + 'static {
    fn invoke_multi_root(&self, request: MultiRootHttpRequest) -> MultiRootInvocationFuture;
}

impl<F, Fut> MultiRootApplicationOwner for F
where
    F: Fn(MultiRootHttpRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn invoke_multi_root(&self, request: MultiRootHttpRequest) -> MultiRootInvocationFuture {
        Box::pin((self)(request))
    }
}

pub fn multi_root_application_router<O>(owner: O) -> Router
where
    O: MultiRootApplicationOwner,
{
    Router::new()
        .route("/multi-root/scope-set/read", post(scope_set_read::<O>))
        .route(
            "/multi-root/scope-set/compare-and-swap",
            post(scope_set_compare_and_swap::<O>),
        )
        .route("/multi-root/execute", post(execute::<O>))
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owner)
}

async fn scope_set_read<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    controls: Extension<HttpApplicationControls>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: MultiRootApplicationOwner,
{
    dispatch(
        MultiRootHttpOperation::ScopeSetRead,
        state,
        request_id,
        controls,
        body,
    )
    .await
}

async fn scope_set_compare_and_swap<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    controls: Extension<HttpApplicationControls>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: MultiRootApplicationOwner,
{
    dispatch(
        MultiRootHttpOperation::ScopeSetCompareAndSwap,
        state,
        request_id,
        controls,
        body,
    )
    .await
}

async fn execute<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    controls: Extension<HttpApplicationControls>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: MultiRootApplicationOwner,
{
    dispatch(
        MultiRootHttpOperation::Execute,
        state,
        request_id,
        controls,
        body,
    )
    .await
}

async fn dispatch<O>(
    operation: MultiRootHttpOperation,
    State(owner): State<O>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: MultiRootApplicationOwner,
{
    let Ok(Json(body)) = body else {
        return invalid_request_response(
            request_id,
            "multi_root.invalid_body",
            "The multi-root request body is invalid or exceeds the configured limit",
        );
    };
    owner
        .invoke_multi_root(MultiRootHttpRequest {
            operation,
            request_id,
            controls,
            body,
        })
        .await
}

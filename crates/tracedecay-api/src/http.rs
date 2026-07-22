use std::future::Future;
use std::pin::Pin;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use tracedecay_application::{
    ApplicationProblem, ApplicationProblemEnvelope, ApplicationProblemKind, CancellationSignal,
    Deadline, PageRequest, ProblemOwningLayer, RequestId, ResultContractRef, RetryDirective,
    SafeDiagnostic,
};
use tracedecay_tool_catalog::SchemaId;

use crate::{CanonicalInvocationResult, HttpJsonEnvelope, HttpProblemEnvelope};

const MAX_HTTP_APPLICATION_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HttpApplicationOperation {
    GitPreview,
    GitApply,
    FeedbackDiagnostics,
    FeedbackGet,
    FeedbackExpand,
    FeedbackList,
    FeedbackImpact,
    AffectedTests,
    TestResults,
    SessionLookup,
    QualifiedName,
    CallChain,
    FileDependents,
    SourceLines,
    SourceBody,
    SourceOutline,
    ModuleApi,
    FileMetadata,
    HealthRead,
    StorageStatus,
    DiagnosticsRead,
}

/// The canonical application owner family responsible for one HTTP binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HttpApplicationOwnerKind {
    Git,
    Feedback,
    Primitive,
}

impl HttpApplicationOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitPreview => "git_preview",
            Self::GitApply => "git_apply",
            Self::FeedbackDiagnostics => "feedback_diagnostics",
            Self::FeedbackGet => "feedback_get",
            Self::FeedbackExpand => "feedback_expand",
            Self::FeedbackList => "feedback_list",
            Self::FeedbackImpact => "feedback_impact",
            Self::AffectedTests => "affected_tests",
            Self::TestResults => "test_results",
            Self::SessionLookup => "session_lookup",
            Self::QualifiedName => "qualified_name",
            Self::CallChain => "call_chain",
            Self::FileDependents => "file_dependents",
            Self::SourceLines => "source_lines",
            Self::SourceBody => "source_body",
            Self::SourceOutline => "source_outline",
            Self::ModuleApi => "module_api",
            Self::FileMetadata => "file_metadata",
            Self::HealthRead => "health_read",
            Self::StorageStatus => "storage_status",
            Self::DiagnosticsRead => "diagnostics_read",
        }
    }

    pub const fn owner_kind(self) -> HttpApplicationOwnerKind {
        match self {
            Self::GitPreview | Self::GitApply => HttpApplicationOwnerKind::Git,
            Self::FeedbackDiagnostics
            | Self::FeedbackGet
            | Self::FeedbackExpand
            | Self::FeedbackList
            | Self::FeedbackImpact => HttpApplicationOwnerKind::Feedback,
            Self::AffectedTests
            | Self::TestResults
            | Self::SessionLookup
            | Self::QualifiedName
            | Self::CallChain
            | Self::FileDependents
            | Self::SourceLines
            | Self::SourceBody
            | Self::SourceOutline
            | Self::ModuleApi
            | Self::FileMetadata
            | Self::HealthRead
            | Self::StorageStatus
            | Self::DiagnosticsRead => HttpApplicationOwnerKind::Primitive,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HttpApplicationControls {
    pub deadline: Deadline,
    pub cancellation: CancellationSignal,
}

#[derive(Clone, Debug)]
pub struct HttpApplicationRequest {
    pub operation: HttpApplicationOperation,
    pub request_id: RequestId,
    pub page: PageRequest,
    pub deadline: Option<Deadline>,
    pub cancellation: CancellationSignal,
    pub body: Value,
}

pub type HttpApplicationInvocationFuture =
    Pin<Box<dyn Future<Output = CanonicalInvocationResult<Value>> + Send + 'static>>;

/// Concrete application owners mounted behind the HTTP adapter.
///
/// Each method delegates to the corresponding application owner family. The
/// adapter performs only extraction, owner selection, and canonical encoding.
pub trait HttpApplicationOwners: Clone + Send + Sync + 'static {
    fn invoke_git(&self, request: HttpApplicationRequest) -> HttpApplicationInvocationFuture;

    fn invoke_feedback(&self, request: HttpApplicationRequest) -> HttpApplicationInvocationFuture;

    fn invoke_primitive(&self, request: HttpApplicationRequest) -> HttpApplicationInvocationFuture;
}

impl<F, Fut> HttpApplicationOwners for F
where
    F: Fn(HttpApplicationRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = CanonicalInvocationResult<Value>> + Send + 'static,
{
    fn invoke_git(&self, request: HttpApplicationRequest) -> HttpApplicationInvocationFuture {
        Box::pin((self)(request))
    }

    fn invoke_feedback(&self, request: HttpApplicationRequest) -> HttpApplicationInvocationFuture {
        Box::pin((self)(request))
    }

    fn invoke_primitive(&self, request: HttpApplicationRequest) -> HttpApplicationInvocationFuture {
        Box::pin((self)(request))
    }
}

fn application_problem_status(kind: ApplicationProblemKind) -> StatusCode {
    match kind {
        ApplicationProblemKind::InvalidRequest => StatusCode::BAD_REQUEST,
        ApplicationProblemKind::NotFoundOrNotAuthorized => StatusCode::NOT_FOUND,
        ApplicationProblemKind::Conflict | ApplicationProblemKind::Stale => StatusCode::CONFLICT,
        ApplicationProblemKind::Unsupported => StatusCode::UNPROCESSABLE_ENTITY,
        ApplicationProblemKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ApplicationProblemKind::Saturated => StatusCode::TOO_MANY_REQUESTS,
        ApplicationProblemKind::Cancelled => StatusCode::REQUEST_TIMEOUT,
        ApplicationProblemKind::TimedOut => StatusCode::GATEWAY_TIMEOUT,
    }
}

impl<T> CanonicalInvocationResult<T> {
    fn http_status(&self) -> StatusCode {
        match &self.result {
            Ok(_) => StatusCode::OK,
            Err(problem) => application_problem_status(problem.problem.kind()),
        }
    }
}

impl<T> CanonicalInvocationResult<T>
where
    T: Serialize,
{
    fn into_http_response(self) -> Response {
        let status = self.http_status();
        (status, Json(self.into_http_json())).into_response()
    }
}

/// Encode a canonical problem for HTTP routes that do not have a catalog
/// binding, such as operation-event subscription and cancellation.
pub fn application_problem_response(application: ApplicationProblemEnvelope) -> Response {
    let status = application_problem_status(application.problem.kind());
    (
        status,
        Json(HttpJsonEnvelope::<Value>::Problem(Box::new(
            HttpProblemEnvelope {
                binding_id: None,
                application,
            },
        ))),
    )
        .into_response()
}

pub(crate) fn invalid_request_problem(
    request_id: RequestId,
    code: &'static str,
    message: &'static str,
) -> ApplicationProblemEnvelope {
    let diagnostic =
        SafeDiagnostic::new(code, message).expect("HTTP adapter diagnostics are static");
    adapter_problem(
        request_id,
        ApplicationProblem::InvalidRequest {
            diagnostic,
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
    )
}

fn adapter_problem(
    request_id: RequestId,
    problem: ApplicationProblem,
) -> ApplicationProblemEnvelope {
    let contract = ResultContractRef::new(
        SchemaId::new("schema.tracedecay.http.adapter-problem.v1")
            .expect("the HTTP adapter problem schema id is static"),
        1,
    )
    .expect("the HTTP adapter problem contract is static");
    ApplicationProblemEnvelope::new(contract, request_id, problem)
        .with_owning_layer(ProblemOwningLayer::Adapter)
}

fn invalid_request_response(
    request_id: RequestId,
    code: &'static str,
    message: &'static str,
) -> Response {
    application_problem_response(invalid_request_problem(request_id, code, message))
}

/// Build the shipped application routes at relative paths.
///
/// The executable nests this router at its root-owned prefix behind
/// authentication and origin middleware. Authorization remains part of
/// canonical application dispatch, including concealed
/// not-found-or-not-authorized results. These PR12 route names are adapter
/// bindings, not a frozen SDK namespace.
pub fn application_router<O>(owners: O) -> Router
where
    O: HttpApplicationOwners,
{
    Router::new()
        .route("/git/preview", post(git_preview::<O>))
        .route("/git/apply", post(git_apply::<O>))
        .route("/feedback/diagnostics", post(feedback_diagnostics::<O>))
        .route("/feedback/get", post(feedback_get::<O>))
        .route("/feedback/expand", post(feedback_expand::<O>))
        .route("/feedback/list", post(feedback_list::<O>))
        .route("/feedback/impact", post(feedback_impact::<O>))
        .route("/tests/affected", post(affected_tests::<O>))
        .route("/tests/results", post(test_results::<O>))
        .route("/primitives/{operation}", post(primitive_read::<O>))
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owners)
}

async fn git_preview<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    cancellation: Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    invoke_route(
        HttpApplicationOperation::GitPreview,
        state,
        request_id,
        cancellation,
        page,
        body,
    )
    .await
}

async fn git_apply<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    cancellation: Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    invoke_route(
        HttpApplicationOperation::GitApply,
        state,
        request_id,
        cancellation,
        page,
        body,
    )
    .await
}

async fn feedback_diagnostics<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    cancellation: Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    invoke_route(
        HttpApplicationOperation::FeedbackDiagnostics,
        state,
        request_id,
        cancellation,
        page,
        body,
    )
    .await
}

async fn feedback_get<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    cancellation: Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    invoke_route(
        HttpApplicationOperation::FeedbackGet,
        state,
        request_id,
        cancellation,
        page,
        body,
    )
    .await
}

async fn feedback_expand<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    cancellation: Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    invoke_route(
        HttpApplicationOperation::FeedbackExpand,
        state,
        request_id,
        cancellation,
        page,
        body,
    )
    .await
}

async fn feedback_list<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    cancellation: Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    invoke_route(
        HttpApplicationOperation::FeedbackList,
        state,
        request_id,
        cancellation,
        page,
        body,
    )
    .await
}

async fn feedback_impact<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    cancellation: Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    invoke_route(
        HttpApplicationOperation::FeedbackImpact,
        state,
        request_id,
        cancellation,
        page,
        body,
    )
    .await
}

async fn affected_tests<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    cancellation: Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    invoke_route(
        HttpApplicationOperation::AffectedTests,
        state,
        request_id,
        cancellation,
        page,
        body,
    )
    .await
}

async fn test_results<O>(
    state: State<O>,
    request_id: Extension<RequestId>,
    cancellation: Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    invoke_route(
        HttpApplicationOperation::TestResults,
        state,
        request_id,
        cancellation,
        page,
        body,
    )
    .await
}

async fn primitive_read<O>(
    Path(operation): Path<String>,
    state: State<O>,
    request_id: Extension<RequestId>,
    cancellation: Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    let operation = match operation.as_str() {
        "session_lookup" => HttpApplicationOperation::SessionLookup,
        "qualified_name" => HttpApplicationOperation::QualifiedName,
        "call_chain" => HttpApplicationOperation::CallChain,
        "file_dependents" => HttpApplicationOperation::FileDependents,
        "source_lines" => HttpApplicationOperation::SourceLines,
        "source_body" => HttpApplicationOperation::SourceBody,
        "source_outline" => HttpApplicationOperation::SourceOutline,
        "module_api" => HttpApplicationOperation::ModuleApi,
        "file_metadata" => HttpApplicationOperation::FileMetadata,
        "health_read" => HttpApplicationOperation::HealthRead,
        "storage_status" => HttpApplicationOperation::StorageStatus,
        "diagnostics_read" => HttpApplicationOperation::DiagnosticsRead,
        _ => {
            return application_problem_response(adapter_problem(
                request_id.0,
                ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
            ));
        }
    };
    invoke_route(operation, state, request_id, cancellation, page, body).await
}

async fn invoke_route<O>(
    operation: HttpApplicationOperation,
    State(owners): State<O>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
    page: Result<Query<PageRequest>, QueryRejection>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response
where
    O: HttpApplicationOwners,
{
    let Query(page) = match page {
        Ok(page) => page,
        Err(_) => {
            return invalid_request_response(
                request_id,
                "http.invalid_query",
                "The HTTP query is invalid",
            );
        }
    };
    let page = match PageRequest::new(page.page_size, page.cursor) {
        Ok(page) => page,
        Err(_) => {
            return invalid_request_response(
                request_id,
                "http.invalid_page",
                "The requested HTTP page is invalid",
            );
        }
    };
    let Json(body) = match body {
        Ok(body) => body,
        Err(_) => {
            return invalid_request_response(
                request_id,
                "http.invalid_body",
                "The HTTP request body is invalid or exceeds the configured limit",
            );
        }
    };

    let owner_kind = operation.owner_kind();
    let request = HttpApplicationRequest {
        operation,
        request_id,
        page,
        deadline: Some(controls.deadline),
        cancellation: controls.cancellation,
        body,
    };
    let invocation = match owner_kind {
        HttpApplicationOwnerKind::Git => owners.invoke_git(request),
        HttpApplicationOwnerKind::Feedback => owners.invoke_feedback(request),
        HttpApplicationOwnerKind::Primitive => owners.invoke_primitive(request),
    };
    invocation.await.into_http_response()
}

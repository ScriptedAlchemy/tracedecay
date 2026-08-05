use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_application::{
    ApplicationProblem, ApplicationProblemEnvelope, ApplicationProblemKind, CancellationSignal,
    Deadline, OpaqueCursor, PageRequest, ProblemOwningLayer, RequestId, ResultContractRef,
    RetryDirective, SafeDiagnostic,
};
use tracedecay_tool_catalog::{
    BindingSurface, CancellationContract, CapabilityId, CatalogSnapshotV1, DeadlineContract,
    FeatureId, PaginationContract, ProfileId, ReceiptContract, SchemaId, ScopeDimension,
    TerminalStateContract,
};

use crate::{CanonicalInvocationResult, HttpJsonEnvelope, HttpProblemEnvelope};

mod manifest;

pub use manifest::{HttpManifestContract, HttpManifestContractError};

pub(crate) const MAX_HTTP_APPLICATION_BODY_BYTES: usize = 1024 * 1024;
const DEFAULT_HTTP_PAGE_SIZE: u32 = 10;

/// Define the handlers that name one fixed operation.
///
/// A route whose path carries no operation segment has nothing left to decide,
/// so its handler is pure forwarding. Stating the extractor list once per
/// router keeps that forwarding from being retyped for every operation.
macro_rules! constant_operation_handlers {
    // Peel one handler per step: the extractor list travels as one token tree
    // because macro_rules cannot re-expand one repetition group inside a
    // sibling group (`$handler` and `$extractor` repeat different counts).
    (
        owner: $generic:ident = $owner:path,
        dispatch = $dispatch:path,
        extractors = $extractors:tt,
        $handler:ident => $operation:expr;
        $($rest:tt)*
    ) => {
        constant_operation_handlers! {
            @one
            owner: $generic = $owner,
            dispatch = $dispatch,
            extractors = $extractors,
            $handler => $operation;
        }
        constant_operation_handlers! {
            owner: $generic = $owner,
            dispatch = $dispatch,
            extractors = $extractors,
            $($rest)*
        }
    };
    (
        owner: $generic:ident = $owner:path,
        dispatch = $dispatch:path,
        extractors = $extractors:tt,
    ) => {};
    (
        @one
        owner: $generic:ident = $owner:path,
        dispatch = $dispatch:path,
        extractors = { $($extractor:ident: $extractor_type:ty),+ $(,)? },
        $handler:ident => $operation:expr;
    ) => {
        async fn $handler<$generic>($($extractor: $extractor_type),+) -> Response
        where
            $generic: $owner,
        {
            $dispatch($operation, $($extractor),+).await
        }
    };
}

pub(crate) use constant_operation_handlers;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpPageQuery {
    #[serde(default = "default_http_page_size")]
    page_size: u32,
    #[serde(default)]
    cursor: Option<OpaqueCursor>,
}

const fn default_http_page_size() -> u32 {
    DEFAULT_HTTP_PAGE_SIZE
}

pub use tracedecay_application::{
    ApplicationOwnerKind as HttpApplicationOwnerKind,
    ApplicationWireOperation as HttpApplicationOperation,
};

const fn is_http_exposed(operation: HttpApplicationOperation) -> bool {
    !matches!(
        operation,
        HttpApplicationOperation::GitPreview | HttpApplicationOperation::GitApply
    )
}

fn route_path(operation: HttpApplicationOperation) -> Option<String> {
    let path = match operation {
        HttpApplicationOperation::GitStatus => "/git/status".to_owned(),
        HttpApplicationOperation::GitDiff => "/git/diff".to_owned(),
        HttpApplicationOperation::GitHistory => "/git/history".to_owned(),
        HttpApplicationOperation::GitBlame => "/git/blame".to_owned(),
        HttpApplicationOperation::GitHunks => "/git/hunks".to_owned(),
        HttpApplicationOperation::AffectedTests => "/tests/affected".to_owned(),
        HttpApplicationOperation::TestResults => "/tests/results".to_owned(),
        HttpApplicationOperation::FeedbackDiagnostics => "/feedback/diagnostics".to_owned(),
        HttpApplicationOperation::FeedbackGet => "/feedback/get".to_owned(),
        HttpApplicationOperation::FeedbackExpand => "/feedback/expand".to_owned(),
        HttpApplicationOperation::FeedbackList => "/feedback/list".to_owned(),
        HttpApplicationOperation::FeedbackImpact => "/feedback/impact".to_owned(),
        HttpApplicationOperation::FeedbackAdvisoryCycle => "/feedback/advisory_cycle".to_owned(),
        operation if operation.is_callable_code() => {
            format!("/code/{}", operation.as_str())
        }
        operation if operation.owner_kind() == HttpApplicationOwnerKind::Primitive => {
            format!("/primitives/{}", operation.as_str())
        }
        operation if operation.owner_kind() == HttpApplicationOwnerKind::Configuration => {
            format!("/configuration/{}", operation.as_str())
        }
        operation if operation.owner_kind() == HttpApplicationOwnerKind::ContextScout => {
            format!("/context-scout/{}", operation.as_str())
        }
        HttpApplicationOperation::GitPreview | HttpApplicationOperation::GitApply => return None,
        _ => return None,
    };
    Some(path)
}

/// Generated route documentation derived from the same catalog snapshot and
/// operation enum used by the shipped HTTP router.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HttpRouteDocumentV1 {
    pub method: &'static str,
    pub path: String,
    pub operation: String,
    pub capability_id: String,
    pub binding_id: String,
    pub request_schema: String,
    pub request_schema_revision: u32,
    pub result_schema: String,
    pub result_schema_revision: u32,
    pub cancellation: CancellationContract,
    pub deadline: DeadlineContract,
    pub pagination: Option<PaginationContract>,
    pub receipt: ReceiptContract,
    pub terminal_states: TerminalStateContract,
}

/// Generate authorized HTTP route documentation. Hidden profile, scope,
/// authorization, feature, or availability entries are omitted exactly like
/// discovery; no static OpenAPI list can drift from the catalog.
pub fn http_route_documents(
    catalog: &CatalogSnapshotV1,
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    negotiated_features: &BTreeSet<FeatureId>,
    protocol_revision: u32,
) -> Vec<HttpRouteDocumentV1> {
    let mut documents = Vec::new();
    for (binding, capability) in catalog.visible_bindings(
        profile_id,
        BindingSurface::Http,
        protocol_revision,
        negotiated_features,
        authorized_capabilities,
        available_scope,
    ) {
        let Some(operation) =
            HttpApplicationOperation::from_catalog_name(binding.operation().as_str())
        else {
            continue;
        };
        if !is_http_exposed(operation) {
            continue;
        }
        let Some(path) = route_path(operation) else {
            continue;
        };
        documents.push(HttpRouteDocumentV1 {
            method: "POST",
            path,
            operation: operation.as_str().to_owned(),
            capability_id: capability.capability_id().as_str().to_owned(),
            binding_id: binding.binding_id().as_str().to_owned(),
            request_schema: capability.request_schema().schema_id().as_str().to_owned(),
            request_schema_revision: capability.request_schema().revision(),
            result_schema: capability.result_schema().schema_id().as_str().to_owned(),
            result_schema_revision: capability.result_schema().revision(),
            cancellation: capability.cancellation().clone(),
            deadline: capability.deadline().clone(),
            pagination: capability.pagination().cloned(),
            receipt: capability.receipt(),
            terminal_states: capability.terminal_states().clone(),
        });
    }
    documents.sort_by(|left, right| left.path.cmp(&right.path));
    documents
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

    fn invoke_callable_code(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture;

    fn invoke_primitive(&self, request: HttpApplicationRequest) -> HttpApplicationInvocationFuture;

    fn invoke_configuration(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture;

    fn invoke_context_scout(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture;
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

    fn invoke_callable_code(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture {
        Box::pin((self)(request))
    }

    fn invoke_primitive(&self, request: HttpApplicationRequest) -> HttpApplicationInvocationFuture {
        Box::pin((self)(request))
    }

    fn invoke_configuration(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture {
        Box::pin((self)(request))
    }

    fn invoke_context_scout(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture {
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
    pub fn into_http_response(self) -> Response {
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

pub(crate) fn adapter_problem(
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

pub(crate) fn invalid_request_response(
    request_id: RequestId,
    code: &'static str,
    message: &'static str,
) -> Response {
    application_problem_response(invalid_request_problem(request_id, code, message))
}

/// Build the catalog-advertised application routes at relative paths.
///
/// The executable nests this router at its root-owned prefix behind
/// authentication and origin middleware. Authorization remains part of
/// canonical application dispatch, including concealed
/// not-found-or-not-authorized results. These route names are adapter
/// bindings, not a frozen SDK namespace.
pub fn application_router<O>(owners: O) -> Router
where
    O: HttpApplicationOwners,
{
    Router::new()
        .route("/git/{operation}", post(git_read::<O>))
        .route("/feedback/{operation}", post(public_feedback_read::<O>))
        .route("/tests/affected", post(affected_tests::<O>))
        .route("/tests/results", post(test_results::<O>))
        .route("/code/{operation}", post(callable_code_read::<O>))
        .route("/primitives/{operation}", post(primitive_read::<O>))
        .route(
            "/configuration/{operation}",
            post(configuration_operation::<O>),
        )
        .route(
            "/context-scout/{operation}",
            post(context_scout_operation::<O>),
        )
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owners)
}

/// Build the dashboard bindings for canonical feedback reads.
///
/// This is a route subset only. It uses the same handlers, request envelopes,
/// dispatcher, and application owner as the complete HTTP application router;
/// the dashboard does not deserialize or reconstruct feedback results.
pub fn feedback_application_router<O>(owners: O) -> Router
where
    O: HttpApplicationOwners,
{
    Router::new()
        .route("/{operation}", post(feedback_read::<O>))
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owners)
}

/// Build only the canonical configuration routes for an adapter that does not
/// advertise the complete HTTP application surface.
///
/// Dashboard mounts this router with a Dashboard-bound application invoker.
/// Keeping the extraction path shared preserves body limits, pagination,
/// cancellation, and canonical response semantics without falsely exposing
/// unrelated HTTP bindings as Dashboard operations.
pub fn configuration_application_router<O>(owners: O) -> Router
where
    O: HttpApplicationOwners,
{
    Router::new()
        .route(
            "/configuration/{operation}",
            post(configuration_operation::<O>),
        )
        .layer(DefaultBodyLimit::max(MAX_HTTP_APPLICATION_BODY_BYTES))
        .with_state(owners)
}

fn parse_git_read_operation(operation: &str) -> Option<HttpApplicationOperation> {
    match operation {
        "status" => Some(HttpApplicationOperation::GitStatus),
        "diff" => Some(HttpApplicationOperation::GitDiff),
        "history" => Some(HttpApplicationOperation::GitHistory),
        "blame" => Some(HttpApplicationOperation::GitBlame),
        "hunks" => Some(HttpApplicationOperation::GitHunks),
        _ => None,
    }
}

fn parse_feedback_read_operation(operation: &str) -> Option<HttpApplicationOperation> {
    crate::feedback::feedback_read_operation(operation)
}

fn parse_public_feedback_operation(operation: &str) -> Option<HttpApplicationOperation> {
    HttpApplicationOperation::from_catalog_name(&format!("feedback_{operation}"))
        .filter(|operation| operation.owner_kind() == HttpApplicationOwnerKind::Feedback)
}

constant_operation_handlers! {
    owner: O = HttpApplicationOwners,
    dispatch = invoke_route,
    extractors = {
        state: State<O>,
        request_id: Extension<RequestId>,
        cancellation: Extension<HttpApplicationControls>,
        page: Result<Query<HttpPageQuery>, QueryRejection>,
        body: Result<Json<Value>, JsonRejection>,
    },
    affected_tests => HttpApplicationOperation::AffectedTests;
    test_results => HttpApplicationOperation::TestResults;
}

fn parse_primitive_read_operation(operation: &str) -> Option<HttpApplicationOperation> {
    HttpApplicationOperation::from_catalog_name(operation).filter(|operation| {
        operation.owner_kind() == HttpApplicationOwnerKind::Primitive
            && *operation != HttpApplicationOperation::TestResults
            && !operation.is_callable_code()
    })
}

fn parse_callable_code_operation(operation: &str) -> Option<HttpApplicationOperation> {
    HttpApplicationOperation::from_catalog_name(operation)
        .filter(|operation| operation.is_callable_code())
}

fn parse_configuration_operation(operation: &str) -> Option<HttpApplicationOperation> {
    HttpApplicationOperation::from_catalog_name(operation)
        .filter(|operation| operation.owner_kind() == HttpApplicationOwnerKind::Configuration)
}

fn parse_context_scout_operation(operation: &str) -> Option<HttpApplicationOperation> {
    HttpApplicationOperation::from_catalog_name(operation)
        .filter(|operation| operation.owner_kind() == HttpApplicationOwnerKind::ContextScout)
}

/// Define the `/{operation}` handlers, which differ only in how the path
/// segment resolves to an operation.
///
/// An unresolvable segment is refused exactly like an unauthorized one, so
/// route membership never becomes an existence oracle. That concealment is the
/// reason these handlers must stay byte-identical to each other.
macro_rules! parsed_operation_handlers {
    ($($handler:ident => $parse:path;)+) => {
        $(
            async fn $handler<O>(
                Path(operation): Path<String>,
                state: State<O>,
                request_id: Extension<RequestId>,
                cancellation: Extension<HttpApplicationControls>,
                page: Result<Query<HttpPageQuery>, QueryRejection>,
                body: Result<Json<Value>, JsonRejection>,
            ) -> Response
            where
                O: HttpApplicationOwners,
            {
                let Some(operation) = $parse(&operation) else {
                    return application_problem_response(adapter_problem(
                        request_id.0,
                        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
                    ));
                };
                invoke_route(operation, state, request_id, cancellation, page, body).await
            }
        )+
    };
}

parsed_operation_handlers! {
    feedback_read => parse_feedback_read_operation;
    public_feedback_read => parse_public_feedback_operation;
    git_read => parse_git_read_operation;
    primitive_read => parse_primitive_read_operation;
    callable_code_read => parse_callable_code_operation;
    configuration_operation => parse_configuration_operation;
    context_scout_operation => parse_context_scout_operation;
}

async fn invoke_route<O>(
    operation: HttpApplicationOperation,
    State(owners): State<O>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
    page: Result<Query<HttpPageQuery>, QueryRejection>,
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
        HttpApplicationOwnerKind::CallableCode => owners.invoke_callable_code(request),
        HttpApplicationOwnerKind::Primitive => owners.invoke_primitive(request),
        HttpApplicationOwnerKind::Configuration => owners.invoke_configuration(request),
        HttpApplicationOwnerKind::ContextScout => owners.invoke_context_scout(request),
    };
    invocation.await.into_http_response()
}

#[cfg(test)]
mod tests;

//! Shared transport adapter contracts for the first callable application surfaces.
//!
//! The adapters resolve catalog bindings and preserve canonical application
//! problem envelopes. They do not open stores, run queries, or bypass the
//! daemon-owned Git transaction authority.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Extension, Path as AxumPath, Query, State};
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio_stream::StreamExt;
use tracedecay_api::{
    CanonicalInvocationResult, HttpApplicationControls, HttpApplicationOperation,
    HttpApplicationRequest, application_problem_response, sse_response,
};
use tracedecay_application::retrieval::{
    AffectedFileTestsPrimitiveRequest, GraphImpactPrimitiveRequest, SymbolGraphScope,
};
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, ApplicationEnvelope,
    ApplicationProblem, ApplicationProblemEnvelope, ApplicationProblemKind, ApplicationResult,
    CancellationContext, CancellationSignal, Deadline, HealthReadRequest, IdempotencyKey,
    LegalAction, OperationTermination, PageRequest, ProblemOwningLayer, RequestContext, RequestId,
    ResultContractRef, ResultProjection, ResumeToken, RetrievalOrder, RetrievalRequestMeta,
    RetryDirective, SafeDiagnostic, SessionLookupRequest, SourceLinesRequest, StreamEvent,
    StreamEventKind,
};
use tracedecay_domain::{
    GitIndexCommitIntentV1, GitIndexPreviewId, GitIndexPreviewV1, GitIndexTransactionOperationV1,
    HunkRefV1, ManifestDigest, ProjectId, RepositoryStateSnapshotV1, UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::{
    BindingSurface, CatalogSnapshotV1, CatalogValidationError, IdentifierError, ProfileId,
    SchemaId, SurfaceOperationName,
};

use crate::application::feedback::observations::{
    Plan26DeliveryRouteV1, Plan26FeedbackOperationV1, Plan26FeedbackOutcomeV1,
    Plan26FeedbackSourceEventV1, Plan26SseLifecycleV1,
};
use crate::application::operation_stream::{
    OperationCancelOutcome, OperationEventAuthority, OperationEventError, OperationId,
};
use crate::application::primitives::{
    CallChainPrimitiveRequest, DiagnosticsPrimitiveRequest, FileDependentsPrimitiveRequest,
    FileMetadataPrimitiveRequest, ModuleApiPrimitiveRequest, Pr12PrimitiveRequest,
    QualifiedNamePrimitiveRequest, SourceBodyPrimitiveRequest, SourceOutlinePrimitiveRequest,
    StorageStatusPrimitiveRequest,
};
use crate::catalog_composition::{CatalogCompositionError, build_application_catalog_snapshot};
use crate::daemon_client::{
    BindingResolution, BindingResolver, CatalogBindingResolver, DaemonInvocationError,
    DispatchError, DispatchInput, DispatchedInvocation, InvocationCancellationPolicy,
    InvocationControls, RequestedOutputFormat, ScopeSelector, resolve_dispatch,
};

const DEFAULT_PAGE_SIZE: u32 = 10;
const DEFAULT_DEADLINE_MICROS: i64 = 30_000_000;
const HTTP_DEADLINE_HEADER: &str = "x-tracedecay-deadline-micros";
const MAX_REQUEST_HANDLE_BYTES: usize = 256;
static NEXT_HTTP_APPLICATION_REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationSurfaceOperation {
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

pub const APPLICATION_SURFACE_OPERATIONS: [ApplicationSurfaceOperation; 21] = [
    ApplicationSurfaceOperation::GitPreview,
    ApplicationSurfaceOperation::GitApply,
    ApplicationSurfaceOperation::FeedbackDiagnostics,
    ApplicationSurfaceOperation::FeedbackGet,
    ApplicationSurfaceOperation::FeedbackExpand,
    ApplicationSurfaceOperation::FeedbackList,
    ApplicationSurfaceOperation::FeedbackImpact,
    ApplicationSurfaceOperation::AffectedTests,
    ApplicationSurfaceOperation::TestResults,
    ApplicationSurfaceOperation::SessionLookup,
    ApplicationSurfaceOperation::QualifiedName,
    ApplicationSurfaceOperation::CallChain,
    ApplicationSurfaceOperation::FileDependents,
    ApplicationSurfaceOperation::SourceLines,
    ApplicationSurfaceOperation::SourceBody,
    ApplicationSurfaceOperation::SourceOutline,
    ApplicationSurfaceOperation::ModuleApi,
    ApplicationSurfaceOperation::FileMetadata,
    ApplicationSurfaceOperation::HealthRead,
    ApplicationSurfaceOperation::StorageStatus,
    ApplicationSurfaceOperation::DiagnosticsRead,
];

impl ApplicationSurfaceOperation {
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

    pub fn from_tool_name(tool_name: &str) -> Option<Self> {
        let operation = tool_name.strip_prefix("tracedecay_").unwrap_or(tool_name);
        APPLICATION_SURFACE_OPERATIONS
            .into_iter()
            .find(|candidate| candidate.as_str() == operation)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackSurfaceRequest {
    pub request_handle: String,
}

impl FeedbackSurfaceRequest {
    pub fn new(request_handle: String) -> Result<Self, ApplicationSurfaceAdapterError> {
        if request_handle.is_empty()
            || request_handle.trim() != request_handle
            || request_handle.len() > MAX_REQUEST_HANDLE_BYTES
            || request_handle.chars().any(char::is_control)
        {
            return Err(ApplicationSurfaceAdapterError::InvalidRequestHandle);
        }
        Ok(Self { request_handle })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AffectedTestsSurfaceRequest {
    pub files: Vec<String>,
    #[serde(default = "default_affected_tests_depth")]
    pub maximum_depth: usize,
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackImpactSurfaceRequest {
    pub node_id: String,
    #[serde(default = "default_impact_depth")]
    pub maximum_depth: u32,
    #[serde(default)]
    pub path_prefix: Option<String>,
}

const fn default_impact_depth() -> u32 {
    3
}

const fn default_affected_tests_depth() -> usize {
    5
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestResultsSurfaceRequest {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitPreviewSurfaceRequest {
    pub operation: GitIndexTransactionOperationV1,
    /// Compatibility input only. The daemon always replaces this value with a
    /// freshly minted preview identity before application admission.
    #[serde(default = "pending_git_preview_id")]
    pub preview_id: GitIndexPreviewId,
    pub repository_snapshot: RepositoryStateSnapshotV1,
    #[serde(default)]
    pub selected_hunks: Vec<HunkRefV1>,
    #[serde(default)]
    pub commit_intent: Option<GitIndexCommitIntentV1>,
}

fn pending_git_preview_id() -> GitIndexPreviewId {
    GitIndexPreviewId::new("preview.pending")
        .expect("the compatibility preview identifier is static")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitApplySurfaceRequest {
    pub preview: GitIndexPreviewV1,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ApplicationSurfaceRequest {
    GitPreview(GitPreviewSurfaceRequest),
    GitApply(GitApplySurfaceRequest),
    Feedback(FeedbackSurfaceRequest),
    FeedbackImpact(FeedbackImpactSurfaceRequest),
    AffectedTests(AffectedTestsSurfaceRequest),
    TestResults(TestResultsSurfaceRequest),
    Primitive(Pr12PrimitiveRequest),
}

pub struct ApplicationSurfaceInvocationResult {
    pub operation: ApplicationSurfaceOperation,
    pub binding_id: tracedecay_tool_catalog::BindingId,
    pub result: ApplicationResult<Value>,
    pub requested_format: RequestedOutputFormat,
}

pub type HttpApplicationInvocationFuture =
    Pin<Box<dyn Future<Output = CanonicalInvocationResult<Value>> + Send + 'static>>;

pub fn http_application_invoker(
    client: crate::daemon_client::DaemonInvocationClient,
) -> Result<
    impl Fn(HttpApplicationRequest) -> HttpApplicationInvocationFuture + Clone + Send + Sync + 'static,
    ApplicationSurfaceAdapterError,
> {
    let catalog = Arc::new(application_surface_catalog()?);
    let resolver = CatalogBindingResolver::new(&catalog);
    for operation in APPLICATION_SURFACE_OPERATIONS {
        if resolve_application_binding(&resolver, BindingSurface::Http, operation).is_none() {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        }
    }
    Ok(move |request| {
        let client = client.clone();
        let catalog = Arc::clone(&catalog);
        let invocation: HttpApplicationInvocationFuture =
            Box::pin(
                async move { invoke_http_application_request(request, &client, &catalog).await },
            );
        invocation
    })
}

pub fn http_application_router(
    client: crate::daemon_client::DaemonInvocationClient,
    operation_events: OperationEventAuthority,
    active_project_id: ProjectId,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    let cancellations = Arc::new(Mutex::new(BTreeMap::new()));
    let event_client = client.clone();
    Ok(
        tracedecay_api::application_router(http_application_invoker(client)?)
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(&cancellations),
                application_http_context,
            ))
            .merge(http_operation_event_router(
                operation_events,
                active_project_id,
                cancellations,
                Some(event_client),
            )),
    )
}

type HttpCancellationRegistry = Arc<Mutex<BTreeMap<RequestId, CancellationSignal>>>;

async fn application_http_context(
    State(cancellations): State<HttpCancellationRegistry>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let sequence = NEXT_HTTP_APPLICATION_REQUEST.fetch_add(1, Ordering::Relaxed);
    let request_id = match RequestId::new(format!(
        "request.http.{}.{}",
        crate::tracedecay::current_timestamp(),
        sequence
    )) {
        Ok(request_id) => request_id,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let cancellation =
        match CancellationSignal::active(format!("cancellation.http.{}", request_id.as_str())) {
            Ok(cancellation) => cancellation,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
    let observed_at = match current_micros() {
        Ok(observed_at) => observed_at,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let default_expires_at = observed_at.0.saturating_add(DEFAULT_DEADLINE_MICROS);
    let caller_expires_at = request
        .headers()
        .get(HTTP_DEADLINE_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(default_expires_at);
    let deadline = match Deadline::new(UtcMicros(caller_expires_at.min(default_expires_at))) {
        Ok(deadline) => deadline,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    request.extensions_mut().insert(request_id.clone());
    request.extensions_mut().insert(cancellation.clone());
    request.extensions_mut().insert(HttpApplicationControls {
        deadline,
        cancellation: cancellation.clone(),
    });
    if let Ok(mut active) = cancellations.lock() {
        active.insert(request_id.clone(), cancellation.clone());
    }
    let mut disconnect = HttpDisconnectCancellation::new(cancellations, request_id, cancellation);
    let response = next.run(request).await;
    disconnect.disarm();
    response
}

struct HttpDisconnectCancellation {
    registry: HttpCancellationRegistry,
    request_id: RequestId,
    cancellation: CancellationSignal,
    armed: bool,
}

impl HttpDisconnectCancellation {
    fn new(
        registry: HttpCancellationRegistry,
        request_id: RequestId,
        cancellation: CancellationSignal,
    ) -> Self {
        Self {
            registry,
            request_id,
            cancellation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
        if let Ok(mut active) = self.registry.lock() {
            active.remove(&self.request_id);
        }
    }
}

impl Drop for HttpDisconnectCancellation {
    fn drop(&mut self) {
        if self.armed {
            let _ = self
                .cancellation
                .cancel(current_micros().unwrap_or(UtcMicros(1)));
            if let Ok(mut active) = self.registry.lock() {
                active.remove(&self.request_id);
            }
        }
    }
}

#[derive(Clone)]
struct HttpOperationEventState {
    authority: OperationEventAuthority,
    active_project_id: ProjectId,
    cancellations: HttpCancellationRegistry,
    client: Option<crate::daemon_client::DaemonInvocationClient>,
}

struct SseDisconnectObserver {
    client: crate::daemon_client::DaemonInvocationClient,
    subject: ManifestDigest,
    terminal: Arc<AtomicBool>,
}

impl Drop for SseDisconnectObserver {
    fn drop(&mut self) {
        if self.terminal.load(Ordering::Relaxed) {
            return;
        }
        let client = self.client.clone();
        let subject = self.subject.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = client
                    .observe_plan26_feedback(
                        subject,
                        current_micros().unwrap_or(UtcMicros(1)),
                        Plan26FeedbackSourceEventV1::SseLifecycle {
                            lifecycle: Plan26SseLifecycleV1::Disconnected,
                            sequence: None,
                            item_count: 0,
                            duration_micros: None,
                        },
                    )
                    .await;
            });
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpOperationEventQuery {
    #[serde(default)]
    next_sequence: u64,
    #[serde(default)]
    resume_token: Option<ResumeToken>,
}

#[derive(Serialize)]
struct HttpOperationCancelResponse {
    status: &'static str,
}

fn http_operation_event_router(
    authority: OperationEventAuthority,
    active_project_id: ProjectId,
    cancellations: HttpCancellationRegistry,
    client: Option<crate::daemon_client::DaemonInvocationClient>,
) -> axum::Router {
    axum::Router::new()
        .route(
            "/operations/{operation_id}/events",
            get(http_operation_events),
        )
        .route(
            "/operations/{operation_id}/cancel",
            post(http_operation_cancel),
        )
        .with_state(HttpOperationEventState {
            authority,
            active_project_id,
            cancellations: Arc::clone(&cancellations),
            client,
        })
        .layer(axum::middleware::from_fn_with_state(
            cancellations,
            application_http_context,
        ))
}

async fn resolve_authenticated_http_request_context(
    state: &HttpOperationEventState,
    operation_id: &OperationId,
    request_id: RequestId,
    cancellation: CancellationContext,
    observed_at: UtcMicros,
    resume_token: Option<&ResumeToken>,
) -> Result<RequestContext, OperationEventError> {
    let deadline = Deadline::new(UtcMicros(
        observed_at.0.saturating_add(DEFAULT_DEADLINE_MICROS),
    ))
    .map_err(|error| OperationEventError::InvalidContext(error.to_string()))?;
    state
        .authority
        .resolve_request_context(
            operation_id,
            &state.active_project_id,
            request_id,
            deadline,
            cancellation,
            observed_at,
            resume_token,
        )
        .await
}

fn sse_observation_subject(request_id: &RequestId, operation_id: &str) -> Option<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.feedback.sse-observation.v1",
        request_id.as_str(),
        operation_id,
    ))
    .ok()
}

async fn emit_http_plan26_observation(
    state: &HttpOperationEventState,
    subject: Option<&ManifestDigest>,
    observed_at: UtcMicros,
    event: Plan26FeedbackSourceEventV1,
) {
    if let (Some(subject), Some(client)) = (subject, state.client.as_ref()) {
        let _ = client
            .observe_plan26_feedback(subject.clone(), observed_at, event)
            .await;
    }
}

fn plan26_sse_stream_event<T>(event: &StreamEvent<T>) -> Option<(Plan26SseLifecycleV1, u32, bool)> {
    match &event.kind {
        StreamEventKind::Item(_) => Some((Plan26SseLifecycleV1::EventDelivered, 1, false)),
        StreamEventKind::Progress { .. } => None,
        StreamEventKind::Gap(_) => Some((Plan26SseLifecycleV1::Gap, 0, false)),
        StreamEventKind::Terminal(terminal) => Some((
            match terminal.termination {
                OperationTermination::Completed => Plan26SseLifecycleV1::Completed,
                OperationTermination::Cancelled => Plan26SseLifecycleV1::Cancelled,
                OperationTermination::TimedOut => Plan26SseLifecycleV1::TimedOut,
                OperationTermination::Failed | OperationTermination::EffectUnknown => {
                    Plan26SseLifecycleV1::Failed
                }
                OperationTermination::Partial => Plan26SseLifecycleV1::Partial,
            },
            0,
            true,
        )),
    }
}

async fn http_operation_events(
    State(state): State<HttpOperationEventState>,
    AxumPath(operation_id): AxumPath<String>,
    Extension(request_id): Extension<RequestId>,
    Extension(cancellation): Extension<CancellationSignal>,
    Query(query): Query<HttpOperationEventQuery>,
) -> Response {
    let observation_subject = sse_observation_subject(&request_id, &operation_id);
    let operation_id = match RequestId::new(operation_id) {
        Ok(operation_id) => OperationId::from_request(operation_id),
        Err(_) => {
            emit_http_plan26_observation(
                &state,
                observation_subject.as_ref(),
                current_micros().unwrap_or(UtcMicros(1)),
                Plan26FeedbackSourceEventV1::ArgumentRejected {
                    operation: Plan26FeedbackOperationV1::SseStream,
                    outcome: Plan26FeedbackOutcomeV1::Rejected,
                },
            )
            .await;
            return operation_event_problem(
                &request_id,
                OperationEventError::NotFoundOrNotAuthorized,
            );
        }
    };
    let observed_at = match current_micros() {
        Ok(observed_at) => observed_at,
        Err(error) => {
            return operation_event_problem(
                &request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let context = match resolve_authenticated_http_request_context(
        &state,
        &operation_id,
        request_id.clone(),
        cancellation.context(),
        observed_at,
        query.resume_token.as_ref(),
    )
    .await
    {
        Ok(context) => context,
        Err(error) => {
            emit_http_plan26_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                Plan26FeedbackSourceEventV1::ArgumentRejected {
                    operation: Plan26FeedbackOperationV1::SseStream,
                    outcome: Plan26FeedbackOutcomeV1::Denied,
                },
            )
            .await;
            return operation_event_problem(&request_id, error);
        }
    };
    emit_http_plan26_observation(
        &state,
        observation_subject.as_ref(),
        observed_at,
        Plan26FeedbackSourceEventV1::Dispatch {
            operation: Plan26FeedbackOperationV1::SseStream,
            outcome: Plan26FeedbackOutcomeV1::Admitted,
            capacity: 1,
            admitted: 1,
        },
    )
    .await;
    let subscription = match state
        .authority
        .subscribe(
            &operation_id,
            &context,
            observed_at,
            query.next_sequence,
            query.resume_token.as_ref(),
        )
        .await
    {
        Ok(subscription) => subscription,
        Err(error) => {
            if matches!(&error, OperationEventError::Saturated) {
                emit_http_plan26_observation(
                    &state,
                    observation_subject.as_ref(),
                    observed_at,
                    Plan26FeedbackSourceEventV1::Dispatch {
                        operation: Plan26FeedbackOperationV1::SseStream,
                        outcome: Plan26FeedbackOutcomeV1::AtCapacity,
                        capacity: 1,
                        admitted: 0,
                    },
                )
                .await;
            }
            let lifecycle = if matches!(
                &error,
                OperationEventError::FrontierExpired | OperationEventError::ResumeExpired
            ) {
                Plan26SseLifecycleV1::Expired
            } else {
                Plan26SseLifecycleV1::Failed
            };
            emit_http_plan26_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                Plan26FeedbackSourceEventV1::SseLifecycle {
                    lifecycle,
                    sequence: None,
                    item_count: 0,
                    duration_micros: None,
                },
            )
            .await;
            return operation_event_problem(&request_id, error);
        }
    };
    emit_http_plan26_observation(
        &state,
        observation_subject.as_ref(),
        observed_at,
        Plan26FeedbackSourceEventV1::SseLifecycle {
            lifecycle: Plan26SseLifecycleV1::Opened,
            sequence: None,
            item_count: 0,
            duration_micros: None,
        },
    )
    .await;
    let (correlation_id, frontier, stream) = subscription.into_sse_parts();
    let observer = observation_subject
        .zip(state.client.clone())
        .map(|(subject, client)| {
            Arc::new(SseDisconnectObserver {
                client,
                subject,
                terminal: Arc::new(AtomicBool::new(false)),
            })
        });
    let observed_stream = stream.then(move |event| {
        let observer = observer.clone();
        async move {
            if let (Some(observer), Some((lifecycle, item_count, is_terminal))) =
                (observer, plan26_sse_stream_event(&event))
            {
                if is_terminal {
                    observer.terminal.store(true, Ordering::Relaxed);
                }
                let _ = observer
                    .client
                    .observe_plan26_feedback(
                        observer.subject.clone(),
                        current_micros().unwrap_or(UtcMicros(1)),
                        Plan26FeedbackSourceEventV1::SseLifecycle {
                            lifecycle,
                            sequence: Some(event.sequence),
                            item_count,
                            duration_micros: None,
                        },
                    )
                    .await;
            }
            event
        }
    });
    sse_response(correlation_id, frontier, observed_stream).into_response()
}

async fn http_operation_cancel(
    State(state): State<HttpOperationEventState>,
    AxumPath(operation_id): AxumPath<String>,
    Extension(request_id): Extension<RequestId>,
    Extension(cancellation): Extension<CancellationSignal>,
) -> Response {
    let observation_subject = sse_observation_subject(&request_id, &operation_id);
    let operation_id = match RequestId::new(operation_id) {
        Ok(operation_id) => OperationId::from_request(operation_id),
        Err(_) => {
            emit_http_plan26_observation(
                &state,
                observation_subject.as_ref(),
                current_micros().unwrap_or(UtcMicros(1)),
                Plan26FeedbackSourceEventV1::ArgumentRejected {
                    operation: Plan26FeedbackOperationV1::SseStream,
                    outcome: Plan26FeedbackOutcomeV1::Rejected,
                },
            )
            .await;
            return operation_event_problem(
                &request_id,
                OperationEventError::NotFoundOrNotAuthorized,
            );
        }
    };
    let observed_at = match current_micros() {
        Ok(observed_at) => observed_at,
        Err(error) => {
            return operation_event_problem(
                &request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let context = match resolve_authenticated_http_request_context(
        &state,
        &operation_id,
        request_id.clone(),
        cancellation.context(),
        observed_at,
        None,
    )
    .await
    {
        Ok(context) => context,
        Err(error) => {
            emit_http_plan26_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                Plan26FeedbackSourceEventV1::Cancellation {
                    operation: Plan26FeedbackOperationV1::SseStream,
                    outcome: Plan26FeedbackOutcomeV1::Denied,
                },
            )
            .await;
            return operation_event_problem(&request_id, error);
        }
    };
    let target_cancellation = state
        .cancellations
        .lock()
        .ok()
        .and_then(|active| active.get(operation_id.request_id()).cloned());
    match state
        .authority
        .cancel(&operation_id, &context, observed_at)
        .await
    {
        Ok(OperationCancelOutcome::Requested) => {
            if let Some(cancellation) = target_cancellation {
                let _ = cancellation.cancel(observed_at);
            }
            emit_http_plan26_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                Plan26FeedbackSourceEventV1::Cancellation {
                    operation: Plan26FeedbackOperationV1::SseStream,
                    outcome: Plan26FeedbackOutcomeV1::Accepted,
                },
            )
            .await;
            (
                StatusCode::ACCEPTED,
                Json(HttpOperationCancelResponse {
                    status: "requested",
                }),
            )
                .into_response()
        }
        Ok(OperationCancelOutcome::AlreadyRequested) => {
            if let Some(cancellation) = target_cancellation {
                let _ = cancellation.cancel(observed_at);
            }
            emit_http_plan26_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                Plan26FeedbackSourceEventV1::Cancellation {
                    operation: Plan26FeedbackOperationV1::SseStream,
                    outcome: Plan26FeedbackOutcomeV1::Duplicate,
                },
            )
            .await;
            (
                StatusCode::OK,
                Json(HttpOperationCancelResponse {
                    status: "already_requested",
                }),
            )
                .into_response()
        }
        Ok(OperationCancelOutcome::AlreadyTerminal) => {
            emit_http_plan26_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                Plan26FeedbackSourceEventV1::Cancellation {
                    operation: Plan26FeedbackOperationV1::SseStream,
                    outcome: Plan26FeedbackOutcomeV1::Completed,
                },
            )
            .await;
            (
                StatusCode::OK,
                Json(HttpOperationCancelResponse {
                    status: "already_terminal",
                }),
            )
                .into_response()
        }
        Err(error) => {
            emit_http_plan26_observation(
                &state,
                observation_subject.as_ref(),
                observed_at,
                Plan26FeedbackSourceEventV1::Cancellation {
                    operation: Plan26FeedbackOperationV1::SseStream,
                    outcome: if matches!(&error, OperationEventError::Saturated) {
                        Plan26FeedbackOutcomeV1::AtCapacity
                    } else {
                        Plan26FeedbackOutcomeV1::Failed
                    },
                },
            )
            .await;
            operation_event_problem(&request_id, error)
        }
    }
}

fn operation_event_problem(request_id: &RequestId, error: OperationEventError) -> Response {
    let problem = match error {
        OperationEventError::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        OperationEventError::FrontierExpired | OperationEventError::ResumeExpired => {
            ApplicationProblem::Stale {
                diagnostic: SafeDiagnostic {
                    code: "operation_event.resume_expired".to_owned(),
                    message: "The operation-event resume frontier has expired".to_owned(),
                },
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![LegalAction::Refresh],
            }
        }
        OperationEventError::InvalidFrontier => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "operation_event.invalid_frontier".to_owned(),
                message: "The requested operation-event frontier is invalid".to_owned(),
            },
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        OperationEventError::RequestNotAdmitted => ApplicationProblem::TimedOut {
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        },
        OperationEventError::Saturated => ApplicationProblem::Saturated {
            diagnostic: SafeDiagnostic {
                code: "operation_event.saturated".to_owned(),
                message: "Operation-event capacity is temporarily saturated".to_owned(),
            },
            retry: RetryDirective::AfterDelay,
            legal_actions: vec![LegalAction::Retry],
        },
        OperationEventError::InvalidConfiguration
        | OperationEventError::InvalidContext(_)
        | OperationEventError::AlreadyBound
        | OperationEventError::InvalidProgress
        | OperationEventError::TerminalAlreadyPublished
        | OperationEventError::InvalidTerminal(_)
        | OperationEventError::InvalidTestRunEvent
        | OperationEventError::ResumeUnavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "operation_event.unavailable".to_owned(),
                message: "The operation-event service is unavailable".to_owned(),
            })
        }
    };
    let contract = ResultContractRef::new(
        SchemaId::new("schema.tracedecay.operation-event.problem.v1")
            .expect("the operation-event problem schema id is static"),
        1,
    )
    .expect("the operation-event problem contract is static");
    let envelope = ApplicationProblemEnvelope::new(contract, request_id.clone(), problem)
        .with_owning_layer(ProblemOwningLayer::Runtime);
    let envelope = if envelope.problem.kind() == ApplicationProblemKind::Saturated {
        envelope
            .with_retry_after_millis(Some(250))
            .expect("the operation-event retry delay is bounded")
    } else {
        envelope
    };
    application_problem_response(envelope)
}

#[derive(Debug, Error)]
pub enum ApplicationSurfaceAdapterError {
    #[error("application catalog could not be composed: {0}")]
    Catalog(#[from] CatalogCompositionError),
    #[error("application surface contract is invalid: {0}")]
    Contract(#[from] ApplicationContractError),
    #[error("application surface identifier is invalid: {0}")]
    Identifier(#[from] IdentifierError),
    #[error("application surface catalog input is invalid: {0}")]
    CatalogValidation(#[from] CatalogValidationError),
    #[error("application surface request handle is invalid")]
    InvalidRequestHandle,
    #[error("application surface request does not match its reviewed schema")]
    InvalidSurfaceRequest,
    #[error("owning daemon application service is unavailable")]
    DaemonUnavailable,
    #[error("application surface was not found or is not authorized")]
    UnknownOrNotAuthorized,
}

pub fn application_surface_catalog() -> Result<CatalogSnapshotV1, ApplicationSurfaceAdapterError> {
    Ok(build_application_catalog_snapshot()?)
}

pub fn application_surface_dispatch_input(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchInput<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    let cancellation = CancellationSignal::active(format!("cancellation.{}", request_id.as_str()))?;
    application_surface_dispatch_input_with_controls(
        operation,
        request_id,
        request,
        PageRequest::first(DEFAULT_PAGE_SIZE)?,
        None,
        cancellation,
        requested_format,
    )
}

pub fn application_surface_dispatch_input_with_controls(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    page: PageRequest,
    deadline: Option<Deadline>,
    cancellation: CancellationSignal,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchInput<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    if !request.matches(operation) {
        return Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest);
    }
    Ok(DispatchInput {
        request_id,
        binding: BindingResolution {
            profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?,
            operation: SurfaceOperationName::new(operation.as_str())?,
            protocol_revision: 1,
            negotiated_features: BTreeSet::new(),
        },
        request,
        controls: InvocationControls {
            scope: ScopeSelector::CurrentProject,
            page,
            deadline,
            cancellation,
            requested_format,
        },
    })
}

impl ApplicationSurfaceRequest {
    fn matches(&self, operation: ApplicationSurfaceOperation) -> bool {
        matches!(
            (self, operation),
            (Self::GitPreview(_), ApplicationSurfaceOperation::GitPreview)
                | (Self::GitApply(_), ApplicationSurfaceOperation::GitApply)
                | (
                    Self::Feedback(_),
                    ApplicationSurfaceOperation::FeedbackDiagnostics
                        | ApplicationSurfaceOperation::FeedbackGet
                        | ApplicationSurfaceOperation::FeedbackExpand
                        | ApplicationSurfaceOperation::FeedbackList
                )
                | (
                    Self::FeedbackImpact(_),
                    ApplicationSurfaceOperation::FeedbackImpact
                )
                | (
                    Self::AffectedTests(_),
                    ApplicationSurfaceOperation::AffectedTests
                )
                | (
                    Self::TestResults(_),
                    ApplicationSurfaceOperation::TestResults
                )
                | (
                    Self::Primitive(_),
                    ApplicationSurfaceOperation::SessionLookup
                        | ApplicationSurfaceOperation::QualifiedName
                        | ApplicationSurfaceOperation::CallChain
                        | ApplicationSurfaceOperation::FileDependents
                        | ApplicationSurfaceOperation::SourceLines
                        | ApplicationSurfaceOperation::SourceBody
                        | ApplicationSurfaceOperation::SourceOutline
                        | ApplicationSurfaceOperation::ModuleApi
                        | ApplicationSurfaceOperation::FileMetadata
                        | ApplicationSurfaceOperation::HealthRead
                        | ApplicationSurfaceOperation::StorageStatus
                        | ApplicationSurfaceOperation::DiagnosticsRead
                )
        )
    }
}

pub fn parse_application_surface_request(
    operation: ApplicationSurfaceOperation,
    value: Value,
) -> Result<ApplicationSurfaceRequest, ApplicationSurfaceAdapterError> {
    match operation {
        ApplicationSurfaceOperation::GitPreview => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::GitPreview)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::GitApply => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::GitApply)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::AffectedTests => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::AffectedTests)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::FeedbackImpact => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::FeedbackImpact)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::TestResults => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::TestResults)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::SessionLookup => {
            serde_json::from_value::<SessionLookupRequest>(value)
                .map(Pr12PrimitiveRequest::SessionLookup)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::QualifiedName => {
            serde_json::from_value::<QualifiedNamePrimitiveRequest>(value)
                .map(Pr12PrimitiveRequest::QualifiedName)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CallChain => {
            serde_json::from_value::<CallChainPrimitiveRequest>(value)
                .map(Pr12PrimitiveRequest::CallChain)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::FileDependents => {
            serde_json::from_value::<FileDependentsPrimitiveRequest>(value)
                .map(Pr12PrimitiveRequest::FileDependents)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::SourceLines => {
            serde_json::from_value::<SourceLinesRequest>(value)
                .map(Pr12PrimitiveRequest::SourceLines)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::SourceBody => {
            serde_json::from_value::<SourceBodyPrimitiveRequest>(value)
                .map(Pr12PrimitiveRequest::SourceBody)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::SourceOutline => {
            serde_json::from_value::<SourceOutlinePrimitiveRequest>(value)
                .map(Pr12PrimitiveRequest::SourceOutline)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::ModuleApi => {
            serde_json::from_value::<ModuleApiPrimitiveRequest>(value)
                .map(Pr12PrimitiveRequest::ModuleApi)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::FileMetadata => {
            serde_json::from_value::<FileMetadataPrimitiveRequest>(value)
                .map(Pr12PrimitiveRequest::FileMetadata)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::HealthRead => {
            serde_json::from_value::<HealthReadRequest>(value)
                .map(Pr12PrimitiveRequest::HealthRead)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::StorageStatus => {
            serde_json::from_value::<StorageStatusPrimitiveRequest>(value)
                .map(Pr12PrimitiveRequest::StorageStatus)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::DiagnosticsRead => {
            serde_json::from_value::<DiagnosticsPrimitiveRequest>(value)
                .map(Pr12PrimitiveRequest::DiagnosticsRead)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::FeedbackDiagnostics
        | ApplicationSurfaceOperation::FeedbackGet
        | ApplicationSurfaceOperation::FeedbackExpand
        | ApplicationSurfaceOperation::FeedbackList => {
            let request: FeedbackSurfaceRequest = serde_json::from_value(value)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
            Ok(ApplicationSurfaceRequest::Feedback(
                FeedbackSurfaceRequest::new(request.request_handle)?,
            ))
        }
    }
}

pub async fn execute_application_surface(
    operation: ApplicationSurfaceOperation,
    dispatched: DispatchedInvocation<ApplicationSurfaceRequest>,
    client: Option<&crate::daemon_client::DaemonInvocationClient>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    let result_contract = ResultContractRef::from_schema(&dispatched.invocation.result_schema);
    let binding_id = dispatched.invocation.binding_id.clone();
    let request_id = dispatched.request_id;
    let delivery_route = plan26_delivery_route(dispatched.surface);
    let (invocation, requested_format) = dispatched.invocation.into_application_invocation();
    let observed_at = current_micros()?;
    let deadline = invocation.deadline.unwrap_or(Deadline::new(UtcMicros(
        observed_at.0.saturating_add(DEFAULT_DEADLINE_MICROS),
    ))?);
    let cancellation = invocation.cancellation;
    let cancellation_context = cancellation.context();
    let request_deadline = deadline.clone();
    let request = match invocation.request {
        ApplicationSurfaceRequest::GitPreview(request) => {
            crate::daemon::DaemonInvocationRequest::git_preview(
                request_id.as_str(),
                request,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::GitApply(request) => {
            crate::daemon::DaemonInvocationRequest::git_apply(
                request_id.as_str(),
                request,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::Feedback(request) => {
            crate::daemon::DaemonInvocationRequest::feedback(
                request_id.as_str(),
                operation,
                request.request_handle,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::FeedbackImpact(request) => {
            crate::daemon::DaemonInvocationRequest::primitive(
                request_id.as_str(),
                operation,
                crate::application::primitives::Pr12PrimitiveRequest::Impact(
                    GraphImpactPrimitiveRequest {
                        node_id: request.node_id,
                        maximum_depth: request.maximum_depth,
                        scope: SymbolGraphScope {
                            path_prefix: request.path_prefix,
                        },
                        meta: RetrievalRequestMeta::current(
                            invocation.page,
                            ResultProjection::Evidence,
                            RetrievalOrder::StableIdentity,
                        ),
                    },
                ),
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::AffectedTests(request) => {
            crate::daemon::DaemonInvocationRequest::primitive(
                request_id.as_str(),
                operation,
                crate::application::primitives::Pr12PrimitiveRequest::AffectedFileTests(
                    AffectedFileTestsPrimitiveRequest {
                        files: request.files,
                        maximum_depth: request.maximum_depth,
                        filter: request.filter,
                        meta: RetrievalRequestMeta::current(
                            invocation.page,
                            ResultProjection::Evidence,
                            RetrievalOrder::StableIdentity,
                        ),
                    },
                ),
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::TestResults(_) => {
            crate::daemon::DaemonInvocationRequest::primitive(
                request_id.as_str(),
                operation,
                crate::application::primitives::Pr12PrimitiveRequest::RecentTestResults,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::Primitive(request) => {
            crate::daemon::DaemonInvocationRequest::primitive(
                request_id.as_str(),
                operation,
                request,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
    }
    .with_delivery_route(delivery_route);
    let Some(client) = client else {
        return Ok(ApplicationSurfaceInvocationResult {
            operation,
            binding_id,
            result: Err(ApplicationProblemEnvelope::new(
                result_contract,
                request_id,
                ApplicationProblem::unavailable(SafeDiagnostic::new(
                    "application.transport.unavailable",
                    "The daemon application transport is unavailable",
                )?),
            )),
            requested_format,
        });
    };
    let policy = if operation == ApplicationSurfaceOperation::GitApply {
        InvocationCancellationPolicy::AuthoritativeEffect
    } else {
        InvocationCancellationPolicy::ReadOnly
    };
    let response = client
        .invoke_controlled(request, request_deadline, cancellation, policy)
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            if plan26_surface_is_observable(operation)
                && let Ok(subject_digest) = canonical_sha256(&(
                    "tracedecay.feedback.transport-observation.v1",
                    request_id.as_str(),
                    operation.as_str(),
                    delivery_route,
                ))
                && let Ok(observed_at) = current_micros()
            {
                let event = match &error {
                    DaemonInvocationError::Cancelled { .. } => {
                        Plan26FeedbackSourceEventV1::Cancellation {
                            operation: plan26_surface_operation(operation),
                            outcome: Plan26FeedbackOutcomeV1::Cancelled,
                        }
                    }
                    DaemonInvocationError::TimedOut { .. } => {
                        Plan26FeedbackSourceEventV1::Cancellation {
                            operation: plan26_surface_operation(operation),
                            outcome: Plan26FeedbackOutcomeV1::TimedOut,
                        }
                    }
                    DaemonInvocationError::Saturated { .. }
                    | DaemonInvocationError::Backpressured { .. } => {
                        Plan26FeedbackSourceEventV1::Dispatch {
                            operation: plan26_surface_operation(operation),
                            outcome: Plan26FeedbackOutcomeV1::AtCapacity,
                            capacity: 1,
                            admitted: 0,
                        }
                    }
                    DaemonInvocationError::Unavailable => Plan26FeedbackSourceEventV1::Delivery {
                        operation: plan26_surface_operation(operation),
                        route: delivery_route,
                        outcome: Plan26FeedbackOutcomeV1::Unavailable,
                        item_count: 0,
                        duration_micros: None,
                    },
                };
                let _ = client
                    .observe_plan26_feedback(subject_digest, observed_at, event)
                    .await;
            }
            return Ok(ApplicationSurfaceInvocationResult {
                operation,
                binding_id,
                result: Err(ApplicationProblemEnvelope::new(
                    result_contract,
                    request_id,
                    error.into_application_problem(),
                )),
                requested_format,
            });
        }
    };
    let result = match response.outcome {
        crate::daemon::DaemonInvocationOutcome::GitPreview { scope, preview } => {
            Ok(ApplicationEnvelope::preview(
                result_contract.clone(),
                request_id.clone(),
                scope,
                preview.into_application_result()?,
            ))
        }
        crate::daemon::DaemonInvocationOutcome::GitApply { scope, effect } => {
            Ok(ApplicationEnvelope::effect(
                result_contract.clone(),
                request_id.clone(),
                scope,
                effect.into_application_result()?,
            ))
        }
        crate::daemon::DaemonInvocationOutcome::Feedback { scope, result } => {
            Ok(ApplicationEnvelope::evidence(
                result_contract.clone(),
                request_id.clone(),
                scope,
                result.into_application(),
            ))
        }
        crate::daemon::DaemonInvocationOutcome::Primitive { scope, result } => {
            Ok(ApplicationEnvelope::evidence(
                result_contract.clone(),
                request_id.clone(),
                scope,
                result.into_application(),
            ))
        }
        crate::daemon::DaemonInvocationOutcome::ApplicationProblem { problem } => Err(
            ApplicationProblemEnvelope::new(result_contract.clone(), request_id.clone(), problem),
        ),
        crate::daemon::DaemonInvocationOutcome::Problem { problem } => {
            Err(ApplicationProblemEnvelope::new(
                result_contract.clone(),
                request_id.clone(),
                invocation_problem(problem)?,
            ))
        }
        _ => Err(ApplicationProblemEnvelope::new(
            result_contract.clone(),
            request_id.clone(),
            ApplicationProblem::unavailable(SafeDiagnostic::new(
                "application.surface.invalid_response",
                "The daemon returned an invalid application response",
            )?),
        )),
    };

    Ok(ApplicationSurfaceInvocationResult {
        operation,
        binding_id,
        result,
        requested_format,
    })
}

fn plan26_delivery_route(surface: BindingSurface) -> Plan26DeliveryRouteV1 {
    match surface {
        BindingSurface::Cli => Plan26DeliveryRouteV1::Cli,
        BindingSurface::Mcp => Plan26DeliveryRouteV1::Mcp,
        BindingSurface::Http | BindingSurface::Dashboard => Plan26DeliveryRouteV1::Http,
        BindingSurface::Lsp => Plan26DeliveryRouteV1::Lsp,
    }
}

fn plan26_surface_operation(operation: ApplicationSurfaceOperation) -> Plan26FeedbackOperationV1 {
    match operation {
        ApplicationSurfaceOperation::FeedbackDiagnostics => {
            Plan26FeedbackOperationV1::FeedbackDiagnostics
        }
        ApplicationSurfaceOperation::FeedbackGet => Plan26FeedbackOperationV1::FeedbackGet,
        ApplicationSurfaceOperation::FeedbackExpand => Plan26FeedbackOperationV1::FeedbackExpand,
        ApplicationSurfaceOperation::FeedbackList => Plan26FeedbackOperationV1::FeedbackList,
        ApplicationSurfaceOperation::FeedbackImpact => Plan26FeedbackOperationV1::PrimitiveImpact,
        ApplicationSurfaceOperation::AffectedTests => {
            Plan26FeedbackOperationV1::PrimitiveAffectedTests
        }
        ApplicationSurfaceOperation::TestResults => Plan26FeedbackOperationV1::PrimitiveTestResults,
        ApplicationSurfaceOperation::GitPreview
        | ApplicationSurfaceOperation::GitApply
        | ApplicationSurfaceOperation::SessionLookup
        | ApplicationSurfaceOperation::QualifiedName
        | ApplicationSurfaceOperation::CallChain
        | ApplicationSurfaceOperation::FileDependents
        | ApplicationSurfaceOperation::SourceLines
        | ApplicationSurfaceOperation::SourceBody
        | ApplicationSurfaceOperation::SourceOutline
        | ApplicationSurfaceOperation::ModuleApi
        | ApplicationSurfaceOperation::FileMetadata
        | ApplicationSurfaceOperation::HealthRead
        | ApplicationSurfaceOperation::StorageStatus
        | ApplicationSurfaceOperation::DiagnosticsRead => Plan26FeedbackOperationV1::FeedbackCycle,
    }
}

fn plan26_surface_is_observable(operation: ApplicationSurfaceOperation) -> bool {
    matches!(
        operation,
        ApplicationSurfaceOperation::FeedbackDiagnostics
            | ApplicationSurfaceOperation::FeedbackGet
            | ApplicationSurfaceOperation::FeedbackExpand
            | ApplicationSurfaceOperation::FeedbackList
            | ApplicationSurfaceOperation::FeedbackImpact
            | ApplicationSurfaceOperation::AffectedTests
            | ApplicationSurfaceOperation::TestResults
            | ApplicationSurfaceOperation::SessionLookup
            | ApplicationSurfaceOperation::QualifiedName
            | ApplicationSurfaceOperation::CallChain
            | ApplicationSurfaceOperation::FileDependents
            | ApplicationSurfaceOperation::SourceLines
            | ApplicationSurfaceOperation::SourceBody
            | ApplicationSurfaceOperation::SourceOutline
            | ApplicationSurfaceOperation::ModuleApi
            | ApplicationSurfaceOperation::FileMetadata
            | ApplicationSurfaceOperation::HealthRead
            | ApplicationSurfaceOperation::StorageStatus
            | ApplicationSurfaceOperation::DiagnosticsRead
    )
}

pub async fn observe_surface_argument_rejection(
    client: Option<&crate::daemon_client::DaemonInvocationClient>,
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    request_id: &RequestId,
) {
    let (Some(client), Ok(subject_digest), Ok(observed_at)) = (
        client,
        canonical_sha256(&(
            "tracedecay.feedback.surface-rejection.v1",
            request_id.as_str(),
            surface,
            operation,
        )),
        current_micros(),
    ) else {
        return;
    };
    let _ = client
        .observe_plan26_feedback(
            subject_digest,
            observed_at,
            Plan26FeedbackSourceEventV1::ArgumentRejected {
                operation: plan26_surface_operation(operation),
                outcome: Plan26FeedbackOutcomeV1::Rejected,
            },
        )
        .await;
}

pub async fn resolve_http_application_surface(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    client: Option<&crate::daemon_client::DaemonInvocationClient>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    let dispatched = match resolve_http_application_surface_dispatch(
        operation,
        request_id.clone(),
        request,
        requested_format,
    ) {
        Ok(dispatched) => dispatched,
        Err(error) => {
            observe_surface_argument_rejection(
                client,
                BindingSurface::Http,
                operation,
                &request_id,
            )
            .await;
            return Err(error);
        }
    };
    execute_application_surface(operation, dispatched, client).await
}

pub fn resolve_http_application_surface_dispatch(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchedInvocation<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    resolve_application_surface_dispatch(
        BindingSurface::Http,
        operation,
        request_id,
        request,
        requested_format,
    )
}

pub fn resolve_application_surface_dispatch(
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchedInvocation<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    let cancellation = CancellationSignal::active(format!("cancellation.{}", request_id.as_str()))?;
    resolve_application_surface_dispatch_with_controls(
        surface,
        operation,
        request_id,
        request,
        PageRequest::first(DEFAULT_PAGE_SIZE)?,
        None,
        cancellation,
        requested_format,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_application_surface_dispatch_with_controls(
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    page: PageRequest,
    deadline: Option<Deadline>,
    cancellation: CancellationSignal,
    requested_format: RequestedOutputFormat,
) -> Result<DispatchedInvocation<ApplicationSurfaceRequest>, ApplicationSurfaceAdapterError> {
    let catalog = application_surface_catalog()?;
    let resolver = CatalogBindingResolver::new(&catalog);
    let input = application_surface_dispatch_input_with_controls(
        operation,
        request_id,
        request,
        page,
        deadline,
        cancellation,
        requested_format,
    )?;
    let dispatched = resolve_dispatch(&resolver, surface, input).map_err(map_dispatch_error)?;
    Ok(dispatched)
}

async fn invoke_http_application_request(
    request: HttpApplicationRequest,
    client: &crate::daemon_client::DaemonInvocationClient,
    catalog: &CatalogSnapshotV1,
) -> CanonicalInvocationResult<Value> {
    let operation = application_operation_for_http(request.operation);
    let resolver = CatalogBindingResolver::new(catalog);
    let binding = resolve_application_binding(&resolver, BindingSurface::Http, operation)
        .expect("HTTP bindings are validated before the application router is mounted");
    let binding_id = binding.binding_id;
    let result_contract = ResultContractRef::from_schema(&binding.result_schema);
    let request_id = request.request_id;
    let application_request = match parse_application_surface_request(operation, request.body) {
        Ok(request) => request,
        Err(error) => {
            observe_surface_argument_rejection(
                Some(client),
                BindingSurface::Http,
                operation,
                &request_id,
            )
            .await;
            return CanonicalInvocationResult::new(
                binding_id,
                Err(http_adapter_problem(result_contract, request_id, error)),
            );
        }
    };
    let input = match application_surface_dispatch_input_with_controls(
        operation,
        request_id.clone(),
        application_request,
        request.page,
        request.deadline,
        request.cancellation,
        RequestedOutputFormat::Json,
    ) {
        Ok(input) => input,
        Err(error) => {
            observe_surface_argument_rejection(
                Some(client),
                BindingSurface::Http,
                operation,
                &request_id,
            )
            .await;
            return CanonicalInvocationResult::new(
                binding_id,
                Err(http_adapter_problem(result_contract, request_id, error)),
            );
        }
    };
    let dispatched = match resolve_dispatch(&resolver, BindingSurface::Http, input) {
        Ok(dispatched) => dispatched,
        Err(error) => {
            observe_surface_argument_rejection(
                Some(client),
                BindingSurface::Http,
                operation,
                &request_id,
            )
            .await;
            return CanonicalInvocationResult::new(
                binding_id,
                Err(http_adapter_problem(
                    result_contract,
                    request_id,
                    map_dispatch_error(error),
                )),
            );
        }
    };
    match execute_application_surface(operation, dispatched, Some(client)).await {
        Ok(result) => CanonicalInvocationResult::new(result.binding_id, result.result),
        Err(error) => CanonicalInvocationResult::new(
            binding_id,
            Err(http_adapter_problem(result_contract, request_id, error)),
        ),
    }
}

fn application_operation_for_http(
    operation: HttpApplicationOperation,
) -> ApplicationSurfaceOperation {
    match operation {
        HttpApplicationOperation::GitPreview => ApplicationSurfaceOperation::GitPreview,
        HttpApplicationOperation::GitApply => ApplicationSurfaceOperation::GitApply,
        HttpApplicationOperation::FeedbackDiagnostics => {
            ApplicationSurfaceOperation::FeedbackDiagnostics
        }
        HttpApplicationOperation::FeedbackGet => ApplicationSurfaceOperation::FeedbackGet,
        HttpApplicationOperation::FeedbackExpand => ApplicationSurfaceOperation::FeedbackExpand,
        HttpApplicationOperation::FeedbackList => ApplicationSurfaceOperation::FeedbackList,
        HttpApplicationOperation::FeedbackImpact => ApplicationSurfaceOperation::FeedbackImpact,
        HttpApplicationOperation::AffectedTests => ApplicationSurfaceOperation::AffectedTests,
        HttpApplicationOperation::TestResults => ApplicationSurfaceOperation::TestResults,
        HttpApplicationOperation::SessionLookup => ApplicationSurfaceOperation::SessionLookup,
        HttpApplicationOperation::QualifiedName => ApplicationSurfaceOperation::QualifiedName,
        HttpApplicationOperation::CallChain => ApplicationSurfaceOperation::CallChain,
        HttpApplicationOperation::FileDependents => ApplicationSurfaceOperation::FileDependents,
        HttpApplicationOperation::SourceLines => ApplicationSurfaceOperation::SourceLines,
        HttpApplicationOperation::SourceBody => ApplicationSurfaceOperation::SourceBody,
        HttpApplicationOperation::SourceOutline => ApplicationSurfaceOperation::SourceOutline,
        HttpApplicationOperation::ModuleApi => ApplicationSurfaceOperation::ModuleApi,
        HttpApplicationOperation::FileMetadata => ApplicationSurfaceOperation::FileMetadata,
        HttpApplicationOperation::HealthRead => ApplicationSurfaceOperation::HealthRead,
        HttpApplicationOperation::StorageStatus => ApplicationSurfaceOperation::StorageStatus,
        HttpApplicationOperation::DiagnosticsRead => ApplicationSurfaceOperation::DiagnosticsRead,
    }
}

fn resolve_application_binding(
    resolver: &impl BindingResolver,
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
) -> Option<crate::daemon_client::ResolvedBinding> {
    let profile_id = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).ok()?;
    let operation = SurfaceOperationName::new(operation.as_str()).ok()?;
    resolver.resolve_binding(
        surface,
        &BindingResolution {
            profile_id,
            operation,
            protocol_revision: 1,
            negotiated_features: BTreeSet::new(),
        },
    )
}

fn http_adapter_problem(
    contract: ResultContractRef,
    request_id: RequestId,
    error: ApplicationSurfaceAdapterError,
) -> ApplicationProblemEnvelope {
    let problem = match error {
        ApplicationSurfaceAdapterError::UnknownOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        ApplicationSurfaceAdapterError::InvalidRequestHandle
        | ApplicationSurfaceAdapterError::InvalidSurfaceRequest => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic {
                    code: "application.surface.invalid_request".to_owned(),
                    message: "The application request is invalid".to_owned(),
                },
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            }
        }
        ApplicationSurfaceAdapterError::Catalog(_)
        | ApplicationSurfaceAdapterError::Contract(_)
        | ApplicationSurfaceAdapterError::Identifier(_)
        | ApplicationSurfaceAdapterError::CatalogValidation(_)
        | ApplicationSurfaceAdapterError::DaemonUnavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.surface.unavailable".to_owned(),
                message: "The application service for this operation is unavailable".to_owned(),
            })
        }
    };
    ApplicationProblemEnvelope::new(contract, request_id, problem)
        .with_owning_layer(ProblemOwningLayer::Adapter)
}

fn current_micros() -> Result<UtcMicros, ApplicationSurfaceAdapterError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
    let now = i64::try_from(now.as_micros()).unwrap_or(i64::MAX);
    Ok(UtcMicros(now))
}

fn invocation_problem(
    problem: crate::daemon::DaemonInvocationProblem,
) -> Result<ApplicationProblem, ApplicationSurfaceAdapterError> {
    Ok(match problem {
        crate::daemon::DaemonInvocationProblem::InvalidRequest
        | crate::daemon::DaemonInvocationProblem::UnsupportedRevision => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic::new(
                    "application.surface.invalid_request",
                    "The daemon rejected the application request",
                )?,
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            }
        }
        crate::daemon::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        crate::daemon::DaemonInvocationProblem::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic::new(
                "application.surface.unavailable",
                "The application service for this operation is unavailable",
            )?)
        }
    })
}

pub fn map_dispatch_error(error: DispatchError) -> ApplicationSurfaceAdapterError {
    match error {
        DispatchError::UnknownOrNotAuthorized => {
            ApplicationSurfaceAdapterError::UnknownOrNotAuthorized
        }
    }
}

#[cfg(test)]
mod tests;

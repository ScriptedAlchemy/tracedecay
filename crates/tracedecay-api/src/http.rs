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
    ApplicationContractError, ApplicationProblem, ApplicationProblemEnvelope,
    ApplicationProblemKind, CancellationSignal, Deadline, OpaqueCursor, PageRequest,
    ProblemOwningLayer, RequestId, ResultContractRef, RetainedSurfaceOperation, RetryDirective,
    SafeDiagnostic,
};
use tracedecay_tool_catalog::{
    BindingSurface, CapabilityId, CatalogSnapshotV1, FeatureId, ProfileId, SchemaId, ScopeDimension,
};

use crate::{CanonicalInvocationResult, HttpJsonEnvelope, HttpProblemEnvelope};
mod application_operation_owner;

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

/// Canonical operation identity shared by every retained application surface.
/// Transport bindings select the exposed subset without defining another
/// operation enum or name conversion.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HttpApplicationOperation {
    GitStatus,
    GitDiff,
    GitHistory,
    GitBlame,
    GitHunks,
    GitPreview,
    GitApply,
    GitHubStackSignalExpand,
    NativeIntegrationStackSnapshot,
    NativeIntegrationPreflight,
    NativeIntegrationApprove,
    NativeIntegrationApply,
    NativeIntegrationStatus,
    NativeIntegrationCancel,
    NativeIntegrationWorktreeInventory,
    NativeIntegrationWorktreeInspect,
    NativeIntegrationWorktreeConfirm,
    NativeIntegrationWorktreeRemove,
    NativeIntegrationWorktreeReconcile,
    FeedbackDiagnostics,
    FeedbackGet,
    FeedbackExpand,
    FeedbackList,
    FeedbackImpact,
    FeedbackAdvisoryCycle,
    AffectedTests,
    TestResults,
    CodeExactOccurrence,
    CodePhraseSearch,
    CodeSymbolSearch,
    CodeSignatureSearch,
    CodeImplementations,
    CodeTypeHierarchy,
    CodeCallers,
    CodeCallees,
    CodeFacets,
    CodeTimeline,
    CodeDeclaration,
    CodeDefinition,
    CodeTypeDefinition,
    CodeReferences,
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
    HealthDelta,
    StorageStatus,
    DiagnosticsRead,
    ObservatoryRead,
    ConfigurationList,
    ConfigurationExplain,
    ConfigurationGet,
    ConfigurationSet,
    ConfigurationUnset,
    ConfigurationBatch,
    ConfigurationWriteCredential,
    ConfigurationObservedState,
    ConfigurationProtectedPreview,
    ConfigurationProtectedApply,
    ConfigurationRollbackPreview,
    ConfigurationRollbackApply,
    ConfigurationAudit,
    ContextScoutStatus,
    ContextScoutRecent,
    ContextScoutExplain,
    ContextScoutCapability,
    ContextScoutBudget,
    ContextScoutPause,
    ContextScoutResume,
    ContextScoutCancel,
    ContextScoutClaim,
    ContextScoutDelivery,
    ContextScoutFeedback,
}

/// The canonical application owner family responsible for one HTTP binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HttpApplicationOwnerKind {
    Git,
    NativeIntegration,
    Feedback,
    CallableCode,
    Primitive,
    Observatory,
    Configuration,
    ContextScout,
}

impl HttpApplicationOperation {
    pub const ALL: [Self; 79] = [
        Self::GitStatus,
        Self::GitDiff,
        Self::GitHistory,
        Self::GitBlame,
        Self::GitHunks,
        Self::GitPreview,
        Self::GitApply,
        Self::GitHubStackSignalExpand,
        Self::NativeIntegrationStackSnapshot,
        Self::NativeIntegrationPreflight,
        Self::NativeIntegrationApprove,
        Self::NativeIntegrationApply,
        Self::NativeIntegrationStatus,
        Self::NativeIntegrationCancel,
        Self::NativeIntegrationWorktreeInventory,
        Self::NativeIntegrationWorktreeInspect,
        Self::NativeIntegrationWorktreeConfirm,
        Self::NativeIntegrationWorktreeRemove,
        Self::NativeIntegrationWorktreeReconcile,
        Self::FeedbackDiagnostics,
        Self::FeedbackGet,
        Self::FeedbackExpand,
        Self::FeedbackList,
        Self::FeedbackImpact,
        Self::FeedbackAdvisoryCycle,
        Self::AffectedTests,
        Self::TestResults,
        Self::CodeExactOccurrence,
        Self::CodePhraseSearch,
        Self::CodeSymbolSearch,
        Self::CodeSignatureSearch,
        Self::CodeImplementations,
        Self::CodeTypeHierarchy,
        Self::CodeCallers,
        Self::CodeCallees,
        Self::CodeFacets,
        Self::CodeTimeline,
        Self::CodeDeclaration,
        Self::CodeDefinition,
        Self::CodeTypeDefinition,
        Self::CodeReferences,
        Self::SessionLookup,
        Self::QualifiedName,
        Self::CallChain,
        Self::FileDependents,
        Self::SourceLines,
        Self::SourceBody,
        Self::SourceOutline,
        Self::ModuleApi,
        Self::FileMetadata,
        Self::HealthRead,
        Self::HealthDelta,
        Self::StorageStatus,
        Self::DiagnosticsRead,
        Self::ObservatoryRead,
        Self::ConfigurationList,
        Self::ConfigurationExplain,
        Self::ConfigurationGet,
        Self::ConfigurationSet,
        Self::ConfigurationUnset,
        Self::ConfigurationBatch,
        Self::ConfigurationWriteCredential,
        Self::ConfigurationObservedState,
        Self::ConfigurationProtectedPreview,
        Self::ConfigurationProtectedApply,
        Self::ConfigurationRollbackPreview,
        Self::ConfigurationRollbackApply,
        Self::ConfigurationAudit,
        Self::ContextScoutStatus,
        Self::ContextScoutRecent,
        Self::ContextScoutExplain,
        Self::ContextScoutCapability,
        Self::ContextScoutBudget,
        Self::ContextScoutPause,
        Self::ContextScoutResume,
        Self::ContextScoutCancel,
        Self::ContextScoutClaim,
        Self::ContextScoutDelivery,
        Self::ContextScoutFeedback,
    ];

    pub fn from_catalog_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|operation| operation.as_str() == name)
    }

    pub fn from_tool_name(tool_name: &str) -> Option<Self> {
        let operation = tool_name.strip_prefix("tracedecay_").unwrap_or(tool_name);
        if operation == "diagnostics" {
            return Some(Self::DiagnosticsRead);
        }
        Self::from_catalog_name(operation)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitStatus => "git_status",
            Self::GitDiff => "git_diff",
            Self::GitHistory => "git_history",
            Self::GitBlame => "git_blame",
            Self::GitHunks => "git_hunks",
            Self::GitPreview => "git_preview",
            Self::GitApply => "git_apply",
            Self::GitHubStackSignalExpand => "github_stack_signal_expand",
            Self::NativeIntegrationStackSnapshot => "stack_snapshot",
            Self::NativeIntegrationPreflight => "preflight_native_integration",
            Self::NativeIntegrationApprove => "approve_native_integration",
            Self::NativeIntegrationApply => "apply_native_integration",
            Self::NativeIntegrationStatus => "native_integration_status",
            Self::NativeIntegrationCancel => "cancel_native_integration",
            Self::NativeIntegrationWorktreeInventory => "worktree_inventory",
            Self::NativeIntegrationWorktreeInspect => "worktree_cleanup_inspect",
            Self::NativeIntegrationWorktreeConfirm => "worktree_cleanup_confirm",
            Self::NativeIntegrationWorktreeRemove => "worktree_cleanup_remove",
            Self::NativeIntegrationWorktreeReconcile => "worktree_cleanup_reconcile",
            Self::FeedbackDiagnostics => "feedback_diagnostics",
            Self::FeedbackGet => "feedback_get",
            Self::FeedbackExpand => "feedback_expand",
            Self::FeedbackList => "feedback_list",
            Self::FeedbackImpact => "feedback_impact",
            Self::FeedbackAdvisoryCycle => "feedback_advisory_cycle",
            Self::AffectedTests => "affected_tests",
            Self::TestResults => "test_results",
            Self::CodeExactOccurrence => "code_exact_occurrence",
            Self::CodePhraseSearch => "code_phrase_search",
            Self::CodeSymbolSearch => "code_symbol_search",
            Self::CodeSignatureSearch => "code_signature_search",
            Self::CodeImplementations => "code_implementations",
            Self::CodeTypeHierarchy => "code_type_hierarchy",
            Self::CodeCallers => "code_callers",
            Self::CodeCallees => "code_callees",
            Self::CodeFacets => "code_facets",
            Self::CodeTimeline => "code_timeline",
            Self::CodeDeclaration => "code_declaration",
            Self::CodeDefinition => "code_definition",
            Self::CodeTypeDefinition => "code_type_definition",
            Self::CodeReferences => "code_references",
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
            Self::HealthDelta => "health_delta",
            Self::StorageStatus => "storage_status",
            Self::DiagnosticsRead => "diagnostics_read",
            Self::ObservatoryRead => "observatory_read",
            Self::ConfigurationList => "configuration_list",
            Self::ConfigurationExplain => "configuration_explain",
            Self::ConfigurationGet => "configuration_get",
            Self::ConfigurationSet => "configuration_set",
            Self::ConfigurationUnset => "configuration_unset",
            Self::ConfigurationBatch => "configuration_batch",
            Self::ConfigurationWriteCredential => "configuration_write_credential",
            Self::ConfigurationObservedState => "configuration_observed_state",
            Self::ConfigurationProtectedPreview => "configuration_protected_preview",
            Self::ConfigurationProtectedApply => "configuration_protected_apply",
            Self::ConfigurationRollbackPreview => "configuration_rollback_preview",
            Self::ConfigurationRollbackApply => "configuration_rollback_apply",
            Self::ConfigurationAudit => "configuration_audit",
            Self::ContextScoutStatus => "context_scout_status",
            Self::ContextScoutRecent => "context_scout_recent",
            Self::ContextScoutExplain => "context_scout_explain",
            Self::ContextScoutCapability => "context_scout_capability",
            Self::ContextScoutBudget => "context_scout_budget",
            Self::ContextScoutPause => "context_scout_pause",
            Self::ContextScoutResume => "context_scout_resume",
            Self::ContextScoutCancel => "context_scout_cancel",
            Self::ContextScoutClaim => "context_scout_claim",
            Self::ContextScoutDelivery => "context_scout_delivery",
            Self::ContextScoutFeedback => "context_scout_feedback",
        }
    }

    /// Whether the operation is addressed under `/code/{operation}`.
    ///
    /// This is not an owner-kind question: the callable-code router also
    /// carries the five search operations a Primitive owner answers, so the
    /// route membership has to be stated once and consulted in both
    /// polarities.
    pub const fn is_callable_code_route(self) -> bool {
        matches!(
            self,
            Self::CodeExactOccurrence
                | Self::CodePhraseSearch
                | Self::CodeSymbolSearch
                | Self::CodeSignatureSearch
                | Self::CodeImplementations
                | Self::CodeTypeHierarchy
                | Self::CodeCallers
                | Self::CodeCallees
                | Self::CodeFacets
                | Self::CodeTimeline
                | Self::CodeDeclaration
                | Self::CodeDefinition
                | Self::CodeTypeDefinition
                | Self::CodeReferences
        )
    }

    /// Whether this canonical operation has a public HTTP catalog binding.
    ///
    /// Git preview/apply remain in the shared operation family but are
    /// intentionally exposed through CLI/MCP mutation bindings only.
    pub const fn is_http_exposed(self) -> bool {
        !matches!(
            self,
            Self::GitPreview
                | Self::GitApply
                | Self::NativeIntegrationStackSnapshot
                | Self::NativeIntegrationPreflight
                | Self::NativeIntegrationApprove
                | Self::NativeIntegrationApply
                | Self::NativeIntegrationStatus
                | Self::NativeIntegrationCancel
                | Self::ObservatoryRead
        )
    }

    pub fn route_path(self) -> String {
        match self {
            Self::GitHubStackSignalExpand => "/github-stack/signal-expand".to_owned(),
            operation if operation.owner_kind() == HttpApplicationOwnerKind::Git => {
                format!(
                    "/git/{}",
                    operation
                        .as_str()
                        .strip_prefix("git_")
                        .expect("Git HTTP operation names use the git_ prefix")
                )
            }
            operation if operation.owner_kind() == HttpApplicationOwnerKind::NativeIntegration => {
                format!("/native-integration/{}", operation.as_str())
            }
            Self::AffectedTests => "/tests/affected".to_owned(),
            Self::TestResults => "/tests/results".to_owned(),
            operation if operation.owner_kind() == HttpApplicationOwnerKind::Feedback => {
                format!(
                    "/feedback/{}",
                    operation
                        .as_str()
                        .strip_prefix("feedback_")
                        .expect("feedback HTTP operation names use the feedback_ prefix")
                )
            }
            operation if operation.is_callable_code_route() => {
                format!("/code/{}", operation.as_str())
            }
            operation if operation.owner_kind() == HttpApplicationOwnerKind::Primitive => {
                format!("/primitives/{}", operation.as_str())
            }
            operation if operation.owner_kind() == HttpApplicationOwnerKind::Configuration => {
                format!("/configuration/{}", operation.as_str())
            }
            Self::ObservatoryRead => "/observatory/read".to_owned(),
            operation => format!("/context-scout/{}", operation.as_str()),
        }
    }

    /// Public route mounted beneath the per-project application prefix.
    ///
    /// [`route_path`](Self::route_path) remains the relative Axum route used
    /// by [`application_router`]. SDK and discovery contracts need the full
    /// path accepted by the daemon HTTP service.
    pub fn application_route_path(self) -> String {
        format!("/application{}", self.route_path())
    }
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
        let path = match HttpApplicationOperation::from_catalog_name(binding.operation().as_str()) {
            Some(operation) if operation.is_http_exposed() => operation.application_route_path(),
            Some(_) => continue,
            None => {
                let Some(operation) =
                    RetainedSurfaceOperation::from_operation_name(binding.operation().as_str())
                        .filter(|operation| operation.is_callable())
                else {
                    continue;
                };
                crate::retained::retained_application_route_path(operation)
            }
        };
        documents.push(HttpRouteDocumentV1 {
            method: "POST",
            path,
            operation: binding.operation().as_str().to_owned(),
            capability_id: capability.capability_id().as_str().to_owned(),
            binding_id: binding.binding_id().as_str().to_owned(),
            request_schema: capability.request_schema().schema_id().as_str().to_owned(),
            request_schema_revision: capability.request_schema().revision(),
            result_schema: capability.result_schema().schema_id().as_str().to_owned(),
            result_schema_revision: capability.result_schema().revision(),
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

pub type HttpApplicationInvocationFuture = Pin<
    Box<
        dyn Future<Output = Result<CanonicalInvocationResult<Value>, ApplicationContractError>>
            + Send
            + 'static,
    >,
>;

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

    fn invoke_observatory(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture;

    fn invoke_configuration(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture;

    fn invoke_context_scout(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture;

    fn invoke_native_integration(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture;
}

impl<F, Fut> HttpApplicationOwners for F
where
    F: Fn(HttpApplicationRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<CanonicalInvocationResult<Value>, ApplicationContractError>>
        + Send
        + 'static,
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

    fn invoke_observatory(
        &self,
        request: HttpApplicationRequest,
    ) -> HttpApplicationInvocationFuture {
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

    fn invoke_native_integration(
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
        ApplicationProblemKind::Conflict
        | ApplicationProblemKind::PartialEffect
        | ApplicationProblemKind::Stale => StatusCode::CONFLICT,
        ApplicationProblemKind::Unsupported => StatusCode::UNPROCESSABLE_ENTITY,
        ApplicationProblemKind::ResetRequired | ApplicationProblemKind::Unavailable => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        ApplicationProblemKind::ExecutionFailed => StatusCode::INTERNAL_SERVER_ERROR,
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

/// Report an internal contract violation before a canonical problem envelope
/// exists. There is no truthful application-problem body to return in this
/// case, because constructing that body is what failed.
pub(crate) fn application_contract_error_response(_error: ApplicationContractError) -> Response {
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

/// Build the stable HTTP envelope for a problem owned by transport admission.
///
/// The executable uses this for failures that occur before an application
/// router can mint its own request context, such as project-route resolution.
pub fn adapter_problem_response(request_id: RequestId, problem: ApplicationProblem) -> Response {
    match adapter_problem(request_id, problem) {
        Ok(problem) => application_problem_response(problem),
        Err(error) => application_contract_error_response(error),
    }
}

pub(crate) fn invalid_request_problem(
    request_id: RequestId,
    code: &'static str,
    message: &'static str,
) -> Result<ApplicationProblemEnvelope, ApplicationContractError> {
    let diagnostic = SafeDiagnostic::new(code, message)?;
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
) -> Result<ApplicationProblemEnvelope, ApplicationContractError> {
    let contract = ResultContractRef::new(
        SchemaId::new("schema.tracedecay.http.adapter-problem.v1")?,
        1,
    )?;
    ApplicationProblemEnvelope::new(contract, request_id, problem)
        .map(|envelope| envelope.with_owning_layer(ProblemOwningLayer::Adapter))
}

pub(crate) fn invalid_request_response(
    request_id: RequestId,
    code: &'static str,
    message: &'static str,
) -> Response {
    match invalid_request_problem(request_id, code, message) {
        Ok(problem) => application_problem_response(problem),
        Err(error) => application_contract_error_response(error),
    }
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
        .route(
            "/github-stack/signal-expand",
            post(github_stack_signal_expand::<O>),
        )
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
        .route(
            "/native-integration/{operation}",
            post(native_integration_operation::<O>),
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
    github_stack_signal_expand => HttpApplicationOperation::GitHubStackSignalExpand;
}

fn parse_primitive_read_operation(operation: &str) -> Option<HttpApplicationOperation> {
    HttpApplicationOperation::from_catalog_name(operation).filter(|operation| {
        operation.owner_kind() == HttpApplicationOwnerKind::Primitive
            && *operation != HttpApplicationOperation::TestResults
            && !operation.is_callable_code_route()
    })
}

fn parse_callable_code_operation(operation: &str) -> Option<HttpApplicationOperation> {
    HttpApplicationOperation::from_catalog_name(operation)
        .filter(|operation| operation.is_callable_code_route())
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
                    return adapter_problem_response(
                        request_id.0,
                        ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
                    );
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
    native_integration_operation => parse_native_integration_operation;
}

fn parse_native_integration_operation(operation: &str) -> Option<HttpApplicationOperation> {
    HttpApplicationOperation::from_catalog_name(operation)
        .filter(|operation| operation.owner_kind() == HttpApplicationOwnerKind::NativeIntegration)
        .filter(|operation| operation.is_http_exposed())
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
        HttpApplicationOwnerKind::Observatory => owners.invoke_observatory(request),
        HttpApplicationOwnerKind::Configuration => owners.invoke_configuration(request),
        HttpApplicationOwnerKind::ContextScout => owners.invoke_context_scout(request),
        HttpApplicationOwnerKind::NativeIntegration => owners.invoke_native_integration(request),
    };
    match invocation.await {
        Ok(result) => result.into_http_response(),
        Err(error) => application_contract_error_response(error),
    }
}

#[cfg(test)]
mod tests;

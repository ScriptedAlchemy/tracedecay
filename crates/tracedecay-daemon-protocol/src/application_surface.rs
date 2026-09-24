//! Owner-neutral application-surface request, result, and parse contracts.
//!
//! These types sit immediately above the daemon invocation envelope. They do
//! not themselves serialize on the socket. [`crate::DaemonInvocationPayload`]
//! does. They name the reviewed request body the MCP/CLI/HTTP adapters
//! share. They live here so `tracedecay-mcp` can own the generic adapter
//! without depending on daemon-service. Execution stays in daemon-service.

mod git;
mod invocation;
mod retained;
mod source_edit;

pub use retained::decode_retained_request;
pub use source_edit::{is_source_edit_operation, parse_source_edit_arguments};

pub use invocation::{
    application_delivery_route, application_outcome_value, application_response,
    application_surface_cancellation_policy,
    application_surface_feedback_is_observable, application_surface_feedback_operation,
    invoke_application_surface, parse_application_surface_invocation_payload,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracedecay_contracts::catalog_composition::CatalogCompositionError;
use tracedecay_contracts::feedback::{
    FeedbackAdvisoryCycleSurfaceRequestV1, FeedbackProximityReadRequestV1,
    TestResultsSurfaceRequestV1,
};
use tracedecay_contracts::git::{
    GitApplySurfaceRequest, GitHubStackSignalExpandSurfaceRequest, GitPreviewSurfaceRequest,
    NativeWorktreeSurfaceRequest,
};
use tracedecay_contracts::retrieval::{
    CallChainPrimitiveRequest, DiagnosticsPrimitiveRequest, FileDependentsPrimitiveRequest,
    HealthDeltaRequest, ModuleApiPrimitiveRequest, PrimitiveRequest, QualifiedNamePrimitiveRequest,
    SourceBodyPrimitiveRequest, SourceOutlinePrimitiveRequest, StorageStatusPrimitiveRequest,
};
use tracedecay_contracts::{
    ApplicationContractError, ApplicationResult, CallableCodeSurfaceRequest,
    CodeCalleesSurfaceRequest, CodeCallersSurfaceRequest, CodeExactOccurrenceSurfaceRequest,
    CodeFacetSurfaceRequest, CodeImplementationsSurfaceRequest, CodeNavigationSurfaceRequest,
    CodePhraseSearchSurfaceRequest, CodeSignatureSearchSurfaceRequest,
    CodeSymbolSearchSurfaceRequest, CodeTimelineSurfaceRequest, CodeTypeHierarchySurfaceRequest,
    ConfigurationWireRequestV1, HealthReadRequest, NativeIntegrationSurfaceRequest,
    ObservatoryReadRequestV1, PrimitiveCodeSurfaceRequest, SessionLookupRequest,
    SourceEditInvocationV1, SourceEditReconciliationInvocationV1, SourceEditRollbackInvocationV1,
    SourceLinesRequest, configuration_wire_request_from_invocation_payload,
};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, CatalogValidationError, IdentifierError,
};

use crate::output_format::{RequestedOutputFormat, requested_output_format};
use crate::surface::GitReadSurfaceRequest;
use tracedecay_contracts::context_scout::ContextScoutSurfaceRequestV1;
use tracedecay_contracts::retained_surfaces::{RetainedSurfaceOperation, RetainedSurfaceRequestV1};

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
    /// `detail` names the field or shape the reviewed schema refused. It
    /// echoes only the caller's own request, never store or session content.
    #[error("application surface request does not match its reviewed schema: {detail}")]
    InvalidSurfaceRequest { detail: String },
    #[error("owning daemon application service is unavailable")]
    DaemonUnavailable,
    /// No daemon accepted the connection after the transport's restart grace;
    /// the request was never sent. Surfaced as a dispatch error, not a
    /// retryable problem envelope, so dispatchers fail fast with the typed
    /// connect diagnostic instead of re-dispatching until their deadline.
    #[error("{detail}")]
    DaemonUnreachable { reason_code: String, detail: String },
    #[error("application surface was not found or is not authorized")]
    UnknownOrNotAuthorized,
}

impl ApplicationSurfaceAdapterError {
    pub fn invalid_request(detail: impl std::fmt::Display) -> Self {
        Self::InvalidSurfaceRequest {
            detail: detail.to_string(),
        }
    }
}

/// Transport keys every surface adapter accepts but no reviewed application
/// request schema declares. `format` selects the rendered output and
/// `__mcp_request_id` carries protocol identity; both are stripped here so
/// that `deny_unknown_fields` request schemas never see them.
const SURFACE_TRANSPORT_ARGUMENT_KEYS: [&str; 2] = ["format", "__mcp_request_id"];

/// A reviewed application request body together with the presentation format
/// that travelled alongside it in the caller's argument object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationToolRequest {
    pub request: Value,
    pub requested_format: RequestedOutputFormat,
}

/// Separates transport-only metadata from the canonical application request.
#[hotpath::measure(label = "application_surface.transport_metadata")]
pub fn separate_application_tool_request(
    mut args: Value,
) -> Result<ApplicationToolRequest, ApplicationSurfaceAdapterError> {
    if let Some(format) = args.get("format")
        && !matches!(format.as_str(), Some("markdown" | "json"))
    {
        return Err(ApplicationSurfaceAdapterError::invalid_request(
            "`format` must be markdown or json",
        ));
    }
    let requested_format = requested_output_format(&args);
    if let Some(object) = args.as_object_mut() {
        for key in SURFACE_TRANSPORT_ARGUMENT_KEYS {
            object.remove(key);
        }
    }
    Ok(ApplicationToolRequest {
        request: args,
        requested_format,
    })
}

/// Adapts shipped public MCP/CLI argument shapes into canonical application
/// requests after separating transport metadata.
pub fn adapt_application_tool_request(
    tool_name: &str,
    args: Value,
) -> Result<ApplicationToolRequest, ApplicationSurfaceAdapterError> {
    let mut separated = separate_application_tool_request(args)?;
    if tool_name == "tracedecay_diagnostics" {
        separated.request = adapt_shipped_diagnostics_request(&separated.request)?;
    }
    Ok(separated)
}

/// Adapts the flat `tracedecay_diagnostics` arguments shipped on the public
/// MCP/CLI protocol into the canonical diagnostics-read request. This adapter
/// is retained because that external shape shipped, not for a branch-local
/// compatibility phase.
fn adapt_shipped_diagnostics_request(
    args: &Value,
) -> Result<Value, ApplicationSurfaceAdapterError> {
    let scope = match args
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace")
    {
        "workspace" => serde_json::json!("workspace"),
        "package" => {
            return Err(ApplicationSurfaceAdapterError::invalid_request(
                "`scope` package is not supported for diagnostics",
            ));
        }
        "file" => serde_json::json!({
            "file": args
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ApplicationSurfaceAdapterError::invalid_request(
                        "`path` is required when `scope` is file",
                    )
                })?
        }),
        other => {
            return Err(ApplicationSurfaceAdapterError::invalid_request(format!(
                "`scope` `{other}` is not one of workspace or file"
            )));
        }
    };
    let maximum_diagnostics = match args.get("maximum_diagnostics") {
        Some(maximum_diagnostics) => maximum_diagnostics.clone(),
        None => Value::from(
            tracedecay_contracts::application_operation_default_page_size(
                ApplicationSurfaceOperation::DiagnosticsRead,
            ),
        ),
    };
    Ok(serde_json::json!({
        "scope": scope,
        "maximum_diagnostics": maximum_diagnostics,
        "cursor": args.get("cursor").cloned().unwrap_or(Value::Null),
    }))
}

pub type FeedbackSurfaceRequest = tracedecay_contracts::feedback::FeedbackHandleRequestV1;

#[derive(Debug, Serialize, Deserialize)]
pub enum ApplicationSurfaceRequest {
    GitRead(GitReadSurfaceRequest),
    GitPreview(GitPreviewSurfaceRequest),
    GitApply(GitApplySurfaceRequest),
    GitHubStackSignalExpand(GitHubStackSignalExpandSurfaceRequest),
    NativeIntegration(NativeIntegrationSurfaceRequest),
    /// Every handle-addressed feedback read, including impact and affected
    /// tests: the operation selects the daemon route, the handle is the body.
    Feedback(FeedbackSurfaceRequest),
    FeedbackAdvisoryCycle(FeedbackAdvisoryCycleSurfaceRequestV1),
    FeedbackProximity(FeedbackProximityReadRequestV1),
    TestResults(TestResultsSurfaceRequestV1),
    CallableCode(CallableCodeSurfaceRequest),
    PrimitiveCode(PrimitiveCodeSurfaceRequest),
    Primitive(PrimitiveRequest),
    ObservatoryRead(ObservatoryReadRequestV1),
    Configuration(ConfigurationWireRequestV1),
    ContextScout(ContextScoutSurfaceRequestV1),
    SourceEdit(SourceEditInvocationV1),
    SourceEditReconcile(SourceEditReconciliationInvocationV1),
    SourceEditRollback(SourceEditRollbackInvocationV1),
    Retained(RetainedSurfaceRequestV1),
    /// A graph or port read's argument object. Its owning handler decodes the
    /// typed request so argument diagnostics stay the handler's own.
    GraphTool(serde_json::Map<String, Value>),
}

pub struct ApplicationSurfaceInvocationResult {
    pub operation: ApplicationSurfaceOperation,
    pub binding_id: tracedecay_tool_catalog::BindingId,
    pub result: ApplicationResult<Value>,
    pub requested_format: RequestedOutputFormat,
}

impl ApplicationSurfaceRequest {
    pub fn matches(&self, operation: ApplicationSurfaceOperation) -> bool {
        if let Self::SourceEdit(invocation) = self {
            return source_edit::source_edit_kind(operation) == Some(invocation.edit.kind());
        }
        if let Self::Retained(request) = self {
            return request.operation().as_str() == operation.as_str();
        }
        if let Self::GraphTool(_) = self {
            return operation.is_graph_tool();
        }
        matches!(
            (self, operation),
            (
                Self::GitRead(_),
                ApplicationSurfaceOperation::GitStatus
                    | ApplicationSurfaceOperation::GitDiff
                    | ApplicationSurfaceOperation::GitHistory
                    | ApplicationSurfaceOperation::GitBlame
                    | ApplicationSurfaceOperation::GitHunks
            ) | (
                Self::GitHubStackSignalExpand(_),
                ApplicationSurfaceOperation::GitHubStackSignalExpand
            ) | (Self::GitPreview(_), ApplicationSurfaceOperation::GitPreview)
                | (Self::GitApply(_), ApplicationSurfaceOperation::GitApply)
                | (
                    Self::NativeIntegration(_),
                    ApplicationSurfaceOperation::NativeIntegrationStackSnapshot
                        | ApplicationSurfaceOperation::NativeIntegrationPreflight
                        | ApplicationSurfaceOperation::NativeIntegrationApprove
                        | ApplicationSurfaceOperation::NativeIntegrationApply
                        | ApplicationSurfaceOperation::NativeIntegrationStatus
                        | ApplicationSurfaceOperation::NativeIntegrationCancel
                        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory
                        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect
                        | ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm
                        | ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove
                        | ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile
                )
                | (
                    Self::Feedback(_),
                    ApplicationSurfaceOperation::FeedbackDiagnostics
                        | ApplicationSurfaceOperation::FeedbackGet
                        | ApplicationSurfaceOperation::FeedbackExpand
                        | ApplicationSurfaceOperation::FeedbackList
                        | ApplicationSurfaceOperation::FeedbackImpact
                        | ApplicationSurfaceOperation::AffectedTests
                )
                | (
                    Self::FeedbackAdvisoryCycle(_),
                    ApplicationSurfaceOperation::FeedbackAdvisoryCycle
                )
                | (
                    Self::FeedbackProximity(_),
                    ApplicationSurfaceOperation::FeedbackProximity
                )
                | (
                    Self::TestResults(_),
                    ApplicationSurfaceOperation::TestResults
                )
                | (
                    Self::CallableCode(CallableCodeSurfaceRequest::ExactOccurrence(_)),
                    ApplicationSurfaceOperation::CodeExactOccurrence
                )
                | (
                    Self::CallableCode(CallableCodeSurfaceRequest::PhraseSearch(_)),
                    ApplicationSurfaceOperation::CodePhraseSearch
                )
                | (
                    Self::CallableCode(CallableCodeSurfaceRequest::Callees(_)),
                    ApplicationSurfaceOperation::CodeCallees
                )
                | (
                    Self::CallableCode(CallableCodeSurfaceRequest::Facets(_)),
                    ApplicationSurfaceOperation::CodeFacets
                )
                | (
                    Self::CallableCode(CallableCodeSurfaceRequest::Timeline(_)),
                    ApplicationSurfaceOperation::CodeTimeline
                )
                | (
                    Self::CallableCode(CallableCodeSurfaceRequest::Declaration(_)),
                    ApplicationSurfaceOperation::CodeDeclaration
                )
                | (
                    Self::CallableCode(CallableCodeSurfaceRequest::TypeDefinition(_)),
                    ApplicationSurfaceOperation::CodeTypeDefinition
                )
                | (
                    Self::CallableCode(CallableCodeSurfaceRequest::References(_)),
                    ApplicationSurfaceOperation::CodeReferences
                )
                | (
                    Self::PrimitiveCode(PrimitiveCodeSurfaceRequest::SymbolSearch(_)),
                    ApplicationSurfaceOperation::CodeSymbolSearch
                )
                | (
                    Self::PrimitiveCode(PrimitiveCodeSurfaceRequest::SignatureSearch(_)),
                    ApplicationSurfaceOperation::CodeSignatureSearch
                )
                | (
                    Self::PrimitiveCode(PrimitiveCodeSurfaceRequest::Implementations(_)),
                    ApplicationSurfaceOperation::CodeImplementations
                )
                | (
                    Self::PrimitiveCode(PrimitiveCodeSurfaceRequest::TypeHierarchy(_)),
                    ApplicationSurfaceOperation::CodeTypeHierarchy
                )
                | (
                    Self::PrimitiveCode(PrimitiveCodeSurfaceRequest::Callers(_)),
                    ApplicationSurfaceOperation::CodeCallers
                )
                | (
                    Self::Primitive(PrimitiveRequest::SessionLookup(_)),
                    ApplicationSurfaceOperation::SessionLookup
                )
                | (
                    Self::Primitive(PrimitiveRequest::QualifiedName(_)),
                    ApplicationSurfaceOperation::QualifiedName
                )
                | (
                    Self::Primitive(PrimitiveRequest::CallChain(_)),
                    ApplicationSurfaceOperation::CallChain
                )
                | (
                    Self::Primitive(PrimitiveRequest::FileDependents(_)),
                    ApplicationSurfaceOperation::FileDependents
                )
                | (
                    Self::Primitive(PrimitiveRequest::SourceLines(_)),
                    ApplicationSurfaceOperation::SourceLines
                )
                | (
                    Self::Primitive(PrimitiveRequest::SourceBody(_)),
                    ApplicationSurfaceOperation::SourceBody
                )
                | (
                    Self::Primitive(PrimitiveRequest::SourceOutline(_)),
                    ApplicationSurfaceOperation::SourceOutline
                )
                | (
                    Self::Primitive(PrimitiveRequest::ModuleApi(_)),
                    ApplicationSurfaceOperation::ModuleApi
                )
                | (
                    Self::Primitive(PrimitiveRequest::HealthRead(_)),
                    ApplicationSurfaceOperation::HealthRead
                )
                | (
                    Self::Primitive(PrimitiveRequest::HealthDelta(_)),
                    ApplicationSurfaceOperation::HealthDelta
                )
                | (
                    Self::Primitive(PrimitiveRequest::StorageStatus(_)),
                    ApplicationSurfaceOperation::StorageStatus
                )
                | (
                    Self::Primitive(PrimitiveRequest::DiagnosticsRead(_)),
                    ApplicationSurfaceOperation::DiagnosticsRead
                )
                | (
                    Self::ObservatoryRead(_),
                    ApplicationSurfaceOperation::ObservatoryRead
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::List(_)),
                    ApplicationSurfaceOperation::ConfigurationList
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::Get(_)),
                    ApplicationSurfaceOperation::ConfigurationGet
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::Set(_)),
                    ApplicationSurfaceOperation::ConfigurationSet
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::Unset(_)),
                    ApplicationSurfaceOperation::ConfigurationUnset
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::Batch(_)),
                    ApplicationSurfaceOperation::ConfigurationBatch
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::ObservedState(_)),
                    ApplicationSurfaceOperation::ConfigurationObservedState
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::ProtectedPreview(_)),
                    ApplicationSurfaceOperation::ConfigurationProtectedPreview
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::ProtectedApply(_)),
                    ApplicationSurfaceOperation::ConfigurationProtectedApply
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::RollbackPreview(_)),
                    ApplicationSurfaceOperation::ConfigurationRollbackPreview
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::RollbackApply(_)),
                    ApplicationSurfaceOperation::ConfigurationRollbackApply
                )
                | (
                    Self::Configuration(ConfigurationWireRequestV1::Audit(_)),
                    ApplicationSurfaceOperation::ConfigurationAudit
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Status(_)),
                    ApplicationSurfaceOperation::ContextScoutStatus
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Recent(_)),
                    ApplicationSurfaceOperation::ContextScoutRecent
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Explain(_)),
                    ApplicationSurfaceOperation::ContextScoutExplain
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Capability(_)),
                    ApplicationSurfaceOperation::ContextScoutCapability
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Budget(_)),
                    ApplicationSurfaceOperation::ContextScoutBudget
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Pause(_)),
                    ApplicationSurfaceOperation::ContextScoutPause
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Resume(_)),
                    ApplicationSurfaceOperation::ContextScoutResume
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Cancel(_)),
                    ApplicationSurfaceOperation::ContextScoutCancel
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Claim(_)),
                    ApplicationSurfaceOperation::ContextScoutClaim
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Delivery(_)),
                    ApplicationSurfaceOperation::ContextScoutDelivery
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequestV1::Feedback(_)),
                    ApplicationSurfaceOperation::ContextScoutFeedback
                )
                | (
                    Self::SourceEditReconcile(_),
                    ApplicationSurfaceOperation::SourceEditReconcile
                )
                | (
                    Self::SourceEditRollback(_),
                    ApplicationSurfaceOperation::SourceEditRollback
                )
        )
    }
}

/// Parse one native-integration request into its exact typed shape.
///
/// `deny_unknown_fields` on every request type means an unexpected key is a
/// rejection rather than a silently ignored hint.
fn parse_native_integration_surface_request(
    operation: ApplicationSurfaceOperation,
    value: Value,
) -> Result<NativeIntegrationSurfaceRequest, ApplicationSurfaceAdapterError> {
    let invalid = ApplicationSurfaceAdapterError::invalid_request;
    match operation {
        ApplicationSurfaceOperation::NativeIntegrationStackSnapshot => {
            serde_json::from_value(value)
                .map(NativeIntegrationSurfaceRequest::StackSnapshot)
                .map_err(invalid)
        }
        ApplicationSurfaceOperation::NativeIntegrationPreflight => serde_json::from_value(value)
            .map(NativeIntegrationSurfaceRequest::Preflight)
            .map_err(invalid),
        ApplicationSurfaceOperation::NativeIntegrationApprove => serde_json::from_value(value)
            .map(NativeIntegrationSurfaceRequest::Approve)
            .map_err(invalid),
        ApplicationSurfaceOperation::NativeIntegrationApply => serde_json::from_value(value)
            .map(NativeIntegrationSurfaceRequest::Apply)
            .map_err(invalid),
        ApplicationSurfaceOperation::NativeIntegrationStatus => serde_json::from_value(value)
            .map(NativeIntegrationSurfaceRequest::Status)
            .map_err(invalid),
        ApplicationSurfaceOperation::NativeIntegrationCancel => serde_json::from_value(value)
            .map(NativeIntegrationSurfaceRequest::Cancel)
            .map_err(invalid),
        ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory => {
            serde_json::from_value(value)
                .map(NativeWorktreeSurfaceRequest::Inventory)
                .map(NativeIntegrationSurfaceRequest::Worktree)
                .map_err(invalid)
        }
        ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect => {
            serde_json::from_value(value)
                .map(NativeWorktreeSurfaceRequest::Inspect)
                .map(NativeIntegrationSurfaceRequest::Worktree)
                .map_err(invalid)
        }
        ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm => {
            serde_json::from_value(value)
                .map(NativeWorktreeSurfaceRequest::Confirm)
                .map(NativeIntegrationSurfaceRequest::Worktree)
                .map_err(invalid)
        }
        ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove => {
            serde_json::from_value(value)
                .map(NativeWorktreeSurfaceRequest::Remove)
                .map(NativeIntegrationSurfaceRequest::Worktree)
                .map_err(invalid)
        }
        ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile => {
            serde_json::from_value(value)
                .map(NativeWorktreeSurfaceRequest::Reconcile)
                .map(NativeIntegrationSurfaceRequest::Worktree)
                .map_err(invalid)
        }
        _ => Err(ApplicationSurfaceAdapterError::invalid_request(
            "operation is not a native-integration surface",
        )),
    }
}

#[hotpath::measure(label = "application_surface.parse")]
pub fn parse_application_surface_request(
    operation: ApplicationSurfaceOperation,
    value: Value,
) -> Result<ApplicationSurfaceRequest, ApplicationSurfaceAdapterError> {
    match operation {
        ApplicationSurfaceOperation::GitStatus
        | ApplicationSurfaceOperation::GitDiff
        | ApplicationSurfaceOperation::GitHistory
        | ApplicationSurfaceOperation::GitBlame
        | ApplicationSurfaceOperation::GitHunks => {
            git::parse_git_read_surface_request(operation, value)
                .map(ApplicationSurfaceRequest::GitRead)
        }
        ApplicationSurfaceOperation::GitHubStackSignalExpand => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::GitHubStackSignalExpand)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::GitPreview => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::GitPreview)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::GitApply => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::GitApply)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::NativeIntegrationStackSnapshot
        | ApplicationSurfaceOperation::NativeIntegrationPreflight
        | ApplicationSurfaceOperation::NativeIntegrationApprove
        | ApplicationSurfaceOperation::NativeIntegrationApply
        | ApplicationSurfaceOperation::NativeIntegrationStatus
        | ApplicationSurfaceOperation::NativeIntegrationCancel
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove
        | ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile => {
            parse_native_integration_surface_request(operation, value)
                .map(ApplicationSurfaceRequest::NativeIntegration)
        }
        ApplicationSurfaceOperation::TestResults => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::TestResults)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::CodeExactOccurrence => {
            serde_json::from_value::<CodeExactOccurrenceSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::ExactOccurrence)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodePhraseSearch => {
            serde_json::from_value::<CodePhraseSearchSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::PhraseSearch)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeSymbolSearch => {
            serde_json::from_value::<CodeSymbolSearchSurfaceRequest>(value)
                .map(PrimitiveCodeSurfaceRequest::SymbolSearch)
                .map(ApplicationSurfaceRequest::PrimitiveCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeSignatureSearch => {
            serde_json::from_value::<CodeSignatureSearchSurfaceRequest>(value)
                .map(PrimitiveCodeSurfaceRequest::SignatureSearch)
                .map(ApplicationSurfaceRequest::PrimitiveCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeImplementations => {
            serde_json::from_value::<CodeImplementationsSurfaceRequest>(value)
                .map(PrimitiveCodeSurfaceRequest::Implementations)
                .map(ApplicationSurfaceRequest::PrimitiveCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeTypeHierarchy => {
            serde_json::from_value::<CodeTypeHierarchySurfaceRequest>(value)
                .map(PrimitiveCodeSurfaceRequest::TypeHierarchy)
                .map(ApplicationSurfaceRequest::PrimitiveCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeCallers => {
            serde_json::from_value::<CodeCallersSurfaceRequest>(value)
                .map(PrimitiveCodeSurfaceRequest::Callers)
                .map(ApplicationSurfaceRequest::PrimitiveCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeCallees => {
            serde_json::from_value::<CodeCalleesSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::Callees)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeFacets => {
            serde_json::from_value::<CodeFacetSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::Facets)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeTimeline => {
            serde_json::from_value::<CodeTimelineSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::Timeline)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeDeclaration => {
            serde_json::from_value::<CodeNavigationSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::Declaration)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeTypeDefinition => {
            serde_json::from_value::<CodeNavigationSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::TypeDefinition)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CodeReferences => {
            serde_json::from_value::<CodeNavigationSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::References)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::SessionLookup => {
            serde_json::from_value::<SessionLookupRequest>(value)
                .map(PrimitiveRequest::SessionLookup)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::QualifiedName => {
            serde_json::from_value::<QualifiedNamePrimitiveRequest>(value)
                .map(PrimitiveRequest::QualifiedName)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::CallChain => {
            serde_json::from_value::<CallChainPrimitiveRequest>(value)
                .map(PrimitiveRequest::CallChain)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::FileDependents => {
            serde_json::from_value::<FileDependentsPrimitiveRequest>(value)
                .map(PrimitiveRequest::FileDependents)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::SourceLines => {
            serde_json::from_value::<SourceLinesRequest>(value)
                .map(PrimitiveRequest::SourceLines)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::SourceBody => {
            serde_json::from_value::<SourceBodyPrimitiveRequest>(value)
                .map(PrimitiveRequest::SourceBody)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::SourceOutline => {
            serde_json::from_value::<SourceOutlinePrimitiveRequest>(value)
                .map(PrimitiveRequest::SourceOutline)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::ModuleApi => {
            serde_json::from_value::<ModuleApiPrimitiveRequest>(value)
                .map(PrimitiveRequest::ModuleApi)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::HealthRead => {
            serde_json::from_value::<HealthReadRequest>(value)
                .map(PrimitiveRequest::HealthRead)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::HealthDelta => {
            serde_json::from_value::<HealthDeltaRequest>(value)
                .map(PrimitiveRequest::HealthDelta)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::StorageStatus => {
            serde_json::from_value::<StorageStatusPrimitiveRequest>(value)
                .map(PrimitiveRequest::StorageStatus)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::DiagnosticsRead => {
            serde_json::from_value::<DiagnosticsPrimitiveRequest>(value)
                .map(PrimitiveRequest::DiagnosticsRead)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::ObservatoryRead => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::ObservatoryRead)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ConfigurationList
        | ApplicationSurfaceOperation::ConfigurationGet
        | ApplicationSurfaceOperation::ConfigurationSet
        | ApplicationSurfaceOperation::ConfigurationUnset
        | ApplicationSurfaceOperation::ConfigurationBatch
        | ApplicationSurfaceOperation::ConfigurationObservedState
        | ApplicationSurfaceOperation::ConfigurationProtectedPreview
        | ApplicationSurfaceOperation::ConfigurationProtectedApply
        | ApplicationSurfaceOperation::ConfigurationRollbackPreview
        | ApplicationSurfaceOperation::ConfigurationRollbackApply
        | ApplicationSurfaceOperation::ConfigurationAudit => {
            configuration_wire_request_from_invocation_payload(operation.as_str(), value)
                .map(ApplicationSurfaceRequest::Configuration)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::ContextScoutStatus => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Status)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ContextScoutRecent => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Recent)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ContextScoutExplain => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Explain)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ContextScoutCapability => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Capability)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ContextScoutBudget => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Budget)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ContextScoutPause => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Pause)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ContextScoutResume => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Resume)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ContextScoutCancel => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Cancel)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ContextScoutClaim => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Claim)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ContextScoutDelivery => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Delivery)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::ContextScoutFeedback => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequestV1::Feedback)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::FeedbackDiagnostics
        | ApplicationSurfaceOperation::FeedbackGet
        | ApplicationSurfaceOperation::FeedbackExpand
        | ApplicationSurfaceOperation::FeedbackList
        | ApplicationSurfaceOperation::FeedbackImpact
        | ApplicationSurfaceOperation::AffectedTests => {
            let request: FeedbackSurfaceRequest = serde_json::from_value(value)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)?;
            Ok(ApplicationSurfaceRequest::Feedback(
                FeedbackSurfaceRequest::new(request.request_handle)
                    .map_err(|_| ApplicationSurfaceAdapterError::InvalidRequestHandle)?,
            ))
        }
        ApplicationSurfaceOperation::FeedbackAdvisoryCycle => {
            let request: FeedbackAdvisoryCycleSurfaceRequestV1 = serde_json::from_value(value)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)?;
            request
                .validate()
                .map_err(ApplicationSurfaceAdapterError::invalid_request)?;
            Ok(ApplicationSurfaceRequest::FeedbackAdvisoryCycle(request))
        }
        ApplicationSurfaceOperation::FeedbackProximity => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::FeedbackProximity)
            .map_err(ApplicationSurfaceAdapterError::invalid_request),
        ApplicationSurfaceOperation::StrReplace
        | ApplicationSurfaceOperation::MultiStrReplace
        | ApplicationSurfaceOperation::InsertAt
        | ApplicationSurfaceOperation::AstGrepRewrite
        | ApplicationSurfaceOperation::ReplaceSymbol
        | ApplicationSurfaceOperation::InsertAtSymbol
        | ApplicationSurfaceOperation::MoveSymbol
        | ApplicationSurfaceOperation::RenameSymbol
        | ApplicationSurfaceOperation::SourceEditReconcile
        | ApplicationSurfaceOperation::SourceEditRollback => {
            parse_source_edit_arguments(operation, &value)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::FactStoreCurate
        | ApplicationSurfaceOperation::FactStoreAdd
        | ApplicationSurfaceOperation::FactStoreSearch
        | ApplicationSurfaceOperation::FactStoreProbe
        | ApplicationSurfaceOperation::FactStoreRelated
        | ApplicationSurfaceOperation::FactStoreReason
        | ApplicationSurfaceOperation::FactStoreContradict
        | ApplicationSurfaceOperation::FactStoreGet
        | ApplicationSurfaceOperation::FactStoreUpdate
        | ApplicationSurfaceOperation::FactStoreRemove
        | ApplicationSurfaceOperation::FactStoreSupersede
        | ApplicationSurfaceOperation::FactStoreList
        | ApplicationSurfaceOperation::FactFeedback
        | ApplicationSurfaceOperation::MemoryStatus
        | ApplicationSurfaceOperation::SessionRefreshStatus
        | ApplicationSurfaceOperation::SessionRefreshCancel
        | ApplicationSurfaceOperation::SessionRefreshBegin
        | ApplicationSurfaceOperation::MessageSearch
        | ApplicationSurfaceOperation::SessionsFor
        | ApplicationSurfaceOperation::Workflows
        | ApplicationSurfaceOperation::LcmStatus
        | ApplicationSurfaceOperation::LcmDoctor
        | ApplicationSurfaceOperation::LcmLoadSession
        | ApplicationSurfaceOperation::LcmGrep
        | ApplicationSurfaceOperation::LcmDescribe
        | ApplicationSurfaceOperation::LcmExpand
        | ApplicationSurfaceOperation::LcmExpandQuery => {
            let retained = RetainedSurfaceOperation::from_application(operation).ok_or_else(|| {
                ApplicationSurfaceAdapterError::invalid_request("operation is not retained")
            })?;
            decode_retained_request(retained, value)
                .map(ApplicationSurfaceRequest::Retained)
                .map_err(ApplicationSurfaceAdapterError::invalid_request)
        }
        ApplicationSurfaceOperation::Node
        | ApplicationSurfaceOperation::Impact
        | ApplicationSurfaceOperation::Similar
        | ApplicationSurfaceOperation::Redundancy
        | ApplicationSurfaceOperation::RenamePreview
        | ApplicationSurfaceOperation::PortStatus
        | ApplicationSurfaceOperation::PortOrder
        | ApplicationSurfaceOperation::Todos => match value {
            Value::Object(arguments) => Ok(ApplicationSurfaceRequest::GraphTool(arguments)),
            _ => Err(ApplicationSurfaceAdapterError::invalid_request(format!(
                "invalid arguments: {} expects a JSON object",
                operation.mcp_tool_name()
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tracedecay_tool_catalog::ApplicationSurfaceOperation;

    #[test]
    fn separate_application_tool_request_strips_transport_keys() {
        let separated = separate_application_tool_request(json!({
            "path": "src/lib.rs",
            "format": "json",
            "__mcp_request_id": "request.surface.fixture"
        }))
        .expect("valid transport metadata");
        assert_eq!(separated.requested_format, RequestedOutputFormat::Json);
        assert_eq!(separated.request, json!({"path": "src/lib.rs"}));
    }

    #[test]
    fn parse_storage_status_request_is_owner_neutral() {
        let request = parse_application_surface_request(
            ApplicationSurfaceOperation::StorageStatus,
            json!({"include_details": false}),
        )
        .expect("storage status request");
        assert!(request.matches(ApplicationSurfaceOperation::StorageStatus));
        assert!(matches!(
            request,
            ApplicationSurfaceRequest::Primitive(PrimitiveRequest::StorageStatus(_))
        ));
    }
}

//! Owner-neutral application-surface request, result, and parse contracts.
//!
//! These types sit immediately above the daemon invocation envelope. They do
//! not themselves serialize on the socket — [`crate::DaemonInvocationPayload`]
//! does — but they name the reviewed request body the MCP/CLI/HTTP adapters
//! share. They live here so `tracedecay-mcp` can own the generic adapter
//! without depending on daemon-service. Execution stays in daemon-service.

mod git;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracedecay_contracts::catalog_composition::CatalogCompositionError;
use tracedecay_contracts::feedback::{
    FeedbackAdvisoryCycleSurfaceRequestV1, TestResultsSurfaceRequestV1,
};
use tracedecay_contracts::git::{
    GitApplySurfaceRequest, GitHubStackSignalExpandSurfaceRequest, GitPreviewSurfaceRequest,
    NativeWorktreeSurfaceRequest,
};
use tracedecay_contracts::retrieval::{
    CallChainPrimitiveRequest, DiagnosticsPrimitiveRequest, FileDependentsPrimitiveRequest,
    FileMetadataPrimitiveRequest, HealthDeltaRequest, ModuleApiPrimitiveRequest, PrimitiveRequest,
    QualifiedNamePrimitiveRequest, SourceBodyPrimitiveRequest, SourceOutlinePrimitiveRequest,
    StorageStatusPrimitiveRequest,
};
use tracedecay_contracts::{
    ApplicationContractError, ApplicationResult, CallableCodeSurfaceRequest,
    CodeCalleesSurfaceRequest, CodeCallersSurfaceRequest, CodeExactOccurrenceSurfaceRequest,
    CodeFacetSurfaceRequest, CodeImplementationsSurfaceRequest, CodeNavigationSurfaceRequest,
    CodePhraseSearchSurfaceRequest, CodeSignatureSearchSurfaceRequest,
    CodeSymbolSearchSurfaceRequest, CodeTimelineSurfaceRequest, CodeTypeHierarchySurfaceRequest,
    ConfigurationWireRequestV1, HealthReadRequest, NativeIntegrationSurfaceRequest,
    ObservatoryReadRequestV1, PrimitiveCodeSurfaceRequest, SessionLookupRequest,
    SourceLinesRequest, configuration_wire_request_from_invocation_payload,
};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, CatalogValidationError, IdentifierError,
};

use crate::output_format::{RequestedOutputFormat, requested_output_format};
use crate::surface::ContextScoutSurfaceRequest;
use crate::surface::GitReadSurfaceRequest;

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
    /// No daemon accepted the connection after the transport's restart grace;
    /// the request was never sent. Surfaced as a dispatch error — not a
    /// retryable problem envelope — so dispatchers fail fast with the typed
    /// connect diagnostic instead of re-dispatching until their deadline.
    #[error("{detail}")]
    DaemonUnreachable { reason_code: String, detail: String },
    #[error("application surface was not found or is not authorized")]
    UnknownOrNotAuthorized,
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
        return Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest);
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
        "package" => return Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        "file" => serde_json::json!({
            "file": args
                .get("path")
                .and_then(Value::as_str)
                .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?
        }),
        _ => return Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
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
    TestResults(TestResultsSurfaceRequestV1),
    CallableCode(CallableCodeSurfaceRequest),
    PrimitiveCode(PrimitiveCodeSurfaceRequest),
    Primitive(PrimitiveRequest),
    ObservatoryRead(ObservatoryReadRequestV1),
    Configuration(ConfigurationWireRequestV1),
    ContextScout(ContextScoutSurfaceRequest),
    Retained(tracedecay_contracts::retained_surfaces::RetainedSurfaceRequestV1),
}

pub struct ApplicationSurfaceInvocationResult {
    pub operation: ApplicationSurfaceOperation,
    pub binding_id: tracedecay_tool_catalog::BindingId,
    pub result: ApplicationResult<Value>,
    pub requested_format: RequestedOutputFormat,
}

impl ApplicationSurfaceRequest {
    pub fn matches(&self, operation: ApplicationSurfaceOperation) -> bool {
        if let Self::Retained(request) = self {
            return request.operation().as_str() == operation.as_str();
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
                    Self::CallableCode(CallableCodeSurfaceRequest::Definition(_)),
                    ApplicationSurfaceOperation::CodeDefinition
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
                    Self::Primitive(PrimitiveRequest::FileMetadata(_)),
                    ApplicationSurfaceOperation::FileMetadata
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
                    Self::Configuration(ConfigurationWireRequestV1::Explain(_)),
                    ApplicationSurfaceOperation::ConfigurationExplain
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
                    Self::Configuration(ConfigurationWireRequestV1::WriteCredential(_)),
                    ApplicationSurfaceOperation::ConfigurationWriteCredential
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
                    Self::ContextScout(ContextScoutSurfaceRequest::Status(_)),
                    ApplicationSurfaceOperation::ContextScoutStatus
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequest::Recent(_)),
                    ApplicationSurfaceOperation::ContextScoutRecent
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequest::Explain(_)),
                    ApplicationSurfaceOperation::ContextScoutExplain
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequest::Capability(_)),
                    ApplicationSurfaceOperation::ContextScoutCapability
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequest::Budget(_)),
                    ApplicationSurfaceOperation::ContextScoutBudget
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequest::Pause(_)),
                    ApplicationSurfaceOperation::ContextScoutPause
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequest::Resume(_)),
                    ApplicationSurfaceOperation::ContextScoutResume
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequest::Cancel(_)),
                    ApplicationSurfaceOperation::ContextScoutCancel
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequest::Claim(_)),
                    ApplicationSurfaceOperation::ContextScoutClaim
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequest::Delivery(_)),
                    ApplicationSurfaceOperation::ContextScoutDelivery
                )
                | (
                    Self::ContextScout(ContextScoutSurfaceRequest::Feedback(_)),
                    ApplicationSurfaceOperation::ContextScoutFeedback
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
    let invalid = |_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest;
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
        _ => Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
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
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::GitPreview => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::GitPreview)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::GitApply => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::GitApply)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
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
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::CodeExactOccurrence => {
            serde_json::from_value::<CodeExactOccurrenceSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::ExactOccurrence)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodePhraseSearch => {
            serde_json::from_value::<CodePhraseSearchSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::PhraseSearch)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeSymbolSearch => {
            serde_json::from_value::<CodeSymbolSearchSurfaceRequest>(value)
                .map(PrimitiveCodeSurfaceRequest::SymbolSearch)
                .map(ApplicationSurfaceRequest::PrimitiveCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeSignatureSearch => {
            serde_json::from_value::<CodeSignatureSearchSurfaceRequest>(value)
                .map(PrimitiveCodeSurfaceRequest::SignatureSearch)
                .map(ApplicationSurfaceRequest::PrimitiveCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeImplementations => {
            serde_json::from_value::<CodeImplementationsSurfaceRequest>(value)
                .map(PrimitiveCodeSurfaceRequest::Implementations)
                .map(ApplicationSurfaceRequest::PrimitiveCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeTypeHierarchy => {
            serde_json::from_value::<CodeTypeHierarchySurfaceRequest>(value)
                .map(PrimitiveCodeSurfaceRequest::TypeHierarchy)
                .map(ApplicationSurfaceRequest::PrimitiveCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeCallers => {
            serde_json::from_value::<CodeCallersSurfaceRequest>(value)
                .map(PrimitiveCodeSurfaceRequest::Callers)
                .map(ApplicationSurfaceRequest::PrimitiveCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeCallees => {
            serde_json::from_value::<CodeCalleesSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::Callees)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeFacets => {
            serde_json::from_value::<CodeFacetSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::Facets)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeTimeline => {
            serde_json::from_value::<CodeTimelineSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::Timeline)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeDeclaration => {
            serde_json::from_value::<CodeNavigationSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::Declaration)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeDefinition => {
            serde_json::from_value::<CodeNavigationSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::Definition)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeTypeDefinition => {
            serde_json::from_value::<CodeNavigationSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::TypeDefinition)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CodeReferences => {
            serde_json::from_value::<CodeNavigationSurfaceRequest>(value)
                .map(CallableCodeSurfaceRequest::References)
                .map(ApplicationSurfaceRequest::CallableCode)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::SessionLookup => {
            serde_json::from_value::<SessionLookupRequest>(value)
                .map(PrimitiveRequest::SessionLookup)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::QualifiedName => {
            serde_json::from_value::<QualifiedNamePrimitiveRequest>(value)
                .map(PrimitiveRequest::QualifiedName)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::CallChain => {
            serde_json::from_value::<CallChainPrimitiveRequest>(value)
                .map(PrimitiveRequest::CallChain)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::FileDependents => {
            serde_json::from_value::<FileDependentsPrimitiveRequest>(value)
                .map(PrimitiveRequest::FileDependents)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::SourceLines => {
            serde_json::from_value::<SourceLinesRequest>(value)
                .map(PrimitiveRequest::SourceLines)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::SourceBody => {
            serde_json::from_value::<SourceBodyPrimitiveRequest>(value)
                .map(PrimitiveRequest::SourceBody)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::SourceOutline => {
            serde_json::from_value::<SourceOutlinePrimitiveRequest>(value)
                .map(PrimitiveRequest::SourceOutline)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::ModuleApi => {
            serde_json::from_value::<ModuleApiPrimitiveRequest>(value)
                .map(PrimitiveRequest::ModuleApi)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::FileMetadata => {
            serde_json::from_value::<FileMetadataPrimitiveRequest>(value)
                .map(PrimitiveRequest::FileMetadata)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::HealthRead => {
            serde_json::from_value::<HealthReadRequest>(value)
                .map(PrimitiveRequest::HealthRead)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::HealthDelta => {
            serde_json::from_value::<HealthDeltaRequest>(value)
                .map(PrimitiveRequest::HealthDelta)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::StorageStatus => {
            serde_json::from_value::<StorageStatusPrimitiveRequest>(value)
                .map(PrimitiveRequest::StorageStatus)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::DiagnosticsRead => {
            serde_json::from_value::<DiagnosticsPrimitiveRequest>(value)
                .map(PrimitiveRequest::DiagnosticsRead)
                .map(ApplicationSurfaceRequest::Primitive)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::ObservatoryRead => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::ObservatoryRead)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationList
        | ApplicationSurfaceOperation::ConfigurationExplain
        | ApplicationSurfaceOperation::ConfigurationGet
        | ApplicationSurfaceOperation::ConfigurationSet
        | ApplicationSurfaceOperation::ConfigurationUnset
        | ApplicationSurfaceOperation::ConfigurationBatch
        | ApplicationSurfaceOperation::ConfigurationWriteCredential
        | ApplicationSurfaceOperation::ConfigurationObservedState
        | ApplicationSurfaceOperation::ConfigurationProtectedPreview
        | ApplicationSurfaceOperation::ConfigurationProtectedApply
        | ApplicationSurfaceOperation::ConfigurationRollbackPreview
        | ApplicationSurfaceOperation::ConfigurationRollbackApply
        | ApplicationSurfaceOperation::ConfigurationAudit => {
            configuration_wire_request_from_invocation_payload(operation.as_str(), value)
                .map(ApplicationSurfaceRequest::Configuration)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        }
        ApplicationSurfaceOperation::ContextScoutStatus => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequest::Status)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ContextScoutRecent => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequest::Recent)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ContextScoutExplain => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequest::Explain)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ContextScoutCapability => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequest::Capability)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ContextScoutBudget => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequest::Budget)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ContextScoutPause => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequest::Pause)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ContextScoutResume => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequest::Resume)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ContextScoutCancel => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequest::Cancel)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ContextScoutClaim => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequest::Claim)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ContextScoutDelivery => serde_json::from_value(value)
            .map(|request| ContextScoutSurfaceRequest::Delivery(Box::new(request)))
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ContextScoutFeedback => serde_json::from_value(value)
            .map(ContextScoutSurfaceRequest::Feedback)
            .map(ApplicationSurfaceRequest::ContextScout)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::FeedbackDiagnostics
        | ApplicationSurfaceOperation::FeedbackGet
        | ApplicationSurfaceOperation::FeedbackExpand
        | ApplicationSurfaceOperation::FeedbackList
        | ApplicationSurfaceOperation::FeedbackImpact
        | ApplicationSurfaceOperation::AffectedTests => {
            let request: FeedbackSurfaceRequest = serde_json::from_value(value)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
            Ok(ApplicationSurfaceRequest::Feedback(
                FeedbackSurfaceRequest::new(request.request_handle)
                    .map_err(|_| ApplicationSurfaceAdapterError::InvalidRequestHandle)?,
            ))
        }
        ApplicationSurfaceOperation::FeedbackAdvisoryCycle => {
            let request: FeedbackAdvisoryCycleSurfaceRequestV1 = serde_json::from_value(value)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
            request
                .validate()
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
            Ok(ApplicationSurfaceRequest::FeedbackAdvisoryCycle(request))
        }
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

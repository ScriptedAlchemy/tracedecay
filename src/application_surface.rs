//! Shared transport adapter contracts for the first callable application surfaces.
//!
//! The adapters resolve catalog bindings and preserve canonical application
//! problem envelopes. They do not open stores, run queries, or bypass the
//! daemon-owned Git transaction authority.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

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
    CanonicalInvocationResult, HttpApplicationControls, HttpApplicationInvocationFuture,
    HttpApplicationOperation, HttpApplicationRequest, WorkOperation, WorkflowOperation,
    application_problem_response, sse_response,
};
use tracedecay_application::handlers::CanonicalApplicationDispatcher;
use tracedecay_application::retrieval::{
    CodeFacetDimension, CodeFacetRequest, CodeLexicalFieldFilter, CodeNavigationRequest,
    CodeQueryScope, CodeRelationRequest, CodeTimelineRequest, ExactOccurrenceRequest,
    GraphRelationRequest, HealthDeltaRequest, ImplementationSelector, ImplementationsRequest,
    PhraseSearchRequest, SignatureSearchRequest, SymbolGraphScope, SymbolSearchPrimitiveRequest,
    TypeHierarchyRequest,
};
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, AcceptProposalCommand, AcceptTaskCommand,
    AdmitExecutionCommand, ApplicationContractError, ApplicationEnvelope, ApplicationOperation,
    ApplicationProblem, ApplicationProblemEnvelope, ApplicationProblemKind, ApplicationResult,
    AttachRuntimeEvidenceCommand, CancellationContext, CancellationSignal, CreateWorkCommand,
    Deadline, HealthReadRequest, IdempotencyKey, LegalAction, OpaqueCursor, OperationTermination,
    PageRequest, ProblemOwningLayer, ReplanDependenciesCommand, RequestContext, RequestId,
    ResultContractRef, ResultProjection, ResumeToken, RetrievalOrder, RetrievalRequestMeta,
    RetryDirective, ReviewProposalRequestV1, SafeDiagnostic, SessionLookupRequest,
    SourceLinesRequest, StreamEvent, StreamEventKind, WorkAttemptAcquireLeaseRequestV1,
    WorkAttemptCancelRequestV1, WorkAttemptFinishRequestV1, WorkAttemptPublishArtifactRequestV1,
    WorkAttemptPublishProgressRequestV1, WorkAttemptRecoverRequestV1,
    WorkAttemptRenewLeaseRequestV1, WorkAttemptResponseV1, WorkAttemptStartRequestV1,
    WorkAttemptTerminalizeRequestV1, WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1,
};
use tracedecay_domain::configuration::{
    ConfigurationAuditEventId, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationValueV1, CredentialKindV1, CredentialReferenceId, RollbackModeV1, SettingKey,
};
use tracedecay_domain::git::{GitDiffScopeV1, GitOidV1};
use tracedecay_domain::{
    ExactTechnicalTermKindV1, GitIndexCommitIntentV1, GitIndexPreviewId, GitIndexPreviewV1,
    GitIndexTransactionOperationV1, HunkRefV1, ManifestDigest, ProjectId,
    QueryNormalizationRevision, RepositoryStateSnapshotV1, SanitizerRevision, UtcMicros,
    WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1, canonical_sha256,
};
use tracedecay_tool_catalog::{
    BindingSurface, CapabilityId, CatalogSnapshotV1, CatalogValidationError, FeatureId,
    IdentifierError, ProfileId, RouteExposureV1, SchemaId, SurfaceOperationName, UseCaseId,
};
pub use tracedecay_usecases::application_surface::{
    ConfigurationProtectedApplySurfaceRequest, ConfigurationProtectedPreviewSurfaceRequest,
};

use crate::application::feedback::observations::{
    Plan26ArgumentRejectionClassV1, Plan26DeliveryRouteV1, Plan26FeedbackOperationV1,
    Plan26FeedbackOutcomeV1, Plan26FeedbackSourceEventV1, Plan26RejectedArgumentV1,
    Plan26SseLifecycleV1,
};
use crate::application::operation_stream::{
    OperationCancelOutcome, OperationEventAuthority, OperationEventError, OperationId,
    OperationRequestControls,
};
use crate::application::primitives::{
    CallChainPrimitiveRequest, DiagnosticsPrimitiveRequest, FileDependentsPrimitiveRequest,
    FileMetadataPrimitiveRequest, ModuleApiPrimitiveRequest, Pr12PrimitiveRequest,
    QualifiedNamePrimitiveRequest, SourceBodyPrimitiveRequest, SourceOutlinePrimitiveRequest,
    StorageStatusPrimitiveRequest,
};
use crate::catalog_composition::{
    ApplicationCatalogComposition, CatalogCompositionError, build_application_catalog_snapshot,
    compose_application_catalog_with,
};
use crate::daemon_client::{
    BindingResolution, BindingResolver, CatalogBindingResolver, DaemonInvocationError,
    DispatchError, DispatchInput, DispatchedInvocation, InvocationCancellationPolicy,
    InvocationControls, RequestedOutputFormat, ResolvedBinding, ScopeSelector, resolve_dispatch,
};
use crate::daemon_contract::{
    WorkApplicationInvocationV1, WorkApplicationOutcomeV1, WorkAttemptInvocationV1,
};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

mod workflow;

use workflow::router_with_executor as workflow_application_router_with_executor;

const DEFAULT_PAGE_SIZE: u32 = 10;
const DEFAULT_DEADLINE_MICROS: i64 = 30_000_000;
const APPLICATION_PROTOCOL_REVISION: u32 = 1;
const HTTP_DEADLINE_HEADER: &str = "x-tracedecay-deadline-micros";
const MAX_REQUEST_HANDLE_BYTES: usize = 256;

/// Canonical operation identity shared by HTTP, MCP, CLI, LSP, SSE, and
/// dashboard adapters. The API crate owns the names and complete operation
/// family; surface bindings decide which transports expose each operation.
pub use tracedecay_api::HttpApplicationOperation as ApplicationSurfaceOperation;

/// Compatibility export for existing callers. The array is the canonical
/// operation authority's list, not a second root-owned registry.
pub const APPLICATION_SURFACE_OPERATIONS: [ApplicationSurfaceOperation; 66] =
    tracedecay_api::HttpApplicationOperation::ALL;

/// Transport keys every surface adapter accepts but no reviewed application
/// request schema declares. `format` selects the rendered output and
/// `__mcp_request_id` carries protocol identity; both are stripped here so
/// that `deny_unknown_fields` request schemas never see them.
const SURFACE_TRANSPORT_ARGUMENT_KEYS: [&str; 2] = ["format", "__mcp_request_id"];

/// A reviewed application request body together with the presentation format
/// that travelled alongside it in the caller's argument object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedApplicationToolArgs {
    pub request: Value,
    pub requested_format: RequestedOutputFormat,
}

/// The single authority for reading the presentation format out of a tool
/// argument object. CLI, MCP, and rendering all resolve `format` through here.
pub fn requested_output_format(args: &Value) -> RequestedOutputFormat {
    match args.get("format").and_then(Value::as_str) {
        Some(format) if format.eq_ignore_ascii_case("json") => RequestedOutputFormat::Json,
        _ => RequestedOutputFormat::Markdown,
    }
}

/// Normalizes compatibility tool arguments before every CLI/MCP transport
/// parses the canonical application request.
pub fn normalize_application_tool_args(
    tool_name: &str,
    mut args: Value,
) -> Result<NormalizedApplicationToolArgs, ApplicationSurfaceAdapterError> {
    let requested_format = requested_output_format(&args);
    if let Some(object) = args.as_object_mut() {
        for key in SURFACE_TRANSPORT_ARGUMENT_KEYS {
            object.remove(key);
        }
    }
    let request = if tool_name == "tracedecay_diagnostics" {
        compatibility_diagnostics_request(&args)?
    } else {
        args
    };
    Ok(NormalizedApplicationToolArgs {
        request,
        requested_format,
    })
}

/// Rewrites the compatibility `tracedecay_diagnostics` argument shape into the
/// reviewed `diagnostics_read` request body.
fn compatibility_diagnostics_request(
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
    Ok(serde_json::json!({
        "scope": scope,
        "maximum_diagnostics": args
            .get("maximum_diagnostics")
            .cloned()
            .unwrap_or_else(|| serde_json::json!(1000)),
        "cursor": args.get("cursor").cloned().unwrap_or(Value::Null),
    }))
}

pub type FeedbackSurfaceRequest = tracedecay_application::feedback::FeedbackHandleRequestV1;

/// Canonical explicit PR13 trigger. Project/root/scope/provider identities and
/// the resulting read handle are all minted by the authenticated daemon.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackAdvisoryCycleSurfaceRequest {
    pub document_uri: String,
}

impl FeedbackAdvisoryCycleSurfaceRequest {
    fn validate(&self) -> Result<(), ApplicationSurfaceAdapterError> {
        if self.document_uri.is_empty()
            || self.document_uri.trim() != self.document_uri
            || self.document_uri.len() > MAX_REQUEST_HANDLE_BYTES * 16
            || self.document_uri.chars().any(char::is_control)
        {
            return Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AffectedTestsSurfaceRequest {
    pub request_handle: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackImpactSurfaceRequest {
    pub request_handle: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestResultsSurfaceRequest {}

/// Surface-owned query semantics. Page size remains an invocation control, but
/// continuation is a request field so CLI, MCP, and HTTP callers all have the
/// same channel for spending a `next_cursor`. HTTP folds its transport cursor
/// into this field before decoding, keeping exactly one page authority at the
/// point of use.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CallableCodeSurfaceMeta {
    pub projection: ResultProjection,
    pub order: RetrievalOrder,
    #[serde(default)]
    pub cursor: Option<OpaqueCursor>,
}

impl CallableCodeSurfaceMeta {
    fn into_application(self, page: PageRequest) -> RetrievalRequestMeta {
        let Self {
            projection,
            order,
            cursor,
        } = self;
        let page = match cursor {
            Some(cursor) => PageRequest {
                page_size: page.page_size,
                cursor: Some(cursor),
            },
            None => page,
        };
        RetrievalRequestMeta::current(page, projection, order)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeExactOccurrenceSurfaceRequest {
    pub literal: String,
    pub kind: Option<ExactTechnicalTermKindV1>,
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeExactOccurrenceSurfaceRequest {
    pub fn into_application_request(
        self,
        page: PageRequest,
    ) -> Result<ExactOccurrenceRequest, ApplicationContractError> {
        ExactOccurrenceRequest::new(
            self.literal,
            self.kind,
            self.scope,
            self.meta.into_application(page),
        )
    }
}

/// Serializable adapter DTO for the request-local phrase query view.
///
/// The callable application request deliberately keeps its sanitized query
/// non-serializable. The owning runtime supplies the exact sanitizer
/// revisions when converting this transport DTO.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodePhraseSearchSurfaceRequest {
    pub query: String,
    pub phrases: Vec<String>,
    #[serde(default)]
    pub field_filters: Vec<CodeLexicalFieldFilter>,
    #[serde(default)]
    pub fuzzy_budget: u32,
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSymbolSearchSurfaceRequest {
    pub query: String,
    pub scope: SymbolGraphScope,
    pub lazy_index_ignored_dependencies: bool,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeSymbolSearchSurfaceRequest {
    pub(crate) fn into_primitive_request(
        self,
        sanitizer_revision: SanitizerRevision,
        normalization_revision: QueryNormalizationRevision,
        page: PageRequest,
    ) -> Result<SymbolSearchPrimitiveRequest, ApplicationContractError> {
        let query = tracedecay_domain::EphemeralSanitizedQueryViewV1::sanitize(
            self.query,
            sanitizer_revision,
            normalization_revision,
        )?;
        Ok(SymbolSearchPrimitiveRequest {
            query,
            scope: self.scope,
            lazy_index_ignored_dependencies: self.lazy_index_ignored_dependencies,
            meta: self.meta.into_application(page),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSignatureSearchSurfaceRequest {
    pub returns: Option<String>,
    pub params: Vec<String>,
    pub is_async: Option<bool>,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeImplementationsSurfaceRequest {
    pub selector: ImplementationSelector,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeTypeHierarchySurfaceRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeCallersSurfaceRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub resolve_trait_dispatch: bool,
    pub scope: SymbolGraphScope,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PrimitiveCodeSurfaceRequest {
    SymbolSearch(CodeSymbolSearchSurfaceRequest),
    SignatureSearch(CodeSignatureSearchSurfaceRequest),
    Implementations(CodeImplementationsSurfaceRequest),
    TypeHierarchy(CodeTypeHierarchySurfaceRequest),
    Callers(CodeCallersSurfaceRequest),
}

impl PrimitiveCodeSurfaceRequest {
    pub(crate) fn into_primitive(
        self,
        sanitizer_revision: SanitizerRevision,
        normalization_revision: QueryNormalizationRevision,
        page: PageRequest,
    ) -> Result<Pr12PrimitiveRequest, ApplicationContractError> {
        Ok(match self {
            Self::SymbolSearch(request) => Pr12PrimitiveRequest::SymbolSearch(
                request.into_primitive_request(sanitizer_revision, normalization_revision, page)?,
            ),
            Self::SignatureSearch(request) => {
                Pr12PrimitiveRequest::SignatureSearch(SignatureSearchRequest {
                    returns: request.returns,
                    params: request.params,
                    is_async: request.is_async,
                    scope: request.scope,
                    meta: request.meta.into_application(page),
                })
            }
            Self::Implementations(request) => {
                Pr12PrimitiveRequest::Implementations(ImplementationsRequest {
                    selector: request.selector,
                    scope: request.scope,
                    meta: request.meta.into_application(page),
                })
            }
            Self::TypeHierarchy(request) => {
                Pr12PrimitiveRequest::TypeHierarchy(TypeHierarchyRequest {
                    node_id: request.node_id,
                    maximum_depth: request.maximum_depth,
                    scope: request.scope,
                    meta: request.meta.into_application(page),
                })
            }
            Self::Callers(request) => Pr12PrimitiveRequest::Callers(GraphRelationRequest {
                node_id: request.node_id,
                maximum_depth: request.maximum_depth,
                resolve_trait_dispatch: request.resolve_trait_dispatch,
                scope: request.scope,
                meta: request.meta.into_application(page),
            }),
        })
    }
}

impl CodePhraseSearchSurfaceRequest {
    pub fn into_application_request(
        self,
        sanitizer_revision: SanitizerRevision,
        normalization_revision: QueryNormalizationRevision,
        page: PageRequest,
    ) -> Result<PhraseSearchRequest, ApplicationContractError> {
        let query = tracedecay_domain::EphemeralSanitizedQueryViewV1::sanitize(
            self.query,
            sanitizer_revision,
            normalization_revision,
        )?;
        PhraseSearchRequest::new(
            query,
            self.phrases,
            self.field_filters,
            self.fuzzy_budget,
            self.scope,
            self.meta.into_application(page),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeCalleesSurfaceRequest {
    pub node_id: String,
    pub maximum_depth: u32,
    pub resolve_trait_dispatch: bool,
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeFacetSurfaceRequest {
    pub dimension: CodeFacetDimension,
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeFacetSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> CodeFacetRequest {
        CodeFacetRequest {
            dimension: self.dimension,
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeTimelineSurfaceRequest {
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeTimelineSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> CodeTimelineRequest {
        CodeTimelineRequest {
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeNavigationSurfaceRequest {
    pub node_id: String,
    pub scope: CodeQueryScope,
    pub meta: CallableCodeSurfaceMeta,
}

impl CodeNavigationSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> CodeNavigationRequest {
        CodeNavigationRequest {
            node_id: self.node_id,
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

impl CodeCalleesSurfaceRequest {
    pub fn into_application_request(self, page: PageRequest) -> CodeRelationRequest {
        CodeRelationRequest {
            node_id: self.node_id,
            maximum_depth: self.maximum_depth,
            resolve_trait_dispatch: self.resolve_trait_dispatch,
            scope: self.scope,
            meta: self.meta.into_application(page),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum CallableCodeSurfaceRequest {
    ExactOccurrence(CodeExactOccurrenceSurfaceRequest),
    PhraseSearch(CodePhraseSearchSurfaceRequest),
    Callees(CodeCalleesSurfaceRequest),
    Facets(CodeFacetSurfaceRequest),
    Timeline(CodeTimelineSurfaceRequest),
    Declaration(CodeNavigationSurfaceRequest),
    Definition(CodeNavigationSurfaceRequest),
    TypeDefinition(CodeNavigationSurfaceRequest),
    References(CodeNavigationSurfaceRequest),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationListSurfaceRequest {}

pub type ConfigurationKeySurfaceRequest =
    tracedecay_application::configuration::ConfigurationGetRequestV1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum ConfigurationDirectMutationSurfaceRequest {
    Set {
        layer: ConfigurationLayerIdV1,
        key: SettingKey,
        value: ConfigurationValueV1,
    },
    Unset {
        layer: ConfigurationLayerIdV1,
        key: SettingKey,
    },
}

pub type ConfigurationSetSurfaceRequest =
    tracedecay_application::configuration::ConfigurationSetRequestV1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationUnsetSurfaceRequest {
    pub layer: ConfigurationLayerIdV1,
    pub key: SettingKey,
    pub expected_revision: ConfigurationRevisionId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationBatchSurfaceRequest {
    pub mutations: Vec<ConfigurationDirectMutationSurfaceRequest>,
    pub expected_revision: ConfigurationRevisionId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationWriteCredentialSurfaceRequest {
    pub expected_reference_id: Option<CredentialReferenceId>,
    pub kind: CredentialKindV1,
    pub write_handle: String,
    pub expected_revision: ConfigurationRevisionId,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationObservedStateSurfaceRequest {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationRollbackPreviewSurfaceRequest {
    pub target_revision_id: ConfigurationRevisionId,
    pub mode: RollbackModeV1,
}

pub type ConfigurationRollbackApplySurfaceRequest = ConfigurationProtectedApplySurfaceRequest;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationAuditSurfaceRequest {
    #[serde(default)]
    pub after_event_id: Option<ConfigurationAuditEventId>,
    pub limit: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation", content = "request")]
pub enum ConfigurationSurfaceRequest {
    List(ConfigurationListSurfaceRequest),
    Explain(ConfigurationKeySurfaceRequest),
    Get(ConfigurationKeySurfaceRequest),
    Set(ConfigurationSetSurfaceRequest),
    Unset(ConfigurationUnsetSurfaceRequest),
    Batch(ConfigurationBatchSurfaceRequest),
    WriteCredential(ConfigurationWriteCredentialSurfaceRequest),
    ObservedState(ConfigurationObservedStateSurfaceRequest),
    ProtectedPreview(ConfigurationProtectedPreviewSurfaceRequest),
    ProtectedApply(ConfigurationProtectedApplySurfaceRequest),
    RollbackPreview(ConfigurationRollbackPreviewSurfaceRequest),
    RollbackApply(ConfigurationRollbackApplySurfaceRequest),
    Audit(ConfigurationAuditSurfaceRequest),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutClaimWindowSurfaceV1 {
    IdleWindow,
    OnRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutExactAddressSurfaceRequest {
    pub address: crate::agents::context_scout_v2::ContextScoutAddressV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutRecentSurfaceRequest {
    pub address: crate::agents::context_scout_v2::ContextScoutAddressV1,
    #[serde(default = "default_context_scout_recent_limit")]
    pub limit: usize,
}

const fn default_context_scout_recent_limit() -> usize {
    8
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutControlSurfaceRequest {
    pub address: crate::agents::context_scout_v2::ContextScoutAddressV1,
    pub expected_revision: ConfigurationRevisionId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutCancelSurfaceRequest {
    pub address: crate::agents::context_scout_v2::ContextScoutAddressV1,
    pub work: crate::agents::context_scout_v2::ContextScoutWorkV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutClaimSurfaceRequest {
    pub address: crate::agents::context_scout_v2::ContextScoutAddressV1,
    pub window: ContextScoutClaimWindowSurfaceV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutDeliverySurfaceRequest {
    pub address: crate::agents::context_scout_v2::ContextScoutAddressV1,
    pub claim: crate::agents::context_scout_v2::ContextScoutDurableClaimV1,
    pub receipt: crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutFeedbackSurfaceRequest {
    pub address: crate::agents::context_scout_v2::ContextScoutAddressV1,
    pub receipt: crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
    pub feedback: crate::agents::context_scout_v2::ContextScoutFeedbackV1,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation", content = "request")]
pub enum ContextScoutSurfaceRequest {
    Status(ContextScoutExactAddressSurfaceRequest),
    Recent(ContextScoutRecentSurfaceRequest),
    Explain(ContextScoutRecentSurfaceRequest),
    Capability(ContextScoutExactAddressSurfaceRequest),
    Budget(ContextScoutExactAddressSurfaceRequest),
    Pause(ContextScoutControlSurfaceRequest),
    Resume(ContextScoutControlSurfaceRequest),
    Cancel(ContextScoutCancelSurfaceRequest),
    Claim(ContextScoutClaimSurfaceRequest),
    Delivery(Box<ContextScoutDeliverySurfaceRequest>),
    Feedback(ContextScoutFeedbackSurfaceRequest),
}

impl ContextScoutSurfaceRequest {
    pub(crate) const fn address(&self) -> crate::agents::context_scout_v2::ContextScoutAddressV1 {
        match self {
            Self::Status(request) | Self::Capability(request) | Self::Budget(request) => {
                request.address
            }
            Self::Recent(request) | Self::Explain(request) => request.address,
            Self::Pause(request) | Self::Resume(request) => request.address,
            Self::Cancel(request) => request.address,
            Self::Claim(request) => request.address,
            Self::Delivery(request) => request.address,
            Self::Feedback(request) => request.address,
        }
    }

    pub(crate) const fn matches(&self, operation: ApplicationSurfaceOperation) -> bool {
        matches!(
            (self, operation),
            (
                Self::Status(_),
                ApplicationSurfaceOperation::ContextScoutStatus
            ) | (
                Self::Recent(_),
                ApplicationSurfaceOperation::ContextScoutRecent
            ) | (
                Self::Explain(_),
                ApplicationSurfaceOperation::ContextScoutExplain
            ) | (
                Self::Capability(_),
                ApplicationSurfaceOperation::ContextScoutCapability
            ) | (
                Self::Budget(_),
                ApplicationSurfaceOperation::ContextScoutBudget
            ) | (
                Self::Pause(_),
                ApplicationSurfaceOperation::ContextScoutPause
            ) | (
                Self::Resume(_),
                ApplicationSurfaceOperation::ContextScoutResume
            ) | (
                Self::Cancel(_),
                ApplicationSurfaceOperation::ContextScoutCancel
            ) | (
                Self::Claim(_),
                ApplicationSurfaceOperation::ContextScoutClaim
            ) | (
                Self::Delivery(_),
                ApplicationSurfaceOperation::ContextScoutDelivery
            ) | (
                Self::Feedback(_),
                ApplicationSurfaceOperation::ContextScoutFeedback
            )
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitPreviewSurfaceRequest {
    pub operation: GitIndexTransactionOperationV1,
    /// Compatibility input only. The daemon always replaces this value with a
    /// freshly minted preview identity before application admission.
    #[serde(default)]
    pub preview_id: GitIndexPreviewId,
    pub repository_snapshot: RepositoryStateSnapshotV1,
    #[serde(default)]
    pub selected_hunks: Vec<HunkRefV1>,
    #[serde(default)]
    pub commit_intent: Option<GitIndexCommitIntentV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitApplySurfaceRequest {
    pub preview: GitIndexPreviewV1,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitReadSurfaceRequest {
    pub request: crate::application::git_reads::GitReadRequestV1,
    pub max_entries: u32,
    pub max_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ApplicationSurfaceRequest {
    GitRead(GitReadSurfaceRequest),
    GitPreview(GitPreviewSurfaceRequest),
    GitApply(GitApplySurfaceRequest),
    Feedback(FeedbackSurfaceRequest),
    FeedbackAdvisoryCycle(FeedbackAdvisoryCycleSurfaceRequest),
    FeedbackImpact(FeedbackImpactSurfaceRequest),
    AffectedTests(AffectedTestsSurfaceRequest),
    TestResults(TestResultsSurfaceRequest),
    CallableCode(CallableCodeSurfaceRequest),
    PrimitiveCode(PrimitiveCodeSurfaceRequest),
    Primitive(Pr12PrimitiveRequest),
    Configuration(ConfigurationSurfaceRequest),
    ContextScout(ContextScoutSurfaceRequest),
}

pub struct ApplicationSurfaceInvocationResult {
    pub operation: ApplicationSurfaceOperation,
    pub binding_id: tracedecay_tool_catalog::BindingId,
    pub result: ApplicationResult<Value>,
    pub requested_format: RequestedOutputFormat,
}

struct HttpApplicationCatalogDispatcher {
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    catalog: Arc<CatalogSnapshotV1>,
}

struct CatalogBoundHttpApplicationRequest {
    capability_id: CapabilityId,
    use_case_id: UseCaseId,
    surface: BindingSurface,
    request: HttpApplicationRequest,
}

impl CanonicalApplicationDispatcher<CatalogBoundHttpApplicationRequest>
    for HttpApplicationCatalogDispatcher
{
    type Output = HttpApplicationInvocationFuture;

    fn invoke(
        &self,
        operation: &ApplicationOperation,
        request: CatalogBoundHttpApplicationRequest,
    ) -> Self::Output {
        assert_eq!(operation.capability_id(), &request.capability_id);
        assert_eq!(operation.use_case_id(), &request.use_case_id);
        let executor = Arc::clone(&self.executor);
        let catalog = self.catalog.clone();
        Box::pin(async move {
            invoke_application_adapter_request(
                request.request,
                request.surface,
                executor.as_ref(),
                &catalog,
            )
            .await
        })
    }
}

fn application_invoker_for_surface(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    surface: BindingSurface,
    required_operations: &[ApplicationSurfaceOperation],
) -> Result<
    impl Fn(HttpApplicationRequest) -> HttpApplicationInvocationFuture + Clone + Send + Sync + 'static,
    ApplicationSurfaceAdapterError,
> {
    let composition = Arc::new(compose_application_catalog_with(|catalog| {
        HttpApplicationCatalogDispatcher {
            executor,
            catalog: Arc::new(catalog.clone()),
        }
    })?);
    let resolver = CatalogBindingResolver::new(composition.snapshot());
    if surface == BindingSurface::Http {
        for operation in HttpApplicationOperation::ALL {
            if !operation.is_http_exposed() {
                continue;
            }
            if resolve_application_binding(&resolver, surface, operation).is_none() {
                return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
            }
        }
    } else {
        for operation in required_operations {
            if resolve_application_binding(&resolver, surface, *operation).is_none() {
                return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
            }
        }
    }
    Ok(move |request| invoke_catalog_bound_application_request(request, surface, &composition))
}

pub fn http_application_invoker(
    client: crate::daemon_client::DaemonInvocationClient,
) -> Result<
    impl Fn(HttpApplicationRequest) -> HttpApplicationInvocationFuture + Clone + Send + Sync + 'static,
    ApplicationSurfaceAdapterError,
> {
    application_invoker_for_surface(
        Arc::new(client),
        BindingSurface::Http,
        &APPLICATION_SURFACE_OPERATIONS,
    )
}

pub(crate) async fn invoke_multi_root_surface_request(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    page: PageRequest,
    deadline: Deadline,
    cancellation: tracedecay_application::CancellationSignal,
    body: Value,
) -> Result<Value, ApplicationSurfaceAdapterError> {
    let request = parse_application_surface_request(operation, body)?;
    let dispatched = resolve_application_surface_dispatch_with_controls(
        BindingSurface::Http,
        operation,
        request_id,
        request,
        page,
        Some(deadline),
        cancellation,
        RequestedOutputFormat::Json,
    )?;
    let response =
        execute_application_surface(operation, dispatched, Some(executor.as_ref())).await?;
    let envelope = response
        .result
        .map_err(|_| ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
    serde_json::to_value(envelope.outcome)
        .ok()
        .and_then(|value| value.get("value")?.get("payload").cloned())
        .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)
}

fn work_application_router_with_executor(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_work_catalog_bindings()?;
    Ok(tracedecay_api::work_application_router(WorkExecutorOwner {
        executor,
    }))
}

/// Refuse to mount Work unless the catalog advertises every descriptor
/// operation at exactly the path this build answers on.
///
/// The descriptor and the catalog are two statements of the same surface, and a
/// mount that disagreed with the catalog would advertise routes nobody serves.
pub(crate) fn validate_work_catalog_bindings() -> Result<(), ApplicationSurfaceAdapterError> {
    let registry = tracedecay_application::work_executable_binding_registry()
        .map_err(ApplicationSurfaceAdapterError::CatalogValidation)?;
    for operation in WorkOperation::ALL {
        let operation_id = tracedecay_tool_catalog::OperationId::new(operation.operation_id())
            .map_err(ApplicationSurfaceAdapterError::Identifier)?;
        let Some(binding) = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
        else {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        };
        let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        };
        if route_path != operation.application_route_path() {
            return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
        }
    }
    Ok(())
}

/// The Work application owner: canonical dispatch behind every Work route,
/// whichever router mounted it.
#[derive(Clone)]
pub(crate) struct WorkExecutorOwner {
    pub(crate) executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
}

impl tracedecay_api::WorkApplicationOwner for WorkExecutorOwner {
    fn invoke_work(
        &self,
        request: tracedecay_api::WorkHttpRequest,
    ) -> tracedecay_api::WorkInvocationFuture {
        Box::pin(invoke_work_operation(Arc::clone(&self.executor), request))
    }
}

async fn invoke_work_operation(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    request: tracedecay_api::WorkHttpRequest,
) -> Response {
    let tracedecay_api::WorkHttpRequest {
        operation,
        request_id,
        controls,
        body,
    } = request;

    macro_rules! core {
        ($request_ty:ty, $variant:ident, $output:ty) => {{
            let Ok(decoded) = serde_json::from_value::<$request_ty>(body) else {
                return tracedecay_api::work_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::work_application(
                request_id.as_str(),
                WorkApplicationInvocationV1::$variant(decoded),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<$output, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::WorkApplication {
                        scope,
                        outcome: WorkApplicationOutcomeV1::$variant(outcome),
                    } => Some((scope, outcome)),
                    _ => None,
                },
            )
            .await
        }};
    }

    macro_rules! attempt {
        ($request_ty:ty, $variant:ident) => {{
            let Ok(decoded) = serde_json::from_value::<$request_ty>(body) else {
                return tracedecay_api::work_invalid_request_response(request_id);
            };
            let invocation = crate::daemon_contract::DaemonInvocationRequest::work_attempt(
                request_id.as_str(),
                WorkAttemptInvocationV1::$variant(decoded.into()),
                crate::daemon_client::invocation_now_micros(),
                controls.deadline.clone(),
                controls.cancellation.context(),
            );
            invoke_registered_http::<WorkAttemptResponseV1, _>(
                executor,
                operation,
                request_id,
                controls,
                invocation,
                |outcome| match outcome {
                    crate::daemon_contract::DaemonInvocationOutcome::WorkAttempt {
                        scope,
                        outcome,
                    } => Some((scope, *outcome)),
                    _ => None,
                },
            )
            .await
        }};
    }

    match operation {
        WorkOperation::Snapshot => core!(
            WorkProjectionSnapshotRequestV1,
            Snapshot,
            WorkProjectionSnapshotV1
        ),
        WorkOperation::Delta => core!(WorkProjectionDeltaRequestV1, Delta, WorkProjectionDeltaV1),
        WorkOperation::Create => core!(CreateWorkCommand, Create, WorkProjection),
        WorkOperation::ReplanDependencies => {
            core!(
                ReplanDependenciesCommand,
                ReplanDependencies,
                WorkProjection
            )
        }
        WorkOperation::ReviewProposal => {
            core!(ReviewProposalRequestV1, ReviewProposal, WorkProjection)
        }
        WorkOperation::AcceptProposal => {
            core!(AcceptProposalCommand, AcceptProposal, WorkProjection)
        }
        WorkOperation::AdmitExecution => {
            core!(AdmitExecutionCommand, AdmitExecution, WorkProjection)
        }
        WorkOperation::AttachRuntimeEvidence => core!(
            AttachRuntimeEvidenceCommand,
            AttachRuntimeEvidence,
            WorkProjection
        ),
        WorkOperation::AcceptTask => core!(AcceptTaskCommand, AcceptTask, WorkProjection),
        WorkOperation::AttemptAcquireLease => {
            attempt!(WorkAttemptAcquireLeaseRequestV1, AcquireLease)
        }
        WorkOperation::AttemptRenewLease => attempt!(WorkAttemptRenewLeaseRequestV1, RenewLease),
        WorkOperation::AttemptStart => attempt!(WorkAttemptStartRequestV1, Start),
        WorkOperation::AttemptPublishProgress => {
            attempt!(WorkAttemptPublishProgressRequestV1, PublishProgress)
        }
        WorkOperation::AttemptPublishArtifact => {
            attempt!(WorkAttemptPublishArtifactRequestV1, PublishArtifact)
        }
        WorkOperation::AttemptCancel => attempt!(WorkAttemptCancelRequestV1, Cancel),
        WorkOperation::AttemptRecover => attempt!(WorkAttemptRecoverRequestV1, Recover),
        WorkOperation::AttemptFinish => attempt!(WorkAttemptFinishRequestV1, Finish),
        WorkOperation::AttemptTerminalize => attempt!(WorkAttemptTerminalizeRequestV1, Terminalize),
    }
}

/// Refuse a Work request that never reached dispatch, in the canonical envelope.
///
/// Everything before the executor call is adapter territory: the catalog would
/// not build, the operation is not advertised, or its binding carries no public
/// route. A bare status here would answer a Work route with an empty body no
/// client can read a code, a retry directive or a request id out of, so these
/// failures are reported as the same `ApplicationProblemEnvelope` the dispatched
/// path returns, owned by the adapter layer rather than the runtime.
fn work_adapter_unavailable(request_id: RequestId, code: &str, message: &str) -> Response {
    let Ok(schema_id) = SchemaId::new("schema.tracedecay.http.adapter-problem.v1") else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(contract) = ResultContractRef::new(schema_id, 1) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    tracedecay_api::application_problem_response(
        ApplicationProblemEnvelope::new(
            contract,
            request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: code.to_owned(),
                message: message.to_owned(),
            }),
        )
        .with_owning_layer(ProblemOwningLayer::Adapter),
    )
}

/// Dispatch one Work operation and encode its canonical result.
///
/// Core and attempt operations differ only in which daemon payload carries them
/// and which outcome they answer with, so both arrive here: one binding lookup,
/// one cancellation policy, one problem taxonomy.
trait RegisteredHttpOperation: Copy {
    fn operation_id_str(self) -> &'static str;
    fn is_read_only(self) -> bool;
    fn registry(
        self,
    ) -> Result<tracedecay_tool_catalog::ExecutableBindingRegistryV1, CatalogValidationError>;
}

impl RegisteredHttpOperation for WorkOperation {
    fn operation_id_str(self) -> &'static str {
        WorkOperation::operation_id_str(self)
    }

    fn is_read_only(self) -> bool {
        WorkOperation::is_read_only(self)
    }

    fn registry(
        self,
    ) -> Result<tracedecay_tool_catalog::ExecutableBindingRegistryV1, CatalogValidationError> {
        tracedecay_application::work_executable_binding_registry()
    }
}

impl RegisteredHttpOperation for WorkflowOperation {
    fn operation_id_str(self) -> &'static str {
        WorkflowOperation::operation_id_str(self)
    }

    fn is_read_only(self) -> bool {
        false
    }

    fn registry(
        self,
    ) -> Result<tracedecay_tool_catalog::ExecutableBindingRegistryV1, CatalogValidationError> {
        tracedecay_application::workflow_executable_binding_registry()
    }
}

async fn invoke_registered_http<T, O>(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    operation: O,
    request_id: RequestId,
    controls: HttpApplicationControls,
    invocation: crate::daemon_contract::DaemonInvocationRequest,
    select_outcome: fn(
        crate::daemon_contract::DaemonInvocationOutcome,
    ) -> Option<(
        tracedecay_application::ResolvedScope,
        tracedecay_application::ApplicationOutcome<T>,
    )>,
) -> Response
where
    T: Serialize,
    O: RegisteredHttpOperation,
{
    let registry = match operation.registry() {
        Ok(registry) => registry,
        Err(_) => {
            return work_adapter_unavailable(
                request_id,
                "work.catalog_unavailable",
                "The Work capability catalog is unavailable",
            );
        }
    };
    let operation_id =
        match tracedecay_tool_catalog::OperationId::new(operation.operation_id_str().to_owned()) {
            Ok(operation_id) => operation_id,
            Err(_) => {
                return work_adapter_unavailable(
                    request_id,
                    "work.operation_identity_unavailable",
                    "The Work operation identity is unavailable",
                );
            }
        };
    let Some(binding) = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
    else {
        return work_adapter_unavailable(
            request_id,
            "work.binding_unavailable",
            "The Work operation is not advertised by this build",
        );
    };
    let RouteExposureV1::Public { binding_id, .. } = binding.exposure() else {
        return work_adapter_unavailable(
            request_id,
            "work.route_unavailable",
            "The Work operation binding carries no public route",
        );
    };
    let result_contract = match ResultContractRef::new(
        binding.result_schema().schema_ref().schema_id().clone(),
        binding.result_schema().schema_ref().revision(),
    ) {
        Ok(contract) => contract,
        Err(_) => {
            return work_adapter_unavailable(
                request_id,
                "work.result_contract_unavailable",
                "The Work operation result contract is unavailable",
            );
        }
    };
    let binding_id = binding_id.clone();
    let policy = if operation.is_read_only() {
        InvocationCancellationPolicy::ReadOnly
    } else {
        InvocationCancellationPolicy::AuthoritativeEffect
    };
    let response = executor
        .invoke_controlled(invocation, controls.deadline, controls.cancellation, policy)
        .await;
    let problem = match response {
        Ok(crate::daemon_contract::DaemonInvocationResponse { outcome, .. }) => match outcome {
            crate::daemon_contract::DaemonInvocationOutcome::ApplicationProblem { problem } => {
                problem
            }
            crate::daemon_contract::DaemonInvocationOutcome::Problem { problem } => match problem {
                crate::daemon_contract::DaemonInvocationProblem::InvalidRequest
                | crate::daemon_contract::DaemonInvocationProblem::UnsupportedRevision => {
                    ApplicationProblem::InvalidRequest {
                        diagnostic: SafeDiagnostic {
                            code: "work.invalid_request".to_owned(),
                            message: "The Work application request is invalid".to_owned(),
                        },
                        retry: RetryDirective::Never,
                        legal_actions: vec![LegalAction::CorrectRequest],
                    }
                }
                crate::daemon_contract::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
                }
                crate::daemon_contract::DaemonInvocationProblem::Unavailable => {
                    ApplicationProblem::unavailable(SafeDiagnostic {
                        code: "work.unavailable".to_owned(),
                        message: "The Work application runtime is unavailable".to_owned(),
                    })
                }
            },
            outcome => match select_outcome(outcome) {
                Some((scope, outcome)) => {
                    return CanonicalInvocationResult::new(
                        binding_id,
                        Ok(ApplicationEnvelope {
                            contract: result_contract,
                            request_id,
                            scope,
                            outcome,
                        }),
                    )
                    .into_http_response();
                }
                None => ApplicationProblem::unavailable(SafeDiagnostic {
                    code: "work.protocol_unavailable".to_owned(),
                    message: "The Work application protocol is unavailable".to_owned(),
                }),
            },
        },
        Err(DaemonInvocationError::Cancelled { .. }) => {
            ApplicationProblem::cancelled_before_admission()
        }
        Err(DaemonInvocationError::TimedOut { .. }) => {
            ApplicationProblem::timed_out_before_admission()
        }
        Err(
            DaemonInvocationError::Saturated { .. }
            | DaemonInvocationError::Backpressured { .. }
            | DaemonInvocationError::Unavailable,
        ) => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "work.transport_unavailable".to_owned(),
            message: "The Work application transport is unavailable".to_owned(),
        }),
    };
    CanonicalInvocationResult::<T>::new(
        binding_id,
        Err(
            ApplicationProblemEnvelope::new(result_contract, request_id, problem)
                .with_owning_layer(ProblemOwningLayer::Runtime),
        ),
    )
    .into_http_response()
}

const DASHBOARD_CONFIGURATION_OPERATIONS: [ApplicationSurfaceOperation; 13] = [
    ApplicationSurfaceOperation::ConfigurationList,
    ApplicationSurfaceOperation::ConfigurationExplain,
    ApplicationSurfaceOperation::ConfigurationGet,
    ApplicationSurfaceOperation::ConfigurationSet,
    ApplicationSurfaceOperation::ConfigurationUnset,
    ApplicationSurfaceOperation::ConfigurationBatch,
    ApplicationSurfaceOperation::ConfigurationWriteCredential,
    ApplicationSurfaceOperation::ConfigurationObservedState,
    ApplicationSurfaceOperation::ConfigurationProtectedPreview,
    ApplicationSurfaceOperation::ConfigurationProtectedApply,
    ApplicationSurfaceOperation::ConfigurationRollbackPreview,
    ApplicationSurfaceOperation::ConfigurationRollbackApply,
    ApplicationSurfaceOperation::ConfigurationAudit,
];

const DASHBOARD_FEEDBACK_OPERATIONS: [ApplicationSurfaceOperation; 3] = [
    ApplicationSurfaceOperation::FeedbackGet,
    ApplicationSurfaceOperation::FeedbackExpand,
    ApplicationSurfaceOperation::FeedbackList,
];

pub fn dashboard_configuration_application_invoker(
    client: crate::daemon_client::DaemonInvocationClient,
) -> Result<
    impl Fn(HttpApplicationRequest) -> HttpApplicationInvocationFuture + Clone + Send + Sync + 'static,
    ApplicationSurfaceAdapterError,
> {
    application_invoker_for_surface(
        Arc::new(client),
        BindingSurface::Dashboard,
        &DASHBOARD_CONFIGURATION_OPERATIONS,
    )
}

pub fn http_application_router(
    client: crate::daemon_client::DaemonInvocationClient,
    operation_events: OperationEventAuthority,
    active_project_id: ProjectId,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    http_application_router_with_executor(Arc::new(client), operation_events, active_project_id)
}

pub fn http_application_router_with_executor(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    operation_events: OperationEventAuthority,
    active_project_id: ProjectId,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    let cancellations = Arc::new(Mutex::new(BTreeMap::new()));
    let event_executor = Arc::clone(&executor);
    let work_router = work_application_router_with_executor(Arc::clone(&executor))?;
    let workflow_router = workflow_application_router_with_executor(Arc::clone(&executor))?;
    Ok(
        tracedecay_api::application_router(application_invoker_for_surface(
            executor,
            BindingSurface::Http,
            &APPLICATION_SURFACE_OPERATIONS,
        )?)
        .merge(work_router)
        .merge(workflow_router)
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&cancellations),
            application_http_context,
        ))
        .merge(http_operation_event_router(
            operation_events,
            active_project_id,
            cancellations,
            Some(event_executor),
        )),
    )
}

/// Build the dashboard's public Work mount.
///
/// It is the core route subset of the application Work router: the same
/// descriptor, the same owner, the same dispatch and problem taxonomy. The
/// attempt-runtime routes are simply not registered here, so the lease protocol
/// is unreachable from the dashboard rather than merely undocumented.
pub fn dashboard_work_application_router_with_executor(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    validate_work_catalog_bindings()?;
    let cancellations = Arc::new(Mutex::new(BTreeMap::new()));
    Ok(
        tracedecay_api::work_core_router(WorkExecutorOwner { executor }).layer(
            axum::middleware::from_fn_with_state(cancellations, application_http_context),
        ),
    )
}

pub fn dashboard_configuration_application_router(
    client: crate::daemon_client::DaemonInvocationClient,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    dashboard_configuration_application_router_with_executor(Arc::new(client))
}

pub fn dashboard_configuration_application_router_with_executor(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    let cancellations = Arc::new(Mutex::new(BTreeMap::new()));
    Ok(
        tracedecay_api::configuration_application_router(application_invoker_for_surface(
            executor,
            BindingSurface::Dashboard,
            &DASHBOARD_CONFIGURATION_OPERATIONS,
        )?)
        .layer(axum::middleware::from_fn_with_state(
            cancellations,
            application_http_context,
        )),
    )
}

pub fn dashboard_feedback_application_router(
    client: crate::daemon_client::DaemonInvocationClient,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    dashboard_feedback_application_router_with_executor(Arc::new(client))
}

pub fn dashboard_feedback_application_router_with_executor(
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
) -> Result<axum::Router, ApplicationSurfaceAdapterError> {
    let cancellations = Arc::new(Mutex::new(BTreeMap::new()));
    let invoker = application_invoker_for_surface(
        executor,
        BindingSurface::Dashboard,
        &DASHBOARD_FEEDBACK_OPERATIONS,
    )?;
    Ok(tracedecay_api::feedback_application_router(invoker).layer(
        axum::middleware::from_fn_with_state(cancellations, application_http_context),
    ))
}

type HttpCancellationRegistry = Arc<Mutex<BTreeMap<RequestId, CancellationSignal>>>;

async fn application_http_context(
    State(cancellations): State<HttpCancellationRegistry>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Ok(request_id) = mint_global_request_id(GlobalRequestSurface::Http) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Ok(cancellation) =
        CancellationSignal::active(format!("cancellation.http.{}", request_id.as_str()))
    else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Ok(observed_at) = current_micros() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let default_expires_at = observed_at.0.saturating_add(DEFAULT_DEADLINE_MICROS);
    let caller_expires_at = match request.headers().get(HTTP_DEADLINE_HEADER) {
        Some(value) => match value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
        {
            Some(expires_at) => expires_at,
            None => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => default_expires_at,
    };
    let Ok(deadline) = Deadline::new(UtcMicros(caller_expires_at)) else {
        return StatusCode::BAD_REQUEST.into_response();
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
    let mut disconnect = HttpDisconnectCancellation::new(cancellations, request_id);
    let response = next.run(request).await;
    disconnect.disarm();
    response
}

struct HttpDisconnectCancellation {
    registry: HttpCancellationRegistry,
    request_id: RequestId,
    armed: bool,
}

impl HttpDisconnectCancellation {
    fn new(registry: HttpCancellationRegistry, request_id: RequestId) -> Self {
        Self {
            registry,
            request_id,
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
        if self.armed
            && let Ok(mut active) = self.registry.lock()
        {
            active.remove(&self.request_id);
        }
    }
}

#[derive(Clone)]
struct HttpOperationEventState {
    authority: OperationEventAuthority,
    active_project_id: ProjectId,
    cancellations: HttpCancellationRegistry,
    executor: Option<Arc<dyn crate::daemon_client::DaemonInvocationExecutor>>,
}

struct SseDisconnectObserver {
    executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor>,
    subject: ManifestDigest,
    terminal: Arc<AtomicBool>,
}

impl Drop for SseDisconnectObserver {
    fn drop(&mut self) {
        if self.terminal.load(Ordering::Relaxed) {
            return;
        }
        let executor = Arc::clone(&self.executor);
        let subject = self.subject.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = executor
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

#[derive(Deserialize)]
struct HttpOperationPath {
    operation_id: String,
}

fn http_operation_event_router(
    authority: OperationEventAuthority,
    active_project_id: ProjectId,
    cancellations: HttpCancellationRegistry,
    executor: Option<Arc<dyn crate::daemon_client::DaemonInvocationExecutor>>,
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
            executor,
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
    deadline: Deadline,
    cancellation: CancellationContext,
    observed_at: UtcMicros,
    resume_token: Option<&ResumeToken>,
) -> Result<RequestContext, OperationEventError> {
    state
        .authority
        .resolve_request_context(
            operation_id,
            &state.active_project_id,
            OperationRequestControls::new(
                request_id,
                deadline,
                cancellation,
                observed_at,
                resume_token,
            ),
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
    if let (Some(subject), Some(executor)) = (subject, state.executor.as_ref()) {
        let _ = executor
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
                OperationTermination::Unavailable => Plan26SseLifecycleV1::Unavailable,
                OperationTermination::Partial => Plan26SseLifecycleV1::Partial,
            },
            0,
            true,
        )),
    }
}

async fn http_operation_events_through_executor(
    executor: &dyn crate::daemon_client::DaemonInvocationExecutor,
    operation_id: &OperationId,
    request_id: &RequestId,
    controls: &HttpApplicationControls,
    next_sequence: u64,
) -> Response {
    let context = match tracedecay_application::ApplicationInvocationContext::new(
        request_id.clone(),
        tracedecay_application::InvocationTarget::CurrentProject,
        controls.deadline.clone(),
        controls.cancellation.clone(),
    ) {
        Ok(context) => context,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let request = match tracedecay_application::ApplicationRequest::operation_events(
        operation_id.request_id().clone(),
        256,
        next_sequence.checked_sub(1),
    ) {
        Ok(request) => request,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let invocation = match tracedecay_application::ApplicationInvocation::new(context, request) {
        Ok(invocation) => invocation,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let response =
        tracedecay_application::ApplicationInvocationExecutor::invoke(executor, invocation).await;
    let tracedecay_application::ApplicationResponse::Stream(response) = (match response {
        Ok(response) => response,
        Err(error) => {
            return operation_event_problem(
                request_id,
                operation_event_error_from_invocation(error),
            );
        }
    }) else {
        return operation_event_problem(request_id, OperationEventError::ResumeUnavailable);
    };
    sse_response(
        request_id.clone(),
        response.stream.frontier,
        tokio_stream::iter(response.stream.events),
    )
    .into_response()
}

fn operation_event_error_from_invocation(
    error: tracedecay_application::InvocationError,
) -> OperationEventError {
    match error {
        tracedecay_application::InvocationError::Denied => {
            OperationEventError::NotFoundOrNotAuthorized
        }
        tracedecay_application::InvocationError::Cancelled
        | tracedecay_application::InvocationError::DeadlineExceeded => {
            OperationEventError::RequestNotAdmitted
        }
        tracedecay_application::InvocationError::Conflict => OperationEventError::InvalidFrontier,
        tracedecay_application::InvocationError::InvalidRequest
        | tracedecay_application::InvocationError::Unavailable => {
            OperationEventError::ResumeUnavailable
        }
    }
}

async fn http_operation_events(
    State(state): State<HttpOperationEventState>,
    AxumPath(HttpOperationPath { operation_id }): AxumPath<HttpOperationPath>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
    Query(query): Query<HttpOperationEventQuery>,
) -> Response {
    let observation_subject = sse_observation_subject(&request_id, &operation_id);
    let operation_id = if let Ok(operation_id) = RequestId::new(operation_id) {
        OperationId::from_request(operation_id)
    } else {
        emit_http_plan26_observation(
            &state,
            observation_subject.as_ref(),
            current_micros().unwrap_or(UtcMicros(1)),
            Plan26FeedbackSourceEventV1::SurfaceArgumentRejected {
                operation: Plan26FeedbackOperationV1::SseStream,
                route: Some(Plan26DeliveryRouteV1::Http),
                argument: Plan26RejectedArgumentV1::RequestHandle,
                rejection: Plan26ArgumentRejectionClassV1::InvalidShape,
                schema_revision: 1,
                outcome: Plan26FeedbackOutcomeV1::Rejected,
            },
        )
        .await;
        return operation_event_problem(&request_id, OperationEventError::NotFoundOrNotAuthorized);
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
    if query.resume_token.is_none()
        && let Some(executor) = state.executor.as_deref()
    {
        return http_operation_events_through_executor(
            executor,
            &operation_id,
            &request_id,
            &controls,
            query.next_sequence,
        )
        .await;
    }
    let context = match resolve_authenticated_http_request_context(
        &state,
        &operation_id,
        request_id.clone(),
        controls.deadline,
        controls.cancellation.context(),
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
                Plan26FeedbackSourceEventV1::SurfaceArgumentRejected {
                    operation: Plan26FeedbackOperationV1::SseStream,
                    route: Some(Plan26DeliveryRouteV1::Http),
                    argument: Plan26RejectedArgumentV1::RequestHandle,
                    rejection: Plan26ArgumentRejectionClassV1::Unauthorized,
                    schema_revision: 1,
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
        .zip(state.executor.clone())
        .map(|(subject, executor)| {
            Arc::new(SseDisconnectObserver {
                executor,
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
                    .executor
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

async fn http_operation_cancel_through_executor(
    state: &HttpOperationEventState,
    executor: &dyn crate::daemon_client::DaemonInvocationExecutor,
    operation_id: &OperationId,
    request_id: &RequestId,
    controls: &HttpApplicationControls,
    observed_at: UtcMicros,
) -> Response {
    let context = match tracedecay_application::ApplicationInvocationContext::new(
        request_id.clone(),
        tracedecay_application::InvocationTarget::CurrentProject,
        controls.deadline.clone(),
        controls.cancellation.clone(),
    ) {
        Ok(context) => context,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let request = match tracedecay_application::ApplicationRequest::operation_cancel(
        operation_id.request_id().clone(),
    ) {
        Ok(request) => request,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let invocation = match tracedecay_application::ApplicationInvocation::new(context, request) {
        Ok(invocation) => invocation,
        Err(error) => {
            return operation_event_problem(
                request_id,
                OperationEventError::InvalidContext(error.to_string()),
            );
        }
    };
    let response =
        tracedecay_application::ApplicationInvocationExecutor::invoke(executor, invocation).await;
    let tracedecay_application::ApplicationResponse::Cancellation(response) = (match response {
        Ok(response) => response,
        Err(error) => {
            return operation_event_problem(
                request_id,
                operation_event_error_from_invocation(error),
            );
        }
    }) else {
        return operation_event_problem(request_id, OperationEventError::ResumeUnavailable);
    };
    if response.cancelled {
        if let Some(cancellation) = state
            .cancellations
            .lock()
            .ok()
            .and_then(|active| active.get(operation_id.request_id()).cloned())
        {
            let _ = cancellation.cancel(observed_at);
        }
        (
            StatusCode::ACCEPTED,
            Json(HttpOperationCancelResponse {
                status: "requested",
            }),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            Json(HttpOperationCancelResponse {
                status: "already_terminal",
            }),
        )
            .into_response()
    }
}

async fn http_operation_cancel(
    State(state): State<HttpOperationEventState>,
    AxumPath(HttpOperationPath { operation_id }): AxumPath<HttpOperationPath>,
    Extension(request_id): Extension<RequestId>,
    Extension(controls): Extension<HttpApplicationControls>,
) -> Response {
    let observation_subject = sse_observation_subject(&request_id, &operation_id);
    let operation_id = if let Ok(operation_id) = RequestId::new(operation_id) {
        OperationId::from_request(operation_id)
    } else {
        emit_http_plan26_observation(
            &state,
            observation_subject.as_ref(),
            current_micros().unwrap_or(UtcMicros(1)),
            Plan26FeedbackSourceEventV1::SurfaceArgumentRejected {
                operation: Plan26FeedbackOperationV1::SseStream,
                route: Some(Plan26DeliveryRouteV1::Http),
                argument: Plan26RejectedArgumentV1::RequestHandle,
                rejection: Plan26ArgumentRejectionClassV1::InvalidShape,
                schema_revision: 1,
                outcome: Plan26FeedbackOutcomeV1::Rejected,
            },
        )
        .await;
        return operation_event_problem(&request_id, OperationEventError::NotFoundOrNotAuthorized);
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
    if let Some(executor) = state.executor.as_deref() {
        return http_operation_cancel_through_executor(
            &state,
            executor,
            &operation_id,
            &request_id,
            &controls,
            observed_at,
        )
        .await;
    }
    let context = match resolve_authenticated_http_request_context(
        &state,
        &operation_id,
        request_id.clone(),
        controls.deadline,
        controls.cancellation.context(),
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
        // Permanently invalid input: the same request can never succeed, so the
        // client must correct it rather than retry.
        OperationEventError::InvalidContext(_)
        | OperationEventError::InvalidProgress
        | OperationEventError::InvalidTerminal(_)
        | OperationEventError::InvalidTestRunEvent => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "operation_event.invalid_request".to_owned(),
                message: "The operation-event request is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
        // Idempotency facts: the identity or terminal receipt is already
        // published, so the client re-reads current state instead of retrying
        // the same publish.
        OperationEventError::AlreadyBound | OperationEventError::TerminalAlreadyPublished => {
            ApplicationProblem::Conflict {
                diagnostic: SafeDiagnostic {
                    code: "operation_event.already_published".to_owned(),
                    message: "The operation-event identity is already published".to_owned(),
                },
                retry: RetryDirective::AfterRevalidate,
                legal_actions: vec![LegalAction::Refresh],
            }
        }
        // A misconfigured authority is a deterministic, process-lifetime
        // failure. It is not the caller's request that is wrong and no amount
        // of retrying will change the outcome.
        OperationEventError::InvalidConfiguration => ApplicationProblem::Unsupported {
            diagnostic: SafeDiagnostic {
                code: "operation_event.unsupported".to_owned(),
                message: "The operation-event authority is not configured for this operation"
                    .to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::ContactAdministrator],
        },
        // Genuinely transient: the resume-token authority could not answer.
        OperationEventError::ResumeUnavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "operation_event.unavailable".to_owned(),
            message: "The operation-event service is unavailable".to_owned(),
        }),
    };
    let Ok(schema_id) = SchemaId::new("schema.tracedecay.operation-event.problem.v1") else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(contract) = ResultContractRef::new(schema_id, 1) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let envelope = ApplicationProblemEnvelope::new(contract, request_id.clone(), problem)
        .with_owning_layer(ProblemOwningLayer::Runtime);
    let envelope = if envelope.problem.kind() == ApplicationProblemKind::Saturated {
        let Ok(envelope) = envelope.with_retry_after_millis(Some(250)) else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        envelope
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

/// Process-immutable application catalog snapshot.
///
/// The catalog is composed entirely from `const` application specs, so nothing
/// about it can change while the process runs. Composition still collects and
/// sorts every contribution, validates the handler/contribution bijection, and
/// derives all four profiles, so rebuilding it per call made a single dispatch
/// pay for the whole pipeline twice: once to resolve the binding and again to
/// re-validate it before execution. The snapshot is built once here and
/// borrowed from then on; the per-call binding identity comparison in
/// [`validate_current_application_binding`] is unchanged and now runs against
/// this cached snapshot.
static APPLICATION_SURFACE_CATALOG: LazyLock<Result<CatalogSnapshotV1, CatalogCompositionError>> =
    LazyLock::new(build_application_catalog_snapshot);

/// Borrow the process-wide catalog snapshot without recomposing it.
pub(crate) fn application_surface_catalog_ref()
-> Result<&'static CatalogSnapshotV1, ApplicationSurfaceAdapterError> {
    match &*APPLICATION_SURFACE_CATALOG {
        Ok(catalog) => Ok(catalog),
        Err(error) => Err(ApplicationSurfaceAdapterError::Catalog(error.clone())),
    }
}

pub fn application_surface_catalog() -> Result<CatalogSnapshotV1, ApplicationSurfaceAdapterError> {
    application_surface_catalog_ref().cloned()
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
            protocol_revision: APPLICATION_PROTOCOL_REVISION,
            negotiated_features: application_negotiated_features(),
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
            (
                Self::GitRead(_),
                ApplicationSurfaceOperation::GitStatus
                    | ApplicationSurfaceOperation::GitDiff
                    | ApplicationSurfaceOperation::GitHistory
                    | ApplicationSurfaceOperation::GitBlame
                    | ApplicationSurfaceOperation::GitHunks
            ) | (Self::GitPreview(_), ApplicationSurfaceOperation::GitPreview)
                | (Self::GitApply(_), ApplicationSurfaceOperation::GitApply)
                | (
                    Self::Feedback(_),
                    ApplicationSurfaceOperation::FeedbackDiagnostics
                        | ApplicationSurfaceOperation::FeedbackGet
                        | ApplicationSurfaceOperation::FeedbackExpand
                        | ApplicationSurfaceOperation::FeedbackList
                )
                | (
                    Self::FeedbackAdvisoryCycle(_),
                    ApplicationSurfaceOperation::FeedbackAdvisoryCycle
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
                    Self::Primitive(Pr12PrimitiveRequest::SessionLookup(_)),
                    ApplicationSurfaceOperation::SessionLookup
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::QualifiedName(_)),
                    ApplicationSurfaceOperation::QualifiedName
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::CallChain(_)),
                    ApplicationSurfaceOperation::CallChain
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::FileDependents(_)),
                    ApplicationSurfaceOperation::FileDependents
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::SourceLines(_)),
                    ApplicationSurfaceOperation::SourceLines
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::SourceBody(_)),
                    ApplicationSurfaceOperation::SourceBody
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::SourceOutline(_)),
                    ApplicationSurfaceOperation::SourceOutline
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::ModuleApi(_)),
                    ApplicationSurfaceOperation::ModuleApi
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::FileMetadata(_)),
                    ApplicationSurfaceOperation::FileMetadata
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::HealthRead(_)),
                    ApplicationSurfaceOperation::HealthRead
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::HealthDelta(_)),
                    ApplicationSurfaceOperation::HealthDelta
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::StorageStatus(_)),
                    ApplicationSurfaceOperation::StorageStatus
                )
                | (
                    Self::Primitive(Pr12PrimitiveRequest::DiagnosticsRead(_)),
                    ApplicationSurfaceOperation::DiagnosticsRead
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::List(_)),
                    ApplicationSurfaceOperation::ConfigurationList
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::Explain(_)),
                    ApplicationSurfaceOperation::ConfigurationExplain
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::Get(_)),
                    ApplicationSurfaceOperation::ConfigurationGet
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::Set(_)),
                    ApplicationSurfaceOperation::ConfigurationSet
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::Unset(_)),
                    ApplicationSurfaceOperation::ConfigurationUnset
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::Batch(_)),
                    ApplicationSurfaceOperation::ConfigurationBatch
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::WriteCredential(_)),
                    ApplicationSurfaceOperation::ConfigurationWriteCredential
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::ObservedState(_)),
                    ApplicationSurfaceOperation::ConfigurationObservedState
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::ProtectedPreview(_)),
                    ApplicationSurfaceOperation::ConfigurationProtectedPreview
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::ProtectedApply(_)),
                    ApplicationSurfaceOperation::ConfigurationProtectedApply
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::RollbackPreview(_)),
                    ApplicationSurfaceOperation::ConfigurationRollbackPreview
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::RollbackApply(_)),
                    ApplicationSurfaceOperation::ConfigurationRollbackApply
                )
                | (
                    Self::Configuration(ConfigurationSurfaceRequest::Audit(_)),
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

fn parse_git_read_surface_request(
    operation: ApplicationSurfaceOperation,
    value: Value,
) -> Result<GitReadSurfaceRequest, ApplicationSurfaceAdapterError> {
    let object = value
        .as_object()
        .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
    let bounded_u64 = |name: &str, default: u64, maximum: u64| match object.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=maximum).contains(value))
            .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
    };
    let boolean = |name: &str, default: bool| match object.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
    };
    let optional_string = |name: &str| match object.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
    };
    let max_entries = bounded_u64(
        "max_entries",
        u64::from(crate::git_query::GIT_QUERY_DEFAULT_MAX_ENTRIES),
        u64::from(crate::git_query::GIT_QUERY_DEFAULT_MAX_ENTRIES),
    )? as u32;
    let max_bytes = bounded_u64(
        "max_bytes",
        crate::git_query::GIT_QUERY_DEFAULT_MAX_BYTES,
        crate::git_query::GIT_QUERY_DEFAULT_MAX_BYTES,
    )?;
    let string = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .map(str::to_owned)
            .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
    };
    let scope_name = match object.get("scope") {
        None => "working_tree",
        Some(value) => value
            .as_str()
            .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?,
    };
    let scope = |allow_commit_range: bool| match scope_name {
        "working_tree" if !object.contains_key("base") && !object.contains_key("head") => {
            Ok(GitDiffScopeV1::WorkingTree)
        }
        "staged" if !object.contains_key("base") && !object.contains_key("head") => {
            Ok(GitDiffScopeV1::Staged)
        }
        "commit_range" if allow_commit_range => Ok(GitDiffScopeV1::CommitRange {
            base: GitOidV1::new(string("base")?)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?,
            head: GitOidV1::new(string("head")?)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?,
        }),
        _ => Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
    };
    let request = match operation {
        ApplicationSurfaceOperation::GitStatus => {
            crate::application::git_reads::GitReadRequestV1::Status
        }
        ApplicationSurfaceOperation::GitDiff => {
            crate::application::git_reads::GitReadRequestV1::Diff {
                scope: scope(true)?,
            }
        }
        ApplicationSurfaceOperation::GitHistory => {
            crate::application::git_reads::GitReadRequestV1::History {
                max_count: bounded_u64("count", 100, 1_000)? as u32,
                path: optional_string("path")?,
                follow: boolean("follow", false)?,
                first_parent: boolean("first_parent", false)?,
            }
        }
        ApplicationSurfaceOperation::GitBlame => {
            crate::application::git_reads::GitReadRequestV1::Blame {
                path: string("path")?,
                follow_renames: boolean("follow_renames", false)?,
            }
        }
        ApplicationSurfaceOperation::GitHunks => {
            crate::application::git_reads::GitReadRequestV1::Hunks {
                scope: scope(false)?,
                preview_id: string("preview_id")?,
                snapshot_digest: ManifestDigest::new(string("snapshot_digest")?)
                    .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?,
            }
        }
        _ => return Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
    };
    let allowed = match operation {
        ApplicationSurfaceOperation::GitStatus => &["max_entries", "max_bytes"][..],
        ApplicationSurfaceOperation::GitDiff => {
            &["scope", "base", "head", "max_entries", "max_bytes"][..]
        }
        ApplicationSurfaceOperation::GitHistory => &[
            "count",
            "path",
            "follow",
            "first_parent",
            "max_entries",
            "max_bytes",
        ][..],
        ApplicationSurfaceOperation::GitBlame => {
            &["path", "follow_renames", "max_entries", "max_bytes"][..]
        }
        ApplicationSurfaceOperation::GitHunks => &[
            "scope",
            "preview_id",
            "snapshot_digest",
            "max_entries",
            "max_bytes",
        ][..],
        _ => &[],
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest);
    }
    Ok(GitReadSurfaceRequest {
        request,
        max_entries,
        max_bytes,
    })
}

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
            parse_git_read_surface_request(operation, value).map(ApplicationSurfaceRequest::GitRead)
        }
        ApplicationSurfaceOperation::GitPreview => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::GitPreview)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::GitApply => serde_json::from_value(value)
            .map(ApplicationSurfaceRequest::GitApply)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::AffectedTests => {
            let request: AffectedTestsSurfaceRequest = serde_json::from_value(value)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
            FeedbackSurfaceRequest::new(request.request_handle.clone())
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidRequestHandle)?;
            Ok(ApplicationSurfaceRequest::AffectedTests(request))
        }
        ApplicationSurfaceOperation::FeedbackImpact => {
            let request: FeedbackImpactSurfaceRequest = serde_json::from_value(value)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
            FeedbackSurfaceRequest::new(request.request_handle.clone())
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidRequestHandle)?;
            Ok(ApplicationSurfaceRequest::FeedbackImpact(request))
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
        ApplicationSurfaceOperation::HealthDelta => {
            serde_json::from_value::<HealthDeltaRequest>(value)
                .map(Pr12PrimitiveRequest::HealthDelta)
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
        ApplicationSurfaceOperation::ConfigurationList => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::List)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationExplain => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::Explain)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationGet => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::Get)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationSet => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::Set)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationUnset => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::Unset)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationBatch => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::Batch)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationWriteCredential => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::WriteCredential)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationObservedState => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::ObservedState)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationProtectedPreview => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::ProtectedPreview)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationProtectedApply => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::ProtectedApply)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationRollbackPreview => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::RollbackPreview)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationRollbackApply => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::RollbackApply)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        ApplicationSurfaceOperation::ConfigurationAudit => serde_json::from_value(value)
            .map(ConfigurationSurfaceRequest::Audit)
            .map(ApplicationSurfaceRequest::Configuration)
            .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
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
        | ApplicationSurfaceOperation::FeedbackList => {
            let request: FeedbackSurfaceRequest = serde_json::from_value(value)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
            Ok(ApplicationSurfaceRequest::Feedback(
                FeedbackSurfaceRequest::new(request.request_handle)
                    .map_err(|_| ApplicationSurfaceAdapterError::InvalidRequestHandle)?,
            ))
        }
        ApplicationSurfaceOperation::FeedbackAdvisoryCycle => {
            let request: FeedbackAdvisoryCycleSurfaceRequest = serde_json::from_value(value)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
            request.validate()?;
            Ok(ApplicationSurfaceRequest::FeedbackAdvisoryCycle(request))
        }
    }
}

pub async fn execute_application_surface(
    operation: ApplicationSurfaceOperation,
    dispatched: DispatchedInvocation<ApplicationSurfaceRequest>,
    executor: Option<&dyn crate::daemon_client::DaemonInvocationExecutor>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    validate_current_application_binding(operation, &dispatched)?;
    let result_contract = ResultContractRef::from_schema(&dispatched.invocation.result_schema);
    let binding_id = dispatched.invocation.binding_id.clone();
    let request_id = dispatched.request_id;
    let surface = dispatched.surface;
    let delivery_route = plan26_delivery_route(dispatched.surface);
    let (invocation, requested_format) = dispatched.invocation.into_application_invocation();
    let observed_at = current_micros()?;
    let deadline = invocation.deadline.unwrap_or(Deadline::new(UtcMicros(
        observed_at.0.saturating_add(DEFAULT_DEADLINE_MICROS),
    ))?);
    let cancellation = invocation.cancellation;
    let cancellation_context = cancellation.context();
    let request_deadline = deadline.clone();
    let migrated_payload = match (&operation, &invocation.request) {
        (
            ApplicationSurfaceOperation::ConfigurationGet
            | ApplicationSurfaceOperation::ConfigurationSet,
            ApplicationSurfaceRequest::Configuration(request),
        ) => Some(
            serde_json::to_value(request)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?,
        ),
        (
            ApplicationSurfaceOperation::FeedbackGet,
            ApplicationSurfaceRequest::Feedback(request),
        ) => Some(
            serde_json::to_value(request)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?,
        ),
        _ => None,
    };
    if let Some(payload) = migrated_payload {
        let Some(executor) = executor else {
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
        let binding = tracedecay_application::ApplicationInvocationBinding::new(
            binding_id.clone(),
            surface,
            SurfaceOperationName::new(operation.as_str())?,
            result_contract.clone(),
            invocation.page,
        )?;
        let context = tracedecay_application::ApplicationInvocationContext::new(
            request_id.clone(),
            invocation.scope,
            deadline,
            cancellation,
        )?;
        let request = tracedecay_application::ApplicationRequest::surface(binding, payload)?;
        let invocation = tracedecay_application::ApplicationInvocation::new(context, request)?;
        let result = match tracedecay_application::ApplicationInvocationExecutor::invoke(
            executor, invocation,
        )
        .await
        {
            Ok(response) => response.envelope().cloned().ok_or_else(|| {
                ApplicationProblemEnvelope::new(
                    result_contract.clone(),
                    request_id.clone(),
                    ApplicationProblem::unavailable(SafeDiagnostic {
                        code: "application.surface.invalid_response".to_owned(),
                        message: "The daemon returned an invalid application response".to_owned(),
                    }),
                )
            }),
            Err(error) => Err(ApplicationProblemEnvelope::new(
                result_contract,
                request_id,
                invocation_contract_problem(error)?,
            )),
        };
        return Ok(ApplicationSurfaceInvocationResult {
            operation,
            binding_id,
            result,
            requested_format,
        });
    }
    let request = match invocation.request {
        ApplicationSurfaceRequest::GitRead(request) => {
            crate::daemon_contract::DaemonInvocationRequest::git_read(
                request_id.as_str(),
                operation,
                request,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::GitPreview(request) => {
            crate::daemon_contract::DaemonInvocationRequest::git_preview(
                request_id.as_str(),
                request,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::GitApply(request) => {
            crate::daemon_contract::DaemonInvocationRequest::git_apply(
                request_id.as_str(),
                request,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::Feedback(request) => {
            crate::daemon_contract::DaemonInvocationRequest::feedback(
                request_id.as_str(),
                operation,
                request.request_handle,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::FeedbackAdvisoryCycle(request) => {
            crate::daemon_contract::DaemonInvocationRequest::feedback_advisory_cycle(
                request_id.as_str(),
                request.document_uri,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::FeedbackImpact(request) => {
            crate::daemon_contract::DaemonInvocationRequest::feedback(
                request_id.as_str(),
                ApplicationSurfaceOperation::FeedbackImpact,
                request.request_handle,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::AffectedTests(request) => {
            crate::daemon_contract::DaemonInvocationRequest::feedback(
                request_id.as_str(),
                ApplicationSurfaceOperation::AffectedTests,
                request.request_handle,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::TestResults(_) => {
            crate::daemon_contract::DaemonInvocationRequest::primitive(
                request_id.as_str(),
                operation,
                crate::application::primitives::Pr12PrimitiveRequest::RecentTestResults(
                    invocation.page,
                ),
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::CallableCode(request) => {
            crate::daemon_contract::DaemonInvocationRequest::callable_code(
                request_id.as_str(),
                operation,
                request,
                invocation.page,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::PrimitiveCode(request) => {
            crate::daemon_contract::DaemonInvocationRequest::primitive_code(
                request_id.as_str(),
                operation,
                request,
                invocation.page,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::Primitive(request) => {
            crate::daemon_contract::DaemonInvocationRequest::primitive(
                request_id.as_str(),
                operation,
                request,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::Configuration(request) => {
            crate::daemon_contract::DaemonInvocationRequest::configuration(
                request_id.as_str(),
                operation,
                request,
                observed_at,
                deadline,
                cancellation_context,
            )
        }
        ApplicationSurfaceRequest::ContextScout(request) => {
            crate::daemon_contract::DaemonInvocationRequest::context_scout(
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
    let Some(executor) = executor else {
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
    let policy = if matches!(
        operation,
        ApplicationSurfaceOperation::GitApply
            | ApplicationSurfaceOperation::ConfigurationSet
            | ApplicationSurfaceOperation::ConfigurationUnset
            | ApplicationSurfaceOperation::ConfigurationBatch
            | ApplicationSurfaceOperation::ConfigurationWriteCredential
            | ApplicationSurfaceOperation::ConfigurationProtectedApply
            | ApplicationSurfaceOperation::ConfigurationRollbackApply
            | ApplicationSurfaceOperation::ContextScoutPause
            | ApplicationSurfaceOperation::ContextScoutResume
            | ApplicationSurfaceOperation::ContextScoutCancel
            | ApplicationSurfaceOperation::ContextScoutClaim
            | ApplicationSurfaceOperation::ContextScoutDelivery
            | ApplicationSurfaceOperation::ContextScoutFeedback
    ) {
        InvocationCancellationPolicy::AuthoritativeEffect
    } else {
        InvocationCancellationPolicy::ReadOnly
    };
    let response = executor
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
                let _ = executor
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
        crate::daemon_contract::DaemonInvocationOutcome::GitRead { scope, result } => {
            Ok(ApplicationEnvelope::evidence(
                result_contract.clone(),
                request_id.clone(),
                scope,
                result.into_application(),
            ))
        }
        crate::daemon_contract::DaemonInvocationOutcome::GitPreview { scope, preview } => {
            Ok(ApplicationEnvelope::preview(
                result_contract.clone(),
                request_id.clone(),
                scope,
                preview.into_application_result()?,
            ))
        }
        crate::daemon_contract::DaemonInvocationOutcome::GitApply { scope, effect } => {
            Ok(ApplicationEnvelope::effect(
                result_contract.clone(),
                request_id.clone(),
                scope,
                effect.into_application_result()?,
            ))
        }
        crate::daemon_contract::DaemonInvocationOutcome::Feedback { scope, result }
        | crate::daemon_contract::DaemonInvocationOutcome::Primitive { scope, result } => {
            Ok(ApplicationEnvelope::evidence(
                result_contract.clone(),
                request_id.clone(),
                scope,
                result.into_application(),
            ))
        }
        crate::daemon_contract::DaemonInvocationOutcome::CallableCode { scope, result } => {
            Ok(ApplicationEnvelope::evidence(
                result_contract.clone(),
                request_id.clone(),
                scope,
                result.into_application(),
            ))
        }
        crate::daemon_contract::DaemonInvocationOutcome::Configuration { scope, outcome } => {
            Ok(ApplicationEnvelope {
                contract: result_contract.clone(),
                request_id: request_id.clone(),
                scope,
                outcome,
            })
        }
        crate::daemon_contract::DaemonInvocationOutcome::ContextScout { scope, outcome } => {
            Ok(ApplicationEnvelope {
                contract: result_contract.clone(),
                request_id: request_id.clone(),
                scope,
                outcome,
            })
        }
        crate::daemon_contract::DaemonInvocationOutcome::ApplicationProblem { problem } => Err(
            ApplicationProblemEnvelope::new(result_contract.clone(), request_id.clone(), problem),
        ),
        crate::daemon_contract::DaemonInvocationOutcome::Problem { problem } => {
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
        ApplicationSurfaceOperation::FeedbackAdvisoryCycle => {
            Plan26FeedbackOperationV1::FeedbackCycle
        }
        ApplicationSurfaceOperation::FeedbackImpact => Plan26FeedbackOperationV1::PrimitiveImpact,
        ApplicationSurfaceOperation::AffectedTests => {
            Plan26FeedbackOperationV1::PrimitiveAffectedTests
        }
        ApplicationSurfaceOperation::TestResults => Plan26FeedbackOperationV1::PrimitiveTestResults,
        ApplicationSurfaceOperation::GitStatus
        | ApplicationSurfaceOperation::GitDiff
        | ApplicationSurfaceOperation::GitHistory
        | ApplicationSurfaceOperation::GitBlame
        | ApplicationSurfaceOperation::GitHunks
        | ApplicationSurfaceOperation::GitPreview
        | ApplicationSurfaceOperation::GitApply
        | ApplicationSurfaceOperation::CodeExactOccurrence
        | ApplicationSurfaceOperation::CodePhraseSearch
        | ApplicationSurfaceOperation::CodeSymbolSearch
        | ApplicationSurfaceOperation::CodeSignatureSearch
        | ApplicationSurfaceOperation::CodeImplementations
        | ApplicationSurfaceOperation::CodeTypeHierarchy
        | ApplicationSurfaceOperation::CodeCallers
        | ApplicationSurfaceOperation::CodeCallees
        | ApplicationSurfaceOperation::CodeFacets
        | ApplicationSurfaceOperation::CodeTimeline
        | ApplicationSurfaceOperation::CodeDeclaration
        | ApplicationSurfaceOperation::CodeDefinition
        | ApplicationSurfaceOperation::CodeTypeDefinition
        | ApplicationSurfaceOperation::CodeReferences
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
        | ApplicationSurfaceOperation::HealthDelta
        | ApplicationSurfaceOperation::StorageStatus
        | ApplicationSurfaceOperation::DiagnosticsRead
        | ApplicationSurfaceOperation::ConfigurationList
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
        | ApplicationSurfaceOperation::ConfigurationAudit
        | ApplicationSurfaceOperation::ContextScoutStatus
        | ApplicationSurfaceOperation::ContextScoutRecent
        | ApplicationSurfaceOperation::ContextScoutExplain
        | ApplicationSurfaceOperation::ContextScoutCapability
        | ApplicationSurfaceOperation::ContextScoutBudget
        | ApplicationSurfaceOperation::ContextScoutPause
        | ApplicationSurfaceOperation::ContextScoutResume
        | ApplicationSurfaceOperation::ContextScoutCancel
        | ApplicationSurfaceOperation::ContextScoutClaim
        | ApplicationSurfaceOperation::ContextScoutDelivery
        | ApplicationSurfaceOperation::ContextScoutFeedback => {
            Plan26FeedbackOperationV1::FeedbackCycle
        }
    }
}

fn plan26_surface_is_observable(operation: ApplicationSurfaceOperation) -> bool {
    matches!(
        operation,
        ApplicationSurfaceOperation::FeedbackDiagnostics
            | ApplicationSurfaceOperation::FeedbackGet
            | ApplicationSurfaceOperation::FeedbackExpand
            | ApplicationSurfaceOperation::FeedbackList
            | ApplicationSurfaceOperation::FeedbackAdvisoryCycle
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
            | ApplicationSurfaceOperation::HealthDelta
            | ApplicationSurfaceOperation::StorageStatus
            | ApplicationSurfaceOperation::DiagnosticsRead
    )
}

pub async fn observe_surface_argument_rejection(
    executor: Option<&dyn crate::daemon_client::DaemonInvocationExecutor>,
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    request_id: &RequestId,
    error: &ApplicationSurfaceAdapterError,
) {
    if !plan26_surface_is_observable(operation) {
        return;
    }
    let Some((argument, rejection, outcome)) = surface_rejection_metadata(error) else {
        return;
    };
    let (Some(executor), Ok(subject_digest), Ok(observed_at)) = (
        executor,
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
    let _ = executor
        .observe_plan26_feedback(
            subject_digest,
            observed_at,
            Plan26FeedbackSourceEventV1::SurfaceArgumentRejected {
                operation: plan26_surface_operation(operation),
                route: Some(plan26_delivery_route(surface)),
                argument,
                rejection,
                schema_revision: 1,
                outcome,
            },
        )
        .await;
}

fn surface_rejection_metadata(
    error: &ApplicationSurfaceAdapterError,
) -> Option<(
    Plan26RejectedArgumentV1,
    Plan26ArgumentRejectionClassV1,
    Plan26FeedbackOutcomeV1,
)> {
    match error {
        ApplicationSurfaceAdapterError::InvalidRequestHandle => Some((
            Plan26RejectedArgumentV1::RequestHandle,
            Plan26ArgumentRejectionClassV1::InvalidShape,
            Plan26FeedbackOutcomeV1::Rejected,
        )),
        ApplicationSurfaceAdapterError::InvalidSurfaceRequest => Some((
            Plan26RejectedArgumentV1::RequestBody,
            Plan26ArgumentRejectionClassV1::InvalidShape,
            Plan26FeedbackOutcomeV1::Rejected,
        )),
        ApplicationSurfaceAdapterError::UnknownOrNotAuthorized => Some((
            Plan26RejectedArgumentV1::Operation,
            Plan26ArgumentRejectionClassV1::Unauthorized,
            Plan26FeedbackOutcomeV1::Denied,
        )),
        ApplicationSurfaceAdapterError::Catalog(_)
        | ApplicationSurfaceAdapterError::Contract(_)
        | ApplicationSurfaceAdapterError::Identifier(_)
        | ApplicationSurfaceAdapterError::CatalogValidation(_)
        | ApplicationSurfaceAdapterError::DaemonUnavailable => None,
    }
}

pub async fn resolve_http_application_surface(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    executor: Option<&dyn crate::daemon_client::DaemonInvocationExecutor>,
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
                executor,
                BindingSurface::Http,
                operation,
                &request_id,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    execute_application_surface(operation, dispatched, executor).await
}

/// Resolve a dashboard action through the same catalog entry and daemon-owned
/// application handler as CLI, MCP, and HTTP. Dashboard adapters may shape
/// presentation responses around this result, but they do not own mutation
/// validation, authorization, CAS, receipts, or rollback semantics.
pub async fn resolve_dashboard_application_surface(
    operation: ApplicationSurfaceOperation,
    request_id: RequestId,
    request: ApplicationSurfaceRequest,
    requested_format: RequestedOutputFormat,
    executor: Option<&dyn crate::daemon_client::DaemonInvocationExecutor>,
) -> Result<ApplicationSurfaceInvocationResult, ApplicationSurfaceAdapterError> {
    let dispatched = resolve_application_surface_dispatch(
        BindingSurface::Dashboard,
        operation,
        request_id,
        request,
        requested_format,
    )?;
    execute_application_surface(operation, dispatched, executor).await
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
    let catalog = application_surface_catalog_ref()?;
    let resolver = CatalogBindingResolver::new(catalog);
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

fn invoke_catalog_bound_application_request(
    request: HttpApplicationRequest,
    surface: BindingSurface,
    composition: &ApplicationCatalogComposition<HttpApplicationCatalogDispatcher>,
) -> HttpApplicationInvocationFuture {
    let profile_id = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)
        .unwrap_or_else(|_| panic!("the application profile id is static"));
    let operation_name = SurfaceOperationName::new(request.operation.as_str())
        .unwrap_or_else(|_| panic!("the application operation name is static"));
    let capability = composition
        .snapshot()
        .resolve_binding(&profile_id, surface, &operation_name, 1, &BTreeSet::new())
        .unwrap_or_else(|| {
            panic!("surface bindings are validated before the application router is mounted")
        });
    let handler = composition
        .handler(capability.use_case_id())
        .unwrap_or_else(|| panic!("catalog composition validates every callable handler"));
    handler.invoke(CatalogBoundHttpApplicationRequest {
        capability_id: capability.capability_id().clone(),
        use_case_id: capability.use_case_id().clone(),
        surface,
        request,
    })
}

async fn invoke_application_adapter_request(
    request: HttpApplicationRequest,
    surface: BindingSurface,
    executor: &dyn crate::daemon_client::DaemonInvocationExecutor,
    catalog: &CatalogSnapshotV1,
) -> CanonicalInvocationResult<Value> {
    let operation = request.operation;
    let resolver = CatalogBindingResolver::new(catalog);
    let binding = resolve_application_binding(&resolver, surface, operation).unwrap_or_else(|| {
        panic!("surface bindings are validated before the application router is mounted")
    });
    let binding_id = binding.binding_id;
    let result_contract = ResultContractRef::from_schema(&binding.result_schema);
    let request_id = request.request_id;
    let body = apply_http_page_to_surface_body(operation, request.body, &request.page);
    let application_request = match parse_application_surface_request(operation, body) {
        Ok(request) => request,
        Err(error) => {
            observe_surface_argument_rejection(
                Some(executor),
                surface,
                operation,
                &request_id,
                &error,
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
                Some(executor),
                surface,
                operation,
                &request_id,
                &error,
            )
            .await;
            return CanonicalInvocationResult::new(
                binding_id,
                Err(http_adapter_problem(result_contract, request_id, error)),
            );
        }
    };
    let dispatched = match resolve_dispatch(&resolver, surface, input) {
        Ok(dispatched) => dispatched,
        Err(error) => {
            let error = map_dispatch_error(error);
            observe_surface_argument_rejection(
                Some(executor),
                surface,
                operation,
                &request_id,
                &error,
            )
            .await;
            return CanonicalInvocationResult::new(
                binding_id,
                Err(http_adapter_problem(result_contract, request_id, error)),
            );
        }
    };
    match execute_application_surface(operation, dispatched, Some(executor)).await {
        Ok(result) => CanonicalInvocationResult::new(result.binding_id, result.result),
        Err(error) => CanonicalInvocationResult::new(
            binding_id,
            Err(http_adapter_problem(result_contract, request_id, error)),
        ),
    }
}

/// Operations whose decoded surface request carries a [`CallableCodeSurfaceMeta`].
fn operation_carries_callable_code_meta(operation: ApplicationSurfaceOperation) -> bool {
    matches!(
        operation,
        ApplicationSurfaceOperation::CodeExactOccurrence
            | ApplicationSurfaceOperation::CodePhraseSearch
            | ApplicationSurfaceOperation::CodeSymbolSearch
            | ApplicationSurfaceOperation::CodeSignatureSearch
            | ApplicationSurfaceOperation::CodeImplementations
            | ApplicationSurfaceOperation::CodeTypeHierarchy
            | ApplicationSurfaceOperation::CodeCallers
            | ApplicationSurfaceOperation::CodeCallees
            | ApplicationSurfaceOperation::CodeFacets
            | ApplicationSurfaceOperation::CodeTimeline
            | ApplicationSurfaceOperation::CodeDeclaration
            | ApplicationSurfaceOperation::CodeDefinition
            | ApplicationSurfaceOperation::CodeTypeDefinition
            | ApplicationSurfaceOperation::CodeReferences
    )
}

fn apply_http_page_to_surface_body(
    operation: ApplicationSurfaceOperation,
    mut body: Value,
    page: &PageRequest,
) -> Value {
    if operation_carries_callable_code_meta(operation) {
        if let Some(meta) = body.get_mut("meta").and_then(Value::as_object_mut)
            && let Some(cursor) = page.cursor.as_ref()
        {
            meta.insert("cursor".to_owned(), Value::from(cursor.as_str()));
        }
        return body;
    }
    if operation != ApplicationSurfaceOperation::DiagnosticsRead {
        return body;
    }
    if let Some(object) = body.as_object_mut() {
        object.insert(
            "maximum_diagnostics".to_owned(),
            Value::from(page.page_size),
        );
        object.insert(
            "cursor".to_owned(),
            page.cursor
                .as_ref()
                .map_or(Value::Null, |cursor| Value::from(cursor.as_str())),
        );
    }
    body
}

fn resolve_application_binding(
    resolver: &impl BindingResolver,
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
) -> Option<crate::daemon_client::ResolvedBinding> {
    resolve_named_binding(resolver, surface, operation.as_str())
}

fn resolve_named_binding(
    resolver: &impl BindingResolver,
    surface: BindingSurface,
    operation: &str,
) -> Option<ResolvedBinding> {
    let profile_id = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).ok()?;
    let operation = SurfaceOperationName::new(operation).ok()?;
    resolver.resolve_binding(
        surface,
        &BindingResolution {
            profile_id,
            operation,
            protocol_revision: APPLICATION_PROTOCOL_REVISION,
            negotiated_features: application_negotiated_features(),
        },
    )
}

/// Resolve any application-catalog transport binding by its public tool name.
///
/// Typed application surfaces continue through [`ApplicationSurfaceOperation`].
/// Catalog bindings whose typed adapters are still being migrated use this
/// gate before entering their retained compatibility owner.
/// Resolves a public tool name through the application catalog for one host surface.
///
/// Compatibility-owned tools use this boundary before entering their retained
/// execution adapter, so catalog metadata remains the single binding authority.
pub fn resolve_catalog_tool_binding(
    surface: BindingSurface,
    tool_name: &str,
) -> Result<Option<ResolvedBinding>, ApplicationSurfaceAdapterError> {
    let operation = tool_name.strip_prefix("tracedecay_").unwrap_or(tool_name);
    let catalog = application_surface_catalog_ref()?;
    let resolver = CatalogBindingResolver::new(catalog);
    Ok(resolve_named_binding(&resolver, surface, operation))
}

fn application_negotiated_features() -> BTreeSet<FeatureId> {
    BTreeSet::new()
}

fn validate_current_application_binding(
    operation: ApplicationSurfaceOperation,
    dispatched: &DispatchedInvocation<ApplicationSurfaceRequest>,
) -> Result<(), ApplicationSurfaceAdapterError> {
    let catalog = application_surface_catalog_ref()?;
    let resolver = CatalogBindingResolver::new(catalog);
    let current = resolve_application_binding(&resolver, dispatched.surface, operation)
        .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
    if current.binding_id != dispatched.invocation.binding_id
        || current.request_schema != dispatched.invocation.request_schema
        || current.result_schema != dispatched.invocation.result_schema
        || !dispatched.invocation.invocation.request.matches(operation)
    {
        return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
    }
    Ok(())
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
        // Catalog composition is derived from `const` application specs, so
        // these failures are deterministic for the lifetime of the process.
        // Reporting them as `unavailable` told clients to retry a request that
        // can never succeed.
        ApplicationSurfaceAdapterError::Catalog(_)
        | ApplicationSurfaceAdapterError::Contract(_)
        | ApplicationSurfaceAdapterError::Identifier(_)
        | ApplicationSurfaceAdapterError::CatalogValidation(_) => ApplicationProblem::Unsupported {
            diagnostic: SafeDiagnostic {
                code: "application.surface.catalog_unavailable".to_owned(),
                message: "The application catalog for this operation could not be composed"
                    .to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::ContactAdministrator],
        },
        // Genuinely transient: the owning daemon transport is not answering.
        ApplicationSurfaceAdapterError::DaemonUnavailable => {
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
    problem: crate::daemon_contract::DaemonInvocationProblem,
) -> Result<ApplicationProblem, ApplicationSurfaceAdapterError> {
    Ok(match problem {
        crate::daemon_contract::DaemonInvocationProblem::InvalidRequest
        | crate::daemon_contract::DaemonInvocationProblem::UnsupportedRevision => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic::new(
                    "application.surface.invalid_request",
                    "The daemon rejected the application request",
                )?,
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            }
        }
        crate::daemon_contract::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        crate::daemon_contract::DaemonInvocationProblem::Unavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic::new(
                "application.surface.unavailable",
                "The application service for this operation is unavailable",
            )?)
        }
    })
}

fn invocation_contract_problem(
    error: tracedecay_application::InvocationError,
) -> Result<ApplicationProblem, ApplicationSurfaceAdapterError> {
    Ok(match error {
        tracedecay_application::InvocationError::Denied => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        tracedecay_application::InvocationError::Cancelled => {
            ApplicationProblem::cancelled_before_admission()
        }
        tracedecay_application::InvocationError::DeadlineExceeded => {
            ApplicationProblem::timed_out_before_admission()
        }
        tracedecay_application::InvocationError::InvalidRequest => {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic::new(
                    "application.surface.invalid_request",
                    "The daemon rejected the application request",
                )?,
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            }
        }
        tracedecay_application::InvocationError::Conflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic::new(
                "application.surface.conflict",
                "The application request conflicts with current state",
            )?,
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        tracedecay_application::InvocationError::Unavailable => {
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

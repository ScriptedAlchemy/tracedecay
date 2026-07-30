//! Single-root daemon LSP gateway request boundary.
//!
//! The gateway has one already-admitted root and delegates post-edit work to
//! the feedback-cycle application boundary. It intentionally does not open a
//! store, supervise an analyzer, resolve workspace folders, or implement any
//! host-specific transport.

#[path = "operation_table.rs"]
pub(crate) mod operation_table;

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Component, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay_domain::ManifestDigest;
use url::Url;

use crate::capabilities::{
    CapabilityAvailability, ClientCapabilities, EffectiveCapabilities, GatewayCapabilities,
    SemanticCapability, UpstreamCapabilities, negotiate_capabilities,
};
use crate::context::{ContextProjectionPort, MAX_CONTEXT_PROJECTION_KINDS};
use crate::diagnostics::{
    DiagnosticMerge, DocumentDiagnosticReport, GatewayDiagnostic, LspPosition, LspRange,
};
use crate::gateway::operation_table::{
    BoundedOperationCapacity, BoundedOperationTable, OperationAdmission, OperationPoll,
};
use crate::protocol::DaemonLspProtocolSession;
use crate::provider::{AnalyzerCancellationPort, DiagnosticSnapshotPort};
use crate::session::{
    AuthorizedLspWorkspace, LspRequestFailure, LspRequestId, LspWorkspaceRouteError,
    MAX_PENDING_REQUESTS,
};

/// A single root that was authoritatively admitted before the LSP session was
/// created. The gateway never chooses a root from CWD or client folder order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedRoot {
    uri: String,
    scope_digest: Option<ManifestDigest>,
}

impl AdmittedRoot {
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            scope_digest: None,
        }
    }

    /// Bind a presentation URI to an exact application-resolved scope.
    pub fn authorized(uri: impl Into<String>, scope_digest: ManifestDigest) -> Self {
        Self {
            uri: uri.into(),
            scope_digest: Some(scope_digest),
        }
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn scope_digest(&self) -> Option<&ManifestDigest> {
        self.scope_digest.as_ref()
    }

    pub(crate) fn is_valid(&self) -> bool {
        strict_file_uri_path(&self.uri).is_some()
    }

    pub(crate) fn matches_root_uri(&self, candidate: &str) -> bool {
        match (
            strict_file_uri_path(&self.uri),
            strict_file_uri_path(candidate),
        ) {
            (Some((admitted_url, admitted_path)), Some((candidate_url, candidate_path))) => {
                admitted_url.host_str() == candidate_url.host_str()
                    && admitted_path == candidate_path
            }
            _ => false,
        }
    }

    /// Presentation-level containment guard. Root admission itself remains a
    /// daemon authorization decision; this rejects non-file and ambiguous URI
    /// forms, then compares decoded filesystem path components rather than raw
    /// URI prefixes.
    pub fn contains_document(&self, document_uri: &str) -> bool {
        let Some((root_url, root_path)) = strict_file_uri_path(&self.uri) else {
            return false;
        };
        let Some((document_url, document_path)) = strict_file_uri_path(document_uri) else {
            return false;
        };
        if root_url.host_str() != document_url.host_str() {
            return false;
        }
        document_path
            .strip_prefix(&root_path)
            .is_ok_and(|relative| {
                !relative.as_os_str().is_empty()
                    && relative
                        .components()
                        .all(|component| matches!(component, Component::Normal(_)))
            })
    }
}

fn strict_file_uri_path(uri: &str) -> Option<(Url, PathBuf)> {
    let (_, after_scheme) = uri.split_once(':')?;
    if after_scheme.contains('\\') {
        return None;
    }
    let raw_path = if let Some(authority_and_path) = after_scheme.strip_prefix("//") {
        authority_and_path
            .find('/')
            .map_or("", |path_start| &authority_and_path[path_start..])
    } else {
        after_scheme
    };
    if !valid_raw_uri_path(raw_path) {
        return None;
    }

    let url = Url::parse(uri).ok()?;
    if url.scheme() != "file"
        || url.cannot_be_a_base()
        || url.path().is_empty()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.to_file_path().ok()?;
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return None;
    }
    Some((url, path))
}

fn valid_raw_uri_path(raw_path: &str) -> bool {
    if raw_path.is_empty() || raw_path.as_bytes().contains(&0) {
        return false;
    }
    let segments = raw_path.split('/').collect::<Vec<_>>();
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            let is_leading = index == 0;
            let is_trailing = index + 1 == segments.len();
            if !is_leading && !is_trailing {
                return false;
            }
            continue;
        }
        let Some(decoded) = decode_uri_segment(segment) else {
            return false;
        };
        if decoded == b"."
            || decoded == b".."
            || decoded.iter().any(|byte| matches!(*byte, b'/' | b'\\' | 0))
        {
            return false;
        }
    }
    true
}

fn decode_uri_segment(segment: &str) -> Option<Vec<u8>> {
    let source = segment.as_bytes();
    let mut decoded = Vec::with_capacity(source.len());
    let mut index = 0;
    while index < source.len() {
        if source[index] != b'%' {
            decoded.push(source[index]);
            index += 1;
            continue;
        }
        let high = source.get(index + 1).copied().and_then(hex_value)?;
        let low = source.get(index + 2).copied().and_then(hex_value)?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    Some(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The feedback-cycle trigger source used by document lifecycle requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticTrigger {
    DocumentSave,
    ExplicitDocumentDiagnostics,
}

/// A bounded request sent from the gateway to the existing feedback-cycle
/// application boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedbackCycleRequest {
    pub root_uri: String,
    pub document_uri: String,
    pub trigger: DiagnosticTrigger,
}

/// A scheduler/application outcome for a feedback-cycle request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FeedbackCycleResponse {
    Accepted,
    Deferred { reason: String },
    Rejected { reason: String },
}

/// Port implemented by the daemon/application adapter.
///
/// The implementation must delegate to the existing feedback-cycle operation
/// (ultimately `tracedecay_application::feedback::FeedbackCycleService`) and
/// must not create a second gateway-local finding store.
pub trait FeedbackCyclePort {
    fn request_feedback_cycle(&self, request: FeedbackCycleRequest) -> FeedbackCycleResponse;
}

impl<T> FeedbackCyclePort for Arc<T>
where
    T: FeedbackCyclePort + ?Sized,
{
    fn request_feedback_cycle(&self, request: FeedbackCycleRequest) -> FeedbackCycleResponse {
        (**self).request_feedback_cycle(request)
    }
}

/// Methods represented by this bounded LSP gateway surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatewayMethod {
    TextDocumentDiagnostic,
    TextDocumentDeclaration,
    TextDocumentDefinition,
    TextDocumentTypeDefinition,
    TextDocumentImplementation,
    TextDocumentReferences,
    TextDocumentHover,
    TextDocumentDocumentSymbol,
    WorkspaceSymbol,
    TextDocumentPrepareCallHierarchy,
    CallHierarchyIncomingCalls,
    CallHierarchyOutgoingCalls,
    TextDocumentSignatureHelp,
    TextDocumentPrepareTypeHierarchy,
    TypeHierarchySupertypes,
    TypeHierarchySubtypes,
    TextDocumentPrepareRename,
    TextDocumentRename,
    TextDocumentCodeAction,
    WorkspaceDiagnostic,
    WorkspaceExecuteCommand,
    WorkspaceFolders,
    GitHubCiProximityTransport,
    DirectDatabaseWrite,
}

impl GatewayMethod {
    pub fn as_lsp_method(self) -> &'static str {
        match self {
            Self::TextDocumentDiagnostic => "textDocument/diagnostic",
            Self::TextDocumentDeclaration => "textDocument/declaration",
            Self::TextDocumentDefinition => "textDocument/definition",
            Self::TextDocumentTypeDefinition => "textDocument/typeDefinition",
            Self::TextDocumentImplementation => "textDocument/implementation",
            Self::TextDocumentReferences => "textDocument/references",
            Self::TextDocumentHover => "textDocument/hover",
            Self::TextDocumentDocumentSymbol => "textDocument/documentSymbol",
            Self::WorkspaceSymbol => "workspace/symbol",
            Self::TextDocumentPrepareCallHierarchy => "textDocument/prepareCallHierarchy",
            Self::CallHierarchyIncomingCalls => "callHierarchy/incomingCalls",
            Self::CallHierarchyOutgoingCalls => "callHierarchy/outgoingCalls",
            Self::TextDocumentSignatureHelp => "textDocument/signatureHelp",
            Self::TextDocumentPrepareTypeHierarchy => "textDocument/prepareTypeHierarchy",
            Self::TypeHierarchySupertypes => "typeHierarchy/supertypes",
            Self::TypeHierarchySubtypes => "typeHierarchy/subtypes",
            Self::TextDocumentPrepareRename => "textDocument/prepareRename",
            Self::TextDocumentRename => "textDocument/rename",
            Self::TextDocumentCodeAction => "textDocument/codeAction",
            Self::WorkspaceDiagnostic => "workspace/diagnostic",
            Self::WorkspaceExecuteCommand => "workspace/executeCommand",
            Self::WorkspaceFolders => "workspace/didChangeWorkspaceFolders",
            Self::GitHubCiProximityTransport => "tracedecay/github-ci-proximity",
            Self::DirectDatabaseWrite => "tracedecay/direct-database-write",
        }
    }
}

/// The reason a request cannot be served by this session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MethodUnavailableReason {
    ExplicitlyUnavailable,
    CapabilityNotNegotiated,
    OutsideAdmittedRoot,
    AmbiguousAdmittedRoot,
    ProviderUnavailable,
}

/// A typed unavailable result. The future JSON-RPC adapter maps this to the
/// standard method-not-found error rather than inventing a fallback value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MethodUnavailable {
    pub method: GatewayMethod,
    pub reason: MethodUnavailableReason,
}

impl MethodUnavailable {
    pub const JSON_RPC_METHOD_NOT_FOUND: i64 = -32601;
}

/// The protocol dispatch outcome used by request handlers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GatewayResponse<T> {
    Value(T),
    Partial {
        value: T,
        coverage: String,
        /// Bounded human-readable failure message for the coverage token,
        /// surfaced to LSP callers in the JSON-RPC error `data`.
        detail: Option<String>,
    },
    Pending,
    Unavailable(MethodUnavailable),
    RequestFailed(LspRequestFailure),
}

impl<T> GatewayResponse<T> {
    fn unavailable(method: GatewayMethod, reason: MethodUnavailableReason) -> Self {
        Self::Unavailable(MethodUnavailable { method, reason })
    }
}

/// LSP `Location` payload shape used by navigation responses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspLocation {
    pub uri: String,
    pub range: LspRange,
}

/// LSP `Hover` payload shape used by semantic providers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hover {
    pub contents: String,
    pub range: Option<LspRange>,
}

/// LSP `DocumentSymbol` payload shape used by semantic providers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSymbol {
    pub name: String,
    /// LSP `SymbolKind` supplied by the admitted provider.
    pub kind: u32,
    pub range: LspRange,
    pub selection_range: LspRange,
    pub children: Vec<DocumentSymbol>,
}

/// LSP `SymbolInformation` payload shape used by semantic providers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSymbol {
    pub name: String,
    /// LSP `SymbolKind` supplied by the admitted provider.
    pub kind: u32,
    pub location: LspLocation,
}

/// LSP `CallHierarchyItem` payload shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallHierarchyItem {
    pub name: String,
    /// LSP `SymbolKind` supplied by the admitted provider.
    pub kind: u32,
    pub uri: String,
    pub range: LspRange,
    pub selection_range: LspRange,
}

/// LSP `CallHierarchyIncomingCall` payload shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncomingCall {
    pub from: CallHierarchyItem,
    pub from_ranges: Vec<LspRange>,
}

/// LSP `CallHierarchyOutgoingCall` payload shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutgoingCall {
    pub to: CallHierarchyItem,
    pub from_ranges: Vec<LspRange>,
}

/// LSP `SignatureHelp` payload shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureHelp {
    pub signatures: Vec<String>,
    pub active_signature: Option<u32>,
    pub active_parameter: Option<u32>,
}

/// LSP `TypeHierarchyItem` payload shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeHierarchyItem {
    pub name: String,
    /// LSP `SymbolKind` supplied by the admitted provider.
    pub kind: u32,
    pub uri: String,
    pub range: LspRange,
    pub selection_range: LspRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenameCandidate {
    pub document_uri: String,
    pub range: LspRange,
    pub placeholder: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenameCandidateUnavailableReason {
    AnalyzerUnavailable,
    GraphUnavailable,
    EvidenceAbsent,
    StaleEvidence,
    AmbiguousEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenameCandidateResult {
    Available(RenameCandidate),
    Unavailable {
        reason: RenameCandidateUnavailableReason,
    },
}

/// A truthful semantic-provider outcome. Empty collections are complete only
/// when the provider says they are complete; unavailable and partial states
/// cannot collapse into a plausible clean empty result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticProviderOutcome<T> {
    Complete(T),
    Partial {
        value: T,
        coverage: String,
        /// Bounded human-readable failure message for the coverage token,
        /// carried through to the JSON-RPC error `data` for LSP callers.
        detail: Option<String>,
    },
    Pending,
    Unavailable,
}

impl<T> SemanticProviderOutcome<T> {
    fn map<U>(self, project: impl FnOnce(T) -> U) -> SemanticProviderOutcome<U> {
        match self {
            Self::Complete(value) => SemanticProviderOutcome::Complete(project(value)),
            Self::Partial {
                value,
                coverage,
                detail,
            } => SemanticProviderOutcome::Partial {
                value: project(value),
                coverage,
                detail,
            },
            Self::Pending => SemanticProviderOutcome::Pending,
            Self::Unavailable => SemanticProviderOutcome::Unavailable,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticRequest {
    Declaration {
        document_uri: String,
        position: LspPosition,
    },
    Definition {
        document_uri: String,
        position: LspPosition,
    },
    TypeDefinition {
        document_uri: String,
        position: LspPosition,
    },
    Implementation {
        document_uri: String,
        position: LspPosition,
    },
    References {
        document_uri: String,
        position: LspPosition,
    },
    Hover {
        document_uri: String,
        position: LspPosition,
    },
    DocumentSymbols {
        document_uri: String,
    },
    WorkspaceSymbols {
        query: String,
    },
    PrepareCallHierarchy {
        document_uri: String,
        position: LspPosition,
    },
    IncomingCalls {
        item: CallHierarchyItem,
    },
    OutgoingCalls {
        item: CallHierarchyItem,
    },
    SignatureHelp {
        document_uri: String,
        position: LspPosition,
    },
    PrepareTypeHierarchy {
        document_uri: String,
        position: LspPosition,
    },
    TypeHierarchySupertypes {
        item: TypeHierarchyItem,
    },
    TypeHierarchySubtypes {
        item: TypeHierarchyItem,
    },
    RenameCandidate {
        document_uri: String,
        position: LspPosition,
    },
}

impl SemanticRequest {
    pub fn method(&self) -> GatewayMethod {
        match self {
            Self::Declaration { .. } => GatewayMethod::TextDocumentDeclaration,
            Self::Definition { .. } => GatewayMethod::TextDocumentDefinition,
            Self::TypeDefinition { .. } => GatewayMethod::TextDocumentTypeDefinition,
            Self::Implementation { .. } => GatewayMethod::TextDocumentImplementation,
            Self::References { .. } => GatewayMethod::TextDocumentReferences,
            Self::Hover { .. } => GatewayMethod::TextDocumentHover,
            Self::DocumentSymbols { .. } => GatewayMethod::TextDocumentDocumentSymbol,
            Self::WorkspaceSymbols { .. } => GatewayMethod::WorkspaceSymbol,
            Self::PrepareCallHierarchy { .. } => GatewayMethod::TextDocumentPrepareCallHierarchy,
            Self::IncomingCalls { .. } => GatewayMethod::CallHierarchyIncomingCalls,
            Self::OutgoingCalls { .. } => GatewayMethod::CallHierarchyOutgoingCalls,
            Self::SignatureHelp { .. } => GatewayMethod::TextDocumentSignatureHelp,
            Self::PrepareTypeHierarchy { .. } => GatewayMethod::TextDocumentPrepareTypeHierarchy,
            Self::TypeHierarchySupertypes { .. } => GatewayMethod::TypeHierarchySupertypes,
            Self::TypeHierarchySubtypes { .. } => GatewayMethod::TypeHierarchySubtypes,
            Self::RenameCandidate { .. } => GatewayMethod::TextDocumentPrepareRename,
        }
    }

    fn capability(&self) -> SemanticCapability {
        match self {
            Self::Declaration { .. } => SemanticCapability::Declaration,
            Self::Definition { .. } => SemanticCapability::Definition,
            Self::TypeDefinition { .. } => SemanticCapability::TypeDefinition,
            Self::Implementation { .. } => SemanticCapability::Implementation,
            Self::References { .. } => SemanticCapability::References,
            Self::Hover { .. } => SemanticCapability::Hover,
            Self::DocumentSymbols { .. } => SemanticCapability::DocumentSymbol,
            Self::WorkspaceSymbols { .. } => SemanticCapability::WorkspaceSymbol,
            Self::PrepareCallHierarchy { .. }
            | Self::IncomingCalls { .. }
            | Self::OutgoingCalls { .. } => SemanticCapability::CallHierarchy,
            Self::SignatureHelp { .. } => SemanticCapability::SignatureHelp,
            Self::PrepareTypeHierarchy { .. }
            | Self::TypeHierarchySupertypes { .. }
            | Self::TypeHierarchySubtypes { .. } => SemanticCapability::TypeHierarchy,
            Self::RenameCandidate { .. } => SemanticCapability::RenameCandidate,
        }
    }

    pub fn document_uri(&self) -> Option<&str> {
        match self {
            Self::Declaration { document_uri, .. }
            | Self::Definition { document_uri, .. }
            | Self::TypeDefinition { document_uri, .. }
            | Self::Implementation { document_uri, .. }
            | Self::References { document_uri, .. }
            | Self::Hover { document_uri, .. }
            | Self::DocumentSymbols { document_uri }
            | Self::PrepareCallHierarchy { document_uri, .. }
            | Self::SignatureHelp { document_uri, .. }
            | Self::PrepareTypeHierarchy { document_uri, .. }
            | Self::RenameCandidate { document_uri, .. } => Some(document_uri),
            Self::IncomingCalls { item } | Self::OutgoingCalls { item } => Some(&item.uri),
            Self::TypeHierarchySupertypes { item } | Self::TypeHierarchySubtypes { item } => {
                Some(&item.uri)
            }
            Self::WorkspaceSymbols { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticResponse {
    Locations(Vec<LspLocation>),
    Hover(Option<Hover>),
    DocumentSymbols(Vec<DocumentSymbol>),
    WorkspaceSymbols(Vec<WorkspaceSymbol>),
    CallHierarchyItems(Vec<CallHierarchyItem>),
    IncomingCalls(Vec<IncomingCall>),
    OutgoingCalls(Vec<OutgoingCall>),
    SignatureHelp(Option<SignatureHelp>),
    TypeHierarchyItems(Vec<TypeHierarchyItem>),
    RenameCandidate(RenameCandidateResult),
}

/// Standard LSP method and JSON parameters produced by the gateway broker.
///
/// Analyzer-process adapters may decode `params` into their preferred
/// `lsp-types` DTOs, but they must not replace the standard method or wire
/// shape.
#[derive(Clone, Debug, PartialEq)]
pub struct LspSemanticRequest {
    method: &'static str,
    params: Value,
}

impl LspSemanticRequest {
    pub(crate) fn from_standard(method: &'static str, params: Value) -> Self {
        Self { method, params }
    }

    pub fn method(&self) -> &'static str {
        self.method
    }

    pub fn params(&self) -> &Value {
        &self.params
    }

    pub fn into_params(self) -> Value {
        self.params
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LspSemanticOperationOutcome {
    Complete(Value),
    Partial {
        value: Value,
        coverage: String,
        detail: Option<&'static str>,
    },
    RenameCandidate(RenameCandidateResult),
    Unavailable,
}

impl LspSemanticOperationOutcome {
    pub const ANALYZER_START_FAILED_DETAIL: &'static str = "Analyzer failed to start.";
    pub const ANALYZER_CANCELLED_DETAIL: &'static str = "Analyzer request was cancelled.";
    pub const ANALYZER_TIMEOUT_DETAIL: &'static str = "Analyzer request timed out.";
    pub const ANALYZER_REMOTE_ERROR_DETAIL: &'static str =
        "Analyzer request failed with a remote error.";
    pub const ANALYZER_TRANSPORT_FAILED_DETAIL: &'static str = "Analyzer transport failed.";
    pub const ANALYZER_INVALID_RESPONSE_DETAIL: &'static str =
        "Analyzer returned an invalid response.";
    pub const GRAPH_READ_FAILED_DETAIL: &'static str = "Graph semantic read failed.";
}

pub type LspRuntimeFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Cancellation handle for a runtime task spawned on behalf of an LSP broker.
pub trait LspRuntimeTask: Send + Sync {
    fn abort(&self);
}

/// Runtime injection used by broker policy without taking a Tokio dependency.
pub trait LspRuntimeSpawner: Send + Sync {
    fn spawn(&self, future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask>;
}

const MAX_RUNTIME_FAILURE_CLASS_BYTES: usize = 96;
pub const MAX_SEMANTIC_OPERATIONS: usize = MAX_PENDING_REQUESTS * 2;

/// Bounded protocol-safe failure class returned by a runtime authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspRuntimeFailure {
    class: String,
}

impl LspRuntimeFailure {
    pub fn new(class: impl Into<String>) -> Self {
        let mut bounded = String::new();
        for character in class.into().chars() {
            if bounded.len().saturating_add(character.len_utf8()) > MAX_RUNTIME_FAILURE_CLASS_BYTES
            {
                break;
            }
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                bounded.push(character);
            }
        }
        if bounded.is_empty() {
            bounded.push_str("runtime-failure");
        }
        Self { class: bounded }
    }

    pub fn class(&self) -> &str {
        &self.class
    }
}

pub const MAX_FEEDBACK_CYCLES: usize = 128;

pub trait FeedbackCycleRuntimePort: Send + Sync {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>>;
}

/// Bounded non-blocking trigger for canonical feedback work.
pub struct FeedbackCycleAdapter {
    runtime: Arc<dyn LspRuntimeSpawner>,
    authority: Arc<dyn FeedbackCycleRuntimePort>,
    capacity: BoundedOperationCapacity,
}

impl FeedbackCycleAdapter {
    pub fn new(
        runtime: Arc<dyn LspRuntimeSpawner>,
        authority: Arc<dyn FeedbackCycleRuntimePort>,
    ) -> Self {
        Self {
            runtime,
            authority,
            capacity: BoundedOperationCapacity::new(MAX_FEEDBACK_CYCLES),
        }
    }
}

impl FeedbackCyclePort for FeedbackCycleAdapter {
    fn request_feedback_cycle(&self, request: FeedbackCycleRequest) -> FeedbackCycleResponse {
        let Some(permit) = self.capacity.acquire() else {
            return FeedbackCycleResponse::Deferred {
                reason: "feedback-cycle-capacity".to_owned(),
            };
        };
        let authority = Arc::clone(&self.authority);
        let _task = self.runtime.spawn(Box::pin(async move {
            let _permit = permit;
            if let Err(error) = authority.execute(request).await {
                eprintln!(
                    "[tracedecay] event=lsp_feedback_cycle_failed failure_class={}",
                    error.class()
                );
            }
        }));
        FeedbackCycleResponse::Accepted
    }
}

/// Canonical asynchronous owner for one standard analyzer request.
pub trait LspSemanticRequestAuthority: Send + Sync {
    fn start(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: LspSemanticRequest,
    ) -> LspRuntimeFuture<LspSemanticOperationOutcome>;

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool;
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SemanticRequestKey {
    root_uri: String,
    request_id: LspRequestId,
}

/// Bounded non-blocking semantic broker over an asynchronous authority.
pub struct SemanticProviderAdapter {
    runtime: Arc<dyn LspRuntimeSpawner>,
    authority: Arc<dyn LspSemanticRequestAuthority>,
    operations:
        BoundedOperationTable<SemanticRequestKey, &'static str, LspSemanticOperationOutcome>,
}

impl SemanticProviderAdapter {
    pub fn new(
        runtime: Arc<dyn LspRuntimeSpawner>,
        authority: Arc<dyn LspSemanticRequestAuthority>,
    ) -> Self {
        Self {
            runtime,
            authority,
            operations: BoundedOperationTable::new(MAX_SEMANTIC_OPERATIONS),
        }
    }

    pub fn shared(
        runtime: Arc<dyn LspRuntimeSpawner>,
        authority: Arc<dyn LspSemanticRequestAuthority>,
    ) -> Arc<Self> {
        Arc::new(Self::new(runtime, authority))
    }

    fn key(root: &AdmittedRoot, request_id: &LspRequestId) -> SemanticRequestKey {
        SemanticRequestKey {
            root_uri: root.uri().to_owned(),
            request_id: request_id.clone(),
        }
    }

    pub fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        let key = Self::key(root, request_id);
        let authority_cancelled = self.authority.cancel_request(root, request_id);
        let broker_cancelled = self.operations.cancel(&key);
        authority_cancelled || broker_cancelled
    }

    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        let wire_request = match lsp_semantic_request(request) {
            Ok(request) => request,
            Err(coverage) => {
                return SemanticProviderOutcome::Partial {
                    value: empty_semantic_response(request),
                    coverage,
                    detail: None,
                };
            }
        };
        let method = wire_request.method();
        let key = Self::key(root, request_id);
        match self
            .operations
            .poll_matching(&key, |pending_method| *pending_method == method)
        {
            OperationPoll::Ready {
                metadata: _,
                result,
            } => return project_semantic_outcome(root, request, result),
            OperationPoll::Pending(_) => return SemanticProviderOutcome::Pending,
            OperationPoll::Mismatch(_) => {
                return SemanticProviderOutcome::Partial {
                    value: empty_semantic_response(request),
                    coverage: "semantic-request-correlation-mismatch".to_owned(),
                    detail: None,
                };
            }
            OperationPoll::Dropped(_) => {
                return SemanticProviderOutcome::Partial {
                    value: empty_semantic_response(request),
                    coverage: "semantic-operation-dropped".to_owned(),
                    detail: None,
                };
            }
            OperationPoll::Busy => {
                return SemanticProviderOutcome::Partial {
                    value: empty_semantic_response(request),
                    coverage: "semantic-runtime-busy".to_owned(),
                    detail: None,
                };
            }
            OperationPoll::Missing => {}
        }

        let authority = Arc::clone(&self.authority);
        let root = root.clone();
        let request_id = request_id.clone();
        match self
            .operations
            .admit(key, method, self.runtime.as_ref(), move || {
                authority.start(root, request_id, wire_request)
            }) {
            OperationAdmission::Started(_) => SemanticProviderOutcome::Pending,
            OperationAdmission::Existing(pending_method) if pending_method == method => {
                SemanticProviderOutcome::Pending
            }
            OperationAdmission::Existing(_) => SemanticProviderOutcome::Partial {
                value: empty_semantic_response(request),
                coverage: "semantic-request-correlation-mismatch".to_owned(),
                detail: None,
            },
            OperationAdmission::Busy => SemanticProviderOutcome::Partial {
                value: empty_semantic_response(request),
                coverage: "semantic-runtime-busy".to_owned(),
                detail: None,
            },
            OperationAdmission::Saturated => SemanticProviderOutcome::Partial {
                value: empty_semantic_response(request),
                coverage: "semantic-operation-capacity".to_owned(),
                detail: None,
            },
        }
    }
}

impl SemanticProviderPort for SemanticProviderAdapter {
    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        SemanticProviderAdapter::request(self, root, request_id, request)
    }
}

/// Cancellation authority for graph/analyzer operations shared by a session.
pub trait LspAnalyzerCancellationAuthority: Send + Sync {
    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool;
}

impl<T> LspAnalyzerCancellationAuthority for Arc<T>
where
    T: LspAnalyzerCancellationAuthority + ?Sized,
{
    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        (**self).cancel_request(root, request_id)
    }
}

pub struct AnalyzerCancellationAdapter {
    authority: Arc<dyn LspAnalyzerCancellationAuthority>,
}

impl AnalyzerCancellationAdapter {
    pub fn new(authority: Arc<dyn LspAnalyzerCancellationAuthority>) -> Self {
        Self { authority }
    }
}

impl AnalyzerCancellationPort for AnalyzerCancellationAdapter {
    fn cancel_upstream(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.authority.cancel_request(root, request_id)
    }
}

pub fn lsp_semantic_request(request: &SemanticRequest) -> Result<LspSemanticRequest, String> {
    let position_params = |document_uri: &str, position: LspPosition| {
        json!({
            "textDocument": { "uri": document_uri },
            "position": position_value(position),
        })
    };
    let (method, params) = match request {
        SemanticRequest::Declaration {
            document_uri,
            position,
        } => (
            "textDocument/declaration",
            position_params(document_uri, *position),
        ),
        SemanticRequest::Definition {
            document_uri,
            position,
        } => (
            "textDocument/definition",
            position_params(document_uri, *position),
        ),
        SemanticRequest::TypeDefinition {
            document_uri,
            position,
        } => (
            "textDocument/typeDefinition",
            position_params(document_uri, *position),
        ),
        SemanticRequest::Implementation {
            document_uri,
            position,
        } => (
            "textDocument/implementation",
            position_params(document_uri, *position),
        ),
        SemanticRequest::References {
            document_uri,
            position,
        } => {
            let mut params = position_params(document_uri, *position);
            params["context"] = json!({ "includeDeclaration": true });
            ("textDocument/references", params)
        }
        SemanticRequest::Hover {
            document_uri,
            position,
        } => (
            "textDocument/hover",
            position_params(document_uri, *position),
        ),
        SemanticRequest::DocumentSymbols { document_uri } => (
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": document_uri } }),
        ),
        SemanticRequest::WorkspaceSymbols { query } => {
            ("workspace/symbol", json!({ "query": query }))
        }
        SemanticRequest::PrepareCallHierarchy {
            document_uri,
            position,
        } => (
            "textDocument/prepareCallHierarchy",
            position_params(document_uri, *position),
        ),
        SemanticRequest::IncomingCalls { item } => (
            "callHierarchy/incomingCalls",
            json!({ "item": call_item_value(item) }),
        ),
        SemanticRequest::OutgoingCalls { item } => (
            "callHierarchy/outgoingCalls",
            json!({ "item": call_item_value(item) }),
        ),
        SemanticRequest::SignatureHelp {
            document_uri,
            position,
        } => (
            "textDocument/signatureHelp",
            position_params(document_uri, *position),
        ),
        SemanticRequest::PrepareTypeHierarchy {
            document_uri,
            position,
        } => (
            "textDocument/prepareTypeHierarchy",
            position_params(document_uri, *position),
        ),
        SemanticRequest::TypeHierarchySupertypes { item } => (
            "typeHierarchy/supertypes",
            json!({ "item": type_item_value(item) }),
        ),
        SemanticRequest::TypeHierarchySubtypes { item } => (
            "typeHierarchy/subtypes",
            json!({ "item": type_item_value(item) }),
        ),
        SemanticRequest::RenameCandidate {
            document_uri,
            position,
        } => (
            "textDocument/prepareRename",
            position_params(document_uri, *position),
        ),
    };
    Ok(LspSemanticRequest { method, params })
}

pub fn project_semantic_outcome(
    root: &AdmittedRoot,
    request: &SemanticRequest,
    outcome: LspSemanticOperationOutcome,
) -> SemanticProviderOutcome<SemanticResponse> {
    match outcome {
        LspSemanticOperationOutcome::Complete(value) => {
            match parse_semantic_response(request, value) {
                Ok((value, coverage)) => {
                    let (value, outside_root) = confine_semantic_response(root, value);
                    match coverage.or_else(|| {
                        outside_root.then(|| "semantic-result-outside-admitted-root".to_owned())
                    }) {
                        Some(coverage) => SemanticProviderOutcome::Partial {
                            value,
                            coverage,
                            detail: None,
                        },
                        None => SemanticProviderOutcome::Complete(value),
                    }
                }
                Err(coverage) => SemanticProviderOutcome::Partial {
                    value: empty_semantic_response(request),
                    coverage,
                    detail: None,
                },
            }
        }
        LspSemanticOperationOutcome::Partial {
            value,
            coverage,
            detail,
        } => {
            let value = parse_semantic_response(request, value)
                .map_or_else(|_| empty_semantic_response(request), |(value, _)| value);
            let (value, _) = confine_semantic_response(root, value);
            SemanticProviderOutcome::Partial {
                value,
                coverage,
                detail: detail.map(str::to_owned),
            }
        }
        LspSemanticOperationOutcome::RenameCandidate(value) => {
            SemanticProviderOutcome::Complete(SemanticResponse::RenameCandidate(value))
        }
        LspSemanticOperationOutcome::Unavailable => SemanticProviderOutcome::Unavailable,
    }
}

fn confine_semantic_response(
    root: &AdmittedRoot,
    response: SemanticResponse,
) -> (SemanticResponse, bool) {
    match response {
        SemanticResponse::Locations(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.uri));
            let omitted = before != values.len();
            (SemanticResponse::Locations(values), omitted)
        }
        SemanticResponse::WorkspaceSymbols(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.location.uri));
            let omitted = before != values.len();
            (SemanticResponse::WorkspaceSymbols(values), omitted)
        }
        SemanticResponse::CallHierarchyItems(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.uri));
            let omitted = before != values.len();
            (SemanticResponse::CallHierarchyItems(values), omitted)
        }
        SemanticResponse::IncomingCalls(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.from.uri));
            let omitted = before != values.len();
            (SemanticResponse::IncomingCalls(values), omitted)
        }
        SemanticResponse::OutgoingCalls(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.to.uri));
            let omitted = before != values.len();
            (SemanticResponse::OutgoingCalls(values), omitted)
        }
        SemanticResponse::TypeHierarchyItems(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.uri));
            let omitted = before != values.len();
            (SemanticResponse::TypeHierarchyItems(values), omitted)
        }
        SemanticResponse::RenameCandidate(RenameCandidateResult::Available(candidate))
            if !root.contains_document(&candidate.document_uri) =>
        {
            (
                SemanticResponse::RenameCandidate(RenameCandidateResult::Unavailable {
                    reason: RenameCandidateUnavailableReason::AmbiguousEvidence,
                }),
                true,
            )
        }
        response => (response, false),
    }
}

fn parse_semantic_response(
    request: &SemanticRequest,
    value: Value,
) -> Result<(SemanticResponse, Option<String>), String> {
    match request {
        SemanticRequest::Declaration { .. }
        | SemanticRequest::Definition { .. }
        | SemanticRequest::TypeDefinition { .. }
        | SemanticRequest::Implementation { .. }
        | SemanticRequest::References { .. } => {
            Ok((SemanticResponse::Locations(parse_locations(value)?), None))
        }
        SemanticRequest::Hover { .. } => Ok((SemanticResponse::Hover(parse_hover(value)?), None)),
        SemanticRequest::DocumentSymbols { .. } => {
            let (symbols, partial) = parse_document_symbols(value)?;
            Ok((
                SemanticResponse::DocumentSymbols(symbols),
                partial.then(|| "document-symbols-unprojectable-items".to_owned()),
            ))
        }
        SemanticRequest::WorkspaceSymbols { .. } => {
            let (symbols, partial) = parse_workspace_symbols(value)?;
            Ok((
                SemanticResponse::WorkspaceSymbols(symbols),
                partial.then(|| "workspace-symbols-unresolved-locations".to_owned()),
            ))
        }
        SemanticRequest::PrepareCallHierarchy { .. } => Ok((
            SemanticResponse::CallHierarchyItems(parse_call_items(value)?),
            None,
        )),
        SemanticRequest::IncomingCalls { .. } => Ok((
            SemanticResponse::IncomingCalls(parse_incoming_calls(value)?),
            None,
        )),
        SemanticRequest::OutgoingCalls { .. } => Ok((
            SemanticResponse::OutgoingCalls(parse_outgoing_calls(value)?),
            None,
        )),
        SemanticRequest::SignatureHelp { .. } => Ok((
            SemanticResponse::SignatureHelp(parse_signature_help(value)?),
            None,
        )),
        SemanticRequest::PrepareTypeHierarchy { .. }
        | SemanticRequest::TypeHierarchySupertypes { .. }
        | SemanticRequest::TypeHierarchySubtypes { .. } => Ok((
            SemanticResponse::TypeHierarchyItems(parse_type_items(value)?),
            None,
        )),
        SemanticRequest::RenameCandidate { .. } => Err("rename-candidate-unmerged".to_owned()),
    }
}

fn empty_semantic_response(request: &SemanticRequest) -> SemanticResponse {
    match request {
        SemanticRequest::Declaration { .. }
        | SemanticRequest::Definition { .. }
        | SemanticRequest::TypeDefinition { .. }
        | SemanticRequest::Implementation { .. }
        | SemanticRequest::References { .. } => SemanticResponse::Locations(Vec::new()),
        SemanticRequest::Hover { .. } => SemanticResponse::Hover(None),
        SemanticRequest::DocumentSymbols { .. } => SemanticResponse::DocumentSymbols(Vec::new()),
        SemanticRequest::WorkspaceSymbols { .. } => SemanticResponse::WorkspaceSymbols(Vec::new()),
        SemanticRequest::PrepareCallHierarchy { .. } => {
            SemanticResponse::CallHierarchyItems(Vec::new())
        }
        SemanticRequest::IncomingCalls { .. } => SemanticResponse::IncomingCalls(Vec::new()),
        SemanticRequest::OutgoingCalls { .. } => SemanticResponse::OutgoingCalls(Vec::new()),
        SemanticRequest::SignatureHelp { .. } => SemanticResponse::SignatureHelp(None),
        SemanticRequest::PrepareTypeHierarchy { .. }
        | SemanticRequest::TypeHierarchySupertypes { .. }
        | SemanticRequest::TypeHierarchySubtypes { .. } => {
            SemanticResponse::TypeHierarchyItems(Vec::new())
        }
        SemanticRequest::RenameCandidate { .. } => {
            SemanticResponse::RenameCandidate(RenameCandidateResult::Unavailable {
                reason: RenameCandidateUnavailableReason::EvidenceAbsent,
            })
        }
    }
}

fn parse_locations(value: Value) -> Result<Vec<LspLocation>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let values = match value {
        Value::Array(values) => values,
        value @ Value::Object(_) => vec![value],
        _ => return Err("semantic-location-response-invalid".to_owned()),
    };
    values
        .into_iter()
        .map(|value| {
            let uri = value
                .get("uri")
                .or_else(|| value.get("targetUri"))
                .and_then(Value::as_str)
                .ok_or_else(|| "semantic-location-uri-invalid".to_owned())?;
            let range = value
                .get("range")
                .or_else(|| value.get("targetRange"))
                .ok_or_else(|| "semantic-location-range-invalid".to_owned())?;
            Ok(LspLocation {
                uri: uri.to_owned(),
                range: parse_range(range)?,
            })
        })
        .collect()
}

fn parse_hover(value: Value) -> Result<Option<Hover>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let contents = value
        .get("contents")
        .map(hover_contents)
        .ok_or_else(|| "semantic-hover-contents-invalid".to_owned())?;
    let range = value.get("range").map(parse_range).transpose()?;
    Ok(Some(Hover { contents, range }))
}

fn hover_contents(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(hover_contents)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(object) => object
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

fn parse_document_symbols(value: Value) -> Result<(Vec<DocumentSymbol>, bool), String> {
    if value.is_null() {
        return Ok((Vec::new(), false));
    }
    let values = value
        .as_array()
        .ok_or_else(|| "semantic-document-symbols-invalid".to_owned())?;
    let mut partial = false;
    let symbols = values
        .iter()
        .filter_map(|value| {
            if let Ok(symbol) = parse_document_symbol(value) {
                Some(symbol)
            } else {
                partial = true;
                None
            }
        })
        .collect();
    Ok((symbols, partial))
}

fn parse_document_symbol(value: &Value) -> Result<DocumentSymbol, String> {
    let range_value = value
        .get("range")
        .or_else(|| {
            value
                .get("location")
                .and_then(|location| location.get("range"))
        })
        .ok_or_else(|| "semantic-document-symbol-range-invalid".to_owned())?;
    let range = parse_range(range_value)?;
    let children = value
        .get("children")
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .map(parse_document_symbol)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(DocumentSymbol {
        name: required_string(value, "name")?,
        kind: required_u32(value, "kind")?,
        range,
        selection_range: value
            .get("selectionRange")
            .map(parse_range)
            .transpose()?
            .unwrap_or(range),
        children,
    })
}

fn parse_workspace_symbols(value: Value) -> Result<(Vec<WorkspaceSymbol>, bool), String> {
    if value.is_null() {
        return Ok((Vec::new(), false));
    }
    let values = value
        .as_array()
        .ok_or_else(|| "semantic-workspace-symbols-invalid".to_owned())?;
    let mut partial = false;
    let symbols = values
        .iter()
        .filter_map(|value| {
            if let Ok(symbol) = parse_workspace_symbol(value) {
                Some(symbol)
            } else {
                partial = true;
                None
            }
        })
        .collect();
    Ok((symbols, partial))
}

fn parse_workspace_symbol(value: &Value) -> Result<WorkspaceSymbol, String> {
    let location = value
        .get("location")
        .ok_or_else(|| "semantic-workspace-symbol-location-invalid".to_owned())?;
    Ok(WorkspaceSymbol {
        name: required_string(value, "name")?,
        kind: required_u32(value, "kind")?,
        location: LspLocation {
            uri: required_string(location, "uri")?,
            range: parse_range(
                location
                    .get("range")
                    .ok_or_else(|| "semantic-workspace-symbol-range-unresolved".to_owned())?,
            )?,
        },
    })
}

fn parse_call_items(value: Value) -> Result<Vec<CallHierarchyItem>, String> {
    nullable_array(value, parse_call_item)
}

fn parse_call_item(value: &Value) -> Result<CallHierarchyItem, String> {
    Ok(CallHierarchyItem {
        name: required_string(value, "name")?,
        kind: required_u32(value, "kind")?,
        uri: required_string(value, "uri")?,
        range: parse_required_range(value, "range")?,
        selection_range: parse_required_range(value, "selectionRange")?,
    })
}

fn parse_incoming_calls(value: Value) -> Result<Vec<IncomingCall>, String> {
    nullable_array(value, |value| {
        Ok(IncomingCall {
            from: parse_call_item(
                value
                    .get("from")
                    .ok_or_else(|| "semantic-incoming-call-from-invalid".to_owned())?,
            )?,
            from_ranges: parse_ranges(value, "fromRanges")?,
        })
    })
}

fn parse_outgoing_calls(value: Value) -> Result<Vec<OutgoingCall>, String> {
    nullable_array(value, |value| {
        Ok(OutgoingCall {
            to: parse_call_item(
                value
                    .get("to")
                    .ok_or_else(|| "semantic-outgoing-call-to-invalid".to_owned())?,
            )?,
            from_ranges: parse_ranges(value, "fromRanges")?,
        })
    })
}

fn parse_signature_help(value: Value) -> Result<Option<SignatureHelp>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let signatures = value
        .get("signatures")
        .and_then(Value::as_array)
        .ok_or_else(|| "semantic-signature-help-invalid".to_owned())?
        .iter()
        .map(|signature| required_string(signature, "label"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(SignatureHelp {
        signatures,
        active_signature: optional_u32(&value, "activeSignature")?,
        active_parameter: optional_u32(&value, "activeParameter")?,
    }))
}

fn parse_type_items(value: Value) -> Result<Vec<TypeHierarchyItem>, String> {
    nullable_array(value, |value| {
        Ok(TypeHierarchyItem {
            name: required_string(value, "name")?,
            kind: required_u32(value, "kind")?,
            uri: required_string(value, "uri")?,
            range: parse_required_range(value, "range")?,
            selection_range: parse_required_range(value, "selectionRange")?,
        })
    })
}

fn nullable_array<T>(
    value: Value,
    parse: impl Fn(&Value) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    value
        .as_array()
        .ok_or_else(|| "semantic-array-response-invalid".to_owned())?
        .iter()
        .map(parse)
        .collect()
}

fn parse_ranges(value: &Value, field: &str) -> Result<Vec<LspRange>, String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| "semantic-ranges-invalid".to_owned())?
        .iter()
        .map(parse_range)
        .collect()
}

fn parse_required_range(value: &Value, field: &str) -> Result<LspRange, String> {
    parse_range(
        value
            .get(field)
            .ok_or_else(|| "semantic-range-invalid".to_owned())?,
    )
}

fn parse_range(value: &Value) -> Result<LspRange, String> {
    Ok(LspRange {
        start: parse_position(
            value
                .get("start")
                .ok_or_else(|| "semantic-range-start-invalid".to_owned())?,
        )?,
        end: parse_position(
            value
                .get("end")
                .ok_or_else(|| "semantic-range-end-invalid".to_owned())?,
        )?,
    })
}

fn parse_position(value: &Value) -> Result<LspPosition, String> {
    Ok(LspPosition {
        line: required_u32(value, "line")?,
        character: required_u32(value, "character")?,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("semantic-{field}-invalid"))
}

fn required_u32(value: &Value, field: &str) -> Result<u32, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("semantic-{field}-invalid"))
}

fn optional_u32(value: &Value, field: &str) -> Result<Option<u32>, String> {
    value
        .get(field)
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| format!("semantic-{field}-invalid"))
        })
        .transpose()
}

fn position_value(position: LspPosition) -> Value {
    json!({ "line": position.line, "character": position.character })
}

fn range_value(range: LspRange) -> Value {
    json!({
        "start": position_value(range.start),
        "end": position_value(range.end),
    })
}

fn call_item_value(item: &CallHierarchyItem) -> Value {
    json!({
        "name": item.name,
        "kind": item.kind,
        "uri": item.uri,
        "range": range_value(item.range),
        "selectionRange": range_value(item.selection_range),
    })
}

fn type_item_value(item: &TypeHierarchyItem) -> Value {
    json!({
        "name": item.name,
        "kind": item.kind,
        "uri": item.uri,
        "range": range_value(item.range),
        "selectionRange": range_value(item.selection_range),
    })
}

/// Typed daemon adapter for admitted upstream/graph semantic operations.
/// Defaults are unavailable rather than fabricated empty answers.
pub trait SemanticProviderPort {
    fn request(
        &self,
        root: &AdmittedRoot,
        _request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        match request {
            SemanticRequest::Declaration {
                document_uri,
                position,
            } => self
                .declaration(root, document_uri, *position)
                .map(SemanticResponse::Locations),
            SemanticRequest::Definition {
                document_uri,
                position,
            } => self
                .definition(root, document_uri, *position)
                .map(SemanticResponse::Locations),
            SemanticRequest::TypeDefinition {
                document_uri,
                position,
            } => self
                .type_definition(root, document_uri, *position)
                .map(SemanticResponse::Locations),
            SemanticRequest::Implementation {
                document_uri,
                position,
            } => self
                .implementation(root, document_uri, *position)
                .map(SemanticResponse::Locations),
            SemanticRequest::References {
                document_uri,
                position,
            } => self
                .references(root, document_uri, *position)
                .map(SemanticResponse::Locations),
            SemanticRequest::Hover {
                document_uri,
                position,
            } => self
                .hover(root, document_uri, *position)
                .map(SemanticResponse::Hover),
            SemanticRequest::DocumentSymbols { document_uri } => self
                .document_symbols(root, document_uri)
                .map(SemanticResponse::DocumentSymbols),
            SemanticRequest::WorkspaceSymbols { query } => self
                .workspace_symbols(root, query)
                .map(SemanticResponse::WorkspaceSymbols),
            SemanticRequest::PrepareCallHierarchy {
                document_uri,
                position,
            } => self
                .prepare_call_hierarchy(root, document_uri, *position)
                .map(SemanticResponse::CallHierarchyItems),
            SemanticRequest::IncomingCalls { item } => self
                .incoming_calls(root, item)
                .map(SemanticResponse::IncomingCalls),
            SemanticRequest::OutgoingCalls { item } => self
                .outgoing_calls(root, item)
                .map(SemanticResponse::OutgoingCalls),
            SemanticRequest::SignatureHelp {
                document_uri,
                position,
            } => self
                .signature_help(root, document_uri, *position)
                .map(SemanticResponse::SignatureHelp),
            SemanticRequest::PrepareTypeHierarchy {
                document_uri,
                position,
            } => self
                .prepare_type_hierarchy(root, document_uri, *position)
                .map(SemanticResponse::TypeHierarchyItems),
            SemanticRequest::TypeHierarchySupertypes { item } => self
                .type_hierarchy_supertypes(root, item)
                .map(SemanticResponse::TypeHierarchyItems),
            SemanticRequest::TypeHierarchySubtypes { item } => self
                .type_hierarchy_subtypes(root, item)
                .map(SemanticResponse::TypeHierarchyItems),
            SemanticRequest::RenameCandidate {
                document_uri,
                position,
            } => self
                .rename_candidate(root, document_uri, *position)
                .map(SemanticResponse::RenameCandidate),
        }
    }

    fn declaration(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        SemanticProviderOutcome::Unavailable
    }

    fn definition(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        SemanticProviderOutcome::Unavailable
    }

    fn type_definition(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        SemanticProviderOutcome::Unavailable
    }

    fn implementation(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        SemanticProviderOutcome::Unavailable
    }

    fn references(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        SemanticProviderOutcome::Unavailable
    }

    fn hover(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Option<Hover>> {
        SemanticProviderOutcome::Unavailable
    }

    fn document_symbols(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
    ) -> SemanticProviderOutcome<Vec<DocumentSymbol>> {
        SemanticProviderOutcome::Unavailable
    }

    fn workspace_symbols(
        &self,
        _root: &AdmittedRoot,
        _query: &str,
    ) -> SemanticProviderOutcome<Vec<WorkspaceSymbol>> {
        SemanticProviderOutcome::Unavailable
    }

    fn prepare_call_hierarchy(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<CallHierarchyItem>> {
        SemanticProviderOutcome::Unavailable
    }

    fn incoming_calls(
        &self,
        _root: &AdmittedRoot,
        _item: &CallHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<IncomingCall>> {
        SemanticProviderOutcome::Unavailable
    }

    fn outgoing_calls(
        &self,
        _root: &AdmittedRoot,
        _item: &CallHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<OutgoingCall>> {
        SemanticProviderOutcome::Unavailable
    }

    fn signature_help(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Option<SignatureHelp>> {
        SemanticProviderOutcome::Unavailable
    }

    fn prepare_type_hierarchy(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<TypeHierarchyItem>> {
        SemanticProviderOutcome::Unavailable
    }

    fn type_hierarchy_supertypes(
        &self,
        _root: &AdmittedRoot,
        _item: &TypeHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<TypeHierarchyItem>> {
        SemanticProviderOutcome::Unavailable
    }

    fn type_hierarchy_subtypes(
        &self,
        _root: &AdmittedRoot,
        _item: &TypeHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<TypeHierarchyItem>> {
        SemanticProviderOutcome::Unavailable
    }

    fn rename_candidate(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<RenameCandidateResult> {
        SemanticProviderOutcome::Unavailable
    }
}

impl<T> SemanticProviderPort for Arc<T>
where
    T: SemanticProviderPort + ?Sized,
{
    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        (**self).request(root, request_id, request)
    }
    fn declaration(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        (**self).declaration(root, document_uri, position)
    }

    fn definition(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        (**self).definition(root, document_uri, position)
    }

    fn type_definition(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        (**self).type_definition(root, document_uri, position)
    }

    fn implementation(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        (**self).implementation(root, document_uri, position)
    }

    fn references(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        (**self).references(root, document_uri, position)
    }

    fn hover(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Option<Hover>> {
        (**self).hover(root, document_uri, position)
    }

    fn document_symbols(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
    ) -> SemanticProviderOutcome<Vec<DocumentSymbol>> {
        (**self).document_symbols(root, document_uri)
    }

    fn workspace_symbols(
        &self,
        root: &AdmittedRoot,
        query: &str,
    ) -> SemanticProviderOutcome<Vec<WorkspaceSymbol>> {
        (**self).workspace_symbols(root, query)
    }

    fn prepare_call_hierarchy(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<CallHierarchyItem>> {
        (**self).prepare_call_hierarchy(root, document_uri, position)
    }

    fn incoming_calls(
        &self,
        root: &AdmittedRoot,
        item: &CallHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<IncomingCall>> {
        (**self).incoming_calls(root, item)
    }

    fn outgoing_calls(
        &self,
        root: &AdmittedRoot,
        item: &CallHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<OutgoingCall>> {
        (**self).outgoing_calls(root, item)
    }

    fn signature_help(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Option<SignatureHelp>> {
        (**self).signature_help(root, document_uri, position)
    }

    fn prepare_type_hierarchy(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<TypeHierarchyItem>> {
        (**self).prepare_type_hierarchy(root, document_uri, position)
    }

    fn type_hierarchy_supertypes(
        &self,
        root: &AdmittedRoot,
        item: &TypeHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<TypeHierarchyItem>> {
        (**self).type_hierarchy_supertypes(root, item)
    }

    fn type_hierarchy_subtypes(
        &self,
        root: &AdmittedRoot,
        item: &TypeHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<TypeHierarchyItem>> {
        (**self).type_hierarchy_subtypes(root, item)
    }

    fn rename_candidate(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<RenameCandidateResult> {
        (**self).rename_candidate(root, document_uri, position)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableSemanticProvider;

impl SemanticProviderPort for UnavailableSemanticProvider {}

pub type DaemonLspProviderBundle = DaemonLspProviderFactory<
    Arc<dyn FeedbackCyclePort + Send + Sync>,
    Arc<dyn SemanticProviderPort + Send + Sync>,
    Arc<dyn DiagnosticSnapshotPort + Send + Sync>,
    Arc<dyn AnalyzerCancellationPort + Send + Sync>,
    Arc<dyn ContextProjectionPort + Send + Sync>,
>;

pub type DaemonLspRuntimeSession = DaemonLspProtocolSession<
    Arc<dyn FeedbackCyclePort + Send + Sync>,
    Arc<dyn SemanticProviderPort + Send + Sync>,
    Arc<dyn DiagnosticSnapshotPort + Send + Sync>,
>;

/// Store-free provider composition for one isolated LSP protocol session.
pub struct DaemonLspProviderFactory<F, S, D, C, X> {
    feedback: F,
    semantics: S,
    diagnostics: D,
    cancellation: C,
    context: X,
    gateway_capabilities: GatewayCapabilities,
    upstream_capabilities: UpstreamCapabilities,
}

impl
    DaemonLspProviderFactory<
        Arc<dyn FeedbackCyclePort + Send + Sync>,
        Arc<dyn SemanticProviderPort + Send + Sync>,
        Arc<dyn DiagnosticSnapshotPort + Send + Sync>,
        Arc<dyn AnalyzerCancellationPort + Send + Sync>,
        Arc<dyn ContextProjectionPort + Send + Sync>,
    >
{
    pub fn from_shared(
        feedback: Arc<dyn FeedbackCyclePort + Send + Sync>,
        semantics: Arc<dyn SemanticProviderPort + Send + Sync>,
        diagnostics: Arc<dyn DiagnosticSnapshotPort + Send + Sync>,
        cancellation: Arc<dyn AnalyzerCancellationPort + Send + Sync>,
        context: Arc<dyn ContextProjectionPort + Send + Sync>,
        gateway_capabilities: GatewayCapabilities,
        upstream_capabilities: UpstreamCapabilities,
    ) -> Self {
        Self::new(
            feedback,
            semantics,
            diagnostics,
            cancellation,
            context,
            gateway_capabilities,
            upstream_capabilities,
        )
    }
}

impl<F, S, D, C, X> DaemonLspProviderFactory<F, S, D, C, X>
where
    F: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
    C: AnalyzerCancellationPort + Send + Sync + 'static,
    X: ContextProjectionPort + Send + Sync + 'static,
{
    pub fn new(
        feedback: F,
        semantics: S,
        diagnostics: D,
        cancellation: C,
        context: X,
        mut gateway_capabilities: GatewayCapabilities,
        upstream_capabilities: UpstreamCapabilities,
    ) -> Self {
        if upstream_capabilities
            .semantic
            .contains(&SemanticCapability::RenameCandidate)
        {
            gateway_capabilities
                .semantic
                .insert(SemanticCapability::RenameCandidate);
        }
        gateway_capabilities.context_projections = context
            .registrations()
            .into_iter()
            .filter(|registration| registration.kind.is_supported() && registration.revision > 0)
            .take(MAX_CONTEXT_PROJECTION_KINDS)
            .map(|registration| (registration.kind, registration.revision))
            .collect::<BTreeMap<_, _>>();
        Self {
            feedback,
            semantics,
            diagnostics,
            cancellation,
            context,
            gateway_capabilities,
            upstream_capabilities,
        }
    }

    pub fn into_session(self, root: AdmittedRoot) -> DaemonLspProtocolSession<F, S, D> {
        self.into_workspace_session(AuthorizedLspWorkspace::single(root))
    }

    pub fn into_workspace_session(
        self,
        workspace: AuthorizedLspWorkspace,
    ) -> DaemonLspProtocolSession<F, S, D> {
        let initial_capabilities = negotiate_capabilities(
            &ClientCapabilities::default(),
            &self.gateway_capabilities,
            &self.upstream_capabilities,
        );
        DaemonLspProtocolSession::from_workspace_ports(
            workspace,
            initial_capabilities,
            self.gateway_capabilities,
            self.upstream_capabilities,
            self.feedback,
            self.semantics,
            self.diagnostics,
        )
        .with_cancellation_port(self.cancellation)
        .with_context_projection_port(self.context)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayDocumentDiagnostics {
    pub report: DocumentDiagnosticReport,
    pub omitted_count: usize,
}

/// A daemon-owned LSP workspace session.
///
/// Both application ports are explicit constructor inputs. A retained daemon
/// must not accidentally mount a whole-session unavailable semantic runtime;
/// individual provider methods may still return
/// [`SemanticProviderOutcome::Unavailable`] truthfully.
pub struct DaemonLspGateway<P, S> {
    workspace: AuthorizedLspWorkspace,
    capabilities: EffectiveCapabilities,
    feedback_cycle: P,
    semantic_provider: S,
}

impl<P, S> DaemonLspGateway<P, S>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
{
    pub fn new(
        root: AdmittedRoot,
        capabilities: EffectiveCapabilities,
        feedback_cycle: P,
        semantic_provider: S,
    ) -> Self {
        Self::for_workspace(
            AuthorizedLspWorkspace::single(root),
            capabilities,
            feedback_cycle,
            semantic_provider,
        )
    }

    pub fn for_workspace(
        workspace: AuthorizedLspWorkspace,
        capabilities: EffectiveCapabilities,
        feedback_cycle: P,
        semantic_provider: S,
    ) -> Self {
        Self {
            workspace,
            capabilities,
            feedback_cycle,
            semantic_provider,
        }
    }

    /// Compatibility alias for callers that already made the semantic port
    /// explicit. New daemon integration should prefer [`Self::new`].
    pub fn with_semantic_provider(
        root: AdmittedRoot,
        capabilities: EffectiveCapabilities,
        feedback_cycle: P,
        semantic_provider: S,
    ) -> Self {
        Self::new(root, capabilities, feedback_cycle, semantic_provider)
    }

    pub fn root(&self) -> &AdmittedRoot {
        self.workspace.primary()
    }

    pub fn workspace(&self) -> &AuthorizedLspWorkspace {
        &self.workspace
    }

    pub fn root_for_document(
        &self,
        document_uri: &str,
    ) -> Result<&AdmittedRoot, MethodUnavailableReason> {
        self.workspace
            .resolve_document(document_uri)
            .map_err(workspace_route_reason)
    }

    pub fn capabilities(&self) -> &EffectiveCapabilities {
        &self.capabilities
    }

    /// Binds the capability intersection negotiated during this authenticated
    /// session's `initialize` request. The protocol actor invokes this before
    /// transitioning the session to `Ready`; clients cannot dynamically widen
    /// the gateway's capabilities afterward.
    pub fn bind_initialized_capabilities(&mut self, capabilities: EffectiveCapabilities) {
        self.capabilities = capabilities;
    }

    pub fn initialization_availability(&self) -> CapabilityAvailability {
        self.capabilities.initialization_availability()
    }

    /// Triggered by `textDocument/didSave`.
    pub fn document_saved(&self, document_uri: impl Into<String>) -> FeedbackCycleResponse {
        let document_uri = document_uri.into();
        if self.root_for_document(&document_uri).is_err() {
            return FeedbackCycleResponse::Rejected {
                reason: "document is outside the admitted root".into(),
            };
        }
        self.trigger_feedback_cycle(document_uri, DiagnosticTrigger::DocumentSave)
    }

    /// Admits `textDocument/diagnostic` through the same feedback-cycle port
    /// as save. The protocol actor then reads only the canonical diagnostic
    /// projection; queued feedback work never creates actor-local findings.
    pub fn request_document_diagnostics(&self, document_uri: &str) -> GatewayResponse<()> {
        if !self.capabilities.supports_document_diagnostics {
            return GatewayResponse::unavailable(
                GatewayMethod::TextDocumentDiagnostic,
                MethodUnavailableReason::CapabilityNotNegotiated,
            );
        }
        if let Err(reason) = self.root_for_document(document_uri) {
            return GatewayResponse::unavailable(GatewayMethod::TextDocumentDiagnostic, reason);
        }
        match self.trigger_feedback_cycle(
            document_uri.to_owned(),
            DiagnosticTrigger::ExplicitDocumentDiagnostics,
        ) {
            FeedbackCycleResponse::Accepted | FeedbackCycleResponse::Deferred { .. } => {
                GatewayResponse::Value(())
            }
            FeedbackCycleResponse::Rejected { .. } => {
                GatewayResponse::RequestFailed(LspRequestFailure::ServerCancelled {
                    retrigger_request: true,
                })
            }
        }
    }

    /// Projects an already-read canonical snapshot into a generation-bound,
    /// bounded document report. This function never schedules feedback work,
    /// which keeps the request-before-read ordering explicit at the actor.
    pub fn project_document_diagnostics(
        &self,
        document_uri: &str,
        result_id: impl Into<String>,
        upstream: Vec<GatewayDiagnostic>,
        tracedecay: Vec<GatewayDiagnostic>,
    ) -> GatewayResponse<GatewayDocumentDiagnostics> {
        if !self.capabilities.supports_document_diagnostics {
            return GatewayResponse::unavailable(
                GatewayMethod::TextDocumentDiagnostic,
                MethodUnavailableReason::CapabilityNotNegotiated,
            );
        }
        if let Err(reason) = self.root_for_document(document_uri) {
            return GatewayResponse::unavailable(GatewayMethod::TextDocumentDiagnostic, reason);
        }
        let result_id = result_id.into();
        if result_id.is_empty() {
            return GatewayResponse::RequestFailed(LspRequestFailure::ServerCancelled {
                retrigger_request: true,
            });
        }

        let DiagnosticMerge {
            items,
            omitted_count,
        } = DiagnosticMerge::for_document(document_uri, upstream, tracedecay);
        GatewayResponse::Value(GatewayDocumentDiagnostics {
            report: DocumentDiagnosticReport::full(result_id, items),
            omitted_count,
        })
    }

    /// Convenience composition for non-protocol callers. The daemon protocol
    /// actor uses [`Self::request_document_diagnostics`] followed by a
    /// canonical snapshot read and [`Self::project_document_diagnostics`].
    pub fn document_diagnostics(
        &self,
        document_uri: &str,
        result_id: impl Into<String>,
        upstream: Vec<GatewayDiagnostic>,
        tracedecay: Vec<GatewayDiagnostic>,
    ) -> GatewayResponse<GatewayDocumentDiagnostics> {
        match self.request_document_diagnostics(document_uri) {
            GatewayResponse::Value(()) => {
                self.project_document_diagnostics(document_uri, result_id, upstream, tracedecay)
            }
            GatewayResponse::Unavailable(unavailable) => GatewayResponse::Unavailable(unavailable),
            GatewayResponse::RequestFailed(failure) => GatewayResponse::RequestFailed(failure),
            GatewayResponse::Partial { .. } => unreachable!("feedback admission is never partial"),
            GatewayResponse::Pending => unreachable!("feedback admission is never pending"),
        }
    }

    pub fn declaration(
        &self,
        document_uri: &str,
        position: LspPosition,
    ) -> GatewayResponse<Vec<LspLocation>> {
        self.route_semantic(
            GatewayMethod::TextDocumentDeclaration,
            SemanticCapability::Declaration,
            Some(document_uri),
            |provider, root| provider.declaration(root, document_uri, position),
        )
    }

    pub fn definition(
        &self,
        document_uri: &str,
        position: LspPosition,
    ) -> GatewayResponse<Vec<LspLocation>> {
        self.route_semantic(
            GatewayMethod::TextDocumentDefinition,
            SemanticCapability::Definition,
            Some(document_uri),
            |provider, root| provider.definition(root, document_uri, position),
        )
    }

    pub fn type_definition(
        &self,
        document_uri: &str,
        position: LspPosition,
    ) -> GatewayResponse<Vec<LspLocation>> {
        self.route_semantic(
            GatewayMethod::TextDocumentTypeDefinition,
            SemanticCapability::TypeDefinition,
            Some(document_uri),
            |provider, root| provider.type_definition(root, document_uri, position),
        )
    }

    pub fn implementation(
        &self,
        document_uri: &str,
        position: LspPosition,
    ) -> GatewayResponse<Vec<LspLocation>> {
        self.route_semantic(
            GatewayMethod::TextDocumentImplementation,
            SemanticCapability::Implementation,
            Some(document_uri),
            |provider, root| provider.implementation(root, document_uri, position),
        )
    }

    pub fn references(
        &self,
        document_uri: &str,
        position: LspPosition,
    ) -> GatewayResponse<Vec<LspLocation>> {
        self.route_semantic(
            GatewayMethod::TextDocumentReferences,
            SemanticCapability::References,
            Some(document_uri),
            |provider, root| provider.references(root, document_uri, position),
        )
    }

    pub fn hover(
        &self,
        document_uri: &str,
        position: LspPosition,
    ) -> GatewayResponse<Option<Hover>> {
        self.route_semantic(
            GatewayMethod::TextDocumentHover,
            SemanticCapability::Hover,
            Some(document_uri),
            |provider, root| provider.hover(root, document_uri, position),
        )
    }

    pub fn document_symbols(&self, document_uri: &str) -> GatewayResponse<Vec<DocumentSymbol>> {
        self.route_semantic(
            GatewayMethod::TextDocumentDocumentSymbol,
            SemanticCapability::DocumentSymbol,
            Some(document_uri),
            |provider, root| provider.document_symbols(root, document_uri),
        )
    }

    pub fn workspace_symbols(&self, query: &str) -> GatewayResponse<Vec<WorkspaceSymbol>> {
        if !self
            .capabilities
            .supports_semantic(SemanticCapability::WorkspaceSymbol)
        {
            return GatewayResponse::unavailable(
                GatewayMethod::WorkspaceSymbol,
                MethodUnavailableReason::CapabilityNotNegotiated,
            );
        }
        let mut symbols = Vec::new();
        let mut completed = 0_usize;
        let mut partial = false;
        let mut pending = false;
        for root in self.workspace.roots() {
            match self.semantic_provider.workspace_symbols(root, query) {
                SemanticProviderOutcome::Complete(mut root_symbols) => {
                    completed += 1;
                    symbols.append(&mut root_symbols);
                }
                SemanticProviderOutcome::Partial {
                    value: mut root_symbols,
                    ..
                } => {
                    partial = true;
                    symbols.append(&mut root_symbols);
                }
                SemanticProviderOutcome::Pending => pending = true,
                SemanticProviderOutcome::Unavailable => {}
            }
        }
        if completed == self.workspace.roots().len() && !partial {
            GatewayResponse::Value(symbols)
        } else if completed > 0 || partial {
            let scope_set = self
                .workspace
                .scope_set_digest()
                .map_or("single-root", ManifestDigest::as_str);
            GatewayResponse::Partial {
                value: symbols,
                coverage: format!(
                    "scope-set={scope_set};completed={completed}/{}",
                    self.workspace.roots().len()
                ),
                detail: Some("one or more admitted roots were incomplete".to_owned()),
            }
        } else if pending {
            GatewayResponse::Pending
        } else {
            GatewayResponse::unavailable(
                GatewayMethod::WorkspaceSymbol,
                MethodUnavailableReason::ProviderUnavailable,
            )
        }
    }

    pub fn prepare_call_hierarchy(
        &self,
        document_uri: &str,
        position: LspPosition,
    ) -> GatewayResponse<Vec<CallHierarchyItem>> {
        self.route_semantic(
            GatewayMethod::TextDocumentPrepareCallHierarchy,
            SemanticCapability::CallHierarchy,
            Some(document_uri),
            |provider, root| provider.prepare_call_hierarchy(root, document_uri, position),
        )
    }

    pub fn incoming_calls(&self, item: &CallHierarchyItem) -> GatewayResponse<Vec<IncomingCall>> {
        self.route_semantic(
            GatewayMethod::CallHierarchyIncomingCalls,
            SemanticCapability::CallHierarchy,
            Some(&item.uri),
            |provider, root| provider.incoming_calls(root, item),
        )
    }

    pub fn outgoing_calls(&self, item: &CallHierarchyItem) -> GatewayResponse<Vec<OutgoingCall>> {
        self.route_semantic(
            GatewayMethod::CallHierarchyOutgoingCalls,
            SemanticCapability::CallHierarchy,
            Some(&item.uri),
            |provider, root| provider.outgoing_calls(root, item),
        )
    }

    pub fn signature_help(
        &self,
        document_uri: &str,
        position: LspPosition,
    ) -> GatewayResponse<Option<SignatureHelp>> {
        self.route_semantic(
            GatewayMethod::TextDocumentSignatureHelp,
            SemanticCapability::SignatureHelp,
            Some(document_uri),
            |provider, root| provider.signature_help(root, document_uri, position),
        )
    }

    pub fn prepare_type_hierarchy(
        &self,
        document_uri: &str,
        position: LspPosition,
    ) -> GatewayResponse<Vec<TypeHierarchyItem>> {
        self.route_semantic(
            GatewayMethod::TextDocumentPrepareTypeHierarchy,
            SemanticCapability::TypeHierarchy,
            Some(document_uri),
            |provider, root| provider.prepare_type_hierarchy(root, document_uri, position),
        )
    }

    pub fn type_hierarchy_supertypes(
        &self,
        item: &TypeHierarchyItem,
    ) -> GatewayResponse<Vec<TypeHierarchyItem>> {
        self.route_semantic(
            GatewayMethod::TypeHierarchySupertypes,
            SemanticCapability::TypeHierarchy,
            Some(&item.uri),
            |provider, root| provider.type_hierarchy_supertypes(root, item),
        )
    }

    pub fn type_hierarchy_subtypes(
        &self,
        item: &TypeHierarchyItem,
    ) -> GatewayResponse<Vec<TypeHierarchyItem>> {
        self.route_semantic(
            GatewayMethod::TypeHierarchySubtypes,
            SemanticCapability::TypeHierarchy,
            Some(&item.uri),
            |provider, root| provider.type_hierarchy_subtypes(root, item),
        )
    }

    /// Internal read-only rename-candidate operation. It never projects
    /// `renameProvider` and never returns or applies a `WorkspaceEdit`.
    pub fn rename_candidate(
        &self,
        request_id: &LspRequestId,
        document_uri: &str,
        position: LspPosition,
    ) -> GatewayResponse<RenameCandidateResult> {
        let unavailable = RenameCandidateResult::Unavailable {
            reason: RenameCandidateUnavailableReason::AmbiguousEvidence,
        };
        match self.semantic_request(
            request_id,
            &SemanticRequest::RenameCandidate {
                document_uri: document_uri.to_owned(),
                position,
            },
        ) {
            GatewayResponse::Value(SemanticResponse::RenameCandidate(value)) => {
                GatewayResponse::Value(value)
            }
            GatewayResponse::Partial {
                value: SemanticResponse::RenameCandidate(value),
                coverage,
                detail,
            } => GatewayResponse::Partial {
                value,
                coverage,
                detail,
            },
            GatewayResponse::Value(_) => GatewayResponse::Partial {
                value: unavailable,
                coverage: "rename-candidate-response-mismatch".to_owned(),
                detail: None,
            },
            GatewayResponse::Partial {
                coverage, detail, ..
            } => GatewayResponse::Partial {
                value: unavailable,
                coverage,
                detail,
            },
            GatewayResponse::Pending => GatewayResponse::Pending,
            GatewayResponse::Unavailable(unavailable) => GatewayResponse::Unavailable(unavailable),
            GatewayResponse::RequestFailed(failure) => GatewayResponse::RequestFailed(failure),
        }
    }

    pub fn semantic_request(
        &self,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> GatewayResponse<SemanticResponse> {
        if let SemanticRequest::WorkspaceSymbols { query } = request {
            return match self.workspace_symbols(query) {
                GatewayResponse::Value(value) => {
                    GatewayResponse::Value(SemanticResponse::WorkspaceSymbols(value))
                }
                GatewayResponse::Partial {
                    value,
                    coverage,
                    detail,
                } => GatewayResponse::Partial {
                    value: SemanticResponse::WorkspaceSymbols(value),
                    coverage,
                    detail,
                },
                GatewayResponse::Pending => GatewayResponse::Pending,
                GatewayResponse::Unavailable(unavailable) => {
                    GatewayResponse::Unavailable(unavailable)
                }
                GatewayResponse::RequestFailed(failure) => {
                    GatewayResponse::RequestFailed(failure)
                }
            };
        }
        self.route_semantic(
            request.method(),
            request.capability(),
            request.document_uri(),
            |provider, root| provider.request(root, request_id, request),
        )
    }

    pub fn prepare_rename(&self) -> GatewayResponse<()> {
        Self::explicitly_unavailable(GatewayMethod::TextDocumentPrepareRename)
    }

    pub fn rename(&self) -> GatewayResponse<()> {
        Self::explicitly_unavailable(GatewayMethod::TextDocumentRename)
    }

    pub fn general_code_actions(&self) -> GatewayResponse<()> {
        Self::explicitly_unavailable(GatewayMethod::TextDocumentCodeAction)
    }

    pub fn workspace_diagnostics(&self) -> GatewayResponse<()> {
        Self::explicitly_unavailable(GatewayMethod::WorkspaceDiagnostic)
    }

    pub fn execute_command(&self) -> GatewayResponse<()> {
        Self::explicitly_unavailable(GatewayMethod::WorkspaceExecuteCommand)
    }

    pub fn add_workspace_folder(&self) -> GatewayResponse<()> {
        Self::explicitly_unavailable(GatewayMethod::WorkspaceFolders)
    }

    pub fn github_ci_proximity_transport(&self) -> GatewayResponse<()> {
        Self::explicitly_unavailable(GatewayMethod::GitHubCiProximityTransport)
    }

    pub fn direct_database_write(&self) -> GatewayResponse<()> {
        Self::explicitly_unavailable(GatewayMethod::DirectDatabaseWrite)
    }

    fn trigger_feedback_cycle(
        &self,
        document_uri: String,
        trigger: DiagnosticTrigger,
    ) -> FeedbackCycleResponse {
        let Ok(root) = self.root_for_document(&document_uri) else {
            return FeedbackCycleResponse::Rejected {
                reason: "document is outside or ambiguous in the admitted workspace".to_owned(),
            };
        };
        self.feedback_cycle
            .request_feedback_cycle(FeedbackCycleRequest {
                root_uri: root.uri.clone(),
                document_uri,
                trigger,
            })
    }

    fn route_semantic<T>(
        &self,
        method: GatewayMethod,
        capability: SemanticCapability,
        document_uri: Option<&str>,
        route: impl FnOnce(&S, &AdmittedRoot) -> SemanticProviderOutcome<T>,
    ) -> GatewayResponse<T> {
        if !self.capabilities.supports_semantic(capability) {
            return GatewayResponse::unavailable(
                method,
                MethodUnavailableReason::CapabilityNotNegotiated,
            );
        }
        let root = match document_uri {
            Some(uri) => match self.root_for_document(uri) {
                Ok(root) => root,
                Err(reason) => return GatewayResponse::unavailable(method, reason),
            },
            None => {
                return GatewayResponse::unavailable(
                    method,
                    MethodUnavailableReason::AmbiguousAdmittedRoot,
                );
            }
        };
        match route(&self.semantic_provider, root) {
            SemanticProviderOutcome::Complete(value) => GatewayResponse::Value(value),
            SemanticProviderOutcome::Partial {
                value,
                coverage,
                detail,
            } => GatewayResponse::Partial {
                value,
                coverage,
                detail,
            },
            SemanticProviderOutcome::Pending => GatewayResponse::Pending,
            SemanticProviderOutcome::Unavailable => {
                GatewayResponse::unavailable(method, MethodUnavailableReason::ProviderUnavailable)
            }
        }
    }

    fn explicitly_unavailable<T>(method: GatewayMethod) -> GatewayResponse<T> {
        GatewayResponse::unavailable(method, MethodUnavailableReason::ExplicitlyUnavailable)
    }
}

fn workspace_route_reason(error: LspWorkspaceRouteError) -> MethodUnavailableReason {
    match error {
        LspWorkspaceRouteError::OutsideAdmittedRoots => {
            MethodUnavailableReason::OutsideAdmittedRoot
        }
        LspWorkspaceRouteError::AmbiguousAdmittedRoots => {
            MethodUnavailableReason::AmbiguousAdmittedRoot
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ClientCapabilities, GatewayCapabilities, UpstreamCapabilities, negotiate_capabilities,
    };
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    #[derive(Default)]
    struct Feedback {
        requests: RefCell<Vec<FeedbackCycleRequest>>,
    }

    impl FeedbackCyclePort for Feedback {
        fn request_feedback_cycle(&self, request: FeedbackCycleRequest) -> FeedbackCycleResponse {
            self.requests.borrow_mut().push(request);
            FeedbackCycleResponse::Accepted
        }
    }

    struct Semantics;

    impl SemanticProviderPort for Semantics {
        fn definition(
            &self,
            _root: &AdmittedRoot,
            document_uri: &str,
            _position: LspPosition,
        ) -> SemanticProviderOutcome<Vec<LspLocation>> {
            SemanticProviderOutcome::Complete(vec![LspLocation {
                uri: document_uri.into(),
                range: zero_range(),
            }])
        }

        fn rename_candidate(
            &self,
            _root: &AdmittedRoot,
            document_uri: &str,
            _position: LspPosition,
        ) -> SemanticProviderOutcome<RenameCandidateResult> {
            SemanticProviderOutcome::Complete(RenameCandidateResult::Available(RenameCandidate {
                document_uri: document_uri.to_owned(),
                range: zero_range(),
                placeholder: "old_name".to_owned(),
            }))
        }
    }

    fn zero_range() -> LspRange {
        LspRange {
            start: LspPosition {
                line: 0,
                character: 0,
            },
            end: LspPosition {
                line: 0,
                character: 0,
            },
        }
    }

    fn capabilities() -> EffectiveCapabilities {
        let client = ClientCapabilities {
            supports_versioned_publish_diagnostics: true,
            publish_diagnostics_related_information: true,
            publish_diagnostics_code_description: true,
            publish_diagnostics_data: true,
            supports_document_diagnostics: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
            ..ClientCapabilities::default()
        };
        let upstream = UpstreamCapabilities {
            supports_diagnostics: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
        };
        negotiate_capabilities(&client, &GatewayCapabilities::default(), &upstream)
    }

    #[test]
    fn save_and_pull_use_the_same_feedback_cycle_authority() {
        let gateway = DaemonLspGateway::new(
            AdmittedRoot::new("file:///root"),
            capabilities(),
            Feedback::default(),
            Semantics,
        );
        assert_eq!(
            gateway.document_saved("file:///root/a.rs"),
            FeedbackCycleResponse::Accepted
        );
        assert!(matches!(
            gateway.document_diagnostics(
                "file:///root/a.rs",
                "generation:7",
                Vec::new(),
                Vec::new(),
            ),
            GatewayResponse::Value(GatewayDocumentDiagnostics {
                report: DocumentDiagnosticReport::Full { .. },
                omitted_count: 0,
            })
        ));
        let requests = gateway.feedback_cycle.requests.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].trigger, DiagnosticTrigger::DocumentSave);
        assert_eq!(
            requests[1].trigger,
            DiagnosticTrigger::ExplicitDocumentDiagnostics
        );
    }

    #[test]
    fn semantic_routes_do_not_fabricate_empty_success() {
        let unavailable = DaemonLspGateway::new(
            AdmittedRoot::new("file:///root"),
            capabilities(),
            Feedback::default(),
            UnavailableSemanticProvider,
        );
        assert!(matches!(
            unavailable.definition(
                "file:///root/a.rs",
                LspPosition {
                    line: 0,
                    character: 0,
                }
            ),
            GatewayResponse::Unavailable(MethodUnavailable {
                reason: MethodUnavailableReason::ProviderUnavailable,
                ..
            })
        ));

        let available = DaemonLspGateway::new(
            AdmittedRoot::new("file:///root"),
            capabilities(),
            Feedback::default(),
            Semantics,
        );
        assert!(matches!(
            available.definition(
                "file:///root/a.rs",
                LspPosition {
                    line: 0,
                    character: 0,
                }
            ),
            GatewayResponse::Value(locations) if locations.len() == 1
        ));
        assert!(matches!(
            available.rename_candidate(
                &LspRequestId::Number(7),
                "file:///root/a.rs",
                LspPosition {
                    line: 0,
                    character: 0,
                },
            ),
            GatewayResponse::Value(RenameCandidateResult::Available(RenameCandidate {
                placeholder,
                ..
            })) if placeholder == "old_name"
        ));
    }

    #[test]
    fn rejects_prefix_confusion_and_future_methods_are_typed_unavailable() {
        let gateway = DaemonLspGateway::new(
            AdmittedRoot::new("file:///root"),
            capabilities(),
            Feedback::default(),
            Semantics,
        );
        assert!(matches!(
            gateway.definition(
                "file:///root-other/a.rs",
                LspPosition {
                    line: 0,
                    character: 0,
                }
            ),
            GatewayResponse::Unavailable(MethodUnavailable {
                reason: MethodUnavailableReason::OutsideAdmittedRoot,
                ..
            })
        ));
        assert!(matches!(
            gateway.rename(),
            GatewayResponse::Unavailable(MethodUnavailable {
                reason: MethodUnavailableReason::ExplicitlyUnavailable,
                ..
            })
        ));
        assert!(matches!(
            gateway.workspace_diagnostics(),
            GatewayResponse::Unavailable(_)
        ));
        assert!(matches!(
            gateway.github_ci_proximity_transport(),
            GatewayResponse::Unavailable(_)
        ));
    }

    #[test]
    fn admitted_root_rejects_ambiguous_or_escaping_document_uris() {
        let root = AdmittedRoot::new("file:///root");
        assert!(root.contains_document("file:///root/src/lib.rs"));
        assert!(root.contains_document("file:///root/with%20space.rs"));

        for document_uri in [
            "file:///root-sibling/a.rs",
            "file:///root/%2e%2e/escape.rs",
            "file:///root/.%2E/escape.rs",
            "file:///root/%2Fescape.rs",
            "file:///root/%5cescape.rs",
            "file:///root/%00escape.rs",
            "file:///root/a.rs?outside=true",
            "file:///root/a.rs#outside",
            "https:///root/a.rs",
        ] {
            assert!(
                !root.contains_document(document_uri),
                "unexpectedly admitted {document_uri}"
            );
        }
    }

    #[test]
    fn admitted_root_matches_equivalent_directory_uri() {
        let root = AdmittedRoot::new("file:///root/project");

        assert!(root.matches_root_uri("file:///root/project/"));
        assert!(!root.matches_root_uri("file:///root/project-other/"));
    }

    #[test]
    fn invalid_document_uri_is_rejected_before_semantic_provider_dispatch() {
        let gateway = DaemonLspGateway::new(
            AdmittedRoot::new("file:///root"),
            capabilities(),
            Feedback::default(),
            Semantics,
        );

        for document_uri in [
            "file:///root-sibling/a.rs",
            "file:///root/%2e%2e/root/a.rs",
            "file:///root/%2fa.rs",
            "file:///root/%5ca.rs",
            "file:///root/%00a.rs",
            "file:///root/a.rs?query",
            "file:///root/a.rs#fragment",
            "untitled:///root/a.rs",
        ] {
            assert!(matches!(
                gateway.definition(
                    document_uri,
                    LspPosition {
                        line: 0,
                        character: 0,
                    }
                ),
                GatewayResponse::Unavailable(MethodUnavailable {
                    reason: MethodUnavailableReason::OutsideAdmittedRoot,
                    ..
                })
            ));
        }
    }

    #[test]
    fn semantic_broker_preserves_standard_wire_shape_and_typed_failure_detail() {
        let request = SemanticRequest::Definition {
            document_uri: "file:///root/lib.rs".to_owned(),
            position: LspPosition {
                line: 3,
                character: 1,
            },
        };
        let wire = lsp_semantic_request(&request).expect("standard request");
        assert_eq!(wire.method(), "textDocument/definition");
        assert_eq!(wire.params()["textDocument"]["uri"], "file:///root/lib.rs");
        assert_eq!(wire.params()["position"]["line"], 3);

        let projected = project_semantic_outcome(
            &AdmittedRoot::new("file:///root"),
            &request,
            LspSemanticOperationOutcome::Partial {
                value: serde_json::json!(null),
                coverage: "analyzer-start-failed".to_owned(),
                detail: Some(LspSemanticOperationOutcome::ANALYZER_START_FAILED_DETAIL),
            },
        );
        assert_eq!(
            projected,
            SemanticProviderOutcome::Partial {
                value: SemanticResponse::Locations(Vec::new()),
                coverage: "analyzer-start-failed".to_owned(),
                detail: Some("Analyzer failed to start.".to_owned()),
            }
        );
    }

    #[test]
    fn semantic_failure_details_are_distinct_static_and_protocol_safe() {
        let templates = [
            LspSemanticOperationOutcome::ANALYZER_START_FAILED_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_CANCELLED_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_TIMEOUT_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_REMOTE_ERROR_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_TRANSPORT_FAILED_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_INVALID_RESPONSE_DETAIL,
            LspSemanticOperationOutcome::GRAPH_READ_FAILED_DETAIL,
        ];
        assert_eq!(
            templates
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            templates.len()
        );
        for detail in templates {
            let serialized = serde_json::to_string(&json!({ "detail": detail })).unwrap();
            for forbidden in [
                "bearer-secret",
                "alice:hunter2",
                "file://",
                "/home/alice",
                r"C:\Users\alice",
                "\n",
            ] {
                assert!(!serialized.contains(forbidden));
            }
            assert!(
                !detail.is_empty()
                    && detail.len() <= 96
                    && detail.is_ascii()
                    && !detail.chars().any(char::is_control)
            );
        }
    }

    struct InlineTask;

    impl LspRuntimeTask for InlineTask {
        fn abort(&self) {}
    }

    struct InlineWake;

    impl Wake for InlineWake {
        fn wake(self: Arc<Self>) {}
    }

    struct InlineSpawner;

    impl LspRuntimeSpawner for InlineSpawner {
        fn spawn(&self, mut future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
            let waker = Waker::from(Arc::new(InlineWake));
            let mut context = Context::from_waker(&waker);
            assert_eq!(future.as_mut().poll(&mut context), Poll::Ready(()));
            Box::new(InlineTask)
        }
    }

    struct RuntimeFeedback {
        calls: Arc<AtomicUsize>,
    }

    impl FeedbackCycleRuntimePort for RuntimeFeedback {
        fn execute(
            &self,
            _request: FeedbackCycleRequest,
        ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
            let calls = Arc::clone(&self.calls);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
        }
    }

    #[test]
    fn feedback_broker_preserves_non_lsp_authority_evidence() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = FeedbackCycleAdapter::new(
            Arc::new(InlineSpawner),
            Arc::new(RuntimeFeedback {
                calls: Arc::clone(&calls),
            }),
        );
        assert_eq!(
            adapter.request_feedback_cycle(FeedbackCycleRequest {
                root_uri: "file:///root".to_owned(),
                document_uri: "file:///root/a.rs".to_owned(),
                trigger: DiagnosticTrigger::DocumentSave,
            }),
            FeedbackCycleResponse::Accepted
        );
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    struct SemanticAuthority {
        cancelled: Arc<AtomicBool>,
    }

    impl LspSemanticRequestAuthority for SemanticAuthority {
        fn start(
            &self,
            _root: AdmittedRoot,
            _request_id: LspRequestId,
            request: LspSemanticRequest,
        ) -> LspRuntimeFuture<LspSemanticOperationOutcome> {
            assert_eq!(request.method(), "textDocument/definition");
            Box::pin(async {
                LspSemanticOperationOutcome::Complete(json!([{
                    "uri": "file:///root/lib.rs",
                    "range": {
                        "start": { "line": 3, "character": 1 },
                        "end": { "line": 3, "character": 4 }
                    }
                }, {
                    "uri": "file:///outside/lib.rs",
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 1 }
                    }
                }]))
            })
        }

        fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
            self.cancelled.store(true, Ordering::Release);
            true
        }
    }

    #[test]
    fn semantic_broker_polls_and_cancels_by_project_scoped_request() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let adapter = SemanticProviderAdapter::new(
            Arc::new(InlineSpawner),
            Arc::new(SemanticAuthority {
                cancelled: Arc::clone(&cancelled),
            }),
        );
        let root = AdmittedRoot::new("file:///root");
        let request_id = LspRequestId::Number(8);
        let request = SemanticRequest::Definition {
            document_uri: "file:///root/lib.rs".to_owned(),
            position: LspPosition {
                line: 3,
                character: 1,
            },
        };

        assert_eq!(
            SemanticProviderPort::request(&adapter, &root, &request_id, &request),
            SemanticProviderOutcome::Pending
        );
        assert_eq!(
            SemanticProviderPort::request(&adapter, &root, &request_id, &request),
            SemanticProviderOutcome::Partial {
                value: SemanticResponse::Locations(vec![LspLocation {
                    uri: "file:///root/lib.rs".to_owned(),
                    range: LspRange {
                        start: LspPosition {
                            line: 3,
                            character: 1,
                        },
                        end: LspPosition {
                            line: 3,
                            character: 4,
                        },
                    },
                }]),
                coverage: "semantic-result-outside-admitted-root".to_owned(),
                detail: None,
            }
        );

        assert!(adapter.cancel_request(&root, &request_id));
        assert!(cancelled.load(Ordering::Acquire));
    }

    struct Cancellation;

    impl AnalyzerCancellationPort for Cancellation {
        fn cancel_upstream(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
            false
        }
    }

    struct ContextPort;

    impl ContextProjectionPort for ContextPort {
        fn registrations(&self) -> Vec<crate::ContextProjectionRegistration> {
            Vec::new()
        }

        fn snapshot(
            &self,
            _root: &AdmittedRoot,
            _request_id: &LspRequestId,
            _request: &crate::ContextProjectionRequest,
        ) -> crate::ContextProjectionOutcome {
            crate::ContextProjectionOutcome::Unsupported
        }
    }

    #[test]
    fn provider_factory_creates_isolated_session_runtime_state() {
        let factory = DaemonLspProviderFactory::new(
            Feedback::default(),
            Semantics,
            crate::UnavailableDiagnosticSnapshotProvider,
            Cancellation,
            ContextPort,
            GatewayCapabilities::default(),
            UpstreamCapabilities::default(),
        );
        let first = factory.into_session(AdmittedRoot::new("file:///root"));
        let second = DaemonLspProviderFactory::new(
            Feedback::default(),
            Semantics,
            crate::UnavailableDiagnosticSnapshotProvider,
            Cancellation,
            ContextPort,
            GatewayCapabilities::default(),
            UpstreamCapabilities::default(),
        )
        .into_session(AdmittedRoot::new("file:///root"));

        assert!(!std::ptr::eq(first.overlays(), second.overlays()));
        assert_eq!(first.root(), second.root());
        assert_eq!(
            first.lifecycle(),
            crate::SessionLifecycle::AwaitingInitialize
        );
        assert_eq!(
            second.lifecycle(),
            crate::SessionLifecycle::AwaitingInitialize
        );
    }
}

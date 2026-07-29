//! Single-root daemon LSP gateway request boundary.
//!
//! The gateway has one already-admitted root and delegates post-edit work to
//! the feedback-cycle application boundary. It intentionally does not open a
//! store, supervise an analyzer, resolve workspace folders, or implement any
//! host-specific transport.

use std::path::{Component, PathBuf};
use std::sync::Arc;

use url::Url;

use crate::capabilities::{CapabilityAvailability, EffectiveCapabilities, SemanticCapability};
use crate::diagnostics::{
    DiagnosticMerge, DocumentDiagnosticReport, GatewayDiagnostic, LspPosition, LspRange,
};
use crate::session::{LspRequestFailure, LspRequestId};

/// A single root that was authoritatively admitted before the LSP session was
/// created. The gateway never chooses a root from CWD or client folder order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedRoot {
    uri: String,
}

impl AdmittedRoot {
    pub fn new(uri: impl Into<String>) -> Self {
        Self { uri: uri.into() }
    }

    pub fn uri(&self) -> &str {
        &self.uri
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayDocumentDiagnostics {
    pub report: DocumentDiagnosticReport,
    pub omitted_count: usize,
}

/// A daemon-owned, single-root LSP session.
///
/// Both application ports are explicit constructor inputs. A retained daemon
/// must not accidentally mount a whole-session unavailable semantic runtime;
/// individual provider methods may still return
/// [`SemanticProviderOutcome::Unavailable`] truthfully.
pub struct DaemonLspGateway<P, S> {
    root: AdmittedRoot,
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
        Self {
            root,
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
        &self.root
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
        if !self.root.contains_document(&document_uri) {
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
        if !self.root.contains_document(document_uri) {
            return GatewayResponse::unavailable(
                GatewayMethod::TextDocumentDiagnostic,
                MethodUnavailableReason::OutsideAdmittedRoot,
            );
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
        if !self.root.contains_document(document_uri) {
            return GatewayResponse::unavailable(
                GatewayMethod::TextDocumentDiagnostic,
                MethodUnavailableReason::OutsideAdmittedRoot,
            );
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
            |provider| provider.declaration(&self.root, document_uri, position),
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
            |provider| provider.definition(&self.root, document_uri, position),
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
            |provider| provider.type_definition(&self.root, document_uri, position),
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
            |provider| provider.implementation(&self.root, document_uri, position),
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
            |provider| provider.references(&self.root, document_uri, position),
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
            |provider| provider.hover(&self.root, document_uri, position),
        )
    }

    pub fn document_symbols(&self, document_uri: &str) -> GatewayResponse<Vec<DocumentSymbol>> {
        self.route_semantic(
            GatewayMethod::TextDocumentDocumentSymbol,
            SemanticCapability::DocumentSymbol,
            Some(document_uri),
            |provider| provider.document_symbols(&self.root, document_uri),
        )
    }

    pub fn workspace_symbols(&self, query: &str) -> GatewayResponse<Vec<WorkspaceSymbol>> {
        self.route_semantic(
            GatewayMethod::WorkspaceSymbol,
            SemanticCapability::WorkspaceSymbol,
            None,
            |provider| provider.workspace_symbols(&self.root, query),
        )
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
            |provider| provider.prepare_call_hierarchy(&self.root, document_uri, position),
        )
    }

    pub fn incoming_calls(&self, item: &CallHierarchyItem) -> GatewayResponse<Vec<IncomingCall>> {
        self.route_semantic(
            GatewayMethod::CallHierarchyIncomingCalls,
            SemanticCapability::CallHierarchy,
            Some(&item.uri),
            |provider| provider.incoming_calls(&self.root, item),
        )
    }

    pub fn outgoing_calls(&self, item: &CallHierarchyItem) -> GatewayResponse<Vec<OutgoingCall>> {
        self.route_semantic(
            GatewayMethod::CallHierarchyOutgoingCalls,
            SemanticCapability::CallHierarchy,
            Some(&item.uri),
            |provider| provider.outgoing_calls(&self.root, item),
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
            |provider| provider.signature_help(&self.root, document_uri, position),
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
            |provider| provider.prepare_type_hierarchy(&self.root, document_uri, position),
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
            |provider| provider.type_hierarchy_supertypes(&self.root, item),
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
            |provider| provider.type_hierarchy_subtypes(&self.root, item),
        )
    }

    /// Internal read-only Plan 05/35 rename-candidate operation. It never
    /// projects `renameProvider` and never returns or applies a `WorkspaceEdit`.
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
        self.route_semantic(
            request.method(),
            request.capability(),
            request.document_uri(),
            |provider| provider.request(&self.root, request_id, request),
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
        self.feedback_cycle
            .request_feedback_cycle(FeedbackCycleRequest {
                root_uri: self.root.uri.clone(),
                document_uri,
                trigger,
            })
    }

    fn route_semantic<T>(
        &self,
        method: GatewayMethod,
        capability: SemanticCapability,
        document_uri: Option<&str>,
        route: impl FnOnce(&S) -> SemanticProviderOutcome<T>,
    ) -> GatewayResponse<T> {
        if !self.capabilities.supports_semantic(capability) {
            return GatewayResponse::unavailable(
                method,
                MethodUnavailableReason::CapabilityNotNegotiated,
            );
        }
        if document_uri.is_some_and(|uri| !self.root.contains_document(uri)) {
            return GatewayResponse::unavailable(
                method,
                MethodUnavailableReason::OutsideAdmittedRoot,
            );
        }
        match route(&self.semantic_provider) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ClientCapabilities, GatewayCapabilities, UpstreamCapabilities, negotiate_capabilities,
    };
    use std::cell::RefCell;

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
}

//! Gateway request/response DTOs and their JSON-RPC wire codecs.

use std::sync::Arc;

use serde_json::{Value, json};

use super::admission::{AdmittedRoot, DocumentConfinement};
use crate::capabilities::SemanticCapability;
use crate::diagnostics::{LspPosition, LspRange};
use crate::session::LspRequestFailure;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticTrigger {
    DocumentSave,
    ExplicitDocumentDiagnostics,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedbackCycleRequest {
    pub root_uri: String,
    pub document_uri: String,
    pub trigger: DiagnosticTrigger,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FeedbackCycleResponse {
    Accepted,
    Deferred { reason: String },
    Rejected { reason: String },
}

/// Port implemented by the daemon/application adapter.
///
/// The implementation must delegate to the existing feedback-cycle operation
/// (ultimately `tracedecay_contracts::feedback::FeedbackCycleService`) and
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
    WorkspaceDiagnostic,
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
            Self::WorkspaceDiagnostic => "workspace/diagnostic",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MethodUnavailableReason {
    ExplicitlyUnavailable,
    CapabilityNotNegotiated,
    Denied,
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
    pub(super) fn unavailable(method: GatewayMethod, reason: MethodUnavailableReason) -> Self {
        Self::Unavailable(MethodUnavailable { method, reason })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspLocation {
    pub uri: String,
    pub range: LspRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hover {
    pub contents: String,
    pub range: Option<LspRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSymbol {
    pub name: String,
    /// LSP `SymbolKind` supplied by the admitted provider.
    pub kind: u32,
    pub range: LspRange,
    pub selection_range: LspRange,
    pub children: Vec<DocumentSymbol>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSymbol {
    pub name: String,
    /// LSP `SymbolKind` supplied by the admitted provider.
    pub kind: u32,
    pub location: LspLocation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallHierarchyItem {
    pub name: String,
    /// LSP `SymbolKind` supplied by the admitted provider.
    pub kind: u32,
    pub uri: String,
    pub range: LspRange,
    pub selection_range: LspRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncomingCall {
    pub from: CallHierarchyItem,
    pub from_ranges: Vec<LspRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutgoingCall {
    pub to: CallHierarchyItem,
    pub from_ranges: Vec<LspRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureHelp {
    pub signatures: Vec<String>,
    pub active_signature: Option<u32>,
    pub active_parameter: Option<u32>,
}

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
    pub(super) fn map<U>(self, project: impl FnOnce(T) -> U) -> SemanticProviderOutcome<U> {
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

    pub(super) fn capability(&self) -> SemanticCapability {
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
    pub const ANALYZER_RETIRED_DETAIL: &'static str =
        "Analyzer client was retired without a result.";
    pub const ANALYZER_TIMEOUT_DETAIL: &'static str = "Analyzer request timed out.";
    pub const ANALYZER_REMOTE_ERROR_DETAIL: &'static str =
        "Analyzer request failed with a remote error.";
    pub const ANALYZER_TRANSPORT_FAILED_DETAIL: &'static str = "Analyzer transport failed.";
    pub const ANALYZER_INVALID_RESPONSE_DETAIL: &'static str =
        "Analyzer returned an invalid response.";
    pub const GRAPH_READ_FAILED_DETAIL: &'static str = "Graph semantic read failed.";
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
    let confinement = DocumentConfinement::for_root(root);
    let contains = |uri: &str| confinement.as_ref().is_some_and(|root| root.contains(uri));
    match response {
        SemanticResponse::Locations(mut values) => {
            let before = values.len();
            values.retain(|value| contains(&value.uri));
            let omitted = before != values.len();
            (SemanticResponse::Locations(values), omitted)
        }
        SemanticResponse::WorkspaceSymbols(mut values) => {
            let before = values.len();
            values.retain(|value| contains(&value.location.uri));
            let omitted = before != values.len();
            (SemanticResponse::WorkspaceSymbols(values), omitted)
        }
        SemanticResponse::CallHierarchyItems(mut values) => {
            let before = values.len();
            values.retain(|value| contains(&value.uri));
            let omitted = before != values.len();
            (SemanticResponse::CallHierarchyItems(values), omitted)
        }
        SemanticResponse::IncomingCalls(mut values) => {
            let before = values.len();
            values.retain(|value| contains(&value.from.uri));
            let omitted = before != values.len();
            (SemanticResponse::IncomingCalls(values), omitted)
        }
        SemanticResponse::OutgoingCalls(mut values) => {
            let before = values.len();
            values.retain(|value| contains(&value.to.uri));
            let omitted = before != values.len();
            (SemanticResponse::OutgoingCalls(values), omitted)
        }
        SemanticResponse::TypeHierarchyItems(mut values) => {
            let before = values.len();
            values.retain(|value| contains(&value.uri));
            let omitted = before != values.len();
            (SemanticResponse::TypeHierarchyItems(values), omitted)
        }
        SemanticResponse::RenameCandidate(RenameCandidateResult::Available(candidate))
            if !contains(&candidate.document_uri) =>
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

pub(super) fn empty_semantic_response(request: &SemanticRequest) -> SemanticResponse {
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

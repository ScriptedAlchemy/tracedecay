//! Single-root daemon LSP gateway request skeleton.
//!
//! The gateway has one already-admitted root and delegates post-edit work to
//! the feedback-cycle application boundary. It intentionally does not open a
//! store, supervise an analyzer, resolve workspace folders, or implement any
//! host-specific transport.

use super::capabilities::{CapabilityAvailability, EffectiveCapabilities, SemanticCapability};
use super::diagnostics::{
    DiagnosticMerge, DocumentDiagnosticReport, GatewayDiagnostic, LspPosition, LspRange,
};
use super::session::LspRequestFailure;

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

    /// Presentation-level containment guard. Root admission itself remains a
    /// daemon authorization decision; this prevents a request from escaping
    /// that already-admitted URI by simple prefix confusion.
    pub fn contains_document(&self, document_uri: &str) -> bool {
        let root = self.uri.trim_end_matches('/');
        document_uri
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
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

/// The protocol dispatch outcome used by request handlers in this scaffold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GatewayResponse<T> {
    Value(T),
    Partial { value: T, coverage: String },
    Unavailable(MethodUnavailable),
    RequestFailed(LspRequestFailure),
}

impl<T> GatewayResponse<T> {
    fn unavailable(method: GatewayMethod, reason: MethodUnavailableReason) -> Self {
        Self::Unavailable(MethodUnavailable { method, reason })
    }
}

/// LSP `Location` payload shape used by empty navigation responses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspLocation {
    pub uri: String,
    pub range: LspRange,
}

/// LSP `Hover` payload shape used by an unavailable-or-null hover response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hover {
    pub contents: String,
    pub range: Option<LspRange>,
}

/// LSP `DocumentSymbol` payload shape used by empty document-symbol responses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSymbol {
    pub name: String,
    pub range: LspRange,
    pub selection_range: LspRange,
    pub children: Vec<DocumentSymbol>,
}

/// LSP `SymbolInformation` payload shape used by empty workspace-symbol
/// responses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSymbol {
    pub name: String,
    pub location: LspLocation,
}

/// LSP `CallHierarchyItem` payload shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallHierarchyItem {
    pub name: String,
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
    pub uri: String,
    pub range: LspRange,
    pub selection_range: LspRange,
}

/// A truthful semantic-provider outcome. Empty collections are complete only
/// when the provider says they are complete; unavailable and partial states
/// cannot collapse into a plausible clean empty result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticProviderOutcome<T> {
    Complete(T),
    Partial { value: T, coverage: String },
    Unavailable,
}

/// Typed daemon adapter for admitted upstream/graph semantic operations.
/// Defaults are unavailable rather than fabricated empty answers.
pub trait SemanticProviderPort {
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
pub struct DaemonLspGateway<P, S = UnavailableSemanticProvider> {
    root: AdmittedRoot,
    capabilities: EffectiveCapabilities,
    feedback_cycle: P,
    semantic_provider: S,
}

impl<P> DaemonLspGateway<P, UnavailableSemanticProvider>
where
    P: FeedbackCyclePort,
{
    pub fn new(root: AdmittedRoot, capabilities: EffectiveCapabilities, feedback_cycle: P) -> Self {
        Self {
            root,
            capabilities,
            feedback_cycle,
            semantic_provider: UnavailableSemanticProvider,
        }
    }
}

impl<P, S> DaemonLspGateway<P, S>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
{
    pub fn with_semantic_provider(
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

    pub fn root(&self) -> &AdmittedRoot {
        &self.root
    }

    pub fn capabilities(&self) -> &EffectiveCapabilities {
        &self.capabilities
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

    /// Handles `textDocument/diagnostic` through the same feedback-cycle port
    /// as save, then projects a generation-bound, bounded merge.
    pub fn document_diagnostics(
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
        if !matches!(
            self.trigger_feedback_cycle(
                document_uri.to_owned(),
                DiagnosticTrigger::ExplicitDocumentDiagnostics,
            ),
            FeedbackCycleResponse::Accepted
        ) {
            return GatewayResponse::RequestFailed(LspRequestFailure::ServerCancelled {
                retrigger_request: true,
            });
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
            SemanticProviderOutcome::Partial { value, coverage } => {
                GatewayResponse::Partial { value, coverage }
            }
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
    use crate::daemon::lsp_gateway::{
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
            document_diagnostics_related_information: true,
            document_diagnostics_code_description: true,
            document_diagnostics_data: true,
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

        let available = DaemonLspGateway::with_semantic_provider(
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
    }

    #[test]
    fn rejects_prefix_confusion_and_future_methods_are_typed_unavailable() {
        let gateway = DaemonLspGateway::new(
            AdmittedRoot::new("file:///root"),
            capabilities(),
            Feedback::default(),
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
}

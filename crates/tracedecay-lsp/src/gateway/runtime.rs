//! Gateway runtime: feedback, semantic, and cancellation adapters, the
//! provider factory, and the daemon gateway itself.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_domain::ManifestDigest;
use tracedecay_runtime_core::logging::log_daemon_event;

use super::admission::AdmittedRoot;
use super::dto::{
    CallHierarchyItem, DiagnosticTrigger, DocumentSymbol, FeedbackCyclePort, FeedbackCycleRequest,
    FeedbackCycleResponse, GatewayMethod, GatewayResponse, Hover, IncomingCall, LspLocation,
    LspSemanticOperationOutcome, LspSemanticRequest, MethodUnavailableReason, OutgoingCall,
    RenameCandidateResult, SemanticProviderOutcome, SemanticRequest, SemanticResponse,
    SignatureHelp, TypeHierarchyItem, WorkspaceSymbol, empty_semantic_response,
    lsp_semantic_request, project_semantic_outcome,
};
use crate::capabilities::{
    CapabilityAvailability, ClientCapabilities, EffectiveCapabilities, GatewayCapabilities,
    SemanticCapability, UpstreamCapabilities, negotiate_capabilities,
};
use crate::context::{ContextProjectionPort, MAX_CONTEXT_PROJECTION_KINDS};
use crate::diagnostics::LspPosition;
use crate::gateway::operation_table::{
    BoundedOperationCapacity, BoundedOperationTable, OperationAdmission, OperationPoll,
};
use crate::protocol::DaemonLspProtocolSession;
use crate::provider::{AnalyzerCancellationPort, DiagnosticSnapshotPort};
use crate::session::{
    AuthorizedLspWorkspace, LspRequestFailure, LspRequestId, LspWorkspaceRouteError,
    MAX_PENDING_REQUESTS,
};

pub type LspRuntimeFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub trait LspRuntimeTask: Send + Sync {
    fn abort(&self);
}

/// Runtime injection used by broker policy without taking a Tokio dependency.
pub trait LspRuntimeSpawner: Send + Sync {
    fn spawn(&self, future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask>;
}

const MAX_RUNTIME_FAILURE_CLASS_BYTES: usize = 96;
pub const MAX_SEMANTIC_OPERATIONS: usize = MAX_PENDING_REQUESTS * 2;

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
                log_daemon_event(
                    "lsp_feedback_cycle_failed",
                    &[("failure_class", error.class().to_owned())],
                );
            }
        }));
        FeedbackCycleResponse::Accepted
    }
}

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

/// A daemon-owned LSP workspace session.
///
/// Both application ports are explicit constructor inputs. A retained daemon
/// must not accidentally mount a whole-session unavailable semantic runtime;
/// individual provider methods may still return
/// [`SemanticProviderOutcome::Unavailable`] truthfully.
pub struct DaemonLspGateway<P, S> {
    workspace: AuthorizedLspWorkspace,
    capabilities: EffectiveCapabilities,
    pub(super) feedback_cycle: P,
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

    pub fn root(&self) -> &AdmittedRoot {
        self.workspace.primary()
    }

    pub fn workspace(&self) -> &AuthorizedLspWorkspace {
        &self.workspace
    }

    /// Installs a workspace the daemon owner already resolved and authorized.
    /// The gateway never derives roots itself, so this is the only way an
    /// admitted root set changes after `initialize`.
    pub(crate) fn replace_workspace(&mut self, workspace: AuthorizedLspWorkspace) {
        self.workspace = workspace;
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
    /// transitioning the session to `Ready`.
    pub fn bind_initialized_capabilities(&mut self, capabilities: EffectiveCapabilities) {
        self.capabilities = capabilities;
    }

    /// Applies the result of one exact LSP dynamic diagnostic registration.
    ///
    /// This is deliberately narrower than rebinding the negotiated capability
    /// set: only the standard diagnostic provider can change, and only the
    /// protocol actor that owns the corresponding client registration may do
    /// so.
    pub(crate) fn bind_dynamic_diagnostics(
        &mut self,
        registered: bool,
        workspace_diagnostics: bool,
        refresh: bool,
    ) {
        self.capabilities.supports_document_diagnostics = registered;
        self.capabilities.workspace_diagnostics_supported = registered && workspace_diagnostics;
        self.capabilities.supports_workspace_diagnostic_refresh = registered && refresh;
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

    /// Multi-root fan-out for `workspace/symbol`. Reached only through
    /// [`Self::semantic_request`], the single production semantic entry point.
    fn workspace_symbols(&self, query: &str) -> GatewayResponse<Vec<WorkspaceSymbol>> {
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

    #[hotpath::measure(label = "lsp_gateway_semantic_request", impl_type = "DaemonLspGateway")]
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
                GatewayResponse::RequestFailed(failure) => GatewayResponse::RequestFailed(failure),
            };
        }
        self.route_semantic(
            request.method(),
            request.capability(),
            request.document_uri(),
            |provider, root| provider.request(root, request_id, request),
        )
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
                root_uri: root.uri().to_owned(),
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

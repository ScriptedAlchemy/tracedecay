//! Analyzer supervision state and truthful semantic/diagnostic adapters.
//!
//! Process creation, configuration, policy, and storage remain daemon-owned
//! concerns outside this module. This code only records bounded health facts
//! supplied by that owner and routes typed provider results without inventing
//! a clean answer when an upstream analyzer is unavailable.

use std::sync::Arc;

use crate::diagnostics::{GatewayDiagnostic, LspPosition};
use crate::gateway::{
    AdmittedRoot, CallHierarchyItem, DocumentSymbol, Hover, IncomingCall, LspLocation,
    OutgoingCall, SemanticProviderOutcome, SemanticProviderPort, SemanticRequest, SemanticResponse,
    SignatureHelp, TypeHierarchyItem, WorkspaceSymbol,
};
use crate::overlay::OverlaySnapshot;
use crate::session::LspRequestId;

/// Restart exhaustion is a stable health state, not an invitation for a
/// bridge or client to start its own analyzer.
pub const MAX_ANALYZER_RESTARTS: u8 = 3;
pub const MAX_DIAGNOSTIC_OPERATION_ID_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnalyzerState {
    AwaitingStart,
    Starting,
    Ready,
    RestartBackoff,
    Unavailable,
    Exhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnalyzerEvent {
    StartRequested,
    Ready,
    Crashed,
    StartupFailed,
    TimedOut,
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnalyzerTransitionError {
    InvalidTransition {
        from: AnalyzerState,
        event: AnalyzerEvent,
    },
}

/// In-memory state fed by the daemon analyzer owner. It deliberately contains
/// no executable path, environment, command line, stderr, or process handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyzerSupervisor {
    state: AnalyzerState,
    restart_attempts: u8,
}

impl Default for AnalyzerSupervisor {
    fn default() -> Self {
        Self {
            state: AnalyzerState::AwaitingStart,
            restart_attempts: 0,
        }
    }
}

impl AnalyzerSupervisor {
    pub fn state(&self) -> AnalyzerState {
        self.state
    }

    pub fn restart_attempts(&self) -> u8 {
        self.restart_attempts
    }

    pub fn apply(
        &mut self,
        event: AnalyzerEvent,
    ) -> Result<AnalyzerState, AnalyzerTransitionError> {
        let next = match (self.state, event) {
            (
                AnalyzerState::AwaitingStart | AnalyzerState::RestartBackoff,
                AnalyzerEvent::StartRequested,
            ) => AnalyzerState::Starting,
            (AnalyzerState::Starting, AnalyzerEvent::Ready) => AnalyzerState::Ready,
            (
                AnalyzerState::Starting,
                AnalyzerEvent::StartupFailed | AnalyzerEvent::Crashed | AnalyzerEvent::TimedOut,
            )
            | (AnalyzerState::Ready, AnalyzerEvent::Crashed | AnalyzerEvent::TimedOut) => {
                self.restart_attempts = self.restart_attempts.saturating_add(1);
                if self.restart_attempts >= MAX_ANALYZER_RESTARTS {
                    AnalyzerState::Exhausted
                } else {
                    AnalyzerState::RestartBackoff
                }
            }
            (_, AnalyzerEvent::Disabled) => AnalyzerState::Unavailable,
            (from, event) => {
                return Err(AnalyzerTransitionError::InvalidTransition { from, event });
            }
        };
        self.state = next;
        Ok(next)
    }

    pub fn is_ready(&self) -> bool {
        self.state == AnalyzerState::Ready
    }
}

/// Best-effort cancellation boundary owned by the actual analyzer runtime.
/// The session actor always suppresses a cancelled downstream response even if
/// this port cannot interrupt the upstream request.
pub trait AnalyzerCancellationPort {
    fn cancel_upstream(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool;
}

impl<T> AnalyzerCancellationPort for Arc<T>
where
    T: AnalyzerCancellationPort + ?Sized,
{
    fn cancel_upstream(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        (**self).cancel_upstream(root, request_id)
    }
}

/// A generation-bound diagnostic read supplied by the daemon's canonical
/// diagnostic/application owners. Overlay content is explicitly marked
/// ephemeral by the input snapshot and cannot be persisted by this adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationDiagnostics {
    pub generation: u64,
    pub upstream: Vec<GatewayDiagnostic>,
    pub tracedecay: Vec<GatewayDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticRefreshIdentity {
    pub operation_id: String,
    pub source_generation: Option<u64>,
    pub target_generation: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticRefreshAdmission {
    Started(DiagnosticRefreshIdentity),
    AlreadyRunning(DiagnosticRefreshIdentity),
    Rejected { failure_class: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticSnapshotOutcome {
    Ready {
        diagnostics: GenerationDiagnostics,
        completed_operation_id: Option<String>,
    },
    Refreshing(DiagnosticRefreshIdentity),
    Partial {
        source_generation: Option<u64>,
        coverage: String,
    },
    Failed {
        source_generation: Option<u64>,
        failure_class: String,
    },
}

/// Port implemented by a daemon/application adapter. It is a read projection;
/// no diagnostic record is owned or persisted by the gateway. Refresh methods
/// are non-blocking: the provider starts canonical upstream/compiler work and
/// later reports `Ready` with the matching completed operation identity.
/// Unsaved overlays are ephemeral inputs and must never be persisted by the
/// adapter.
pub trait DiagnosticSnapshotPort {
    fn document_diagnostics(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome;

    fn request_document_refresh(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _overlay: Option<&OverlaySnapshot>,
        _source_generation: Option<u64>,
    ) -> DiagnosticRefreshAdmission {
        DiagnosticRefreshAdmission::Rejected {
            failure_class: "refresh-unsupported".to_owned(),
        }
    }
}

impl<T> DiagnosticSnapshotPort for Arc<T>
where
    T: DiagnosticSnapshotPort + ?Sized,
{
    fn document_diagnostics(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        (**self).document_diagnostics(root, document_uri, overlay)
    }

    fn request_document_refresh(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
        source_generation: Option<u64>,
    ) -> DiagnosticRefreshAdmission {
        (**self).request_document_refresh(root, document_uri, overlay, source_generation)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableDiagnosticSnapshotProvider;

impl DiagnosticSnapshotPort for UnavailableDiagnosticSnapshotProvider {
    fn document_diagnostics(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        DiagnosticSnapshotOutcome::Failed {
            source_generation: None,
            failure_class: "provider-unavailable".to_owned(),
        }
    }

    fn request_document_refresh(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _overlay: Option<&OverlaySnapshot>,
        _source_generation: Option<u64>,
    ) -> DiagnosticRefreshAdmission {
        DiagnosticRefreshAdmission::Rejected {
            failure_class: "provider-unavailable".to_owned(),
        }
    }
}

/// Routes to a ready analyzer and otherwise to a separately admitted
/// graph-backed provider. A partial analyzer answer remains partial; it is not
/// merged with a fallback into a misleading complete result.
pub struct AnalyzerSemanticAdapter<U, G> {
    state: AnalyzerState,
    upstream: U,
    graph: G,
}

impl<U, G> AnalyzerSemanticAdapter<U, G> {
    pub fn new(state: AnalyzerState, upstream: U, graph: G) -> Self {
        Self {
            state,
            upstream,
            graph,
        }
    }

    pub fn state(&self) -> AnalyzerState {
        self.state
    }

    fn route<T>(
        &self,
        upstream: impl FnOnce(&U) -> SemanticProviderOutcome<T>,
        graph: impl FnOnce(&G) -> SemanticProviderOutcome<T>,
    ) -> SemanticProviderOutcome<T> {
        if self.state == AnalyzerState::Ready {
            match upstream(&self.upstream) {
                SemanticProviderOutcome::Unavailable => graph(&self.graph),
                result => result,
            }
        } else {
            graph(&self.graph)
        }
    }
}

impl<U, G> SemanticProviderPort for AnalyzerSemanticAdapter<U, G>
where
    U: SemanticProviderPort,
    G: SemanticProviderPort,
{
    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        self.route(
            |provider| provider.request(root, request_id, request),
            |provider| provider.request(root, request_id, request),
        )
    }

    fn declaration(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        self.route(
            |provider| provider.declaration(root, document_uri, position),
            |provider| provider.declaration(root, document_uri, position),
        )
    }

    fn definition(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        self.route(
            |provider| provider.definition(root, document_uri, position),
            |provider| provider.definition(root, document_uri, position),
        )
    }

    fn type_definition(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        self.route(
            |provider| provider.type_definition(root, document_uri, position),
            |provider| provider.type_definition(root, document_uri, position),
        )
    }

    fn implementation(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        self.route(
            |provider| provider.implementation(root, document_uri, position),
            |provider| provider.implementation(root, document_uri, position),
        )
    }

    fn references(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        self.route(
            |provider| provider.references(root, document_uri, position),
            |provider| provider.references(root, document_uri, position),
        )
    }

    fn hover(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Option<Hover>> {
        self.route(
            |provider| provider.hover(root, document_uri, position),
            |provider| provider.hover(root, document_uri, position),
        )
    }

    fn document_symbols(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
    ) -> SemanticProviderOutcome<Vec<DocumentSymbol>> {
        self.route(
            |provider| provider.document_symbols(root, document_uri),
            |provider| provider.document_symbols(root, document_uri),
        )
    }

    fn workspace_symbols(
        &self,
        root: &AdmittedRoot,
        query: &str,
    ) -> SemanticProviderOutcome<Vec<WorkspaceSymbol>> {
        self.route(
            |provider| provider.workspace_symbols(root, query),
            |provider| provider.workspace_symbols(root, query),
        )
    }

    fn prepare_call_hierarchy(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<CallHierarchyItem>> {
        self.route(
            |provider| provider.prepare_call_hierarchy(root, document_uri, position),
            |provider| provider.prepare_call_hierarchy(root, document_uri, position),
        )
    }

    fn incoming_calls(
        &self,
        root: &AdmittedRoot,
        item: &CallHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<IncomingCall>> {
        self.route(
            |provider| provider.incoming_calls(root, item),
            |provider| provider.incoming_calls(root, item),
        )
    }

    fn outgoing_calls(
        &self,
        root: &AdmittedRoot,
        item: &CallHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<OutgoingCall>> {
        self.route(
            |provider| provider.outgoing_calls(root, item),
            |provider| provider.outgoing_calls(root, item),
        )
    }

    fn signature_help(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Option<SignatureHelp>> {
        self.route(
            |provider| provider.signature_help(root, document_uri, position),
            |provider| provider.signature_help(root, document_uri, position),
        )
    }

    fn prepare_type_hierarchy(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<TypeHierarchyItem>> {
        self.route(
            |provider| provider.prepare_type_hierarchy(root, document_uri, position),
            |provider| provider.prepare_type_hierarchy(root, document_uri, position),
        )
    }

    fn type_hierarchy_supertypes(
        &self,
        root: &AdmittedRoot,
        item: &TypeHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<TypeHierarchyItem>> {
        self.route(
            |provider| provider.type_hierarchy_supertypes(root, item),
            |provider| provider.type_hierarchy_supertypes(root, item),
        )
    }

    fn type_hierarchy_subtypes(
        &self,
        root: &AdmittedRoot,
        item: &TypeHierarchyItem,
    ) -> SemanticProviderOutcome<Vec<TypeHierarchyItem>> {
        self.route(
            |provider| provider.type_hierarchy_subtypes(root, item),
            |provider| provider.type_hierarchy_subtypes(root, item),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::LspRange;

    struct Provider(bool);

    impl SemanticProviderPort for Provider {
        fn definition(
            &self,
            _root: &AdmittedRoot,
            uri: &str,
            _position: LspPosition,
        ) -> SemanticProviderOutcome<Vec<LspLocation>> {
            if self.0 {
                SemanticProviderOutcome::Complete(vec![LspLocation {
                    uri: uri.into(),
                    range: LspRange {
                        start: LspPosition {
                            line: 0,
                            character: 0,
                        },
                        end: LspPosition {
                            line: 0,
                            character: 0,
                        },
                    },
                }])
            } else {
                SemanticProviderOutcome::Unavailable
            }
        }
    }

    #[test]
    fn supervisor_exhausts_restart_budget_without_spawning_a_fallback_process() {
        let mut supervisor = AnalyzerSupervisor::default();
        for attempt in 1..=MAX_ANALYZER_RESTARTS {
            supervisor.apply(AnalyzerEvent::StartRequested).unwrap();
            let state = supervisor.apply(AnalyzerEvent::StartupFailed).unwrap();
            if attempt == MAX_ANALYZER_RESTARTS {
                assert_eq!(state, AnalyzerState::Exhausted);
            } else {
                assert_eq!(state, AnalyzerState::RestartBackoff);
            }
        }
    }

    #[test]
    fn semantic_adapter_uses_graph_only_when_analyzer_is_not_truthfully_ready() {
        let root = AdmittedRoot::new("file:///root");
        let position = LspPosition {
            line: 0,
            character: 0,
        };
        let fallback = AnalyzerSemanticAdapter::new(
            AnalyzerState::RestartBackoff,
            Provider(false),
            Provider(true),
        );
        assert!(matches!(
            fallback.definition(&root, "file:///root/a.rs", position),
            SemanticProviderOutcome::Complete(locations) if locations.len() == 1
        ));

        let unavailable =
            AnalyzerSemanticAdapter::new(AnalyzerState::Ready, Provider(false), Provider(false));
        assert!(matches!(
            unavailable.definition(&root, "file:///root/a.rs", position),
            SemanticProviderOutcome::Unavailable
        ));
    }
}

//! Daemon composition for store-free LSP provider/session factories.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tokio::runtime::Handle;
use tracedecay_lsp::{
    AdmittedRoot, AnalyzerCancellationAdapter, AnalyzerCancellationPort,
    CanonicalContextProjectionAuthority, CanonicalDiagnosticSnapshotAuthority,
    ContextExpansionOutcome, ContextExpansionRequest, ContextProjectionAdapter,
    ContextProjectionChange, ContextProjectionOutcome, ContextProjectionPort,
    ContextProjectionRegistration, ContextProjectionRequest, DaemonLspProviderBundle,
    DaemonLspRuntimeSession, DiagnosticRefreshAdmission, DiagnosticSnapshotAdapter,
    DiagnosticSnapshotOutcome, DiagnosticSnapshotPort, FeedbackCycleAdapter, FeedbackCyclePort,
    FeedbackCycleRequest, FeedbackCycleResponse, FeedbackCycleRuntimePort, GatewayCapabilities,
    LspAnalyzerCancellationAuthority, LspRequestId, OverlaySnapshot, SemanticProviderOutcome,
    SemanticProviderPort, SemanticRequest, SemanticResponse, UpstreamCapabilities,
};

use super::runtime_adapters::runtime_spawner;

/// Cloneable daemon registration for one admitted project.
#[derive(Clone)]
pub struct DaemonLspSessionFactory {
    runtime: Handle,
    feedback: Arc<FeedbackCycleAdapter>,
    semantics: Arc<dyn SemanticProviderPort + Send + Sync>,
    diagnostics: Arc<dyn CanonicalDiagnosticSnapshotAuthority>,
    cancellation: Arc<dyn LspAnalyzerCancellationAuthority>,
    context: Arc<dyn CanonicalContextProjectionAuthority>,
    gateway_capabilities: GatewayCapabilities,
    upstream_capabilities: UpstreamCapabilities,
}

impl DaemonLspSessionFactory {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime: Handle,
        feedback: Arc<dyn FeedbackCycleRuntimePort>,
        semantics: Arc<dyn SemanticProviderPort + Send + Sync>,
        diagnostics: Arc<dyn CanonicalDiagnosticSnapshotAuthority>,
        cancellation: Arc<dyn LspAnalyzerCancellationAuthority>,
        context: Arc<dyn CanonicalContextProjectionAuthority>,
        gateway_capabilities: GatewayCapabilities,
        upstream_capabilities: UpstreamCapabilities,
    ) -> Self {
        let feedback = Arc::new(FeedbackCycleAdapter::new(
            runtime_spawner(runtime.clone()),
            feedback,
        ));
        Self {
            runtime,
            feedback,
            semantics,
            diagnostics,
            cancellation,
            context,
            gateway_capabilities,
            upstream_capabilities,
        }
    }

    /// Creates isolated per-client correlation adapters around shared daemon
    /// authorities.
    pub fn provider_bundle(&self) -> DaemonLspProviderBundle {
        DaemonLspProviderBundle::from_shared(
            self.feedback.clone() as Arc<dyn FeedbackCyclePort + Send + Sync>,
            self.semantics.clone(),
            Arc::new(DiagnosticSnapshotAdapter::new(
                runtime_spawner(self.runtime.clone()),
                self.diagnostics.clone(),
            )) as Arc<dyn DiagnosticSnapshotPort + Send + Sync>,
            Arc::new(AnalyzerCancellationAdapter::new(self.cancellation.clone()))
                as Arc<dyn AnalyzerCancellationPort + Send + Sync>,
            Arc::new(ContextProjectionAdapter::new(
                runtime_spawner(self.runtime.clone()),
                self.context.clone(),
            )) as Arc<dyn ContextProjectionPort + Send + Sync>,
            self.gateway_capabilities.clone(),
            self.upstream_capabilities.clone(),
        )
    }

    pub fn open_session(&self, root: AdmittedRoot) -> DaemonLspRuntimeSession {
        self.provider_bundle().into_session(root)
    }

    pub fn open_workspace_session(
        &self,
        workspace: AuthorizedLspWorkspace,
    ) -> DaemonLspRuntimeSession {
        self.provider_bundle().into_workspace_session(workspace)
    }

    pub fn open_federated_workspace_session(
        workspace: AuthorizedLspWorkspace,
        factories: Vec<(AdmittedRoot, Arc<Self>)>,
    ) -> Option<DaemonLspRuntimeSession> {
        if factories.len() != workspace.roots().len() {
            return None;
        }
        let mut feedback = BTreeMap::new();
        let mut semantics = BTreeMap::new();
        let mut diagnostics = BTreeMap::new();
        let mut cancellation = BTreeMap::new();
        let mut context = BTreeMap::new();
        let mut gateway_capabilities = None;
        let mut upstream_capabilities = None;
        for (root, factory) in factories {
            if !workspace.roots().contains(&root) || feedback.contains_key(root.uri()) {
                return None;
            }
            let root_uri = root.uri().to_owned();
            feedback.insert(
                root_uri.clone(),
                factory.feedback.clone() as Arc<dyn FeedbackCyclePort + Send + Sync>,
            );
            semantics.insert(root_uri.clone(), factory.semantics.clone());
            diagnostics.insert(
                root_uri.clone(),
                Arc::new(DiagnosticSnapshotAdapter::new(
                    runtime_spawner(factory.runtime.clone()),
                    factory.diagnostics.clone(),
                )) as Arc<dyn DiagnosticSnapshotPort + Send + Sync>,
            );
            cancellation.insert(
                root_uri.clone(),
                Arc::new(AnalyzerCancellationAdapter::new(
                    factory.cancellation.clone(),
                )) as Arc<dyn AnalyzerCancellationPort + Send + Sync>,
            );
            context.insert(
                root_uri,
                Arc::new(ContextProjectionAdapter::new(
                    runtime_spawner(factory.runtime.clone()),
                    factory.context.clone(),
                )) as Arc<dyn ContextProjectionPort + Send + Sync>,
            );
            gateway_capabilities.get_or_insert_with(|| factory.gateway_capabilities.clone());
            upstream_capabilities.get_or_insert_with(|| factory.upstream_capabilities.clone());
        }
        let bundle = DaemonLspProviderBundle::from_shared(
            Arc::new(FederatedFeedback { roots: feedback }),
            Arc::new(FederatedSemantics { roots: semantics }),
            Arc::new(FederatedDiagnostics { roots: diagnostics }),
            Arc::new(FederatedCancellation {
                roots: cancellation,
            }),
            Arc::new(FederatedContext { roots: context }),
            gateway_capabilities?,
            upstream_capabilities?,
        );
        Some(bundle.into_workspace_session(workspace))
    }
}

struct FederatedFeedback {
    roots: BTreeMap<String, Arc<dyn FeedbackCyclePort + Send + Sync>>,
}

impl FeedbackCyclePort for FederatedFeedback {
    fn request_feedback_cycle(&self, request: FeedbackCycleRequest) -> FeedbackCycleResponse {
        self.roots.get(&request.root_uri).map_or_else(
            || FeedbackCycleResponse::Rejected {
                reason: "root-not-authorized".to_owned(),
            },
            |port| port.request_feedback_cycle(request),
        )
    }
}

struct FederatedSemantics {
    roots: BTreeMap<String, Arc<dyn SemanticProviderPort + Send + Sync>>,
}

impl SemanticProviderPort for FederatedSemantics {
    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        self.roots
            .get(root.uri())
            .map_or(SemanticProviderOutcome::Unavailable, |port| {
                port.request(root, request_id, request)
            })
    }
}

struct FederatedDiagnostics {
    roots: BTreeMap<String, Arc<dyn DiagnosticSnapshotPort + Send + Sync>>,
}

impl DiagnosticSnapshotPort for FederatedDiagnostics {
    fn document_diagnostics(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        self.roots.get(root.uri()).map_or(
            DiagnosticSnapshotOutcome::Failed {
                source_generation: None,
                failure_class: "root-not-authorized".to_owned(),
            },
            |port| port.document_diagnostics(root, document_uri, overlay),
        )
    }

    fn request_document_refresh(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
        source_generation: Option<u64>,
    ) -> DiagnosticRefreshAdmission {
        self.roots.get(root.uri()).map_or(
            DiagnosticRefreshAdmission::Rejected {
                failure_class: "root-not-authorized".to_owned(),
            },
            |port| port.request_document_refresh(root, document_uri, overlay, source_generation),
        )
    }
}

struct FederatedCancellation {
    roots: BTreeMap<String, Arc<dyn AnalyzerCancellationPort + Send + Sync>>,
}

impl AnalyzerCancellationPort for FederatedCancellation {
    fn cancel_upstream(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.roots
            .get(root.uri())
            .is_some_and(|port| port.cancel_upstream(root, request_id))
    }
}

struct FederatedContext {
    roots: BTreeMap<String, Arc<dyn ContextProjectionPort + Send + Sync>>,
}

impl ContextProjectionPort for FederatedContext {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        let mut registrations = self
            .roots
            .values()
            .map(|port| port.registrations().into_iter().collect::<BTreeSet<_>>());
        let Some(mut common) = registrations.next() else {
            return Vec::new();
        };
        for root in registrations {
            common.retain(|registration| root.contains(registration));
        }
        common.into_iter().collect()
    }

    fn snapshot(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &ContextProjectionRequest,
    ) -> ContextProjectionOutcome {
        self.roots
            .get(root.uri())
            .map_or(ContextProjectionOutcome::Denied, |port| {
                port.snapshot(root, request_id, request)
            })
    }

    fn poll_snapshot(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
    ) -> Option<ContextProjectionOutcome> {
        self.roots
            .get(root.uri())
            .and_then(|port| port.poll_snapshot(root, request_id))
    }

    fn expand(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &ContextExpansionRequest,
    ) -> ContextExpansionOutcome {
        self.roots
            .get(root.uri())
            .map_or(ContextExpansionOutcome::Denied, |port| {
                port.expand(root, request_id, request)
            })
    }

    fn poll_expansion(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
    ) -> Option<ContextExpansionOutcome> {
        self.roots
            .get(root.uri())
            .and_then(|port| port.poll_expansion(root, request_id))
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.roots
            .get(root.uri())
            .is_some_and(|port| port.cancel_request(root, request_id))
    }

    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        self.roots
            .get(root.uri())
            .map_or_else(Vec::new, |port| port.poll_changes(root, subscriptions))
    }

    fn update_subscriptions(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) {
        if let Some(port) = self.roots.get(root.uri()) {
            port.update_subscriptions(root, subscriptions);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_lsp::LspPosition;

    struct RootSemantic {
        root_uri: &'static str,
    }

    impl SemanticProviderPort for RootSemantic {
        fn request(
            &self,
            root: &AdmittedRoot,
            _request_id: &LspRequestId,
            _request: &SemanticRequest,
        ) -> SemanticProviderOutcome<SemanticResponse> {
            if root.uri() == self.root_uri {
                SemanticProviderOutcome::Complete(SemanticResponse::Hover(None))
            } else {
                SemanticProviderOutcome::Unavailable
            }
        }
    }

    #[test]
    fn federated_semantics_routes_secondary_root_to_its_exact_provider() {
        let primary = AdmittedRoot::new("file:///primary");
        let secondary = AdmittedRoot::new("file:///secondary");
        let provider = FederatedSemantics {
            roots: BTreeMap::from([
                (
                    primary.uri().to_owned(),
                    Arc::new(RootSemantic {
                        root_uri: "file:///primary",
                    }) as Arc<dyn SemanticProviderPort + Send + Sync>,
                ),
                (
                    secondary.uri().to_owned(),
                    Arc::new(RootSemantic {
                        root_uri: "file:///secondary",
                    }) as Arc<dyn SemanticProviderPort + Send + Sync>,
                ),
            ]),
        };
        let request = SemanticRequest::Hover {
            document_uri: "file:///secondary/lib.rs".to_owned(),
            position: LspPosition {
                line: 0,
                character: 0,
            },
        };

        assert!(matches!(
            provider.request(
                &secondary,
                &LspRequestId::String("request.secondary".to_owned()),
                &request,
            ),
            SemanticProviderOutcome::Complete(SemanticResponse::Hover(None))
        ));
    }
}

//! Daemon composition for store-free LSP provider/session factories.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::runtime::Handle;
use tracedecay_application::NativeIntegrationStatusProjectionV1;
use tracedecay_lsp::{
    AdmittedRoot, AnalyzerCancellationAdapter, AnalyzerCancellationPort, AuthorizedLspWorkspace,
    CanonicalContextProjectionAuthority, CanonicalDiagnosticSnapshotAuthority,
    ContextExpansionOutcome, ContextExpansionRequest, ContextProjectionAdapter,
    ContextProjectionChange, ContextProjectionOutcome, ContextProjectionPort,
    ContextProjectionRegistration, ContextProjectionRequest, DaemonLspProviderBundle,
    DaemonLspRuntimeSession, DiagnosticRefreshAdmission, DiagnosticSnapshotAdapter,
    DiagnosticSnapshotOutcome, DiagnosticSnapshotPort, FeedbackCycleAdapter, FeedbackCyclePort,
    FeedbackCycleRequest, FeedbackCycleResponse, FeedbackCycleRuntimePort, GatewayCapabilities,
    LspAnalyzerCancellationAuthority, LspRequestId, LspRuntimeFailure, LspRuntimeFuture,
    NativeIntegrationStatusPort, OverlaySnapshot, SemanticProviderOutcome, SemanticProviderPort,
    SemanticRequest, SemanticResponse, UpstreamCapabilities, WorkspaceDiagnosticSnapshotOutcome,
};

use super::runtime_adapters::runtime_spawner;

/// Supplies exact upstream semantic capabilities when an LSP session is
/// actually opened.
///
/// Production implementations retain one analyzer client per route and obtain
/// these facts from that client's standard initialize response. The factory
/// does not cache a second copy of that response.
pub trait UpstreamCapabilityInitializationAuthority: Send + Sync {
    fn initialize_upstream_capabilities(
        &self,
    ) -> LspRuntimeFuture<std::result::Result<UpstreamCapabilities, LspRuntimeFailure>>;
}

struct StaticUpstreamCapabilities {
    capabilities: UpstreamCapabilities,
}

impl UpstreamCapabilityInitializationAuthority for StaticUpstreamCapabilities {
    fn initialize_upstream_capabilities(
        &self,
    ) -> LspRuntimeFuture<std::result::Result<UpstreamCapabilities, LspRuntimeFailure>> {
        let capabilities = self.capabilities.clone();
        Box::pin(async move { Ok(capabilities) })
    }
}

/// Cloneable daemon registration for one admitted project.
#[derive(Clone)]
pub struct DaemonLspSessionFactory {
    runtime: Handle,
    feedback: Arc<FeedbackCycleAdapter>,
    semantics: Arc<dyn SemanticProviderPort + Send + Sync>,
    diagnostics: Arc<dyn CanonicalDiagnosticSnapshotAuthority>,
    cancellation: Arc<dyn LspAnalyzerCancellationAuthority>,
    context: Arc<dyn CanonicalContextProjectionAuthority>,
    native_integration_status: Option<Arc<dyn NativeIntegrationStatusPort>>,
    gateway_capabilities: GatewayCapabilities,
    upstream_capabilities: UpstreamCapabilities,
    upstream_capability_initializer: Arc<dyn UpstreamCapabilityInitializationAuthority>,
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
            native_integration_status: None,
            gateway_capabilities,
            upstream_capability_initializer: Arc::new(StaticUpstreamCapabilities {
                capabilities: upstream_capabilities.clone(),
            }),
            upstream_capabilities,
        }
    }

    /// Mounts the daemon-owned native-integration status read. Sessions opened
    /// from this factory forward observed transaction statuses to their client
    /// as read-only notifications.
    #[must_use]
    pub fn with_native_integration_status_port(
        mut self,
        port: Arc<dyn NativeIntegrationStatusPort>,
    ) -> Self {
        self.native_integration_status = Some(port);
        self
    }

    /// Replaces the static test capability source with the production
    /// initializer backed by the shared analyzer client.
    pub fn with_upstream_capability_initializer(
        mut self,
        upstream_capability_initializer: Arc<dyn UpstreamCapabilityInitializationAuthority>,
    ) -> Self {
        self.upstream_capability_initializer = upstream_capability_initializer;
        self
    }

    /// Creates isolated per-client correlation adapters around shared daemon
    /// authorities.
    pub fn provider_bundle(&self) -> DaemonLspProviderBundle {
        self.provider_bundle_with_upstream_capabilities(self.upstream_capabilities.clone())
    }

    fn provider_bundle_with_upstream_capabilities(
        &self,
        upstream_capabilities: UpstreamCapabilities,
    ) -> DaemonLspProviderBundle {
        let gateway_capabilities = self.current_gateway_capabilities();
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
            gateway_capabilities,
            upstream_capabilities,
        )
    }

    pub async fn initialize_upstream_capabilities(
        &self,
    ) -> std::result::Result<UpstreamCapabilities, LspRuntimeFailure> {
        self.upstream_capability_initializer
            .initialize_upstream_capabilities()
            .await
    }

    fn current_gateway_capabilities(&self) -> GatewayCapabilities {
        self.gateway_capabilities.clone()
    }

    pub fn open_session(&self, root: AdmittedRoot) -> DaemonLspRuntimeSession {
        self.attach_native_integration_status(self.provider_bundle().into_session(root))
    }

    pub async fn open_workspace_session(
        &self,
        workspace: AuthorizedLspWorkspace,
    ) -> std::result::Result<DaemonLspRuntimeSession, LspRuntimeFailure> {
        let upstream_capabilities = self.initialize_upstream_capabilities().await?;
        Ok(self.attach_native_integration_status(
            self.provider_bundle_with_upstream_capabilities(upstream_capabilities)
                .into_workspace_session(workspace),
        ))
    }

    fn attach_native_integration_status(
        &self,
        session: DaemonLspRuntimeSession,
    ) -> DaemonLspRuntimeSession {
        match self.native_integration_status.as_ref() {
            Some(port) => session.with_native_integration_status_port(Arc::clone(port)),
            None => session,
        }
    }

    pub async fn open_federated_workspace_session(
        workspace: AuthorizedLspWorkspace,
        factories: Vec<(AdmittedRoot, Arc<Self>)>,
    ) -> std::result::Result<Option<DaemonLspRuntimeSession>, LspRuntimeFailure> {
        let mut initialized = Vec::with_capacity(factories.len());
        for (root, factory) in factories {
            let upstream_capabilities = factory.initialize_upstream_capabilities().await?;
            initialized.push((root, factory, upstream_capabilities));
        }
        Ok(
            Self::open_federated_workspace_session_with_upstream_capabilities(
                workspace,
                initialized,
            ),
        )
    }

    fn open_federated_workspace_session_with_upstream_capabilities(
        workspace: AuthorizedLspWorkspace,
        factories: Vec<(AdmittedRoot, Arc<Self>, UpstreamCapabilities)>,
    ) -> Option<DaemonLspRuntimeSession> {
        if factories.len() != workspace.roots().len() {
            return None;
        }
        let mut feedback = BTreeMap::new();
        let mut semantics = BTreeMap::new();
        let mut diagnostics = BTreeMap::new();
        let mut cancellation = BTreeMap::new();
        let mut context = BTreeMap::new();
        let mut native_integration_status = Vec::new();
        let mut gateway_capabilities: Option<GatewayCapabilities> = None;
        let mut upstream_capabilities: Option<UpstreamCapabilities> = None;
        for (root, factory, factory_upstream_capabilities) in factories {
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
            if let Some(port) = factory.native_integration_status.as_ref() {
                native_integration_status.push(Arc::clone(port));
            }
            let current_gateway_capabilities = factory.current_gateway_capabilities();
            if let Some(capabilities) = gateway_capabilities.as_mut() {
                capabilities.supports_publish_diagnostics &=
                    current_gateway_capabilities.supports_publish_diagnostics;
                capabilities.supports_document_diagnostics &=
                    current_gateway_capabilities.supports_document_diagnostics;
                capabilities.supports_managed_diagnostics &=
                    current_gateway_capabilities.supports_managed_diagnostics;
                capabilities.supports_workspace_folders &=
                    current_gateway_capabilities.supports_workspace_folders;
                capabilities.supports_workspace_diagnostics &=
                    current_gateway_capabilities.supports_workspace_diagnostics;
                capabilities.supports_context_expansion &=
                    current_gateway_capabilities.supports_context_expansion;
                capabilities.semantic.retain(|capability| {
                    current_gateway_capabilities.semantic.contains(capability)
                });
                capabilities.context_projections.retain(|kind, revision| {
                    current_gateway_capabilities.context_projections.get(kind) == Some(revision)
                });
            } else {
                gateway_capabilities = Some(current_gateway_capabilities);
            }
            if let Some(capabilities) = upstream_capabilities.as_mut() {
                capabilities.supports_diagnostics &=
                    factory_upstream_capabilities.supports_diagnostics;
                capabilities.semantic.retain(|capability| {
                    factory_upstream_capabilities.semantic.contains(capability)
                });
            } else {
                upstream_capabilities = Some(factory_upstream_capabilities);
            }
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
        let session = bundle.into_workspace_session(workspace);
        if native_integration_status.is_empty() {
            return Some(session);
        }
        Some(session.with_native_integration_status_port(Arc::new(
            FederatedNativeIntegrationStatus {
                roots: native_integration_status,
                next_root: AtomicUsize::new(0),
            },
        )))
    }
}

/// Merges the participating roots' status reads under one poll bound. Each
/// projection already names its exact repository and transaction identity, so
/// merging discloses nothing a single-root session would not see.
struct FederatedNativeIntegrationStatus {
    roots: Vec<Arc<dyn NativeIntegrationStatusPort>>,
    next_root: AtomicUsize,
}

impl NativeIntegrationStatusPort for FederatedNativeIntegrationStatus {
    fn poll_status(&self, maximum: usize) -> Vec<NativeIntegrationStatusProjectionV1> {
        if maximum == 0 || self.roots.is_empty() {
            return Vec::new();
        }
        let mut root_statuses = self
            .roots
            .iter()
            .map(|root| root.poll_status(maximum).into_iter())
            .collect::<Vec<_>>();
        let root_count = root_statuses.len();
        let first_root = self.next_root.fetch_add(1, Ordering::Relaxed) % root_count;
        let mut merged = Vec::with_capacity(maximum);
        loop {
            let mut progressed = false;
            for offset in 0..root_count {
                let root_index = (first_root + offset) % root_count;
                if let Some(status) = root_statuses[root_index].next() {
                    merged.push(status);
                    progressed = true;
                    if merged.len() == maximum {
                        return merged;
                    }
                }
            }
            if !progressed {
                break;
            }
        }
        merged
    }
}

#[cfg(test)]
mod native_integration_status_tests {
    use std::sync::Arc;

    use tracedecay_application::NativeIntegrationStatusProjectionV1;
    use tracedecay_domain::{
        ManifestDigest, NativeIntegrationPhaseV1, NativeIntegrationPreviewId,
        NativeIntegrationTransactionId, RefId, RepositoryId, UtcMicros,
    };
    use tracedecay_lsp::NativeIntegrationStatusPort;

    use super::FederatedNativeIntegrationStatus;

    struct StaticStatuses(Vec<NativeIntegrationStatusProjectionV1>);

    impl NativeIntegrationStatusPort for StaticStatuses {
        fn poll_status(&self, maximum: usize) -> Vec<NativeIntegrationStatusProjectionV1> {
            self.0.iter().take(maximum).cloned().collect()
        }
    }

    fn status(
        repository: &str,
        transaction: &str,
        updated_at: i64,
    ) -> NativeIntegrationStatusProjectionV1 {
        NativeIntegrationStatusProjectionV1 {
            transaction_id: NativeIntegrationTransactionId::new(transaction).expect("transaction"),
            preview_id: NativeIntegrationPreviewId::new(format!("preview.{transaction}"))
                .expect("preview"),
            preview_digest: ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("digest"),
            repository_id: RepositoryId::new(repository).expect("repository"),
            destination_ref: RefId::new("refs/heads/main").expect("ref"),
            phase: NativeIntegrationPhaseV1::Prepared,
            phase_revision: 1,
            cancellation_requested: false,
            terminal_outcome: None,
            updated_at: UtcMicros(updated_at),
        }
    }

    #[test]
    fn bounded_federated_poll_represents_each_root_before_reusing_one_root() {
        let first: Arc<dyn NativeIntegrationStatusPort> = Arc::new(StaticStatuses(vec![
            status("repository.first", "transaction.first-a", 3),
            status("repository.first", "transaction.first-b", 2),
        ]));
        let second: Arc<dyn NativeIntegrationStatusPort> = Arc::new(StaticStatuses(vec![status(
            "repository.second",
            "transaction.second",
            1,
        )]));
        let federated = FederatedNativeIntegrationStatus {
            roots: vec![first, second],
            next_root: std::sync::atomic::AtomicUsize::new(0),
        };

        let statuses = federated.poll_status(2);

        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].repository_id.as_str(), "repository.first");
        assert_eq!(statuses[1].repository_id.as_str(), "repository.second");
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

    fn supports_workspace_diagnostics(&self) -> bool {
        !self.roots.is_empty()
            && self
                .roots
                .values()
                .all(|port| port.supports_workspace_diagnostics())
    }

    fn workspace_diagnostics(
        &self,
        workspace: &AuthorizedLspWorkspace,
        root: &AdmittedRoot,
        overlays: &[OverlaySnapshot],
    ) -> WorkspaceDiagnosticSnapshotOutcome {
        self.roots.get(root.uri()).map_or(
            WorkspaceDiagnosticSnapshotOutcome::Failed {
                code_generation_id: None,
                failure_class: "root-not-authorized".to_owned(),
            },
            |port| port.workspace_diagnostics(workspace, root, overlays),
        )
    }

    fn request_workspace_refresh(
        &self,
        workspace: &AuthorizedLspWorkspace,
        root: &AdmittedRoot,
        overlays: &[OverlaySnapshot],
    ) -> DiagnosticRefreshAdmission {
        self.roots.get(root.uri()).map_or(
            DiagnosticRefreshAdmission::Rejected {
                failure_class: "root-not-authorized".to_owned(),
            },
            |port| port.request_workspace_refresh(workspace, root, overlays),
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
        maximum: usize,
    ) -> Vec<ContextProjectionChange> {
        self.roots.get(root.uri()).map_or_else(Vec::new, |port| {
            port.poll_changes(root, subscriptions, maximum)
        })
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use serde_json::{Value, json};
    use tracedecay_lsp::{
        CanonicalDiagnosticRefreshRequest, ContextProjectionRequest, FeedbackCycleRequest,
        LspPosition, LspRuntimeFailure, LspRuntimeFuture, UnavailableSemanticProvider,
    };

    struct RuntimeFeedback;

    impl FeedbackCycleRuntimePort for RuntimeFeedback {
        fn execute(
            &self,
            _request: FeedbackCycleRequest,
        ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct RuntimeCancellation;

    impl LspAnalyzerCancellationAuthority for RuntimeCancellation {
        fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
            false
        }
    }

    struct RuntimeContext;

    impl CanonicalContextProjectionAuthority for RuntimeContext {
        fn registrations(&self) -> Vec<ContextProjectionRegistration> {
            Vec::new()
        }

        fn snapshot(
            &self,
            _root: AdmittedRoot,
            _request_id: LspRequestId,
            _request: ContextProjectionRequest,
        ) -> LspRuntimeFuture<ContextProjectionOutcome> {
            Box::pin(async { ContextProjectionOutcome::Unsupported })
        }
    }

    struct ExactUpstreamCapabilities(UpstreamCapabilities);

    impl UpstreamCapabilityInitializationAuthority for ExactUpstreamCapabilities {
        fn initialize_upstream_capabilities(
            &self,
        ) -> LspRuntimeFuture<std::result::Result<UpstreamCapabilities, LspRuntimeFailure>>
        {
            let capabilities = self.0.clone();
            Box::pin(async move { Ok(capabilities) })
        }
    }

    struct FailingUpstreamCapabilities;

    impl UpstreamCapabilityInitializationAuthority for FailingUpstreamCapabilities {
        fn initialize_upstream_capabilities(
            &self,
        ) -> LspRuntimeFuture<std::result::Result<UpstreamCapabilities, LspRuntimeFailure>>
        {
            Box::pin(async { Err(LspRuntimeFailure::new("upstream-initialize-failed")) })
        }
    }

    fn factory_with_upstream_initializer(
        upstream_capability_initializer: Arc<dyn UpstreamCapabilityInitializationAuthority>,
    ) -> DaemonLspSessionFactory {
        DaemonLspSessionFactory::new(
            Handle::current(),
            Arc::new(RuntimeFeedback),
            Arc::new(UnavailableSemanticProvider),
            Arc::new(ToggleWorkspaceDiagnostics(Arc::new(AtomicBool::new(false)))),
            Arc::new(RuntimeCancellation),
            Arc::new(RuntimeContext),
            GatewayCapabilities::default(),
            UpstreamCapabilities::default(),
        )
        .with_upstream_capability_initializer(upstream_capability_initializer)
    }

    struct ToggleWorkspaceDiagnostics(Arc<AtomicBool>);

    impl CanonicalDiagnosticSnapshotAuthority for ToggleWorkspaceDiagnostics {
        fn refresh(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<tracedecay_lsp::GenerationDiagnostics, LspRuntimeFailure>>
        {
            Box::pin(async { Err(LspRuntimeFailure::new("diagnostics-unavailable")) })
        }

        fn supports_workspace_diagnostics(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
    }

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

    struct RootWorkspaceDiagnostics;

    impl DiagnosticSnapshotPort for RootWorkspaceDiagnostics {
        fn document_diagnostics(
            &self,
            _root: &AdmittedRoot,
            _document_uri: &str,
            _overlay: Option<&OverlaySnapshot>,
        ) -> DiagnosticSnapshotOutcome {
            DiagnosticSnapshotOutcome::Failed {
                source_generation: None,
                failure_class: "document-diagnostics-unused".to_owned(),
            }
        }

        fn supports_workspace_diagnostics(&self) -> bool {
            true
        }

        fn workspace_diagnostics(
            &self,
            _workspace: &AuthorizedLspWorkspace,
            root: &AdmittedRoot,
            _overlays: &[OverlaySnapshot],
        ) -> WorkspaceDiagnosticSnapshotOutcome {
            WorkspaceDiagnosticSnapshotOutcome::Partial {
                code_generation_id: None,
                coverage: root.uri().to_owned(),
            }
        }

        fn request_workspace_refresh(
            &self,
            _workspace: &AuthorizedLspWorkspace,
            root: &AdmittedRoot,
            _overlays: &[OverlaySnapshot],
        ) -> DiagnosticRefreshAdmission {
            DiagnosticRefreshAdmission::Rejected {
                failure_class: root.uri().to_owned(),
            }
        }
    }

    #[test]
    fn federated_workspace_diagnostics_route_only_to_the_exact_root_provider() {
        let primary = AdmittedRoot::new("file:///primary");
        let secondary = AdmittedRoot::new("file:///secondary");
        let provider = FederatedDiagnostics {
            roots: BTreeMap::from([
                (
                    primary.uri().to_owned(),
                    Arc::new(RootWorkspaceDiagnostics)
                        as Arc<dyn DiagnosticSnapshotPort + Send + Sync>,
                ),
                (
                    secondary.uri().to_owned(),
                    Arc::new(RootWorkspaceDiagnostics)
                        as Arc<dyn DiagnosticSnapshotPort + Send + Sync>,
                ),
            ]),
        };
        let workspace = AuthorizedLspWorkspace::single(secondary.clone());

        assert!(provider.supports_workspace_diagnostics());
        assert!(matches!(
            provider.workspace_diagnostics(&workspace, &secondary, &[]),
            WorkspaceDiagnosticSnapshotOutcome::Partial { coverage, .. }
                if coverage == secondary.uri()
        ));
        assert!(matches!(
            provider.request_workspace_refresh(&workspace, &secondary, &[]),
            DiagnosticRefreshAdmission::Rejected { failure_class }
                if failure_class == secondary.uri()
        ));
        assert!(matches!(
            provider.workspace_diagnostics(
                &workspace,
                &AdmittedRoot::new("file:///unregistered"),
                &[],
            ),
            WorkspaceDiagnosticSnapshotOutcome::Failed { failure_class, .. }
                if failure_class == "root-not-authorized"
        ));
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

    #[tokio::test]
    async fn initialized_workspace_session_uses_exact_upstream_capabilities() {
        let factory = factory_with_upstream_initializer(Arc::new(ExactUpstreamCapabilities(
            UpstreamCapabilities {
                supports_diagnostics: false,
                semantic: [tracedecay_lsp::SemanticCapability::DocumentSymbol]
                    .into_iter()
                    .collect(),
            },
        )));
        let workspace = AuthorizedLspWorkspace::single(AdmittedRoot::new("file:///workspace"));
        let mut session = factory
            .open_workspace_session(workspace)
            .await
            .expect("exact upstream capability initialization");
        let initialize = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": "file:///workspace",
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": {
                        "documentSymbol": { "dynamicRegistration": false },
                        "hover": { "dynamicRegistration": false },
                    },
                },
            },
        }))
        .expect("serialize initialize");
        session.handle_payload(&initialize, 0);
        let response: Value =
            serde_json::from_slice(&session.drain_outbound()[0]).expect("initialize response");

        assert_eq!(
            response["result"]["capabilities"]["documentSymbolProvider"],
            true
        );
        assert!(response["result"]["capabilities"]["hoverProvider"].is_null());
    }

    #[tokio::test]
    async fn initialized_workspace_session_fails_closed_when_upstream_initialization_fails() {
        let factory = factory_with_upstream_initializer(Arc::new(FailingUpstreamCapabilities));
        let workspace = AuthorizedLspWorkspace::single(AdmittedRoot::new("file:///workspace"));

        let error = match factory.open_workspace_session(workspace).await {
            Ok(_) => panic!("failed upstream initialization must not mint a session actor"),
            Err(error) => error,
        };

        assert_eq!(error.class(), "upstream-initialize-failed");
    }

    #[tokio::test]
    async fn workspace_diagnostics_are_advertised_when_readiness_arrives_after_owner_registration()
    {
        let ready = Arc::new(AtomicBool::new(false));
        let factory = DaemonLspSessionFactory::new(
            Handle::current(),
            Arc::new(RuntimeFeedback),
            Arc::new(UnavailableSemanticProvider),
            Arc::new(ToggleWorkspaceDiagnostics(Arc::clone(&ready))),
            Arc::new(RuntimeCancellation),
            Arc::new(RuntimeContext),
            GatewayCapabilities {
                supports_document_diagnostics: true,
                supports_managed_diagnostics: true,
                supports_workspace_diagnostics: true,
                supports_workspace_folders: true,
                ..GatewayCapabilities::default()
            },
            UpstreamCapabilities::default(),
        );
        let initialize = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": "file:///workspace",
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": { "diagnostic": {} },
                    "workspace": { "diagnostic": { "refreshSupport": true } },
                },
            },
        }))
        .unwrap();
        let mut unavailable_session = factory.open_session(AdmittedRoot::new("file:///workspace"));
        unavailable_session.handle_payload(&initialize, 0);
        let unavailable: Value =
            serde_json::from_slice(&unavailable_session.drain_outbound()[0]).unwrap();
        assert_ne!(
            unavailable["result"]["capabilities"]["diagnosticProvider"]["workspaceDiagnostics"],
            true
        );

        ready.store(true, Ordering::Release);
        let mut ready_session = factory.open_session(AdmittedRoot::new("file:///workspace"));
        ready_session.handle_payload(&initialize, 0);
        let response: Value = serde_json::from_slice(&ready_session.drain_outbound()[0]).unwrap();

        assert_eq!(
            response["result"]["capabilities"]["diagnosticProvider"]["workspaceDiagnostics"],
            true
        );
    }
}

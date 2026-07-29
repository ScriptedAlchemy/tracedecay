//! Concrete assembly of daemon-owned PR12 LSP providers.
//!
//! The factory accepts existing daemon/application service references through
//! the gateway's established ports. It creates no provider registry, cache,
//! analyzer, feedback engine, or context truth of its own.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::runtime::Handle;
use tracedecay_lsp::{
    AdmittedRoot, AnalyzerCancellationPort, ClientCapabilities, ContextProjectionPort,
    DaemonLspProtocolSession, DiagnosticSnapshotPort, FeedbackCyclePort, GatewayCapabilities,
    MAX_CONTEXT_PROJECTION_KINDS, SemanticCapability, SemanticProviderPort, UpstreamCapabilities,
    negotiate_capabilities,
};

use super::runtime_adapters::{
    CanonicalContextProjectionAuthority, CanonicalDiagnosticSnapshotAuthority,
    FeedbackCycleRuntimePort, LspAnalyzerCancellationAuthority, Pr12AnalyzerCancellationAdapter,
    Pr12ContextProjectionAdapter, Pr12DiagnosticSnapshotAdapter, Pr12FeedbackCycleAdapter,
};

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

/// Cloneable daemon registration retained for one admitted project.
///
/// Immutable/concurrent authorities are shared. Every `provider_bundle` call
/// creates new diagnostic/context correlation adapters, so request ids,
/// overlays, publications, and cancellation state never cross LSP clients.
#[derive(Clone)]
pub struct Pr12LspSessionFactory {
    runtime: Handle,
    feedback: Arc<Pr12FeedbackCycleAdapter>,
    semantics: Arc<dyn SemanticProviderPort + Send + Sync>,
    diagnostics: Arc<dyn CanonicalDiagnosticSnapshotAuthority>,
    cancellation: Arc<dyn LspAnalyzerCancellationAuthority>,
    context: Arc<dyn CanonicalContextProjectionAuthority>,
    gateway_capabilities: GatewayCapabilities,
    upstream_capabilities: UpstreamCapabilities,
}

impl Pr12LspSessionFactory {
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
        let feedback = Arc::new(Pr12FeedbackCycleAdapter::new(runtime.clone(), feedback));
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

    /// Creates isolated per-client adapters around shared production owners.
    pub fn provider_bundle(&self) -> DaemonLspProviderBundle {
        DaemonLspProviderBundle::from_shared(
            self.feedback.clone(),
            self.semantics.clone(),
            Arc::new(Pr12DiagnosticSnapshotAdapter::new(
                self.runtime.clone(),
                self.diagnostics.clone(),
            )),
            Arc::new(Pr12AnalyzerCancellationAdapter::new(
                self.cancellation.clone(),
            )),
            Arc::new(Pr12ContextProjectionAdapter::new(
                self.runtime.clone(),
                self.context.clone(),
            )),
            self.gateway_capabilities.clone(),
            self.upstream_capabilities.clone(),
        )
    }

    /// Central daemon call for each authenticated LSP open.
    pub fn open_session(&self, root: AdmittedRoot) -> DaemonLspRuntimeSession {
        self.provider_bundle().into_session(root)
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
            .filter(|registration| {
                registration.kind.is_pr12_supported() && registration.revision > 0
            })
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
        let initial_capabilities = negotiate_capabilities(
            &ClientCapabilities::default(),
            &self.gateway_capabilities,
            &self.upstream_capabilities,
        );
        DaemonLspProtocolSession::from_ports(
            root,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::lsp_gateway::{
        CanonicalDiagnosticRefreshRequest, ContextProjectionOutcome, ContextProjectionRegistration,
        ContextProjectionRequest, FeedbackCycleRequest, GenerationDiagnostics, LspRequestId,
        LspRuntimeFailure, LspRuntimeFuture,
    };

    struct Feedback;

    impl FeedbackCycleRuntimePort for Feedback {
        fn execute(
            &self,
            _request: FeedbackCycleRequest,
        ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct Semantics;
    impl SemanticProviderPort for Semantics {}

    struct Diagnostics;

    impl CanonicalDiagnosticSnapshotAuthority for Diagnostics {
        fn refresh(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>> {
            Box::pin(async {
                Ok(GenerationDiagnostics {
                    generation: 1,
                    upstream: Vec::new(),
                    tracedecay: Vec::new(),
                })
            })
        }
    }

    struct Cancellation;

    impl LspAnalyzerCancellationAuthority for Cancellation {
        fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
            true
        }
    }

    struct Context;

    impl CanonicalContextProjectionAuthority for Context {
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

    #[tokio::test]
    async fn cloned_factory_creates_isolated_session_state() {
        let factory = Pr12LspSessionFactory::new(
            Handle::current(),
            Arc::new(Feedback),
            Arc::new(Semantics),
            Arc::new(Diagnostics),
            Arc::new(Cancellation),
            Arc::new(Context),
            GatewayCapabilities::default(),
            UpstreamCapabilities::default(),
        );
        let clone = factory.clone();
        let first = factory.open_session(AdmittedRoot::new("file:///root"));
        let second = clone.open_session(AdmittedRoot::new("file:///root"));

        assert!(!std::ptr::eq(first.overlays(), second.overlays()));
        assert_eq!(first.root(), second.root());
        assert_eq!(
            first.lifecycle(),
            crate::daemon::lsp_gateway::SessionLifecycle::AwaitingInitialize
        );
        assert_eq!(
            second.lifecycle(),
            crate::daemon::lsp_gateway::SessionLifecycle::AwaitingInitialize
        );
    }
}

//! Daemon composition for store-free LSP provider/session factories.

use std::sync::Arc;

use tokio::runtime::Handle;
use tracedecay_lsp::{
    AdmittedRoot, AnalyzerCancellationAdapter, AnalyzerCancellationPort,
    CanonicalContextProjectionAuthority, CanonicalDiagnosticSnapshotAuthority,
    ContextProjectionAdapter, ContextProjectionPort, DaemonLspProviderBundle,
    DaemonLspRuntimeSession, DiagnosticSnapshotAdapter, DiagnosticSnapshotPort,
    FeedbackCycleAdapter, FeedbackCyclePort, FeedbackCycleRuntimePort, GatewayCapabilities,
    LspAnalyzerCancellationAuthority, SemanticProviderPort, UpstreamCapabilities,
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
}

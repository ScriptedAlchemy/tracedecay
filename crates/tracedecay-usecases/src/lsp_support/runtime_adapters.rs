//! Daemon-specific bindings for the store-free LSP runtime brokers.

use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::AbortHandle;
use tracedecay_lsp::analyzer::broker::{
    CodeDiagnostic, DiagnosticBroker, DiagnosticSeverity as BrokerDiagnosticSeverity,
};
use tracedecay_lsp::analyzer::client::{
    LspDocument, LspSemanticRequest as AnalyzerSemanticRequest, decode_semantic_request,
};
use tracedecay_lsp::{
    AdmittedRoot, CanonicalDiagnosticRefreshRequest, CanonicalDiagnosticSnapshotAuthority,
    CanonicalWorkspaceDiagnosticRefreshRequest, DiagnosticSource, GatewayDiagnostic,
    GenerationDiagnostics, LspAnalyzerCancellationAuthority, LspPosition, LspRange, LspRequestId,
    LspRuntimeFailure, LspRuntimeFuture, LspRuntimeSpawner, LspRuntimeTask,
    LspSemanticOperationOutcome, LspSemanticRequest,
    LspSemanticRequestAuthority as ProtocolSemanticRequestAuthority, ManagedDiagnosticSnapshotPort,
    SemanticProviderAdapter, SemanticProviderOutcome, SemanticProviderPort, SemanticRequest,
    SemanticResponse, WorkspaceDocumentDiagnostics, WorkspaceGenerationDiagnostics,
};

#[derive(Clone)]
struct TokioLspRuntime {
    handle: Handle,
}

struct TokioLspTask {
    abort: AbortHandle,
}

impl LspRuntimeTask for TokioLspTask {
    fn abort(&self) {
        self.abort.abort();
    }
}

impl LspRuntimeSpawner for TokioLspRuntime {
    fn spawn(&self, future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
        Box::new(TokioLspTask {
            abort: self.handle.spawn(future).abort_handle(),
        })
    }
}

pub(crate) fn runtime_spawner(handle: Handle) -> Arc<dyn LspRuntimeSpawner> {
    Arc::new(TokioLspRuntime { handle })
}

/// Reads an admitted document through the daemon's source/overlay authority.
pub trait LspDiagnosticDocumentPort: Send + Sync {
    fn load_document(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<LspDocument, LspRuntimeFailure>>;
}

pub trait LspWorkspaceDocumentIndexPort: Send + Sync {
    fn is_mounted(&self) -> bool {
        false
    }

    fn indexed_documents(
        &self,
        root: AdmittedRoot,
        maximum_documents: usize,
    ) -> LspRuntimeFuture<Result<tracedecay_lsp::IndexedWorkspaceDocuments, LspRuntimeFailure>>;
}

/// Diagnostic authority binding the daemon's upstream broker to canonical
/// managed diagnostic truth.
pub struct BrokerDiagnosticSnapshotAuthority<S, M> {
    broker: Arc<AsyncMutex<DiagnosticBroker>>,
    documents: Arc<S>,
    managed: Arc<M>,
    diagnostics_quiet_window: Duration,
}

impl<S, M> BrokerDiagnosticSnapshotAuthority<S, M> {
    pub fn new(
        broker: Arc<AsyncMutex<DiagnosticBroker>>,
        documents: Arc<S>,
        managed: Arc<M>,
        diagnostics_quiet_window: Duration,
    ) -> Self {
        Self {
            broker,
            documents,
            managed,
            diagnostics_quiet_window,
        }
    }
}

impl<S, M> CanonicalDiagnosticSnapshotAuthority for BrokerDiagnosticSnapshotAuthority<S, M>
where
    S: LspDiagnosticDocumentPort + LspWorkspaceDocumentIndexPort + 'static,
    M: ManagedDiagnosticSnapshotPort + 'static,
{
    fn refresh(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>> {
        let broker = Arc::clone(&self.broker);
        let documents = Arc::clone(&self.documents);
        let managed = Arc::clone(&self.managed);
        let diagnostics_quiet_window = self.diagnostics_quiet_window;
        Box::pin(async move {
            refresh_document_snapshot(
                broker,
                documents,
                managed,
                diagnostics_quiet_window,
                request,
            )
            .await
        })
    }

    fn supports_workspace_diagnostics(&self) -> bool {
        self.documents.is_mounted()
    }

    fn refresh_workspace(
        &self,
        request: CanonicalWorkspaceDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<WorkspaceGenerationDiagnostics, LspRuntimeFailure>> {
        let broker = Arc::clone(&self.broker);
        let documents = Arc::clone(&self.documents);
        let managed = Arc::clone(&self.managed);
        let diagnostics_quiet_window = self.diagnostics_quiet_window;
        Box::pin(async move {
            let indexed = documents
                .indexed_documents(
                    request.root.clone(),
                    tracedecay_lsp::MAX_WORKSPACE_DIAGNOSTIC_RESULTS,
                )
                .await?;
            if request
                .overlays
                .iter()
                .any(|overlay| !indexed.documents.iter().any(|item| item.uri == overlay.uri))
            {
                return Err(LspRuntimeFailure::new(
                    "workspace-diagnostic-overlay-not-indexed",
                ));
            }
            let mut snapshots = Vec::with_capacity(indexed.documents.len());
            for indexed_document in indexed.documents {
                let overlay = request
                    .overlays
                    .iter()
                    .find(|overlay| overlay.uri == indexed_document.uri)
                    .cloned();
                let version = overlay.as_ref().map(|overlay| overlay.version);
                let content_digest = overlay
                    .as_ref()
                    .map_or(indexed_document.content_digest, |overlay| {
                        tracedecay_domain::ContentDigest::of_bytes(overlay.text.as_bytes())
                    });
                let document_request = CanonicalDiagnosticRefreshRequest {
                    root: request.root.clone(),
                    document_uri: indexed_document.uri.clone(),
                    overlay,
                    source_generation: None,
                    expected_content_digest: Some(content_digest.clone()),
                };
                let diagnostics = refresh_document_snapshot(
                    Arc::clone(&broker),
                    Arc::clone(&documents),
                    Arc::clone(&managed),
                    diagnostics_quiet_window,
                    document_request,
                )
                .await?;
                snapshots.push(WorkspaceDocumentDiagnostics {
                    uri: indexed_document.uri,
                    version,
                    content_digest,
                    diagnostics,
                });
            }
            Ok(WorkspaceGenerationDiagnostics {
                code_generation_id: indexed.code_generation_id,
                snapshot_digest: indexed.snapshot_digest,
                documents: snapshots,
            })
        })
    }
}

async fn refresh_document_snapshot<S, M>(
    broker: Arc<AsyncMutex<DiagnosticBroker>>,
    documents: Arc<S>,
    managed: Arc<M>,
    diagnostics_quiet_window: Duration,
    request: CanonicalDiagnosticRefreshRequest,
) -> Result<GenerationDiagnostics, LspRuntimeFailure>
where
    S: LspDiagnosticDocumentPort + 'static,
    M: ManagedDiagnosticSnapshotPort + 'static,
{
    let document = documents.load_document(request.clone()).await?;
    let language = document.language.clone();
    let relative_path = document.relative_path.clone();
    let prepared = {
        let mut broker = broker.lock().await;
        broker
            .prepare_refresh(&language, vec![document])
            .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-preparation-failed"))?
    };
    if let Some(prepared) = prepared {
        let completed = prepared.collect_diagnostics(diagnostics_quiet_window).await;
        broker
            .lock()
            .await
            .finish_refresh(completed)
            .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-refresh-failed"))?;
    }
    let upstream = broker
        .lock()
        .await
        .snapshot()
        .diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.file == relative_path)
        .map(|diagnostic| broker_diagnostic(request.document_uri.as_str(), diagnostic))
        .collect();
    let managed = managed.snapshot(request).await?;
    Ok(GenerationDiagnostics {
        generation: managed.generation,
        upstream,
        tracedecay: managed.diagnostics,
    })
}

fn broker_diagnostic(document_uri: &str, diagnostic: CodeDiagnostic) -> GatewayDiagnostic {
    let start_line = diagnostic.line_start.saturating_sub(1);
    let end_line = diagnostic
        .line_end
        .max(diagnostic.line_start)
        .saturating_sub(1);
    let start_character = diagnostic.character_start.unwrap_or_default();
    let end_character = diagnostic.character_end.unwrap_or(start_character);
    GatewayDiagnostic {
        uri: document_uri.to_owned(),
        range: LspRange {
            start: LspPosition {
                line: start_line,
                character: start_character,
            },
            end: LspPosition {
                line: end_line,
                character: if end_line == start_line {
                    end_character.max(start_character)
                } else {
                    end_character
                },
            },
        },
        severity: Some(match diagnostic.severity {
            BrokerDiagnosticSeverity::Error => tracedecay_lsp::DiagnosticSeverity::Error,
            BrokerDiagnosticSeverity::Warning => tracedecay_lsp::DiagnosticSeverity::Warning,
            BrokerDiagnosticSeverity::Information => {
                tracedecay_lsp::DiagnosticSeverity::Information
            }
            BrokerDiagnosticSeverity::Hint => tracedecay_lsp::DiagnosticSeverity::Hint,
        }),
        code: diagnostic.code,
        code_description_uri: None,
        message: diagnostic.message,
        source: DiagnosticSource::Upstream,
        related_information: Vec::new(),
        data: None,
    }
}

/// Existing daemon analyzer authorities retain their `lsp-types` DTO boundary.
pub trait LspSemanticRequestAuthority: Send + Sync {
    fn start(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: AnalyzerSemanticRequest,
    ) -> LspRuntimeFuture<LspSemanticOperationOutcome>;

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool;
}

struct SemanticAuthorityAdapter {
    inner: Arc<dyn LspSemanticRequestAuthority>,
}

impl tracedecay_lsp::LspSemanticRequestAuthority for SemanticAuthorityAdapter {
    fn start(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: LspSemanticRequest,
    ) -> LspRuntimeFuture<LspSemanticOperationOutcome> {
        let decoded = decode_semantic_request(request);
        let authority = Arc::clone(&self.inner);
        Box::pin(async move {
            match decoded {
                Ok(request) => authority.start(root, request_id, request).await,
                Err(_) => LspSemanticOperationOutcome::Partial {
                    value: serde_json::Value::Null,
                    coverage: "semantic-request-invalid".to_owned(),
                    detail: None,
                },
            }
        })
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.inner.cancel_request(root, request_id)
    }
}

pub struct DaemonSemanticProviderAdapter {
    inner: Arc<SemanticProviderAdapter>,
}

impl DaemonSemanticProviderAdapter {
    pub fn new(runtime: Handle, authority: Arc<dyn LspSemanticRequestAuthority>) -> Self {
        Self {
            inner: SemanticProviderAdapter::shared(
                runtime_spawner(runtime),
                Arc::new(SemanticAuthorityAdapter { inner: authority }),
            ),
        }
    }

    pub fn shared(runtime: Handle, authority: Arc<dyn LspSemanticRequestAuthority>) -> Arc<Self> {
        Arc::new(Self::new(runtime, authority))
    }

    pub fn shared_protocol(
        runtime: Handle,
        authority: Arc<dyn ProtocolSemanticRequestAuthority>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: SemanticProviderAdapter::shared(runtime_spawner(runtime), authority),
        })
    }

    pub fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.inner.cancel_request(root, request_id)
    }
}

impl SemanticProviderPort for DaemonSemanticProviderAdapter {
    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        self.inner.request(root, request_id, request)
    }
}

impl LspAnalyzerCancellationAuthority for DaemonSemanticProviderAdapter {
    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        DaemonSemanticProviderAdapter::cancel_request(self, root, request_id)
    }
}

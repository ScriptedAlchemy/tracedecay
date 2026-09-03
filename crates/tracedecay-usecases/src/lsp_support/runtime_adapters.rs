//! Daemon-specific bindings for the store-free LSP runtime brokers.

use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::AbortHandle;
use tracedecay_lsp::analyzer::broker::{
    CodeDiagnostic, DiagnosticBroker, DiagnosticSeverity as BrokerDiagnosticSeverity,
    RefreshCommitOutcome,
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

pub(crate) fn managed_diagnostic_authority_digest(
    scope: &crate::lsp_runtime::LspFeedbackProjectionScope,
) -> Result<tracedecay_domain::ManifestDigest, LspRuntimeFailure> {
    tracedecay_domain::canonical_sha256(&(
        "tracedecay.lsp.managed-diagnostic-authority.v1",
        scope.head_commit_id.as_str(),
        scope.code_generation_id.as_str(),
        scope.snapshot_digest.as_str(),
        scope.invalidation_digest.as_str(),
        scope.snapshot_content_digest.as_str(),
        scope
            .document_content_digest
            .as_ref()
            .map(tracedecay_domain::ContentDigest::as_str),
        scope.generation,
    ))
    .map_err(|_| LspRuntimeFailure::new("managed-diagnostic-authority-identity-unavailable"))
}

pub(crate) fn validate_managed_diagnostic_scope(
    request: &CanonicalDiagnosticRefreshRequest,
    scope: &crate::lsp_runtime::LspFeedbackProjectionScope,
) -> Result<(), LspRuntimeFailure> {
    let expected_content_digest = request
        .expected_content_digest
        .as_ref()
        .ok_or_else(|| LspRuntimeFailure::new("managed-diagnostic-content-identity-unavailable"))?;
    if scope.document_content_digest.as_ref() != Some(expected_content_digest) {
        return Err(LspRuntimeFailure::new("managed-diagnostic-content-stale"));
    }
    if request
        .expected_code_generation_id
        .as_ref()
        .is_some_and(|expected| expected != &scope.code_generation_id)
        || request
            .expected_snapshot_digest
            .as_ref()
            .is_some_and(|expected| expected != &scope.snapshot_digest)
    {
        return Err(LspRuntimeFailure::new(
            "managed-diagnostic-generation-stale",
        ));
    }
    Ok(())
}

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
            let mut indexed = documents
                .indexed_documents(
                    request.root.clone(),
                    tracedecay_lsp::MAX_WORKSPACE_DIAGNOSTIC_RESULTS,
                )
                .await?;
            indexed.documents.retain(|document| {
                request
                    .workspace
                    .resolve_document(&document.uri)
                    .is_ok_and(|owner| owner == &request.root)
            });
            let code_generation_id =
                tracedecay_domain::CodeGenerationId::new(indexed.code_generation_id.clone())
                    .map_err(|_| {
                        LspRuntimeFailure::new("workspace-code-generation-identity-invalid")
                    })?;
            let admitted_index = indexed.clone();
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
                    expected_code_generation_id: Some(code_generation_id.clone()),
                    expected_snapshot_digest: Some(indexed.snapshot_digest.clone()),
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
            let mut current_index = documents
                .indexed_documents(
                    request.root.clone(),
                    tracedecay_lsp::MAX_WORKSPACE_DIAGNOSTIC_RESULTS,
                )
                .await?;
            current_index.documents.retain(|document| {
                request
                    .workspace
                    .resolve_document(&document.uri)
                    .is_ok_and(|owner| owner == &request.root)
            });
            if current_index != admitted_index {
                return Err(LspRuntimeFailure::new("workspace-code-generation-stale"));
            }
            Ok(WorkspaceGenerationDiagnostics {
                code_generation_id: admitted_index.code_generation_id,
                snapshot_digest: admitted_index.snapshot_digest,
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
    mut request: CanonicalDiagnosticRefreshRequest,
) -> Result<GenerationDiagnostics, LspRuntimeFailure>
where
    S: LspDiagnosticDocumentPort + 'static,
    M: ManagedDiagnosticSnapshotPort + 'static,
{
    let document = documents.load_document(request.clone()).await?;
    request.expected_content_digest = Some(tracedecay_domain::ContentDigest::of_bytes(
        document.text.as_bytes(),
    ));
    let language = document.language.clone();
    let relative_path = document.relative_path.clone();
    let (prepared, immediate_snapshot) = {
        let mut broker = broker.lock().await;
        let prepared = broker
            .prepare_refresh(&language, vec![document])
            .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-preparation-failed"))?;
        let immediate_snapshot = prepared.is_none().then(|| broker.snapshot());
        (prepared, immediate_snapshot)
    };
    let snapshot = if let Some(prepared) = prepared {
        let completed = prepared.collect_diagnostics(diagnostics_quiet_window).await;
        match broker
            .lock()
            .await
            .finish_refresh_snapshot(completed)
            .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-refresh-failed"))?
        {
            RefreshCommitOutcome::Applied(snapshot) => snapshot,
            RefreshCommitOutcome::Superseded => {
                return Err(LspRuntimeFailure::new(
                    "diagnostic-broker-refresh-superseded",
                ));
            }
        }
    } else {
        immediate_snapshot
            .ok_or_else(|| LspRuntimeFailure::new("diagnostic-broker-snapshot-unavailable"))?
    };
    let upstream_authority_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.lsp.upstream-diagnostic-authority.v1",
        &snapshot.settings,
        &snapshot.settings_unavailable,
    ))
    .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-authority-identity-unavailable"))?;
    let upstream = snapshot
        .diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.file == relative_path)
        .map(|diagnostic| broker_diagnostic(request.document_uri.as_str(), diagnostic))
        .collect();
    let expected_code_generation_id = request.expected_code_generation_id.clone();
    let expected_snapshot_digest = request.expected_snapshot_digest.clone();
    let managed = managed.snapshot(request).await?;
    if expected_code_generation_id
        .as_ref()
        .is_some_and(|expected| expected != &managed.code_generation_id)
        || expected_snapshot_digest
            .as_ref()
            .is_some_and(|expected| expected != &managed.snapshot_digest)
    {
        return Err(LspRuntimeFailure::new(
            "managed-diagnostic-generation-stale",
        ));
    }
    let authority_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.lsp.merged-diagnostic-authority.v1",
        upstream_authority_digest,
        &managed.authority_digest,
    ))
    .map_err(|_| LspRuntimeFailure::new("diagnostic-authority-identity-unavailable"))?;
    Ok(GenerationDiagnostics {
        generation: managed.generation,
        authority_digest,
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_domain::{CodeGenerationId, ContentDigest, ManifestDigest};
    use tracedecay_lsp::{
        AuthorizedLspWorkspace, CanonicalWorkspaceDiagnosticRefreshRequest,
        IndexedWorkspaceDocument, ManagedDiagnosticSnapshot,
    };

    use super::*;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    struct PublishingWorkspaceIndex {
        reads: AtomicUsize,
        drift: WorkspaceIndexDrift,
    }

    #[derive(Clone, Copy)]
    enum WorkspaceIndexDrift {
        None,
        Publish,
        AddFile,
    }

    impl LspDiagnosticDocumentPort for PublishingWorkspaceIndex {
        fn load_document(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<LspDocument, LspRuntimeFailure>> {
            Box::pin(async {
                Ok(LspDocument {
                    language: "unknown".to_owned(),
                    language_id: "unknown".to_owned(),
                    relative_path: "src/lib.rs".to_owned(),
                    text: "fn main() {}".to_owned(),
                })
            })
        }
    }

    impl LspWorkspaceDocumentIndexPort for PublishingWorkspaceIndex {
        fn is_mounted(&self) -> bool {
            true
        }

        fn indexed_documents(
            &self,
            _root: AdmittedRoot,
            _maximum_documents: usize,
        ) -> LspRuntimeFuture<Result<tracedecay_lsp::IndexedWorkspaceDocuments, LspRuntimeFailure>>
        {
            let read = self.reads.fetch_add(1, Ordering::SeqCst);
            let drifted = read > 0;
            let published = drifted && matches!(self.drift, WorkspaceIndexDrift::Publish);
            let file_added = drifted && matches!(self.drift, WorkspaceIndexDrift::AddFile);
            Box::pin(async move {
                let mut documents = vec![IndexedWorkspaceDocument {
                    uri: "file:///workspace/src/lib.rs".to_owned(),
                    content_digest: ContentDigest::of_bytes(b"fn main() {}"),
                }];
                if file_added {
                    documents.push(IndexedWorkspaceDocument {
                        uri: "file:///workspace/src/new.rs".to_owned(),
                        content_digest: ContentDigest::of_bytes(b"fn added() {}"),
                    });
                }
                Ok(tracedecay_lsp::IndexedWorkspaceDocuments {
                    code_generation_id: if published {
                        "code-generation-2".to_owned()
                    } else {
                        "code-generation-1".to_owned()
                    },
                    snapshot_digest: if published { digest('b') } else { digest('a') },
                    documents,
                })
            })
        }
    }

    struct FixedManagedDiagnostics;

    impl ManagedDiagnosticSnapshotPort for FixedManagedDiagnostics {
        fn snapshot(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<ManagedDiagnosticSnapshot, LspRuntimeFailure>> {
            Box::pin(async {
                Ok(ManagedDiagnosticSnapshot {
                    generation: 1,
                    code_generation_id: CodeGenerationId::new("code-generation-1").unwrap(),
                    snapshot_digest: digest('a'),
                    authority_digest: digest('c'),
                    diagnostics: Vec::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn workspace_sweep_rejects_a_generation_published_before_completion() {
        let root = AdmittedRoot::authorized("file:///workspace", digest('d'));
        let workspace = AuthorizedLspWorkspace::new(Some(digest('e')), vec![root.clone()]).unwrap();
        let authority = BrokerDiagnosticSnapshotAuthority::new(
            Arc::new(AsyncMutex::new(DiagnosticBroker::new_for_test(
                "/workspace",
                Vec::new(),
            ))),
            Arc::new(PublishingWorkspaceIndex {
                reads: AtomicUsize::new(0),
                drift: WorkspaceIndexDrift::Publish,
            }),
            Arc::new(FixedManagedDiagnostics),
            Duration::from_millis(1),
        );

        let error = authority
            .refresh_workspace(CanonicalWorkspaceDiagnosticRefreshRequest {
                workspace,
                root,
                overlays: Vec::new(),
            })
            .await
            .expect_err("a cross-generation sweep must not be published");

        assert_eq!(error.class(), "workspace-code-generation-stale");
    }

    #[tokio::test]
    async fn workspace_sweep_rejects_a_file_added_before_completion() {
        let root = AdmittedRoot::authorized("file:///workspace", digest('d'));
        let workspace = AuthorizedLspWorkspace::new(Some(digest('e')), vec![root.clone()]).unwrap();
        let authority = BrokerDiagnosticSnapshotAuthority::new(
            Arc::new(AsyncMutex::new(DiagnosticBroker::new_for_test(
                "/workspace",
                Vec::new(),
            ))),
            Arc::new(PublishingWorkspaceIndex {
                reads: AtomicUsize::new(0),
                drift: WorkspaceIndexDrift::AddFile,
            }),
            Arc::new(FixedManagedDiagnostics),
            Duration::from_millis(1),
        );

        let error = authority
            .refresh_workspace(CanonicalWorkspaceDiagnosticRefreshRequest {
                workspace,
                root,
                overlays: Vec::new(),
            })
            .await
            .expect_err("a file-set change must make the sweep partial");

        assert_eq!(error.class(), "workspace-code-generation-stale");
    }

    struct MismatchedManagedDiagnostics;

    impl ManagedDiagnosticSnapshotPort for MismatchedManagedDiagnostics {
        fn snapshot(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<ManagedDiagnosticSnapshot, LspRuntimeFailure>> {
            Box::pin(async {
                Ok(ManagedDiagnosticSnapshot {
                    generation: 2,
                    code_generation_id: CodeGenerationId::new("code-generation-2").unwrap(),
                    snapshot_digest: digest('b'),
                    authority_digest: digest('c'),
                    diagnostics: Vec::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn workspace_sweep_rejects_managed_diagnostics_from_another_generation() {
        let root = AdmittedRoot::authorized("file:///workspace", digest('d'));
        let workspace = AuthorizedLspWorkspace::new(Some(digest('e')), vec![root.clone()]).unwrap();
        let authority = BrokerDiagnosticSnapshotAuthority::new(
            Arc::new(AsyncMutex::new(DiagnosticBroker::new_for_test(
                "/workspace",
                Vec::new(),
            ))),
            Arc::new(PublishingWorkspaceIndex {
                reads: AtomicUsize::new(0),
                drift: WorkspaceIndexDrift::None,
            }),
            Arc::new(MismatchedManagedDiagnostics),
            Duration::from_millis(1),
        );

        let error = authority
            .refresh_workspace(CanonicalWorkspaceDiagnosticRefreshRequest {
                workspace,
                root,
                overlays: Vec::new(),
            })
            .await
            .expect_err("managed diagnostics must share the admitted generation");

        assert_eq!(error.class(), "managed-diagnostic-generation-stale");
    }
}

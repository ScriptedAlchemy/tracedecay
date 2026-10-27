//! Daemon-specific bindings for the store-free LSP runtime brokers.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;
use tracedecay_domain::ManifestDigest;
#[cfg(test)]
use tracedecay_lsp::analyzer::broker::DiagnosticsSnapshot;
use tracedecay_lsp::analyzer::broker::{
    BrokerDocumentEvidence, CodeDiagnostic, DiagnosticBroker,
    DiagnosticSeverity as BrokerDiagnosticSeverity, RefreshCommitOutcome,
};
use tracedecay_lsp::analyzer::client::LspDocument;
use tracedecay_lsp::{
    AdmittedRoot, CanonicalDiagnosticRefreshRequest, CanonicalDiagnosticSnapshotAuthority,
    CanonicalWorkspaceDiagnosticRefreshRequest, DiagnosticSource, GatewayDiagnostic,
    GenerationDiagnostics, LspPosition, LspRange, LspRuntimeFailure, LspRuntimeFuture,
    LspRuntimeSpawner, LspRuntimeTask, ManagedDiagnosticSnapshotPort, WorkspaceDocumentDiagnostics,
    WorkspaceGenerationDiagnostics,
};
use tracedecay_runtime_core::native_runtime::NativeTask;

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

struct NativeLspTask(Option<NativeTask<()>>);

impl LspRuntimeTask for NativeLspTask {
    fn abort(&self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

struct NativeLspRuntime;

impl LspRuntimeSpawner for NativeLspRuntime {
    fn spawn(&self, future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
        match NativeTask::spawn(future) {
            Ok(task) => Box::new(NativeLspTask(Some(task))),
            Err(error) => {
                // The dropped future closes its result channel, so the
                // operation table reports the operation as dropped.
                tracing::error!(%error, "LSP runtime work was not started");
                Box::new(NativeLspTask(None))
            }
        }
    }
}

/// Runs LSP broker work on the native runtime. Only for work that never
/// reaches Tokio: the analyzer clients, the store, and in-process state.
pub fn native_lsp_runtime() -> Arc<dyn LspRuntimeSpawner> {
    Arc::new(NativeLspRuntime)
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

    fn current_authority(
        &self,
        mut request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<ManifestDigest, LspRuntimeFailure>> {
        let broker = Arc::clone(&self.broker);
        let documents = Arc::clone(&self.documents);
        let managed = Arc::clone(&self.managed);
        Box::pin(async move {
            let document = documents.load_document(request.clone()).await?;
            request.expected_content_digest = Some(tracedecay_domain::ContentDigest::of_bytes(
                document.text.as_bytes(),
            ));
            let document_uri = request.document_uri.clone();
            let managed = managed.current_authority(request).await?;
            let evidence = broker
                .lock()
                .await
                .document_diagnostic_evidence(&document)
                .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-authority-unavailable"))?;
            let (upstream, _) = project_upstream_evidence(evidence, document_uri.as_str())?;
            merged_diagnostic_authority_digest(&upstream, &managed)
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
    let (prepared, immediate_evidence) = {
        let mut broker = broker.lock().await;
        let prepared = broker
            .prepare_refresh(&language, vec![document.clone()])
            .await
            .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-preparation-failed"))?;
        let immediate_evidence = if prepared.is_none() {
            Some(
                broker
                    .document_diagnostic_evidence(&document)
                    .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-coverage-incomplete"))?,
            )
        } else {
            None
        };
        (prepared, immediate_evidence)
    };
    let evidence = if let Some(prepared) = prepared {
        let completed = prepared.collect_diagnostics(diagnostics_quiet_window).await;
        let mut broker = broker.lock().await;
        match broker
            .finish_refresh_snapshot(completed)
            .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-refresh-failed"))?
        {
            RefreshCommitOutcome::Applied(_) => broker
                .document_diagnostic_evidence(&document)
                .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-coverage-incomplete"))?,
            RefreshCommitOutcome::Superseded => {
                return Err(LspRuntimeFailure::new(
                    "diagnostic-broker-refresh-superseded",
                ));
            }
        }
    } else {
        immediate_evidence
            .ok_or_else(|| LspRuntimeFailure::new("diagnostic-broker-snapshot-unavailable"))?
    };
    let (upstream_authority_digest, upstream) =
        project_upstream_evidence(evidence, request.document_uri.as_str())?;
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
    let authority_digest =
        merged_diagnostic_authority_digest(&upstream_authority_digest, &managed.authority_digest)?;
    Ok(GenerationDiagnostics {
        generation: managed.generation,
        authority_digest,
        upstream,
        tracedecay: managed.diagnostics,
    })
}

fn project_upstream_evidence(
    evidence: BrokerDocumentEvidence,
    document_uri: &str,
) -> Result<(ManifestDigest, Vec<GatewayDiagnostic>), LspRuntimeFailure> {
    let (settings, settings_unavailable, coverage, nonparticipation, diagnostics) = match evidence {
        BrokerDocumentEvidence::Published {
            settings,
            settings_unavailable,
            coverage,
            diagnostics,
        } => (
            settings,
            settings_unavailable,
            Some(coverage),
            None,
            diagnostics,
        ),
        BrokerDocumentEvidence::NotParticipating {
            settings,
            settings_unavailable,
            reason,
        } => (
            settings,
            settings_unavailable,
            None,
            Some(reason),
            Vec::new(),
        ),
    };
    let (upstream, identity) = tracedecay_lsp::project_upstream_diagnostics(
        diagnostics
            .into_iter()
            .map(|diagnostic| broker_diagnostic(document_uri, diagnostic))
            .collect(),
    )
    .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-authority-identity-unavailable"))?;
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.lsp.upstream-diagnostic-authority.v1",
        settings,
        settings_unavailable,
        coverage,
        nonparticipation,
        identity,
    ))
    .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-authority-identity-unavailable"))?;
    Ok((digest, upstream))
}

fn merged_diagnostic_authority_digest(
    upstream: &ManifestDigest,
    managed: &ManifestDigest,
) -> Result<ManifestDigest, LspRuntimeFailure> {
    tracedecay_domain::canonical_sha256(&(
        "tracedecay.lsp.merged-diagnostic-authority.v1",
        upstream,
        managed,
    ))
    .map_err(|_| LspRuntimeFailure::new("diagnostic-authority-identity-unavailable"))
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{Value, json};
    use tracedecay_domain::{CodeGenerationId, ContentDigest};
    use tracedecay_lsp::analyzer::adapters::{DiagnosticMode, LspAdapterDefinition};
    use tracedecay_lsp::{
        AuthorizedLspWorkspace, CanonicalWorkspaceDiagnosticRefreshRequest, ClientCapabilities,
        DaemonLspProtocolSession, DiagnosticSnapshotAdapter, FeedbackCyclePort,
        FeedbackCycleRequest, FeedbackCycleResponse, GatewayCapabilities, IndexedWorkspaceDocument,
        ManagedDiagnosticSnapshot, UnavailableSemanticProvider, UpstreamCapabilities,
        negotiate_capabilities,
    };
    use url::Url;

    use super::*;

    use tracedecay_domain::test_fixtures::digest;

    struct PublishingWorkspaceIndex {
        reads: AtomicUsize,
        drift: WorkspaceIndexDrift,
        documents: BTreeMap<String, LspDocument>,
    }

    impl PublishingWorkspaceIndex {
        fn new(drift: WorkspaceIndexDrift) -> Self {
            Self {
                reads: AtomicUsize::new(0),
                drift,
                documents: BTreeMap::from([(
                    "file:///workspace/src/lib.rs".to_owned(),
                    LspDocument {
                        language: "unknown".to_owned(),
                        language_id: "unknown".to_owned(),
                        relative_path: "src/lib.rs".to_owned(),
                        text: "fn main() {}".to_owned(),
                        snapshot: None,
                    },
                )]),
            }
        }
    }

    #[derive(Clone, Copy)]
    enum WorkspaceIndexDrift {
        None,
        Publish,
    }

    impl LspDiagnosticDocumentPort for PublishingWorkspaceIndex {
        fn load_document(
            &self,
            request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<LspDocument, LspRuntimeFailure>> {
            let document = self.documents.get(&request.document_uri).cloned();
            Box::pin(async move {
                document.ok_or_else(|| LspRuntimeFailure::new("fixture-document-not-found"))
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
            Box::pin(async move {
                let documents = vec![IndexedWorkspaceDocument {
                    uri: "file:///workspace/src/lib.rs".to_owned(),
                    content_digest: ContentDigest::of_bytes(b"fn main() {}"),
                }];
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

    #[derive(Default)]
    struct FixedManagedDiagnostics {
        snapshots: AtomicUsize,
    }

    impl ManagedDiagnosticSnapshotPort for FixedManagedDiagnostics {
        fn current_authority(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<ManifestDigest, LspRuntimeFailure>> {
            Box::pin(async { Ok(digest('c')) })
        }

        fn snapshot(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<ManagedDiagnosticSnapshot, LspRuntimeFailure>> {
            self.snapshots.fetch_add(1, Ordering::SeqCst);
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

    struct DiagnosticTask(Mutex<Option<LspRuntimeFuture<()>>>);

    struct DiagnosticTaskHandle(Arc<DiagnosticTask>);

    impl LspRuntimeTask for DiagnosticTaskHandle {
        fn abort(&self) {
            self.0.0.lock().unwrap().take();
        }
    }

    #[derive(Default)]
    struct DiagnosticRuntime {
        tasks: Mutex<VecDeque<Arc<DiagnosticTask>>>,
        started: AtomicUsize,
    }

    impl LspRuntimeSpawner for DiagnosticRuntime {
        fn spawn(&self, future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
            let task = Arc::new(DiagnosticTask(Mutex::new(Some(future))));
            self.tasks.lock().unwrap().push_back(Arc::clone(&task));
            self.started.fetch_add(1, Ordering::SeqCst);
            Box::new(DiagnosticTaskHandle(task))
        }
    }

    impl DiagnosticRuntime {
        async fn complete_next(&self) {
            let task = self.tasks.lock().unwrap().pop_front().unwrap();
            let future = task.0.lock().unwrap().take().unwrap();
            future.await;
        }
    }

    struct Feedback;

    impl FeedbackCyclePort for Feedback {
        fn request_feedback_cycle(&self, _request: FeedbackCycleRequest) -> FeedbackCycleResponse {
            FeedbackCycleResponse::Accepted
        }
    }

    fn file_uri(path: &Path) -> String {
        Url::from_file_path(path).unwrap().to_string()
    }

    const PUBLISHING_ANALYZER: &str = r#"
import json
import sys
from urllib.parse import urlparse

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        headers[name.lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))

def send(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n" + body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {"textDocumentSync": 1}}})
    elif method in ("textDocument/didOpen", "textDocument/didChange"):
        uri = message["params"]["textDocument"]["uri"]
        with open(sys.argv[1], encoding="utf-8") as findings:
            diagnostics = json.load(findings)[urlparse(uri).path.rsplit("/", 1)[-1]]
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {
            "uri": uri, "diagnostics": diagnostics,
        }})
"#;

    struct BrokerDiagnosticHarness {
        session: DaemonLspProtocolSession<
            Feedback,
            UnavailableSemanticProvider,
            DiagnosticSnapshotAdapter,
        >,
        runtime: Arc<DiagnosticRuntime>,
        broker: Arc<AsyncMutex<DiagnosticBroker>>,
        documents: Arc<PublishingWorkspaceIndex>,
        managed: Arc<FixedManagedDiagnostics>,
        project: tempfile::TempDir,
        next_request: u64,
    }

    impl BrokerDiagnosticHarness {
        fn new(findings: Value) -> Self {
            let project = tempfile::tempdir().unwrap();
            let script = project.path().join("analyzer.py");
            let publications = project.path().join("findings.json");
            std::fs::write(&script, PUBLISHING_ANALYZER).unwrap();
            std::fs::write(&publications, serde_json::to_vec(&findings).unwrap()).unwrap();
            let python = if cfg!(windows) {
                "python"
            } else if std::path::Path::new("/usr/bin/python3").is_file() {
                "/usr/bin/python3"
            } else {
                "python3"
            };
            let broker = Arc::new(AsyncMutex::new(DiagnosticBroker::new_for_test(
                project.path(),
                vec![LspAdapterDefinition {
                    language: "fake".to_owned(),
                    language_id: "fake".to_owned(),
                    command: python.to_owned(),
                    args: vec![
                        script.display().to_string(),
                        publications.display().to_string(),
                    ],
                    extensions: vec!["fake".to_owned()],
                    root_markers: Vec::new(),
                    install_options: Vec::new(),
                    diagnostics: DiagnosticMode::Push,
                }],
            )));
            let documents = Arc::new(PublishingWorkspaceIndex {
                reads: AtomicUsize::new(0),
                drift: WorkspaceIndexDrift::None,
                documents: ["a.fake", "b.fake"]
                    .into_iter()
                    .map(|path| {
                        let document = LspDocument {
                            language: "fake".to_owned(),
                            language_id: "fake".to_owned(),
                            relative_path: path.to_owned(),
                            text: "fn main() {}".to_owned(),
                            snapshot: None,
                        };
                        std::fs::write(project.path().join(path), &document.text).unwrap();
                        (file_uri(&project.path().join(path)), document)
                    })
                    .collect(),
            });
            let managed = Arc::new(FixedManagedDiagnostics::default());
            let runtime = Arc::new(DiagnosticRuntime::default());
            let authority = Arc::new(BrokerDiagnosticSnapshotAuthority::new(
                Arc::clone(&broker),
                Arc::clone(&documents),
                Arc::clone(&managed),
                Duration::from_millis(150),
            ));
            let root = AdmittedRoot::authorized(file_uri(project.path()), digest('d'));
            let workspace = AuthorizedLspWorkspace::new(Some(digest('e')), vec![root]).unwrap();
            let gateway = GatewayCapabilities::default();
            let upstream = UpstreamCapabilities {
                supports_diagnostics: true,
                ..UpstreamCapabilities::default()
            };
            let session = DaemonLspProtocolSession::from_workspace_ports(
                workspace,
                negotiate_capabilities(&ClientCapabilities::default(), &gateway, &upstream),
                gateway,
                upstream,
                Feedback,
                UnavailableSemanticProvider,
                DiagnosticSnapshotAdapter::new(runtime.clone(), authority),
            );
            let mut harness = Self {
                session,
                runtime,
                broker,
                documents,
                managed,
                project,
                next_request: 1,
            };
            let initialized = harness.request(
                "initialize",
                json!({
                    "rootUri": file_uri(harness.project.path()),
                    "capabilities": {
                        "general": { "positionEncodings": ["utf-16"] },
                        "textDocument": { "diagnostic": {} },
                    },
                }),
            );
            assert!(initialized.get("error").is_none(), "{initialized}");
            harness.session.handle_payload(
                &serde_json::to_vec(&json!({
                    "jsonrpc": "2.0", "method": "initialized", "params": {},
                }))
                .unwrap(),
                1,
            );
            assert!(harness.session.drain_outbound().is_empty());
            harness
        }

        fn uri(&self, path: &str) -> String {
            file_uri(&self.project.path().join(path))
        }

        fn request(&mut self, method: &str, params: Value) -> Value {
            let id = self.next_request;
            self.next_request += 1;
            self.session.handle_payload(
                &serde_json::to_vec(&json!({
                    "jsonrpc": "2.0", "id": id, "method": method, "params": params,
                }))
                .unwrap(),
                id,
            );
            self.session
                .drain_outbound()
                .iter()
                .map(|message| serde_json::from_slice::<Value>(message).unwrap())
                .find(|message| message["id"] == id)
                .expect("diagnostic protocol response")
        }

        fn pull(&mut self, uri: &str, previous: Option<&str>) -> Value {
            let mut params = json!({ "textDocument": { "uri": uri } });
            if let Some(previous) = previous {
                params["previousResultId"] = json!(previous);
            }
            self.request("textDocument/diagnostic", params)
        }

        async fn warm(&mut self, uri: &str) -> Value {
            let pending = self.pull(uri, None);
            assert_eq!(pending["error"]["code"], -32802, "{pending}");
            assert_eq!(pending["error"]["data"]["coverage"], "refresh-required");
            self.runtime.complete_next().await;
            let full = self.pull(uri, None);
            assert_eq!(full["result"]["kind"], "full", "{full}");
            full["result"].clone()
        }

        async fn restart_analyzer_and_republish(&self, uri: &str) -> DiagnosticsSnapshot {
            let document = self.documents.documents.get(uri).unwrap().clone();
            let mut broker = self.broker.lock().await;
            let settings = broker.settings().clone();
            broker.update_settings(settings);
            broker
                .refresh_documents("fake", vec![document], Duration::from_millis(150))
                .await
                .unwrap();
            broker.snapshot()
        }
    }

    fn analyzer_finding(message: &str, start: u32, end: u32) -> Value {
        json!({
            "range": { "start": { "line": 0, "character": start }, "end": { "line": 0, "character": end } },
            "severity": 1,
            "source": "fake-ls",
            "message": message,
        })
    }

    #[tokio::test]
    async fn retained_report_rejects_changed_broker_findings_with_identical_coverage() {
        let original_finding = analyzer_finding("first finding", 0, 1);
        let mut harness = BrokerDiagnosticHarness::new(json!({
            "a.fake": [original_finding], "b.fake": [],
        }));
        let uri = harness.uri("a.fake");
        let original = harness.warm(&uri).await;
        let result_id = original["resultId"].as_str().unwrap();
        assert_eq!(original["items"][0]["message"], "first finding");
        let before = harness.broker.lock().await.snapshot();

        let identical = harness.restart_analyzer_and_republish(&uri).await;
        assert_eq!(identical.coverage, before.coverage);
        assert_eq!(identical.settings, before.settings);
        assert_eq!(identical.diagnostics.len(), 1);
        assert_eq!(identical.diagnostics[0].message, "first finding");
        assert_eq!(identical.diagnostics[0].character_start, Some(0));
        assert_eq!(identical.diagnostics[0].character_end, Some(1));
        assert_eq!(
            harness.pull(&uri, Some(result_id))["result"],
            json!({ "kind": "unchanged", "resultId": result_id })
        );
        assert_eq!(harness.managed.snapshots.load(Ordering::SeqCst), 1);
        assert_eq!(harness.runtime.started.load(Ordering::SeqCst), 1);

        let changed_finding = analyzer_finding("replacement finding", 3, 7);
        std::fs::write(
            harness.project.path().join("findings.json"),
            serde_json::to_vec(&json!({ "a.fake": [changed_finding], "b.fake": [] })).unwrap(),
        )
        .unwrap();
        let changed = harness.restart_analyzer_and_republish(&uri).await;
        assert_eq!(changed.coverage, before.coverage);
        assert_eq!(changed.settings, before.settings);
        assert_eq!(changed.settings_unavailable, before.settings_unavailable);
        assert_eq!(changed.diagnostics.len(), 1);
        assert_eq!(changed.diagnostics[0].message, "replacement finding");
        assert_eq!(changed.diagnostics[0].character_start, Some(3));
        assert_eq!(changed.diagnostics[0].character_end, Some(7));

        let stale = harness.pull(&uri, Some(result_id));
        assert_eq!(stale["error"]["code"], -32802, "{stale}");
        assert!(
            stale.get("result").is_none(),
            "changed findings must reject retained reuse"
        );
        harness.runtime.complete_next().await;
        let replacement = harness.pull(&uri, Some(result_id));
        assert_eq!(replacement["result"]["kind"], "full", "{replacement}");
        assert_ne!(replacement["result"]["resultId"], original["resultId"]);
        assert_eq!(
            replacement["result"]["items"],
            json!([{
                "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 7 } },
                "severity": 1, "code": null, "source": "upstream", "message": "replacement finding",
            }])
        );
        assert_eq!(harness.managed.snapshots.load(Ordering::SeqCst), 2);
        assert_eq!(harness.runtime.started.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retained_reports_survive_other_document_refreshes_in_the_same_language() {
        let mut harness = BrokerDiagnosticHarness::new(json!({
            "a.fake": [analyzer_finding("retained finding", 0, 1)], "b.fake": [],
        }));
        let a = harness.uri("a.fake");
        let b = harness.uri("b.fake");
        let report_a = harness.warm(&a).await;
        let report_b = harness.warm(&b).await;
        assert_eq!(report_a["items"][0]["message"], "retained finding");
        assert_eq!(report_b["items"], json!([]));
        let latest = harness.broker.lock().await.snapshot();
        assert_eq!(latest.diagnostics.len(), 1);
        assert_eq!(latest.diagnostics[0].file, "a.fake");
        assert_eq!(
            latest.coverage["fake"]
                .documents
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["b.fake"],
            "dashboard coverage must still describe only the latest refresh"
        );
        assert_eq!(harness.managed.snapshots.load(Ordering::SeqCst), 2);
        assert_eq!(harness.runtime.started.load(Ordering::SeqCst), 2);

        for (uri, report) in [
            (&a, &report_a),
            (&b, &report_b),
            (&a, &report_a),
            (&b, &report_b),
        ] {
            let result_id = report["resultId"].as_str().unwrap();
            let reused = harness.pull(uri, Some(result_id));
            assert_eq!(
                reused["result"],
                json!({ "kind": "unchanged", "resultId": result_id }),
                "unchanged document must survive another document's refresh: {reused}"
            );
            assert_eq!(harness.runtime.started.load(Ordering::SeqCst), 2);
            assert_eq!(harness.managed.snapshots.load(Ordering::SeqCst), 2);
        }

        let document_a = harness.documents.documents[&a].clone();
        harness
            .broker
            .lock()
            .await
            .refresh_documents("fake", vec![document_a], Duration::from_millis(150))
            .await
            .unwrap();
        assert_eq!(
            harness.pull(&b, report_b["resultId"].as_str())["result"],
            json!({ "kind": "unchanged", "resultId": report_b["resultId"] }),
            "a clean publication must also survive a later refresh of another document"
        );
        assert_eq!(harness.managed.snapshots.load(Ordering::SeqCst), 2);
        assert_eq!(harness.runtime.started.load(Ordering::SeqCst), 2);

        harness
            .broker
            .lock()
            .await
            .set_language_enabled("fake", false);
        let stale = harness.pull(&a, report_a["resultId"].as_str());
        assert_eq!(stale["error"]["code"], -32802, "{stale}");
        assert!(stale.get("result").is_none());
        harness.runtime.complete_next().await;
        let disabled = harness.pull(&a, report_a["resultId"].as_str());
        assert_eq!(disabled["result"]["kind"], "full", "{disabled}");
        assert_eq!(disabled["result"]["items"], json!([]));
        assert_ne!(disabled["result"]["resultId"], report_a["resultId"]);
        assert_eq!(harness.managed.snapshots.load(Ordering::SeqCst), 3);
        assert_eq!(harness.runtime.started.load(Ordering::SeqCst), 3);
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
            Arc::new(PublishingWorkspaceIndex::new(WorkspaceIndexDrift::Publish)),
            Arc::new(FixedManagedDiagnostics::default()),
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

    struct MismatchedManagedDiagnostics;

    impl ManagedDiagnosticSnapshotPort for MismatchedManagedDiagnostics {
        fn current_authority(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<ManifestDigest, LspRuntimeFailure>> {
            Box::pin(async { Ok(digest('c')) })
        }

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
            Arc::new(PublishingWorkspaceIndex::new(WorkspaceIndexDrift::None)),
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

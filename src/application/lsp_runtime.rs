//! PR12 LSP composition over the canonical feedback runtime.
//!
//! The adapter mints authorized reads through [`Pr12FeedbackRuntime`] and
//! invokes its daemon owner. The cloned [`ProjectFeedbackStore`] is the same
//! durable publication/dedupe authority used by the Plan 09 cycle; this module
//! creates no feedback store, cache, cursor codec, or diagnostic authority.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cap_std::ambient_authority;
use cap_std::fs::{Dir, File};
use same_file::Handle;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as AsyncMutex;
use tracedecay_application::feedback::{
    FeedbackDiagnosticsReadRequestV1, FeedbackDiagnosticsReadResultV1, FeedbackFindingReadV1,
    FeedbackListResultV1,
};
use tracedecay_application::{ApplicationOutcome, OperationTermination};
use tracedecay_domain::feedback::{
    FeedbackCycleResultV1, FeedbackFindingV1, FeedbackImpactStateV1, ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, DiagnosticRecordStateV1, DiagnosticSeverityV1, UtcMicros,
};
use tracedecay_store::DiagnosticStore as _;
use url::Url;

use crate::application::feedback::concrete::{
    ConcretePr12FeedbackOwner, Pr12FeedbackRuntime, ProjectFeedbackStore,
};
use crate::application::feedback::owner::{
    FeedbackReadInvocationResultV1, FeedbackReadOperationV1,
};
use crate::application::operation_stream::{
    ManagedTestRunSnapshot, OperationEventAuthority, OperationEventError, operation_event_authority,
};
use crate::daemon::lsp_gateway::AnalyzerSemanticAdapter;
use crate::daemon::lsp_gateway::LspAnalyzerCancellationAuthority;
use crate::daemon::lsp_gateway::{
    AdmittedRoot, AnalyzerState, BrokerDiagnosticSnapshotAuthority,
    CanonicalContextProjectionAuthority, CanonicalDiagnosticRefreshRequest, ContextCoverage,
    ContextProjectionChange, ContextProjectionEnvelope, ContextProjectionItem,
    ContextProjectionKind, ContextProjectionOutcome, ContextProjectionRegistration,
    ContextProjectionRequest, DiagnosticSeverity, DiagnosticSource, FeedbackCycleRequest,
    FeedbackCycleRuntimePort, GatewayCapabilities, GatewayDiagnostic, LspDiagnosticDocumentPort,
    LspRuntimeFailure, LspRuntimeFuture, MAX_CONTEXT_PROJECTION_ITEMS,
    MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES, MAX_CONTEXT_SUMMARY_BYTES, ManagedDiagnosticSnapshot,
    ManagedDiagnosticSnapshotPort, Pr12LspSessionFactory, SemanticProviderPort,
    TRACEDECAY_CONTEXT_REVISION, UpstreamCapabilities, byte_offset_to_utf16_position,
};
use crate::db::Database;
use crate::diagnostics::lsp::adapters::builtin_adapters;
use crate::diagnostics::lsp::broker::DiagnosticBroker;
use crate::diagnostics::lsp::client::LspDocument;
use crate::diagnostics_store::DiagnosticsStore;

/// Current canonical Git/graph address for an admitted LSP root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspFeedbackProjectionScope {
    pub head_commit_id: CommitId,
    pub code_generation_id: CodeGenerationId,
    pub generation: u64,
}

/// Resolves current scope through the existing admitted Git/graph owner.
pub trait LspFeedbackProjectionScopePort: Send + Sync {
    fn resolve(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
    ) -> LspRuntimeFuture<Result<LspFeedbackProjectionScope, LspRuntimeFailure>>;
}

/// Exact registered project/root authority used by production LSP sessions.
///
/// Project identity and authorization scope come from the already-registered
/// feedback runtime. Current generation comes from the canonical diagnostics
/// store and HEAD comes from the admitted repository root.
#[derive(Clone)]
pub struct RegisteredProjectLspAuthority {
    feedback: Arc<Pr12FeedbackRuntime>,
    publications: ProjectFeedbackStore,
    database: Database,
    project_root: PathBuf,
    project_dir: Arc<Dir>,
    root_uri: Url,
}

impl RegisteredProjectLspAuthority {
    pub fn new(
        feedback: Arc<Pr12FeedbackRuntime>,
        database: Database,
    ) -> Result<Self, LspRuntimeFailure> {
        let project_root = feedback
            .project_root()
            .canonicalize()
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-unavailable"))?;
        let root_uri = Url::from_directory_path(&project_root)
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-invalid"))?;
        let project_dir = Dir::open_ambient_dir(&project_root, ambient_authority())
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-unavailable"))?;
        let path_handle = Handle::from_path(&project_root)
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-unavailable"))?;
        let directory_handle = project_dir
            .try_clone()
            .map(Dir::into_std_file)
            .and_then(Handle::from_file)
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-unavailable"))?;
        if path_handle != directory_handle {
            return Err(LspRuntimeFailure::new(
                "registered-project-root-unavailable",
            ));
        }
        let publications = feedback.publication_store();
        Ok(Self {
            feedback,
            publications,
            database,
            project_root,
            project_dir: Arc::new(project_dir),
            root_uri,
        })
    }

    pub fn publication_store(&self) -> ProjectFeedbackStore {
        self.publications.clone()
    }

    fn validate_root(&self, root: &AdmittedRoot) -> Result<(), LspRuntimeFailure> {
        let path = strict_file_url(root.uri())
            .and_then(|url| {
                url.to_file_path()
                    .map_err(|_| LspRuntimeFailure::new("registered-project-root-mismatch"))
            })
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-mismatch"))?;
        let same_root = same_file::is_same_file(path, &self.project_root).unwrap_or(false);
        same_root
            .then_some(())
            .ok_or_else(|| LspRuntimeFailure::new("registered-project-root-mismatch"))
    }

    fn document_path(&self, document_uri: &str) -> Result<(PathBuf, String), LspRuntimeFailure> {
        let document = validated_document_path(
            &self.project_root,
            &self.root_uri,
            &self.project_dir,
            document_uri,
        )?;
        let relative_path = document
            .relative
            .to_str()
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| LspRuntimeFailure::new("document-path-invalid"))?;
        Ok((document.absolute, relative_path))
    }

    async fn read_disk_document(&self, relative: &Path) -> Result<String, LspRuntimeFailure> {
        let (_canonical, file) = open_project_file(&self.project_dir, relative)?;
        let mut file = tokio::fs::File::from_std(file.into_std());
        let mut text = String::new();
        file.read_to_string(&mut text)
            .await
            .map_err(|_| LspRuntimeFailure::new("document-unavailable"))?;
        Ok(text)
    }

    async fn current_scope(&self) -> Result<LspFeedbackProjectionScope, LspRuntimeFailure> {
        self.feedback
            .scope()
            .validate()
            .map_err(|_| LspRuntimeFailure::new("registered-project-scope-invalid"))?;
        let repository = gix::open(&self.project_root)
            .map_err(|_| LspRuntimeFailure::new("registered-repository-unavailable"))?;
        let head_commit_id = repository
            .head_commit()
            .ok()
            .and_then(|commit| CommitId::new(commit.id().to_hex().to_string()).ok())
            .ok_or_else(|| LspRuntimeFailure::new("registered-head-unavailable"))?;
        if self
            .database
            .get_metadata("last_synced_commit")
            .await
            .map_err(|_| LspRuntimeFailure::new("registered-generation-watermark-failed"))?
            .as_deref()
            != Some(head_commit_id.as_str())
        {
            return Err(LspRuntimeFailure::new("registered-generation-not-current"));
        }
        let code_generation_id = DiagnosticsStore::new(self.database.conn())
            .current_generation()
            .await
            .map_err(|_| LspRuntimeFailure::new("current-generation-read-failed"))?
            .ok_or_else(|| LspRuntimeFailure::new("current-generation-unavailable"))?;
        let generation = generation_sequence(&code_generation_id)
            .ok_or_else(|| LspRuntimeFailure::new("current-generation-invalid"))?;
        Ok(LspFeedbackProjectionScope {
            head_commit_id,
            code_generation_id,
            generation,
        })
    }
}

impl LspFeedbackProjectionScopePort for RegisteredProjectLspAuthority {
    fn resolve(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
    ) -> LspRuntimeFuture<Result<LspFeedbackProjectionScope, LspRuntimeFailure>> {
        let authority = self.clone();
        Box::pin(async move {
            authority.validate_root(&root)?;
            if let Some(document_uri) = document_uri {
                authority.document_path(&document_uri)?;
            }
            authority.current_scope().await
        })
    }
}

impl LspDiagnosticDocumentPort for RegisteredProjectLspAuthority {
    fn load_document(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<LspDocument, LspRuntimeFailure>> {
        let authority = self.clone();
        Box::pin(async move {
            authority.validate_root(&request.root)?;
            let (path, relative_path) = authority.document_path(&request.document_uri)?;
            let relative = Path::new(&relative_path);
            let (language, language_id, text) = match request.overlay {
                Some(overlay)
                    if overlay.ephemeral
                        && overlay.uri == request.document_uri
                        && !overlay.language_id.is_empty() =>
                {
                    let adapter = builtin_adapters()
                        .into_iter()
                        .find(|adapter| adapter.language_id == overlay.language_id)
                        .ok_or_else(|| {
                            LspRuntimeFailure::new("document-language-not-registered")
                        })?;
                    (adapter.language, adapter.language_id, overlay.text)
                }
                Some(_) => return Err(LspRuntimeFailure::new("document-overlay-invalid")),
                None => {
                    let adapter = adapter_for_path(&path).ok_or_else(|| {
                        LspRuntimeFailure::new("document-language-not-registered")
                    })?;
                    let text = authority.read_disk_document(relative).await?;
                    (adapter.language, adapter.language_id, text)
                }
            };
            Ok(LspDocument {
                language,
                language_id,
                relative_path,
                text,
            })
        })
    }
}

impl LspFeedbackDocumentSnapshotPort for RegisteredProjectLspAuthority {
    fn snapshot(
        &self,
        root: AdmittedRoot,
        document_uri: String,
    ) -> LspRuntimeFuture<Result<LspFeedbackDocumentSnapshot, LspRuntimeFailure>> {
        let authority = self.clone();
        Box::pin(async move {
            authority.validate_root(&root)?;
            let (_, relative_path) = authority.document_path(&document_uri)?;
            let text = authority
                .read_disk_document(Path::new(&relative_path))
                .await?;
            Ok(LspFeedbackDocumentSnapshot { text })
        })
    }
}

/// Hydrates canonical feedback finding anchors through the existing
/// diagnostics/source owner and performs exact UTF-16 projection.
pub trait LspFeedbackDiagnosticProjectionPort: Send + Sync {
    fn project(
        &self,
        root: AdmittedRoot,
        document_uri: String,
        scope: LspFeedbackProjectionScope,
        cycle: FeedbackCycleResultV1,
    ) -> LspRuntimeFuture<Result<Vec<GatewayDiagnostic>, LspRuntimeFailure>>;
}

/// Canonical source identity and text needed to project byte-addressed
/// generation diagnostics into negotiated UTF-16 LSP ranges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspFeedbackDocumentSnapshot {
    pub text: String,
}

pub trait LspFeedbackDocumentSnapshotPort: Send + Sync {
    fn snapshot(
        &self,
        root: AdmittedRoot,
        document_uri: String,
    ) -> LspRuntimeFuture<Result<LspFeedbackDocumentSnapshot, LspRuntimeFailure>>;
}

/// Real finding-anchor hydration over the canonical managed diagnostics store.
pub struct DiagnosticsStoreLspFeedbackProjection<S> {
    database: Database,
    documents: Arc<S>,
}

impl<S> DiagnosticsStoreLspFeedbackProjection<S> {
    pub fn new(database: Database, documents: Arc<S>) -> Self {
        Self {
            database,
            documents,
        }
    }
}

impl<S> LspFeedbackDiagnosticProjectionPort for DiagnosticsStoreLspFeedbackProjection<S>
where
    S: LspFeedbackDocumentSnapshotPort + 'static,
{
    fn project(
        &self,
        root: AdmittedRoot,
        document_uri: String,
        scope: LspFeedbackProjectionScope,
        cycle: FeedbackCycleResultV1,
    ) -> LspRuntimeFuture<Result<Vec<GatewayDiagnostic>, LspRuntimeFailure>> {
        let database = self.database.clone();
        let documents = Arc::clone(&self.documents);
        Box::pin(async move {
            let document = documents.snapshot(root, document_uri.clone()).await?;
            let store = DiagnosticsStore::new(database.conn());
            let mut diagnostics = Vec::new();
            for finding in cycle.findings {
                let Some(anchor) = finding.retrieval_anchor_id else {
                    continue;
                };
                let Some(record) = store
                    .diagnostic_by_anchor(&anchor)
                    .await
                    .map_err(|_| LspRuntimeFailure::new("diagnostic-anchor-read-failed"))?
                else {
                    continue;
                };
                let target_file = cycle.impact.as_ref().map(|impact| &impact.target.file);
                if target_file != Some(&record.file_occurrence_id)
                    || record.generation_id != scope.code_generation_id
                    || !matches!(record.state, DiagnosticRecordStateV1::Current)
                    || record
                        .source_revision
                        .as_ref()
                        .is_some_and(|revision| revision != &cycle.scope.head_commit_id)
                {
                    continue;
                }
                let start = usize::try_from(record.span.start_byte)
                    .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                let end = usize::try_from(record.span.end_byte)
                    .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                let start = byte_offset_to_utf16_position(&document.text, start)
                    .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                let end = byte_offset_to_utf16_position(&document.text, end)
                    .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                diagnostics.push(GatewayDiagnostic {
                    uri: document_uri.clone(),
                    range: crate::daemon::lsp_gateway::LspRange { start, end },
                    severity: Some(match record.severity {
                        DiagnosticSeverityV1::Error => DiagnosticSeverity::Error,
                        DiagnosticSeverityV1::Warning => DiagnosticSeverity::Warning,
                        DiagnosticSeverityV1::Information => DiagnosticSeverity::Information,
                        DiagnosticSeverityV1::Hint => DiagnosticSeverity::Hint,
                    }),
                    code: Some(record.code),
                    message: record.message,
                    source: DiagnosticSource::TraceDecay,
                });
            }
            Ok(diagnostics)
        })
    }
}

/// Real test execution result projection owner. Feedback impact owns affected
/// test identities; execution results remain in their separate canonical
/// owner and are not copied into the feedback publication ledger.
pub trait LspTestRunProjectionPort: Send + Sync {
    fn snapshot(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
    ) -> LspRuntimeFuture<ContextProjectionOutcome>;

    fn poll_changes(
        &self,
        _root: &AdmittedRoot,
        _subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        Vec::new()
    }
}

#[derive(Clone)]
pub struct OperationEventTestRunProjection {
    events: OperationEventAuthority,
    scope: Arc<dyn LspFeedbackProjectionScopePort>,
}

impl OperationEventTestRunProjection {
    pub fn new(
        events: OperationEventAuthority,
        scope: Arc<dyn LspFeedbackProjectionScopePort>,
    ) -> Self {
        Self { events, scope }
    }
}

pub fn lsp_test_result_port(
    scope: Arc<dyn LspFeedbackProjectionScopePort>,
) -> Arc<dyn LspTestRunProjectionPort> {
    Arc::new(OperationEventTestRunProjection::new(
        operation_event_authority(),
        scope,
    ))
}

impl LspTestRunProjectionPort for OperationEventTestRunProjection {
    fn snapshot(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
    ) -> LspRuntimeFuture<ContextProjectionOutcome> {
        let projection = self.clone();
        Box::pin(async move {
            if let Err(error) = projection
                .scope
                .resolve(root.clone(), document_uri.clone())
                .await
            {
                return ContextProjectionOutcome::Deferred {
                    reason: error.class().to_owned(),
                };
            }
            match projection.events.latest_managed_test_run(root.uri()).await {
                Ok(snapshot) => test_run_projection(root, document_uri, snapshot),
                Err(OperationEventError::FrontierExpired) => ContextProjectionOutcome::Deferred {
                    reason: "managed-test-run-frontier-expired".to_owned(),
                },
                Err(_) => ContextProjectionOutcome::Failed {
                    reason: "managed-test-run-projection-failed".to_owned(),
                },
            }
        })
    }
}

/// Shared feedback source mounted as both `FeedbackCyclePort` and the managed
/// diagnostics/context authority in [`Pr12LspRuntimeAdapters`].
#[derive(Clone)]
pub struct ConcretePr12FeedbackLspSource {
    runtime: Arc<Pr12FeedbackRuntime>,
    owner: Arc<ConcretePr12FeedbackOwner>,
    publications: ProjectFeedbackStore,
    cycle: Arc<dyn FeedbackCycleRuntimePort>,
    scope: Arc<dyn LspFeedbackProjectionScopePort>,
    diagnostic_projection: Arc<dyn LspFeedbackDiagnosticProjectionPort>,
    test_runs: Arc<dyn LspTestRunProjectionPort>,
    next_request: Arc<AtomicU64>,
}

impl ConcretePr12FeedbackLspSource {
    pub fn new<F>(
        runtime: Arc<Pr12FeedbackRuntime>,
        cycle: F,
        scope: Arc<dyn LspFeedbackProjectionScopePort>,
        diagnostic_projection: Arc<dyn LspFeedbackDiagnosticProjectionPort>,
        test_runs: Arc<dyn LspTestRunProjectionPort>,
    ) -> Self
    where
        F: FnOnce(ProjectFeedbackStore) -> Arc<dyn FeedbackCycleRuntimePort>,
    {
        let owner = runtime.owner();
        let publications = runtime.publication_store();
        let cycle = cycle(publications.clone());
        Self {
            runtime,
            owner,
            publications,
            cycle,
            scope,
            diagnostic_projection,
            test_runs,
            next_request: Arc::new(AtomicU64::new(1)),
        }
    }

    /// The exact store clone supplied to the Plan 09 cycle dedupe/publication
    /// boundary. Exposing it lets the daemon composition root prove both
    /// surfaces use one authority.
    pub fn publication_store(&self) -> ProjectFeedbackStore {
        self.publications.clone()
    }

    async fn current_cycle(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
    ) -> Result<(LspFeedbackProjectionScope, FeedbackDiagnosticsReadResultV1), LspRuntimeFailure>
    {
        let scope = self.scope.resolve(root, document_uri).await?;
        let observed_at = now_micros();
        let sequence = self.next_request.fetch_add(1, Ordering::Relaxed);
        let handle = self
            .runtime
            .mint_diagnostics(
                format!("lsp-feedback-diagnostics-{sequence}"),
                FeedbackDiagnosticsReadRequestV1 {
                    head_commit_id: scope.head_commit_id.clone(),
                },
                observed_at,
            )
            .map_err(|_| LspRuntimeFailure::new("feedback-request-mint-failed"))?;
        let result = self
            .owner
            .invoke(FeedbackReadOperationV1::Diagnostics, &handle, observed_at)
            .await
            .map_err(|_| LspRuntimeFailure::new("feedback-read-unavailable"))?;
        let FeedbackReadInvocationResultV1::Diagnostics(result) = result else {
            return Err(LspRuntimeFailure::new("feedback-read-kind-mismatch"));
        };
        let envelope = result.map_err(|_| LspRuntimeFailure::new("feedback-read-failed"))?;
        let ApplicationOutcome::Evidence(evidence) = envelope.outcome else {
            return Err(LspRuntimeFailure::new("feedback-read-outcome-invalid"));
        };
        if evidence.execution.termination != OperationTermination::Completed {
            return Err(LspRuntimeFailure::new("feedback-read-incomplete"));
        }
        let payload = evidence
            .payload
            .ok_or_else(|| LspRuntimeFailure::new("feedback-current-result-unavailable"))?;
        Ok((scope, payload))
    }

    async fn current_finding_items(
        &self,
        scope: &LspFeedbackProjectionScope,
    ) -> Result<Vec<ContextProjectionItem>, LspRuntimeFailure> {
        let observed_at = now_micros();
        let sequence = self.next_request.fetch_add(1, Ordering::Relaxed);
        let handle = self
            .runtime
            .mint_list(
                format!("lsp-feedback-list-{sequence}"),
                Some(scope.head_commit_id.clone()),
                MAX_CONTEXT_PROJECTION_ITEMS as u32,
                observed_at,
            )
            .map_err(|_| LspRuntimeFailure::new("feedback-list-mint-failed"))?;
        let result = self
            .owner
            .invoke(FeedbackReadOperationV1::List, &handle, observed_at)
            .await
            .map_err(|_| LspRuntimeFailure::new("feedback-list-unavailable"))?;
        let FeedbackReadInvocationResultV1::List(result) = result else {
            return Err(LspRuntimeFailure::new("feedback-list-kind-mismatch"));
        };
        let envelope = result.map_err(|_| LspRuntimeFailure::new("feedback-list-failed"))?;
        let ApplicationOutcome::Evidence(evidence) = envelope.outcome else {
            return Err(LspRuntimeFailure::new("feedback-list-outcome-invalid"));
        };
        if evidence.execution.termination != OperationTermination::Completed {
            return Err(LspRuntimeFailure::new("feedback-list-incomplete"));
        }
        let FeedbackListResultV1 { findings } = evidence
            .payload
            .ok_or_else(|| LspRuntimeFailure::new("feedback-list-result-unavailable"))?;
        Ok(findings.into_iter().filter_map(finding_read_item).collect())
    }
}

impl FeedbackCycleRuntimePort for ConcretePr12FeedbackLspSource {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        self.cycle.execute(request)
    }
}

impl ManagedDiagnosticSnapshotPort for ConcretePr12FeedbackLspSource {
    fn snapshot(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<ManagedDiagnosticSnapshot, LspRuntimeFailure>> {
        let source = self.clone();
        Box::pin(async move {
            let (scope, result) = source
                .current_cycle(request.root.clone(), Some(request.document_uri.clone()))
                .await?;
            let diagnostics = source
                .diagnostic_projection
                .project(
                    request.root,
                    request.document_uri,
                    scope.clone(),
                    result.cycle,
                )
                .await?;
            Ok(ManagedDiagnosticSnapshot {
                generation: scope.generation,
                diagnostics,
            })
        })
    }
}

impl CanonicalContextProjectionAuthority for ConcretePr12FeedbackLspSource {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        [
            ContextProjectionKind::diagnostics(),
            ContextProjectionKind::post_edit_impact(),
            ContextProjectionKind::affected_tests(),
            ContextProjectionKind::test_run_results(),
        ]
        .into_iter()
        .map(|kind| ContextProjectionRegistration {
            kind,
            revision: TRACEDECAY_CONTEXT_REVISION,
        })
        .collect()
    }

    fn snapshot(
        &self,
        root: AdmittedRoot,
        _request_id: crate::daemon::lsp_gateway::LspRequestId,
        request: ContextProjectionRequest,
    ) -> LspRuntimeFuture<ContextProjectionOutcome> {
        if request.kind == ContextProjectionKind::test_run_results() {
            return self.test_runs.snapshot(root, request.document_uri);
        }
        let source = self.clone();
        Box::pin(async move {
            let (scope, result) = match source
                .current_cycle(root.clone(), request.document_uri.clone())
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    return ContextProjectionOutcome::Deferred {
                        reason: error.class().to_owned(),
                    };
                }
            };
            let cycle = result.cycle;
            let (coverage, items, omitted_count) =
                if request.kind == ContextProjectionKind::diagnostics() {
                    let items = match source.current_finding_items(&scope).await {
                        Ok(items) => items,
                        Err(error) => {
                            return ContextProjectionOutcome::Deferred {
                                reason: error.class().to_owned(),
                            };
                        }
                    };
                    let omitted_count =
                        cycle.total_findings.saturating_sub(items.len() as u64) as usize;
                    (cycle_coverage(&cycle), items, omitted_count)
                } else if request.kind == ContextProjectionKind::post_edit_impact() {
                    impact_projection(&cycle)
                } else if request.kind == ContextProjectionKind::affected_tests() {
                    affected_test_projection(&cycle)
                } else {
                    return ContextProjectionOutcome::Unsupported;
                };
            ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
                root_uri: root.uri().to_owned(),
                document_uri: request.document_uri,
                kind: request.kind,
                generation: scope.generation,
                coverage,
                revision: TRACEDECAY_CONTEXT_REVISION,
                items,
                omitted_count,
                retrieval_handle: None,
            })
        })
    }

    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        self.test_runs.poll_changes(root, subscriptions)
    }
}

/// Mount-ready bundle construction. The same concrete feedback source is
/// shared by cycle triggers, managed diagnostics, and context projections.
#[allow(clippy::too_many_arguments)]
pub fn pr12_lsp_session_factory<F, U, G>(
    runtime: tokio::runtime::Handle,
    feedback_runtime: Arc<Pr12FeedbackRuntime>,
    database: Database,
    feedback_cycle: F,
    analyzer_state: AnalyzerState,
    upstream_semantics: U,
    graph_semantics: G,
    diagnostic_broker: Arc<AsyncMutex<DiagnosticBroker>>,
    diagnostics_quiet_window: Duration,
    cancellation: Arc<dyn LspAnalyzerCancellationAuthority>,
    gateway_capabilities: GatewayCapabilities,
    upstream_capabilities: UpstreamCapabilities,
) -> Result<Pr12LspSessionFactory, LspRuntimeFailure>
where
    F: FnOnce(ProjectFeedbackStore) -> Arc<dyn FeedbackCycleRuntimePort>,
    U: SemanticProviderPort + Send + Sync + 'static,
    G: SemanticProviderPort + Send + Sync + 'static,
{
    let project = Arc::new(RegisteredProjectLspAuthority::new(
        feedback_runtime.clone(),
        database.clone(),
    )?);
    let test_runs = lsp_test_result_port(project.clone());
    let diagnostic_projection = Arc::new(DiagnosticsStoreLspFeedbackProjection::new(
        database,
        project.clone(),
    ));
    let feedback = Arc::new(ConcretePr12FeedbackLspSource::new(
        feedback_runtime,
        feedback_cycle,
        project.clone(),
        diagnostic_projection,
        test_runs,
    ));
    let diagnostics = Arc::new(BrokerDiagnosticSnapshotAuthority::new(
        diagnostic_broker,
        project,
        feedback.clone(),
        diagnostics_quiet_window,
    ));
    Ok(Pr12LspSessionFactory::new(
        runtime,
        feedback.clone(),
        Arc::new(AnalyzerSemanticAdapter::new(
            analyzer_state,
            upstream_semantics,
            graph_semantics,
        )),
        diagnostics,
        cancellation,
        feedback,
        gateway_capabilities,
        upstream_capabilities,
    ))
}

fn test_run_projection(
    root: AdmittedRoot,
    document_uri: Option<String>,
    snapshot: ManagedTestRunSnapshot,
) -> ContextProjectionOutcome {
    let Some(termination) = snapshot.termination else {
        return ContextProjectionOutcome::Pending;
    };
    let omitted_count = usize::try_from(
        snapshot
            .completed
            .saturating_sub(snapshot.results.len() as u64)
            .saturating_add(
                snapshot
                    .results
                    .len()
                    .saturating_sub(MAX_CONTEXT_PROJECTION_ITEMS) as u64,
            ),
    )
    .unwrap_or(usize::MAX);
    let operation_id = snapshot.operation_id.to_string();
    let items = snapshot
        .results
        .into_iter()
        .take(MAX_CONTEXT_PROJECTION_ITEMS)
        .enumerate()
        .map(|(index, result)| ContextProjectionItem {
            stable_id: format!("{operation_id}.{index}"),
            summary: bounded_test_run_summary(&result.test, result.passed),
            retrieval_handle: None,
        })
        .collect();
    let coverage = match termination {
        OperationTermination::Completed
            if snapshot
                .total
                .is_none_or(|total| snapshot.completed == total)
                && omitted_count == 0 =>
        {
            ContextCoverage::Complete
        }
        OperationTermination::Completed | OperationTermination::Partial => ContextCoverage::Partial,
        OperationTermination::Failed => ContextCoverage::Failed,
        OperationTermination::Cancelled | OperationTermination::TimedOut => {
            ContextCoverage::Partial
        }
        OperationTermination::EffectUnknown => ContextCoverage::Unavailable,
    };
    ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
        root_uri: root.uri().to_owned(),
        document_uri,
        kind: ContextProjectionKind::test_run_results(),
        generation: snapshot.generation,
        coverage,
        revision: TRACEDECAY_CONTEXT_REVISION,
        items,
        omitted_count,
        retrieval_handle: None,
    })
}

fn bounded_test_run_summary(test: &str, passed: bool) -> String {
    let prefix = if passed { "passed: " } else { "failed: " };
    let mut end = test
        .len()
        .min(MAX_CONTEXT_SUMMARY_BYTES.saturating_sub(prefix.len()));
    while !test.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{prefix}{}", &test[..end])
}

fn finding_item(finding: &FeedbackFindingV1) -> Option<ContextProjectionItem> {
    projection_item(
        finding.finding_id.as_str(),
        finding
            .safe_bounded_preview
            .clone()
            .unwrap_or_else(|| "feedback finding".to_owned()),
    )
}

fn finding_read_item(finding: FeedbackFindingReadV1) -> Option<ContextProjectionItem> {
    let retrieval_handle = finding
        .expand_handle
        .as_ref()
        .unwrap_or(&finding.get_handle)
        .as_str()
        .to_owned();
    let mut item = finding_item(&finding.finding)?;
    item.retrieval_handle = Some(retrieval_handle);
    Some(item)
}

fn impact_projection(
    cycle: &FeedbackCycleResultV1,
) -> (ContextCoverage, Vec<ContextProjectionItem>, usize) {
    let Some(impact) = cycle.impact.as_ref() else {
        return (impact_coverage(cycle.impact_state), Vec::new(), 0);
    };
    let total_items = impact.affected_files.len() + impact.affected_callers.len();
    let mut items = impact
        .affected_files
        .iter()
        .filter_map(|file| projection_item(file.as_str(), "affected file"))
        .chain(
            impact
                .affected_callers
                .iter()
                .filter_map(|caller| projection_item(caller.as_str(), "affected caller")),
        )
        .collect::<Vec<_>>();
    let omitted_count = total_items.saturating_sub(items.len().min(MAX_CONTEXT_PROJECTION_ITEMS));
    items.truncate(MAX_CONTEXT_PROJECTION_ITEMS);
    (impact_coverage(Some(impact.state)), items, omitted_count)
}

fn affected_test_projection(
    cycle: &FeedbackCycleResultV1,
) -> (ContextCoverage, Vec<ContextProjectionItem>, usize) {
    let Some(impact) = cycle.impact.as_ref() else {
        return (impact_coverage(cycle.affected_tests_state), Vec::new(), 0);
    };
    let total_items = impact.affected_tests.len();
    let mut items = impact
        .affected_tests
        .iter()
        .filter_map(|test| projection_item(test.as_str(), "affected test"))
        .collect::<Vec<_>>();
    let omitted_count = total_items.saturating_sub(items.len().min(MAX_CONTEXT_PROJECTION_ITEMS));
    items.truncate(MAX_CONTEXT_PROJECTION_ITEMS);
    (
        impact_coverage(Some(impact.affected_tests_state)),
        items,
        omitted_count,
    )
}

fn projection_item(stable_id: &str, summary: impl Into<String>) -> Option<ContextProjectionItem> {
    (stable_id.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES).then(|| ContextProjectionItem {
        stable_id: stable_id.to_owned(),
        summary: summary.into(),
        retrieval_handle: None,
    })
}

fn cycle_coverage(cycle: &FeedbackCycleResultV1) -> ContextCoverage {
    if cycle.omitted_findings == 0
        && !cycle.provider_states.is_empty()
        && cycle
            .provider_states
            .iter()
            .all(|state| *state == ProviderEvaluationStateV1::SupportedCompletedComplete)
    {
        ContextCoverage::Complete
    } else if cycle.provider_states.iter().all(|state| {
        matches!(
            state,
            ProviderEvaluationStateV1::Unsupported
                | ProviderEvaluationStateV1::Absent
                | ProviderEvaluationStateV1::Unavailable
        )
    }) {
        ContextCoverage::Unavailable
    } else {
        ContextCoverage::Partial
    }
}

fn impact_coverage(state: Option<FeedbackImpactStateV1>) -> ContextCoverage {
    match state {
        Some(FeedbackImpactStateV1::Complete) => ContextCoverage::Complete,
        Some(FeedbackImpactStateV1::Partial | FeedbackImpactStateV1::Stale) => {
            ContextCoverage::Partial
        }
        Some(FeedbackImpactStateV1::Unavailable) | None => ContextCoverage::Unavailable,
    }
}

fn adapter_for_path(
    path: &Path,
) -> Option<crate::diagnostics::lsp::adapters::LspAdapterDefinition> {
    let extension = path.extension()?.to_str()?;
    builtin_adapters().into_iter().find(|adapter| {
        adapter.extensions.iter().any(|candidate| {
            candidate
                .strip_prefix('.')
                .unwrap_or(candidate)
                .eq_ignore_ascii_case(extension)
        })
    })
}

#[derive(Debug, Eq, PartialEq)]
struct ValidatedDocumentPath {
    absolute: PathBuf,
    relative: PathBuf,
}

fn strict_file_url(uri: &str) -> Result<Url, LspRuntimeFailure> {
    let (_, after_scheme) = uri
        .split_once(':')
        .ok_or_else(|| LspRuntimeFailure::new("document-uri-invalid"))?;
    if after_scheme.contains('\\') {
        return Err(LspRuntimeFailure::new("document-uri-invalid"));
    }
    let raw_path = if let Some(authority_and_path) = after_scheme.strip_prefix("//") {
        authority_and_path
            .find('/')
            .map(|path_start| &authority_and_path[path_start..])
            .unwrap_or("")
    } else {
        after_scheme
    };
    validate_raw_uri_path(raw_path)?;

    let url = Url::parse(uri).map_err(|_| LspRuntimeFailure::new("document-uri-invalid"))?;
    if url.scheme() != "file"
        || url.cannot_be_a_base()
        || url.path().is_empty()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(LspRuntimeFailure::new("document-uri-invalid"));
    }
    Ok(url)
}

fn validate_raw_uri_path(raw_path: &str) -> Result<(), LspRuntimeFailure> {
    if raw_path.is_empty() || raw_path.as_bytes().contains(&0) {
        return Err(LspRuntimeFailure::new("document-uri-invalid"));
    }
    let segments = raw_path.split('/').collect::<Vec<_>>();
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            let is_leading = index == 0;
            let is_trailing = index + 1 == segments.len();
            if !is_leading && !is_trailing {
                return Err(LspRuntimeFailure::new("document-uri-invalid"));
            }
            continue;
        }
        let decoded = decode_uri_segment(segment)?;
        if decoded == b"."
            || decoded == b".."
            || decoded.iter().any(|byte| matches!(*byte, b'/' | b'\\' | 0))
        {
            return Err(LspRuntimeFailure::new("document-uri-invalid"));
        }
    }
    Ok(())
}

fn decode_uri_segment(segment: &str) -> Result<Vec<u8>, LspRuntimeFailure> {
    let source = segment.as_bytes();
    let mut decoded = Vec::with_capacity(source.len());
    let mut index = 0;
    while index < source.len() {
        if source[index] != b'%' {
            decoded.push(source[index]);
            index += 1;
            continue;
        }
        let high = source
            .get(index + 1)
            .copied()
            .and_then(hex_value)
            .ok_or_else(|| LspRuntimeFailure::new("document-uri-invalid"))?;
        let low = source
            .get(index + 2)
            .copied()
            .and_then(hex_value)
            .ok_or_else(|| LspRuntimeFailure::new("document-uri-invalid"))?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    Ok(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn validated_document_path(
    project_root: &Path,
    root_uri: &Url,
    project_dir: &Dir,
    document_uri: &str,
) -> Result<ValidatedDocumentPath, LspRuntimeFailure> {
    let url = strict_file_url(document_uri)?;
    let relative_uri = root_uri
        .make_relative(&url)
        .ok_or_else(|| LspRuntimeFailure::new("document-outside-registered-root"))?;
    if relative_uri.is_empty()
        || relative_uri.starts_with('/')
        || relative_uri == ".."
        || relative_uri.starts_with("../")
    {
        return Err(LspRuntimeFailure::new("document-outside-registered-root"));
    }

    let path = url
        .to_file_path()
        .map_err(|_| LspRuntimeFailure::new("document-uri-invalid"))?;
    let relative = path
        .strip_prefix(project_root)
        .map_err(|_| LspRuntimeFailure::new("document-outside-registered-root"))?;
    validate_relative_path(relative)?;
    let relative = normalize_overlay_relative(project_dir, relative)?;
    validate_relative_path(&relative)?;
    Ok(ValidatedDocumentPath {
        absolute: project_root.join(&relative),
        relative,
    })
}

fn validate_relative_path(path: &Path) -> Result<(), LspRuntimeFailure> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(LspRuntimeFailure::new("document-path-invalid"));
    }
    Ok(())
}

fn normalize_overlay_relative(
    project_dir: &Dir,
    relative: &Path,
) -> Result<PathBuf, LspRuntimeFailure> {
    let mut probe = relative.to_path_buf();
    let mut missing_suffix = Vec::<OsString>::new();
    let mut canonical = loop {
        if probe.as_os_str().is_empty() {
            break PathBuf::new();
        }
        match project_dir.canonicalize(&probe) {
            Ok(path) => break path,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                match project_dir.symlink_metadata(&probe) {
                    Ok(_) => {
                        return Err(LspRuntimeFailure::new("document-outside-registered-root"));
                    }
                    Err(metadata_error) if metadata_error.kind() == ErrorKind::NotFound => {}
                    Err(_) => return Err(LspRuntimeFailure::new("document-path-invalid")),
                }
                let name = probe
                    .file_name()
                    .map(OsString::from)
                    .ok_or_else(|| LspRuntimeFailure::new("document-path-invalid"))?;
                missing_suffix.push(name);
                if !probe.pop() {
                    return Err(LspRuntimeFailure::new("document-path-invalid"));
                }
            }
            Err(_) => {
                return Err(LspRuntimeFailure::new("document-outside-registered-root"));
            }
        }
    };
    for component in missing_suffix.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

fn open_project_file(
    project_dir: &Dir,
    relative: &Path,
) -> Result<(PathBuf, File), LspRuntimeFailure> {
    validate_relative_path(relative)?;
    let canonical = project_dir.canonicalize(relative).map_err(|error| {
        if error.kind() == ErrorKind::PermissionDenied {
            LspRuntimeFailure::new("document-outside-registered-root")
        } else {
            LspRuntimeFailure::new("document-unavailable")
        }
    })?;
    validate_relative_path(&canonical)
        .map_err(|_| LspRuntimeFailure::new("document-outside-registered-root"))?;
    let file = project_dir
        .open(&canonical)
        .map_err(|_| LspRuntimeFailure::new("document-unavailable"))?;
    Ok((canonical, file))
}

fn generation_sequence(generation: &CodeGenerationId) -> Option<u64> {
    generation.as_str().split('.').nth(3)?.parse().ok()
}

fn now_micros() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_micros() as i64);
    UtcMicros(micros)
}

#[cfg(test)]
mod path_tests {
    use std::path::{Component, Path};

    use cap_std::ambient_authority;
    use cap_std::fs::Dir;
    use tempfile::TempDir;
    use url::Url;

    use super::{open_project_file, strict_file_url, validated_document_path};

    fn admitted_root() -> (TempDir, std::path::PathBuf, Url, Dir) {
        let temp = TempDir::new().expect("temporary directory");
        let root = temp.path().join("root");
        std::fs::create_dir(&root).expect("create admitted root");
        let root = root.canonicalize().expect("canonical admitted root");
        let root_url = Url::from_directory_path(&root).expect("root file URI");
        let root_dir =
            Dir::open_ambient_dir(&root, ambient_authority()).expect("open admitted root");
        (temp, root, root_url, root_dir)
    }

    #[test]
    fn document_paths_reject_parent_and_encoded_traversal() {
        let (_temp, root, root_url, root_dir) = admitted_root();
        for suffix in [
            "../outside.rs",
            "%2e%2e/outside.rs",
            "%2E%2E/outside.rs",
            "src/./lib.rs",
            "src/%2e/lib.rs",
        ] {
            let uri = format!("{}{suffix}", root_url.as_str());
            assert!(
                validated_document_path(&root, &root_url, &root_dir, &uri).is_err(),
                "accepted noncanonical URI path {uri}"
            );
        }
    }

    #[test]
    fn document_paths_reject_encoded_separators_and_nul() {
        let (_temp, root, root_url, root_dir) = admitted_root();
        for suffix in [
            "src%2flib.rs",
            "src%2Flib.rs",
            "src%5clib.rs",
            "src%00lib.rs",
        ] {
            let uri = format!("{}{suffix}", root_url.as_str());
            assert!(
                validated_document_path(&root, &root_url, &root_dir, &uri).is_err(),
                "accepted encoded separator or NUL in {uri}"
            );
        }
    }

    #[test]
    fn document_paths_reject_sibling_prefixes_before_join() {
        let (temp, root, root_url, root_dir) = admitted_root();
        let sibling = temp.path().join("root-sibling").join("src").join("lib.rs");
        let sibling_uri = Url::from_file_path(sibling).expect("sibling file URI");
        assert!(
            validated_document_path(&root, &root_url, &root_dir, sibling_uri.as_str()).is_err()
        );
    }

    #[test]
    fn unsaved_overlay_keeps_a_normal_relative_path_without_existing() {
        let (_temp, root, root_url, root_dir) = admitted_root();
        let uri = root_url.join("new/nested/overlay.rs").expect("overlay URI");
        let document =
            validated_document_path(&root, &root_url, &root_dir, uri.as_str()).expect("overlay");

        assert_eq!(document.absolute, root.join("new/nested/overlay.rs"));
        assert_eq!(document.relative, Path::new("new/nested/overlay.rs"));
        assert!(
            document
                .relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        );
    }

    #[cfg(unix)]
    #[test]
    fn disk_document_open_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let (temp, _root, _root_url, root_dir) = admitted_root();
        let outside = temp.path().join("outside.rs");
        std::fs::write(&outside, "fn outside() {}\n").expect("write outside document");
        symlink(&outside, temp.path().join("root").join("escape.rs")).expect("create escape");

        assert!(open_project_file(&root_dir, Path::new("escape.rs")).is_err());
    }

    #[test]
    fn strict_file_uris_preserve_windows_drive_unc_and_path_case() {
        let drive = strict_file_url("FILE:///C:/Workspace/Src/Lib.rs").expect("drive URI");
        let unc = strict_file_url("file://Server/Share/Src/Lib.rs").expect("UNC URI");

        assert!(drive.path().contains("/C:/Workspace/Src/Lib.rs"));
        assert_eq!(unc.host_str(), Some("server"));
        assert!(unc.path().contains("/Share/Src/Lib.rs"));
        assert!(strict_file_url("https://server/Share/Src/Lib.rs").is_err());
        assert!(strict_file_url("file:///C:/Workspace/../escape.rs").is_err());
        assert!(strict_file_url(r"file:///C:\Workspace\Src\Lib.rs").is_err());
    }
}

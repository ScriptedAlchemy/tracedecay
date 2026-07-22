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
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as AsyncMutex;
use tracedecay_application::feedback::{
    FeedbackDiagnosticsReadRequestV1, FeedbackDiagnosticsReadResultV1, FeedbackListResultV1,
};
use tracedecay_application::{ApplicationOutcome, ApplicationResult, OperationTermination};
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
    FeedbackReadInvocationResultV1, FeedbackReadOperationV1, FeedbackReadOwnerErrorV1,
};
use crate::application::operation_stream::{
    ManagedTestRunSnapshot, OperationEventAuthority, OperationEventError, operation_event_authority,
};
use crate::daemon::lsp_gateway::LspAnalyzerCancellationAuthority;
use crate::daemon::lsp_gateway::{
    AdmittedRoot, BrokerDiagnosticSnapshotAuthority, CanonicalContextProjectionAuthority,
    CanonicalDiagnosticRefreshRequest, ContextCoverage, ContextExpansionEnvelope,
    ContextExpansionOutcome, ContextExpansionRequest, ContextExpansionScope,
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
use crate::mcp::response_handles::{
    ResponseHandleLookup, retrieve_response_handle, store_response_handle,
};

const LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION: u16 = 1;

/// Current canonical Git/graph address for an admitted LSP root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspFeedbackProjectionScope {
    pub head_commit_id: CommitId,
    pub code_generation_id: CodeGenerationId,
    pub generation: u64,
}

struct CurrentFeedbackCycle {
    scope: LspFeedbackProjectionScope,
    result: FeedbackDiagnosticsReadResultV1,
    canonical_handle: String,
    observed_at: UtcMicros,
    expires_at: UtcMicros,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredLspContextExpansionV1 {
    schema_version: u16,
    root_uri: String,
    document_uri: Option<String>,
    kind: ContextProjectionKind,
    stable_id: String,
    scope_digest: String,
    head_commit_id: String,
    code_generation_id: String,
    generation: u64,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
    canonical_operation: FeedbackReadOperationV1,
    canonical_handle: String,
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

    fn authorize_projection_root(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let authority = self.clone();
        Box::pin(async move {
            authority.validate_root(&root)?;
            if let Some(document_uri) = document_uri {
                authority.document_path(&document_uri)?;
            }
            Ok(())
        })
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
        let head_commit_id = {
            let repository = gix::open(&self.project_root)
                .map_err(|_| LspRuntimeFailure::new("registered-repository-unavailable"))?;
            repository
                .head_commit()
                .ok()
                .and_then(|commit| CommitId::new(commit.id().to_hex().to_string()).ok())
                .ok_or_else(|| LspRuntimeFailure::new("registered-head-unavailable"))?
        };
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
    project: Arc<RegisteredProjectLspAuthority>,
}

impl OperationEventTestRunProjection {
    pub fn new(
        events: OperationEventAuthority,
        project: Arc<RegisteredProjectLspAuthority>,
    ) -> Self {
        Self { events, project }
    }
}

pub fn lsp_test_result_port(
    project: Arc<RegisteredProjectLspAuthority>,
) -> Arc<dyn LspTestRunProjectionPort> {
    Arc::new(OperationEventTestRunProjection::new(
        operation_event_authority(),
        project,
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
                .project
                .authorize_projection_root(root.clone(), document_uri.clone())
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
    ) -> Result<CurrentFeedbackCycle, LspRuntimeFailure> {
        let scope = self.scope.resolve(root, document_uri).await?;
        let observed_at = now_micros();
        let expires_at = self
            .runtime
            .request_expiry_at(observed_at)
            .map_err(|_| LspRuntimeFailure::new("feedback-request-expiry-unavailable"))?;
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
        Ok(CurrentFeedbackCycle {
            scope,
            result: payload,
            canonical_handle: handle,
            observed_at,
            expires_at,
        })
    }

    async fn current_finding_items(
        &self,
        root: &AdmittedRoot,
        document_uri: Option<&str>,
        scope: &LspFeedbackProjectionScope,
    ) -> Result<Vec<ContextProjectionItem>, LspRuntimeFailure> {
        let observed_at = now_micros();
        let expires_at = self
            .runtime
            .request_expiry_at(observed_at)
            .map_err(|_| LspRuntimeFailure::new("feedback-list-expiry-unavailable"))?;
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
        findings
            .into_iter()
            .filter_map(|finding| {
                let canonical_operation = if finding.expand_handle.is_some() {
                    FeedbackReadOperationV1::Expand
                } else {
                    FeedbackReadOperationV1::Get
                };
                let canonical_handle = finding
                    .expand_handle
                    .as_ref()
                    .unwrap_or(&finding.get_handle)
                    .as_str()
                    .to_owned();
                finding_item(&finding.finding).map(|item| {
                    self.attach_context_handle(
                        root,
                        document_uri,
                        ContextProjectionKind::diagnostics(),
                        scope,
                        observed_at,
                        expires_at,
                        canonical_operation,
                        &canonical_handle,
                        item,
                    )
                })
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn attach_context_handle(
        &self,
        root: &AdmittedRoot,
        document_uri: Option<&str>,
        kind: ContextProjectionKind,
        scope: &LspFeedbackProjectionScope,
        observed_at: UtcMicros,
        expires_at: UtcMicros,
        canonical_operation: FeedbackReadOperationV1,
        canonical_handle: &str,
        mut item: ContextProjectionItem,
    ) -> Result<ContextProjectionItem, LspRuntimeFailure> {
        let record = StoredLspContextExpansionV1 {
            schema_version: LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION,
            root_uri: root.uri().to_owned(),
            document_uri: document_uri.map(str::to_owned),
            kind,
            stable_id: item.stable_id.clone(),
            scope_digest: self.runtime.scope().scope_digest.as_str().to_owned(),
            head_commit_id: scope.head_commit_id.as_str().to_owned(),
            code_generation_id: scope.code_generation_id.as_str().to_owned(),
            generation: scope.generation,
            issued_at: observed_at,
            expires_at,
            canonical_operation,
            canonical_handle: canonical_handle.to_owned(),
        };
        let content = serde_json::to_string(&record)
            .map_err(|_| LspRuntimeFailure::new("context-expansion-handle-invalid"))?;
        let stored = store_response_handle(
            self.runtime.project_root(),
            &content,
            micros_to_seconds(observed_at),
        )
        .map_err(|_| LspRuntimeFailure::new("context-expansion-handle-store-failed"))?;
        item.retrieval_handle = Some(stored.handle);
        Ok(item)
    }

    async fn expand_context(
        &self,
        root: AdmittedRoot,
        request: ContextExpansionRequest,
    ) -> ContextExpansionOutcome {
        let observed_at = now_micros();
        let record = match retrieve_response_handle(
            self.runtime.project_root(),
            &request.retrieval_handle,
            micros_to_seconds(observed_at),
        ) {
            Ok(ResponseHandleLookup::Found(record)) => {
                match serde_json::from_str::<StoredLspContextExpansionV1>(&record.content) {
                    Ok(record) => record,
                    Err(_) => return ContextExpansionOutcome::Denied,
                }
            }
            Ok(ResponseHandleLookup::Missing | ResponseHandleLookup::Expired { .. }) => {
                return ContextExpansionOutcome::Denied;
            }
            Err(_) => {
                return ContextExpansionOutcome::Failed {
                    reason: "context-expansion-handle-unavailable".to_owned(),
                };
            }
        };
        if self.runtime.request_expiry_at(record.issued_at).ok() != Some(record.expires_at) {
            return ContextExpansionOutcome::Denied;
        }
        if !valid_context_expansion_record(
            &record,
            &root,
            self.runtime.scope().scope_digest.as_str(),
            observed_at,
        ) {
            return ContextExpansionOutcome::Denied;
        }
        let current = match self.scope.resolve(root, record.document_uri.clone()).await {
            Ok(scope) => scope,
            Err(error)
                if matches!(
                    error.class(),
                    "registered-generation-not-current"
                        | "registered-head-unavailable"
                        | "current-generation-read-failed"
                        | "current-generation-unavailable"
                        | "current-generation-invalid"
                ) =>
            {
                return ContextExpansionOutcome::Ready(context_expansion_envelope(
                    record,
                    ContextCoverage::Partial,
                    None,
                    Some("scope-revalidation-unavailable".to_owned()),
                ));
            }
            Err(_) => return ContextExpansionOutcome::Denied,
        };
        if !context_expansion_scope_is_current(&record, &current) {
            return ContextExpansionOutcome::Ready(context_expansion_envelope(
                record,
                ContextCoverage::Partial,
                None,
                Some("stale-generation".to_owned()),
            ));
        }
        let invocation = match self
            .owner
            .invoke(
                record.canonical_operation,
                &record.canonical_handle,
                observed_at,
            )
            .await
        {
            Ok(invocation) => invocation,
            Err(FeedbackReadOwnerErrorV1::NotFoundOrNotAuthorized) => {
                return ContextExpansionOutcome::Denied;
            }
            Err(FeedbackReadOwnerErrorV1::Unavailable) => {
                return ContextExpansionOutcome::Ready(context_expansion_envelope(
                    record,
                    ContextCoverage::Partial,
                    None,
                    Some("canonical-feedback-unavailable".to_owned()),
                ));
            }
        };
        let (complete, evidence) =
            match canonical_feedback_value(record.canonical_operation, invocation) {
                Ok(value) => value,
                Err(()) => {
                    return ContextExpansionOutcome::Failed {
                        reason: "context-expansion-kind-mismatch".to_owned(),
                    };
                }
            };
        ContextExpansionOutcome::Ready(context_expansion_envelope(
            record,
            if complete {
                ContextCoverage::Complete
            } else {
                ContextCoverage::Partial
            },
            Some(evidence),
            (!complete).then(|| "canonical-feedback-partial".to_owned()),
        ))
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
            let current = source
                .current_cycle(request.root.clone(), Some(request.document_uri.clone()))
                .await?;
            let scope = current.scope;
            let diagnostics = source
                .diagnostic_projection
                .project(
                    request.root,
                    request.document_uri,
                    scope.clone(),
                    current.result.cycle,
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
            let current = match source
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
            let CurrentFeedbackCycle {
                scope,
                result,
                canonical_handle,
                observed_at,
                expires_at,
            } = current;
            let cycle = result.cycle;
            let kind = request.kind.clone();
            let (coverage, mut items, omitted_count) =
                if request.kind == ContextProjectionKind::diagnostics() {
                    let items = match source
                        .current_finding_items(&root, request.document_uri.as_deref(), &scope)
                        .await
                    {
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
            if kind != ContextProjectionKind::diagnostics() {
                items = match items
                    .into_iter()
                    .map(|item| {
                        source.attach_context_handle(
                            &root,
                            request.document_uri.as_deref(),
                            kind.clone(),
                            &scope,
                            observed_at,
                            expires_at,
                            FeedbackReadOperationV1::Diagnostics,
                            &canonical_handle,
                            item,
                        )
                    })
                    .collect()
                {
                    Ok(items) => items,
                    Err(error) => {
                        return ContextProjectionOutcome::Deferred {
                            reason: error.class().to_owned(),
                        };
                    }
                };
            }
            let retrieval_handle = match source.attach_context_handle(
                &root,
                request.document_uri.as_deref(),
                kind.clone(),
                &scope,
                observed_at,
                expires_at,
                FeedbackReadOperationV1::Diagnostics,
                &canonical_handle,
                ContextProjectionItem {
                    stable_id: "__projection__".to_owned(),
                    summary: String::new(),
                    retrieval_handle: None,
                },
            ) {
                Ok(item) => item.retrieval_handle,
                Err(error) => {
                    return ContextProjectionOutcome::Deferred {
                        reason: error.class().to_owned(),
                    };
                }
            };
            ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
                root_uri: root.uri().to_owned(),
                document_uri: request.document_uri,
                kind,
                generation: scope.generation,
                coverage,
                revision: TRACEDECAY_CONTEXT_REVISION,
                items,
                omitted_count,
                retrieval_handle,
            })
        })
    }

    fn expand(
        &self,
        root: AdmittedRoot,
        _request_id: crate::daemon::lsp_gateway::LspRequestId,
        request: ContextExpansionRequest,
    ) -> LspRuntimeFuture<ContextExpansionOutcome> {
        let source = self.clone();
        Box::pin(async move { source.expand_context(root, request).await })
    }

    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        self.test_runs.poll_changes(root, subscriptions)
    }
}

fn valid_context_expansion_record(
    record: &StoredLspContextExpansionV1,
    root: &AdmittedRoot,
    scope_digest: &str,
    observed_at: UtcMicros,
) -> bool {
    record.schema_version == LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION
        && record.root_uri == root.uri()
        && record
            .document_uri
            .as_deref()
            .is_none_or(|uri| root.contains_document(uri))
        && record.kind.is_valid()
        && !record.stable_id.is_empty()
        && record.stable_id.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES
        && record.scope_digest == scope_digest
        && !record.head_commit_id.is_empty()
        && !record.code_generation_id.is_empty()
        && record.issued_at < record.expires_at
        && record.issued_at <= observed_at
        && observed_at < record.expires_at
        && matches!(
            record.canonical_operation,
            FeedbackReadOperationV1::Diagnostics
                | FeedbackReadOperationV1::Get
                | FeedbackReadOperationV1::Expand
        )
        && !record.canonical_handle.is_empty()
        && record.canonical_handle.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES
        && record
            .canonical_handle
            .bytes()
            .all(|byte| byte.is_ascii_graphic())
}

fn context_expansion_scope_is_current(
    record: &StoredLspContextExpansionV1,
    current: &LspFeedbackProjectionScope,
) -> bool {
    current.head_commit_id.as_str() == record.head_commit_id
        && current.code_generation_id.as_str() == record.code_generation_id
        && current.generation == record.generation
}

fn context_expansion_envelope(
    record: StoredLspContextExpansionV1,
    coverage: ContextCoverage,
    evidence: Option<serde_json::Value>,
    omission_reason: Option<String>,
) -> ContextExpansionEnvelope {
    ContextExpansionEnvelope {
        root_uri: record.root_uri,
        document_uri: record.document_uri,
        kind: record.kind,
        stable_id: record.stable_id,
        generation: record.generation,
        scope: ContextExpansionScope {
            scope_digest: record.scope_digest,
            head_commit_id: record.head_commit_id,
            code_generation_id: record.code_generation_id,
        },
        expires_at: record.expires_at.0,
        coverage,
        revision: TRACEDECAY_CONTEXT_REVISION,
        evidence,
        omission_reason,
    }
}

fn canonical_feedback_value(
    operation: FeedbackReadOperationV1,
    invocation: FeedbackReadInvocationResultV1,
) -> Result<(bool, serde_json::Value), ()> {
    match (operation, invocation) {
        (
            FeedbackReadOperationV1::Diagnostics,
            FeedbackReadInvocationResultV1::Diagnostics(result),
        ) => canonical_application_value(result),
        (FeedbackReadOperationV1::Get, FeedbackReadInvocationResultV1::Get(result)) => {
            canonical_application_value(result)
        }
        (FeedbackReadOperationV1::Expand, FeedbackReadInvocationResultV1::Expand(result)) => {
            canonical_application_value(result)
        }
        _ => Err(()),
    }
}

fn canonical_application_value<T: Serialize>(
    result: ApplicationResult<T>,
) -> Result<(bool, serde_json::Value), ()> {
    let complete = result.is_ok();
    serde_json::to_value(result)
        .map(|value| (complete, value))
        .map_err(|_| ())
}

/// Mount-ready bundle construction. The same concrete feedback source is
/// shared by cycle triggers, managed diagnostics, and context projections.
#[allow(clippy::too_many_arguments)]
pub fn pr12_lsp_session_factory<F>(
    runtime: tokio::runtime::Handle,
    feedback_runtime: Arc<Pr12FeedbackRuntime>,
    database: Database,
    feedback_cycle: F,
    semantics: Arc<dyn SemanticProviderPort + Send + Sync>,
    diagnostic_broker: Arc<AsyncMutex<DiagnosticBroker>>,
    diagnostics_quiet_window: Duration,
    cancellation: Arc<dyn LspAnalyzerCancellationAuthority>,
    gateway_capabilities: GatewayCapabilities,
    upstream_capabilities: UpstreamCapabilities,
) -> Result<Pr12LspSessionFactory, LspRuntimeFailure>
where
    F: FnOnce(ProjectFeedbackStore) -> Arc<dyn FeedbackCycleRuntimePort>,
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
        semantics,
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

fn micros_to_seconds(value: UtcMicros) -> i64 {
    value.0.div_euclid(1_000_000)
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

#[cfg(test)]
mod context_expansion_tests {
    use super::{
        LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION, LspFeedbackProjectionScope,
        StoredLspContextExpansionV1, context_expansion_scope_is_current,
        valid_context_expansion_record,
    };
    use crate::application::feedback::owner::FeedbackReadOperationV1;
    use crate::daemon::lsp_gateway::{AdmittedRoot, ContextProjectionKind};
    use tracedecay_domain::{CodeGenerationId, CommitId, UtcMicros};

    fn record() -> StoredLspContextExpansionV1 {
        StoredLspContextExpansionV1 {
            schema_version: LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION,
            root_uri: "file:///root".to_owned(),
            document_uri: Some("file:///root/src/lib.rs".to_owned()),
            kind: ContextProjectionKind::diagnostics(),
            stable_id: "finding.1".to_owned(),
            scope_digest: "sha256:scope".to_owned(),
            head_commit_id: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            code_generation_id: "generation.v1.aaaaaaaa.00000001".to_owned(),
            generation: 1,
            issued_at: UtcMicros(10),
            expires_at: UtcMicros(20),
            canonical_operation: FeedbackReadOperationV1::Expand,
            canonical_handle: "rh_0123456789abcdef01234567".to_owned(),
        }
    }

    #[test]
    fn expansion_handles_deny_expiry_wrong_root_and_wrong_scope() {
        let record = record();
        let root = AdmittedRoot::new("file:///root");
        assert!(valid_context_expansion_record(
            &record,
            &root,
            "sha256:scope",
            UtcMicros(19)
        ));
        assert!(!valid_context_expansion_record(
            &record,
            &root,
            "sha256:scope",
            UtcMicros(20)
        ));
        assert!(!valid_context_expansion_record(
            &record,
            &AdmittedRoot::new("file:///other"),
            "sha256:scope",
            UtcMicros(19)
        ));
        assert!(!valid_context_expansion_record(
            &record,
            &root,
            "sha256:other",
            UtcMicros(19)
        ));
    }

    #[test]
    fn expansion_handles_become_stale_on_exact_generation_drift() {
        let record = record();
        let current = LspFeedbackProjectionScope {
            head_commit_id: CommitId::new(record.head_commit_id.clone()).expect("commit"),
            code_generation_id: CodeGenerationId::new(record.code_generation_id.clone())
                .expect("generation"),
            generation: record.generation,
        };
        assert!(context_expansion_scope_is_current(&record, &current));

        let stale = LspFeedbackProjectionScope {
            generation: current.generation + 1,
            ..current
        };
        assert!(!context_expansion_scope_is_current(&record, &stale));
    }
}

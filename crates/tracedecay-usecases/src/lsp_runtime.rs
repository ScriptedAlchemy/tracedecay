//! Production LSP composition over the canonical feedback runtime.
//!
//! The adapter mints authorized reads through [`FeedbackRuntime`] and
//! invokes its daemon owner. The cloned [`ProjectFeedbackStore`] is the same
//! durable publication/dedupe authority used by the feedback cycle; this
//! module creates no feedback store, cache, cursor codec, or diagnostic
//! authority.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use cap_std::ambient_authority;
use cap_std::fs::{Dir, File};
use same_file::Handle;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as AsyncMutex;
use tracedecay_application::feedback::{
    FeedbackDiagnosticsReadRequestV1, FeedbackDiagnosticsReadResultV1, FeedbackExpandRequestV1,
};
use tracedecay_application::{
    AnchorExpandRequest, ApplicationOutcome, ApplicationResult, OperationTermination, PageRequest,
    ResultProjection, RetrievalOrder, RetrievalRequestMeta, now_micros,
};
use tracedecay_domain::feedback::{
    FeedbackContentIdentityV1, FeedbackCycleResultV1, FeedbackDiagnosticProducerV1,
    FeedbackFindingLifecycleV1, FeedbackFindingV1, FeedbackImpactStateV1,
    ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, DiagnosticSeverityV1, FileOccurrenceId,
    ManifestDigest, UtcMicros,
};
use tracedecay_lsp::analyzer::adapters::{LspAdapterDefinition, builtin_adapters};
use tracedecay_lsp::analyzer::broker::DiagnosticBroker;
use tracedecay_lsp::analyzer::client::LspDocument;
use tracedecay_lsp::{
    AdmittedRoot, CanonicalContextProjectionAuthority, CanonicalDiagnosticRefreshRequest,
    ContextCoverage, ContextExpansionEnvelope, ContextExpansionOutcome, ContextExpansionRequest,
    ContextExpansionScope, ContextFreshness, ContextProducerState, ContextProjectionChange,
    ContextProjectionEnvelope, ContextProjectionIdentity, ContextProjectionItem,
    ContextProjectionKind, ContextProjectionOutcome, ContextProjectionRegistration,
    ContextProjectionRequest, DiagnosticSeverity, DiagnosticSource, DiagnosticTrigger,
    FeedbackCycleRequest, FeedbackCycleRuntimePort, GatewayCapabilities, GatewayDiagnostic,
    GatewayDiagnosticCoverage, GatewayDiagnosticData, GatewayDiagnosticIdentity,
    GatewayDiagnosticLifecycle, GatewayDiagnosticProviderState, LspAnalyzerCancellationAuthority,
    LspRange, LspRequestId, LspRuntimeFailure, LspRuntimeFuture, MAX_CONTEXT_PROJECTION_ITEMS,
    MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES, MAX_CONTEXT_SUMMARY_BYTES, ManagedDiagnosticSnapshot,
    ManagedDiagnosticSnapshotPort, SemanticProviderPort, TRACEDECAY_CONTEXT_REVISION,
    UpstreamCapabilities, byte_offset_to_utf16_position, strict_file_uri_path, strict_file_url,
};

use tracedecay_policy::diagnostic_curation::{DiagnosticCurationDecisionV1, curate_diagnostic};
use url::Url;

use crate::feedback::concrete::{ConcreteFeedbackOwner, FeedbackRuntime, ProjectFeedbackStore};
use crate::feedback::owner::{
    FeedbackReadInvocationResultV1, FeedbackReadOperationV1, FeedbackReadOwnerErrorV1,
};
pub use crate::lsp_support::{
    BrokerDiagnosticSnapshotAuthority, DaemonLspSessionFactory, DaemonSemanticProviderAdapter,
    LspDiagnosticDocumentPort, LspSemanticRequestAuthority, LspWorkspaceDocumentIndexPort,
    UpstreamCapabilityInitializationAuthority,
};
use crate::operation_stream::{
    CanonicalManagedTestRunReader, ManagedTestRunCurrentScope, ManagedTestRunReadOutcome,
    ManagedTestRunSnapshot, ManagedTestRunStaleReason, ManagedTestRunUnavailableReason,
    operation_event_authority,
};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use crate::response_handles::{
    ResponseHandleLookup, retrieve_response_handle, store_response_handle,
};
const LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION: u16 = 1;
const LSP_TEST_RUN_EXPANSION_HANDLE_SCHEMA_VERSION: u16 = 1;
const LSP_TEST_RUN_EXPANSION_TTL_MICROS: i64 = 15 * 60 * 1_000_000;

mod projection_identity;
pub use projection_identity::{LspCodeIndexProjectionIdentity, LspCodeIndexProjectionIdentityPort};
mod overlay_admission;
use overlay_admission::admit_overlay;
mod diagnostic_records;
pub use diagnostic_records::LspFeedbackDiagnosticRecordPort;
#[cfg(test)]
mod advisory_source_tests;

/// Current canonical Git/graph address for an admitted LSP root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspFeedbackProjectionScope {
    pub head_commit_id: CommitId,
    pub code_generation_id: CodeGenerationId,
    pub snapshot_digest: ManifestDigest,
    pub invalidation_digest: ManifestDigest,
    pub snapshot_content_digest: ContentDigest,
    /// Canonical sealed file identity for a document-scoped request.
    pub document_file_occurrence_id: Option<FileOccurrenceId>,
    pub document_content_digest: Option<ContentDigest>,
    /// Project-relative path for path-addressed external findings. Sealed
    /// code-index findings use `document_file_occurrence_id`.
    pub document_relative_path: Option<String>,
    pub generation: u64,
}

impl LspFeedbackProjectionScope {
    fn projection_identity(&self) -> ContextProjectionIdentity {
        ContextProjectionIdentity {
            head_commit_id: self.head_commit_id.as_str().to_owned(),
            code_generation_id: self.code_generation_id.as_str().to_owned(),
            snapshot_digest: self.snapshot_digest.as_str().to_owned(),
            invalidation_digest: self.invalidation_digest.as_str().to_owned(),
            snapshot_content_digest: self.snapshot_content_digest.as_str().to_owned(),
            document_content_digest: self
                .document_content_digest
                .as_ref()
                .map(|digest| digest.as_str().to_owned()),
        }
    }
}

struct CurrentFeedbackCycle {
    scope: LspFeedbackProjectionScope,
    /// `None` when the diagnostics authority returned terminal evidence
    /// without a cycle. A project that has ingested nothing yet is the
    /// ordinary first-run case, and the read is still authoritative about it,
    /// so consumers receive the typed coverage below rather than nothing.
    result: Option<FeedbackDiagnosticsReadResultV1>,
    termination: OperationTermination,
    canonical_handle: String,
    observed_at: UtcMicros,
    expires_at: UtcMicros,
}

struct FindingContextTarget<'a> {
    root: &'a AdmittedRoot,
    document_uri: Option<&'a str>,
}

/// Coverage and producer state for a diagnostics read that terminated without
/// a cycle, using the same termination vocabulary the projection path already
/// applies to operation snapshots. A read that did not complete is evidence
/// about the producer, not an absence of evidence.
fn incomplete_read_projection(
    termination: OperationTermination,
) -> (ContextCoverage, ContextProducerState) {
    match termination {
        // A completed read that carried no cycle has nothing to report yet,
        // which is unknown coverage rather than a clean document.
        OperationTermination::Completed | OperationTermination::Partial => {
            (ContextCoverage::Partial, ContextProducerState::Partial)
        }
        OperationTermination::Cancelled => (
            ContextCoverage::Unavailable,
            ContextProducerState::Cancelled,
        ),
        OperationTermination::TimedOut => {
            (ContextCoverage::Unavailable, ContextProducerState::TimedOut)
        }
        OperationTermination::Failed => (ContextCoverage::Failed, ContextProducerState::Failed),
        OperationTermination::Unavailable => (
            ContextCoverage::Unavailable,
            ContextProducerState::Unavailable,
        ),
        OperationTermination::EffectUnknown => (
            ContextCoverage::Unavailable,
            ContextProducerState::Unavailable,
        ),
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ProjectionChangeKey {
    root_uri: String,
    document_uri: Option<String>,
    kind: ContextProjectionKind,
}

#[derive(Default)]
struct ProjectionChangeState {
    latest: BTreeMap<ProjectionChangeKey, (String, ContextProjectionChange)>,
}

#[derive(Clone, Default)]
struct ProjectionChangeQueue {
    state: Arc<StdMutex<ProjectionChangeState>>,
}

impl ProjectionChangeQueue {
    fn offer(&self, source_revision: String, change: ContextProjectionChange) {
        let key = ProjectionChangeKey {
            root_uri: change.root_uri.clone(),
            document_uri: change.document_uri.clone(),
            kind: change.kind.clone(),
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .latest
            .get(&key)
            .is_some_and(|(revision, current)| revision == &source_revision && current == &change)
        {
            return;
        }
        state.latest.insert(key, (source_revision, change));
    }

    fn snapshot(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut changes = state
            .latest
            .iter()
            .filter(|(key, _)| {
                key.root_uri == root.uri()
                    && subscriptions.contains(&ContextProjectionRegistration {
                        kind: key.kind.clone(),
                        revision: TRACEDECAY_CONTEXT_REVISION,
                    })
            })
            .map(|(_, (_, change))| change.clone())
            .collect::<Vec<_>>();
        changes.sort_by_key(|change| {
            (
                match change.kind.as_str() {
                    ContextProjectionKind::DIAGNOSTICS => 0,
                    ContextProjectionKind::POST_EDIT_IMPACT => 1,
                    ContextProjectionKind::AFFECTED_TESTS => 2,
                    ContextProjectionKind::GITHUB_REVIEW => 3,
                    ContextProjectionKind::CI_FAILURE_LOCALIZATION => 4,
                    ContextProjectionKind::AGENT_PROXIMITY => 5,
                    ContextProjectionKind::TEST_RUN_RESULTS => 6,
                    _ => 7,
                },
                change.document_uri.clone(),
            )
        });
        changes
    }
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
    identity: ContextProjectionIdentity,
    generation: u64,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
    canonical_operation: FeedbackReadOperationV1,
    canonical_handle: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredLspTestRunExpansionV1 {
    schema_version: u16,
    revision: u32,
    root_uri: String,
    document_uri: Option<String>,
    stable_id: String,
    scope_digest: String,
    identity: ContextProjectionIdentity,
    generation: u64,
    operation_id: String,
    operation_generation: u64,
    operation_completed: u64,
    operation_total: Option<u64>,
    operation_termination: Option<OperationTermination>,
    available_results: usize,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
    result_offset: usize,
    page_size: u32,
}

/// Resolves current scope through the existing admitted Git/graph owner.
pub trait LspFeedbackProjectionScopePort: Send + Sync {
    fn resolve(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
    ) -> LspRuntimeFuture<Result<LspFeedbackProjectionScope, LspRuntimeFailure>>;
}

/// Exact registered project/root authority used by production LSP sessions,
/// bound to the admitted feedback scope and code-index generation.
#[derive(Clone)]
pub struct RegisteredProjectLspAuthority {
    feedback: Arc<FeedbackRuntime>,
    publications: ProjectFeedbackStore,
    project_root: PathBuf,
    project_dir: Arc<Dir>,
    root_uri: Url,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    workspace_index: Arc<dyn crate::lsp_support::LspWorkspaceDocumentIndexPort>,
}

impl RegisteredProjectLspAuthority {
    pub fn new(
        feedback: Arc<FeedbackRuntime>,
        code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
        workspace_index: Arc<dyn crate::lsp_support::LspWorkspaceDocumentIndexPort>,
    ) -> Result<Self, LspRuntimeFailure> {
        let project_root = feedback
            .project_root()
            .canonicalize()
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-unavailable"))?;
        let root_uri = Url::from_directory_path(&project_root)
            .map_err(|()| LspRuntimeFailure::new("registered-project-root-invalid"))?;
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
            project_root,
            project_dir: Arc::new(project_dir),
            root_uri,
            code_index,
            workspace_index,
        })
    }

    pub fn publication_store(&self) -> ProjectFeedbackStore {
        self.publications.clone()
    }

    fn validate_root(&self, root: &AdmittedRoot) -> Result<(), LspRuntimeFailure> {
        if root.scope_digest() != Some(&self.feedback.scope().scope_digest) {
            return Err(LspRuntimeFailure::new("registered-project-root-mismatch"));
        }
        let (_, path) = strict_file_uri_path(root.uri())
            .ok_or_else(|| LspRuntimeFailure::new("registered-project-root-mismatch"))?;
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
            .map(|path| path.replace('\\', "/"))
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

    async fn current_scope(
        &self,
        document_relative_path: Option<String>,
    ) -> Result<LspFeedbackProjectionScope, LspRuntimeFailure> {
        let scope = self.feedback.scope();
        scope
            .validate()
            .map_err(|_| LspRuntimeFailure::new("registered-project-scope-invalid"))?;
        let identity = self
            .code_index
            .current_identity(self.project_root.clone(), document_relative_path.clone())
            .await?;
        let mut projection = identity.admit_for_scope(scope)?;
        projection.document_relative_path = document_relative_path;
        Ok(projection)
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
            let document_relative_path = document_uri
                .as_deref()
                .map(|uri| authority.document_path(uri).map(|(_, relative)| relative))
                .transpose()?;
            authority.current_scope(document_relative_path).await
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
                Some(overlay) => {
                    let overlay = admit_overlay(overlay, &request.document_uri)?;
                    let adapter = builtin_adapters()
                        .into_iter()
                        .find(|adapter| adapter.language_id == overlay.language_id)
                        .ok_or_else(|| {
                            LspRuntimeFailure::new("document-language-not-registered")
                        })?;
                    (
                        adapter.language,
                        adapter.language_id,
                        overlay.text.to_string(),
                    )
                }
                None => {
                    let adapter = adapter_for_path(&path).ok_or_else(|| {
                        LspRuntimeFailure::new("document-language-not-registered")
                    })?;
                    let text = authority.read_disk_document(relative).await?;
                    (adapter.language, adapter.language_id, text)
                }
            };
            let document = LspDocument {
                language,
                language_id,
                relative_path,
                text,
            };
            let observed_content_digest = ContentDigest::of_bytes(document.text.as_bytes());
            if request
                .expected_content_digest
                .as_ref()
                .is_some_and(|expected| expected != &observed_content_digest)
            {
                return Err(LspRuntimeFailure::new("document-content-stale"));
            }
            Ok(document)
        })
    }
}

impl crate::lsp_support::LspWorkspaceDocumentIndexPort for RegisteredProjectLspAuthority {
    fn is_mounted(&self) -> bool {
        self.workspace_index.is_mounted()
    }

    fn indexed_documents(
        &self,
        root: AdmittedRoot,
        maximum_documents: usize,
    ) -> LspRuntimeFuture<Result<tracedecay_lsp::IndexedWorkspaceDocuments, LspRuntimeFailure>>
    {
        if let Err(error) = self.validate_root(&root) {
            return Box::pin(async move { Err(error) });
        }
        self.workspace_index
            .indexed_documents(root, maximum_documents)
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
        expansion_handles: BTreeMap<String, String>,
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

/// Why one feedback finding did not become a published LSP diagnostic.
///
/// Projection refusals used to be anonymous `continue`s, so an empty Problems
/// list was indistinguishable from "the store never had the record".
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FeedbackDiagnosticProjectionSkipV1 {
    /// The finding is not in the active lifecycle state.
    LifecycleNotActive,
    /// The finding carries no `RetrievalAnchorId`, so no durable record can
    /// be addressed.
    NoRetrievalAnchor,
    /// The anchor resolves to no record: the producing pillar never published
    /// this finding into the diagnostics store.
    AnchorNotPublished,
    /// The record attaches to a different file than the cycle's impact target.
    ImpactTargetFileMismatch,
    /// The cycle carries no impact target to compare the record's file with.
    ImpactTargetAbsent,
    /// The finding belongs to another file than the admitted document.
    DocumentFileMismatch,
    /// The record belongs to a different clean generation.
    GenerationMismatch,
    /// The record was collected against different file content.
    ContentDigestMismatch,
    /// The record is superseded or cleared rather than current.
    RecordNotCurrent,
    /// The record names a different source revision than the cycle scope.
    SourceRevisionDrift,
}

impl FeedbackDiagnosticProjectionSkipV1 {
    /// Stable classification label for logs and typed status projections.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LifecycleNotActive => "lifecycle-not-active",
            Self::NoRetrievalAnchor => "no-retrieval-anchor",
            Self::AnchorNotPublished => "anchor-not-published",
            Self::ImpactTargetFileMismatch => "impact-target-file-mismatch",
            Self::ImpactTargetAbsent => "impact-target-absent",
            Self::DocumentFileMismatch => "document-file-mismatch",
            Self::GenerationMismatch => "generation-mismatch",
            Self::ContentDigestMismatch => "content-digest-mismatch",
            Self::RecordNotCurrent => "record-not-current",
            Self::SourceRevisionDrift => "source-revision-drift",
        }
    }
}

/// Records one typed projection refusal. Refusals are observable rather than
/// silent so an empty Problems list can be attributed to a cause.
fn skipped(finding_id: &str, skip: FeedbackDiagnosticProjectionSkipV1) {
    tracing::debug!(
        target: "tracedecay::lsp::diagnostics",
        finding_id,
        reason = skip.label(),
        "feedback finding was not projected as an LSP diagnostic"
    );
}

/// Decides whether a resolved durable record may be projected for this cycle.
///
/// Pure and total: every refusal is named.
pub fn classify_feedback_diagnostic_admission(
    record: &tracedecay_domain::GenerationDiagnosticV1,
    impact_target_file: Option<&tracedecay_domain::FileOccurrenceId>,
    code_generation_id: &tracedecay_domain::CodeGenerationId,
    document_content_digest: &tracedecay_domain::ContentDigest,
    head_commit_id: &tracedecay_domain::CommitId,
) -> Result<(), FeedbackDiagnosticProjectionSkipV1> {
    let Some(target_file) = impact_target_file else {
        return Err(FeedbackDiagnosticProjectionSkipV1::ImpactTargetAbsent);
    };
    match curate_diagnostic(
        record,
        target_file,
        code_generation_id,
        document_content_digest,
        head_commit_id,
    ) {
        DiagnosticCurationDecisionV1::Admit => Ok(()),
        DiagnosticCurationDecisionV1::TargetFileMismatch => {
            Err(FeedbackDiagnosticProjectionSkipV1::ImpactTargetFileMismatch)
        }
        DiagnosticCurationDecisionV1::GenerationMismatch => {
            Err(FeedbackDiagnosticProjectionSkipV1::GenerationMismatch)
        }
        DiagnosticCurationDecisionV1::ContentDigestMismatch => {
            Err(FeedbackDiagnosticProjectionSkipV1::ContentDigestMismatch)
        }
        DiagnosticCurationDecisionV1::RecordNotCurrent => {
            Err(FeedbackDiagnosticProjectionSkipV1::RecordNotCurrent)
        }
        DiagnosticCurationDecisionV1::SourceRevisionDrift => {
            Err(FeedbackDiagnosticProjectionSkipV1::SourceRevisionDrift)
        }
    }
}

fn gateway_diagnostic_data(
    finding: &FeedbackFindingV1,
    anchor: &tracedecay_domain::RetrievalAnchorId,
    scope: &LspFeedbackProjectionScope,
    coverage: GatewayDiagnosticCoverage,
    expansion_handles: &BTreeMap<String, String>,
) -> Option<GatewayDiagnosticData> {
    let document_content_digest = scope.document_content_digest.as_ref()?;
    let expansion_handle = expansion_handles.get(finding.finding_id.as_str())?;
    Some(GatewayDiagnosticData {
        identity: GatewayDiagnosticIdentity {
            finding_id: finding.finding_id.as_str().to_owned(),
            anchor_id: anchor.as_str().to_owned(),
            generation: scope.generation,
            head_commit_id: scope.head_commit_id.as_str().to_owned(),
            code_generation_id: scope.code_generation_id.as_str().to_owned(),
            snapshot_digest: scope.snapshot_digest.as_str().to_owned(),
            invalidation_digest: scope.invalidation_digest.as_str().to_owned(),
            snapshot_content_digest: scope.snapshot_content_digest.as_str().to_owned(),
            document_content_digest: document_content_digest.as_str().to_owned(),
        },
        lifecycle: gateway_diagnostic_lifecycle(finding.lifecycle),
        provider_state: gateway_diagnostic_provider_state(finding.provider_state),
        coverage,
        expansion_handle: expansion_handle.clone(),
    })
}

const fn gateway_severity(severity: DiagnosticSeverityV1) -> DiagnosticSeverity {
    match severity {
        DiagnosticSeverityV1::Error => DiagnosticSeverity::Error,
        DiagnosticSeverityV1::Warning => DiagnosticSeverity::Warning,
        DiagnosticSeverityV1::Information => DiagnosticSeverity::Information,
        DiagnosticSeverityV1::Hint => DiagnosticSeverity::Hint,
    }
}

const fn advisory_diagnostic_source(
    producer: tracedecay_domain::feedback::FeedbackDiagnosticProducerV1,
) -> DiagnosticSource {
    use tracedecay_domain::feedback::FeedbackDiagnosticProducerV1;
    match producer {
        FeedbackDiagnosticProducerV1::GitHubReview => DiagnosticSource::TraceDecayGitHub,
        FeedbackDiagnosticProducerV1::CiLocalization => DiagnosticSource::TraceDecayCi,
        FeedbackDiagnosticProducerV1::Proximity => DiagnosticSource::TraceDecayProximity,
    }
}

/// Real finding-anchor hydration over the canonical managed diagnostics store.
pub struct DiagnosticsStoreLspFeedbackProjection<S> {
    records: Arc<dyn LspFeedbackDiagnosticRecordPort>,
    documents: Arc<S>,
}

impl<S> DiagnosticsStoreLspFeedbackProjection<S> {
    pub fn new(records: Arc<dyn LspFeedbackDiagnosticRecordPort>, documents: Arc<S>) -> Self {
        Self { records, documents }
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
        expansion_handles: BTreeMap<String, String>,
    ) -> LspRuntimeFuture<Result<Vec<GatewayDiagnostic>, LspRuntimeFailure>> {
        let records = Arc::clone(&self.records);
        let documents = Arc::clone(&self.documents);
        Box::pin(async move {
            let document = documents.snapshot(root, document_uri.clone()).await?;
            let Some(document_content_digest) = scope.document_content_digest.as_ref() else {
                return Err(LspRuntimeFailure::new(
                    "diagnostic-document-identity-unavailable",
                ));
            };
            if tracedecay_code_index::intake::content_digest(document.text.as_bytes())
                != *document_content_digest
            {
                return Err(LspRuntimeFailure::new("diagnostic-document-content-stale"));
            }
            let coverage = gateway_diagnostic_coverage(cycle_coverage(&cycle));
            let mut diagnostics = Vec::new();
            let impact_target_file = cycle.impact.as_ref().map(|impact| &impact.target.file);
            for finding in &cycle.findings {
                let finding_id = finding.finding_id.as_str();
                if !finding_matches_document(finding, &scope, impact_target_file) {
                    skipped(
                        finding_id,
                        FeedbackDiagnosticProjectionSkipV1::DocumentFileMismatch,
                    );
                    continue;
                }
                if finding.lifecycle != FeedbackFindingLifecycleV1::Active {
                    skipped(
                        finding_id,
                        FeedbackDiagnosticProjectionSkipV1::LifecycleNotActive,
                    );
                    continue;
                }
                let Some(anchor) = finding.retrieval_anchor_id.as_ref() else {
                    skipped(
                        finding_id,
                        FeedbackDiagnosticProjectionSkipV1::NoRetrievalAnchor,
                    );
                    continue;
                };
                if let Some(projection) = finding.diagnostic_projection.as_ref() {
                    let Some(target_file) = impact_target_file else {
                        skipped(
                            finding_id,
                            FeedbackDiagnosticProjectionSkipV1::ImpactTargetAbsent,
                        );
                        continue;
                    };
                    if projection.file != *target_file {
                        skipped(
                            finding_id,
                            FeedbackDiagnosticProjectionSkipV1::ImpactTargetFileMismatch,
                        );
                        continue;
                    }
                    let start = usize::try_from(projection.span.start_byte)
                        .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                    let end = usize::try_from(projection.span.end_byte)
                        .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                    let start = byte_offset_to_utf16_position(&document.text, start)
                        .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                    let end = byte_offset_to_utf16_position(&document.text, end)
                        .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                    diagnostics.push(GatewayDiagnostic {
                        uri: document_uri.clone(),
                        range: LspRange { start, end },
                        severity: Some(gateway_severity(projection.severity)),
                        code: Some(projection.code.clone()),
                        code_description_uri: projection.code_description_uri.clone(),
                        message: projection.safe_bounded_message.clone(),
                        source: advisory_diagnostic_source(projection.producer),
                        related_information: Vec::new(),
                        data: gateway_diagnostic_data(
                            finding,
                            anchor,
                            &scope,
                            coverage,
                            &expansion_handles,
                        ),
                    });
                    continue;
                }
                let Some(record) = records.diagnostic_by_anchor(anchor.clone()).await? else {
                    skipped(
                        finding_id,
                        FeedbackDiagnosticProjectionSkipV1::AnchorNotPublished,
                    );
                    continue;
                };
                if let Err(skip) = classify_feedback_diagnostic_admission(
                    &record,
                    impact_target_file,
                    &scope.code_generation_id,
                    document_content_digest,
                    &cycle.scope.head_commit_id,
                ) {
                    skipped(finding_id, skip);
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
                let data =
                    gateway_diagnostic_data(finding, anchor, &scope, coverage, &expansion_handles);
                diagnostics.push(GatewayDiagnostic {
                    uri: document_uri.clone(),
                    range: LspRange { start, end },
                    severity: Some(gateway_severity(record.severity)),
                    code: Some(record.code),
                    code_description_uri: None,
                    message: record.message,
                    // Name the real producer instead of an anonymous
                    // `tracedecay` lane (Plan 35).
                    source: DiagnosticSource::from_producer(record.provenance.producer.as_str()),
                    related_information: Vec::new(),
                    data,
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
        document_content_digest: Option<ContentDigest>,
    ) -> LspRuntimeFuture<ContextProjectionOutcome>;

    fn expand(
        &self,
        _root: AdmittedRoot,
        _stored_record: String,
    ) -> LspRuntimeFuture<ContextExpansionOutcome> {
        Box::pin(async { ContextExpansionOutcome::Denied })
    }

    fn poll_changes(
        &self,
        _root: &AdmittedRoot,
        _subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        Vec::new()
    }
}

#[derive(Clone)]
pub(crate) struct OperationEventTestRunProjection {
    reader: CanonicalManagedTestRunReader,
    project: Arc<RegisteredProjectLspAuthority>,
    current_scopes: Arc<StdMutex<BTreeMap<String, CachedTestRunScope>>>,
    observed_revisions: Arc<StdMutex<BTreeMap<String, String>>>,
    changes: ProjectionChangeQueue,
}

#[derive(Clone)]
struct CachedTestRunScope {
    current: ManagedTestRunCurrentScope,
    projection: LspFeedbackProjectionScope,
}

#[derive(Clone)]
struct LspTestRunExpansionContext {
    operation_id: String,
    operation_generation: u64,
    operation_completed: u64,
    operation_total: Option<u64>,
    operation_termination: Option<OperationTermination>,
    available_results: usize,
    result_offset: usize,
    page_size: u32,
}

impl OperationEventTestRunProjection {
    pub(crate) fn new(
        reader: CanonicalManagedTestRunReader,
        project: Arc<RegisteredProjectLspAuthority>,
    ) -> Self {
        Self {
            reader,
            project,
            current_scopes: Arc::new(StdMutex::new(BTreeMap::new())),
            observed_revisions: Arc::new(StdMutex::new(BTreeMap::new())),
            changes: ProjectionChangeQueue::default(),
        }
    }

    fn store_expansion(
        &self,
        root: &AdmittedRoot,
        document_uri: Option<&str>,
        scope: &LspFeedbackProjectionScope,
        stable_id: String,
        context: LspTestRunExpansionContext,
    ) -> Result<String, LspRuntimeFailure> {
        let LspTestRunExpansionContext {
            operation_id,
            operation_generation,
            operation_completed,
            operation_total,
            operation_termination,
            available_results,
            result_offset,
            page_size,
        } = context;
        let issued_at = now_micros();
        let record = StoredLspTestRunExpansionV1 {
            schema_version: LSP_TEST_RUN_EXPANSION_HANDLE_SCHEMA_VERSION,
            revision: TRACEDECAY_CONTEXT_REVISION,
            root_uri: root.uri().to_owned(),
            document_uri: document_uri.map(str::to_owned),
            stable_id,
            scope_digest: self
                .project
                .feedback
                .scope()
                .scope_digest
                .as_str()
                .to_owned(),
            identity: scope.projection_identity(),
            generation: scope.generation,
            operation_id,
            operation_generation,
            operation_completed,
            operation_total,
            operation_termination,
            available_results,
            issued_at,
            expires_at: UtcMicros(
                issued_at
                    .0
                    .saturating_add(LSP_TEST_RUN_EXPANSION_TTL_MICROS),
            ),
            result_offset,
            page_size,
        };
        let content = serde_json::to_string(&record)
            .map_err(|_| LspRuntimeFailure::new("test-run-expansion-handle-invalid"))?;
        store_response_handle(
            &self.project.project_root,
            &content,
            micros_to_seconds(issued_at),
        )
        .map(|stored| stored.handle)
        .map_err(|_| LspRuntimeFailure::new("test-run-expansion-handle-store-failed"))
    }
}

pub(crate) fn lsp_test_result_port(
    project: Arc<RegisteredProjectLspAuthority>,
) -> Arc<dyn LspTestRunProjectionPort> {
    Arc::new(OperationEventTestRunProjection::new(
        CanonicalManagedTestRunReader::new(operation_event_authority()),
        project,
    ))
}

impl LspTestRunProjectionPort for OperationEventTestRunProjection {
    fn snapshot(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
        document_content_digest: Option<ContentDigest>,
    ) -> LspRuntimeFuture<ContextProjectionOutcome> {
        let projection = self.clone();
        Box::pin(async move {
            let mut scope = match projection
                .project
                .resolve(root.clone(), document_uri.clone())
                .await
            {
                Ok(scope) => scope,
                Err(error) => {
                    return ContextProjectionOutcome::Deferred {
                        reason: error.class().to_owned(),
                    };
                }
            };
            if let Err(reason) = bind_test_run_document_content(&mut scope, document_content_digest)
            {
                return ContextProjectionOutcome::Deferred {
                    reason: reason.to_owned(),
                };
            }
            let current = ManagedTestRunCurrentScope {
                root_uri: root.uri().to_owned(),
                head_commit_id: Some(scope.head_commit_id.clone()),
                code_generation_id: Some(scope.code_generation_id.clone()),
                document_uri: document_uri.clone(),
                document_content_digest: scope.document_content_digest.clone(),
            };
            projection
                .current_scopes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    current_scope_key(&current),
                    CachedTestRunScope {
                        current: current.clone(),
                        projection: scope.clone(),
                    },
                );
            let page = match PageRequest::first(MAX_CONTEXT_PROJECTION_ITEMS as u32) {
                Ok(page) => page,
                Err(_) => {
                    return ContextProjectionOutcome::Deferred {
                        reason: "managed-test-run-page-invalid".to_owned(),
                    };
                }
            };
            match projection.reader.latest_current_page(&current, &page).await {
                ManagedTestRunReadOutcome::Current(snapshot) => {
                    projection
                        .observed_revisions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(
                            current_scope_key(&current),
                            test_run_source_revision(&snapshot),
                        );
                    let expansion_context = LspTestRunExpansionContext {
                        operation_id: snapshot.operation_id.to_string(),
                        operation_generation: snapshot.generation,
                        operation_completed: snapshot.completed,
                        operation_total: snapshot.total,
                        operation_termination: snapshot.termination,
                        available_results: snapshot.available_results,
                        result_offset: snapshot.result_offset,
                        page_size: 1,
                    };
                    let has_bounded_results = snapshot.next_cursor.is_some();
                    let mut outcome = test_run_projection(
                        root.clone(),
                        document_uri.clone(),
                        scope.clone(),
                        snapshot,
                    );
                    if let ContextProjectionOutcome::Ready(envelope) = &mut outcome {
                        for (index, item) in envelope.items.iter_mut().enumerate() {
                            item.retrieval_handle = match projection.store_expansion(
                                &root,
                                document_uri.as_deref(),
                                &scope,
                                item.stable_id.clone(),
                                LspTestRunExpansionContext {
                                    result_offset: expansion_context
                                        .result_offset
                                        .saturating_add(index),
                                    ..expansion_context.clone()
                                },
                            ) {
                                Ok(handle) => Some(handle),
                                Err(error) => {
                                    return ContextProjectionOutcome::Deferred {
                                        reason: error.class().to_owned(),
                                    };
                                }
                            };
                        }
                        if has_bounded_results && !envelope.items.is_empty() {
                            envelope.retrieval_handle = match projection.store_expansion(
                                &root,
                                document_uri.as_deref(),
                                &scope,
                                format!("{}.__remaining__", expansion_context.operation_id),
                                LspTestRunExpansionContext {
                                    result_offset: expansion_context
                                        .result_offset
                                        .saturating_add(envelope.items.len()),
                                    page_size: MAX_CONTEXT_PROJECTION_ITEMS as u32,
                                    ..expansion_context
                                },
                            ) {
                                Ok(handle) => Some(handle),
                                Err(error) => {
                                    return ContextProjectionOutcome::Deferred {
                                        reason: error.class().to_owned(),
                                    };
                                }
                            };
                        }
                    }
                    outcome
                }
                ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::FrontierExpired,
                ) => ContextProjectionOutcome::Deferred {
                    reason: "managed-test-run-frontier-expired".to_owned(),
                },
                ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::RetainedHeadUnbound,
                ) => ContextProjectionOutcome::Deferred {
                    reason: "managed-test-run-head-unbound".to_owned(),
                },
                ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::RetainedCodeGenerationUnbound,
                ) => ContextProjectionOutcome::Deferred {
                    reason: "managed-test-run-code-generation-unbound".to_owned(),
                },
                ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::CurrentDocumentUnbound
                    | ManagedTestRunUnavailableReason::RetainedDocumentUnbound,
                ) => ContextProjectionOutcome::Deferred {
                    reason: "managed-test-run-document-content-unbound".to_owned(),
                },
                ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::CurrentHeadUnbound
                    | ManagedTestRunUnavailableReason::CurrentCodeGenerationUnbound,
                ) => ContextProjectionOutcome::Deferred {
                    reason: "managed-test-run-current-identity-unbound".to_owned(),
                },
                ManagedTestRunReadOutcome::Stale(ManagedTestRunStaleReason::SourceIdentity) => {
                    ContextProjectionOutcome::Deferred {
                        reason: "managed-test-run-source-identity-stale".to_owned(),
                    }
                }
                ManagedTestRunReadOutcome::Stale(ManagedTestRunStaleReason::DocumentContent) => {
                    ContextProjectionOutcome::Deferred {
                        reason: "managed-test-run-document-content-stale".to_owned(),
                    }
                }
                ManagedTestRunReadOutcome::Unavailable(
                    ManagedTestRunUnavailableReason::AuthorityFailure,
                ) => ContextProjectionOutcome::Failed {
                    reason: "managed-test-run-projection-failed".to_owned(),
                },
            }
        })
    }

    fn expand(
        &self,
        root: AdmittedRoot,
        stored_record: String,
    ) -> LspRuntimeFuture<ContextExpansionOutcome> {
        self.expand_stored(root, stored_record)
    }

    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        let registration = ContextProjectionRegistration {
            kind: ContextProjectionKind::test_run_results(),
            revision: TRACEDECAY_CONTEXT_REVISION,
        };
        if !subscriptions.contains(&registration) {
            return Vec::new();
        }
        let scopes = self
            .current_scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|scope| scope.current.root_uri == root.uri())
            .cloned()
            .collect::<Vec<_>>();
        for cached in scopes {
            let current = cached.current;
            let Some(ManagedTestRunReadOutcome::Current(snapshot)) =
                self.reader.try_latest_current(&current)
            else {
                continue;
            };
            let key = current_scope_key(&current);
            let source_revision = test_run_source_revision(&snapshot);
            let changed = {
                let mut observed = self
                    .observed_revisions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match observed.insert(key, source_revision.clone()) {
                    Some(previous) => previous != source_revision,
                    None => false,
                }
            };
            if !changed {
                continue;
            }
            let document_uri = current.document_uri.clone();
            let ContextProjectionOutcome::Ready(envelope) =
                test_run_projection(root.clone(), document_uri, cached.projection, snapshot)
            else {
                continue;
            };
            self.changes.offer(
                source_revision,
                ContextProjectionChange {
                    root_uri: envelope.root_uri,
                    document_uri: envelope.document_uri,
                    kind: envelope.kind,
                    generation: envelope.generation,
                    identity: envelope.identity,
                    freshness: envelope.freshness,
                    producer_state: envelope.producer_state,
                    coverage: envelope.coverage,
                    revision: envelope.revision,
                    retrieval_handle: envelope.retrieval_handle,
                },
            );
        }
        self.changes.snapshot(root, subscriptions)
    }
}

fn current_scope_key(scope: &ManagedTestRunCurrentScope) -> String {
    format!(
        "{}\u{0}{}",
        scope.root_uri,
        scope.document_uri.as_deref().unwrap_or_default()
    )
}

fn test_run_source_revision(snapshot: &ManagedTestRunSnapshot) -> String {
    format!("{}:{}", snapshot.operation_id, snapshot.source_revision)
}

/// A retained managed test run is evidence about saved source. An LSP overlay
/// may reuse it only while its exact bytes still match that saved document.
fn bind_test_run_document_content(
    scope: &mut LspFeedbackProjectionScope,
    overlay_digest: Option<ContentDigest>,
) -> Result<(), &'static str> {
    let Some(overlay_digest) = overlay_digest else {
        return Ok(());
    };
    let Some(saved_digest) = scope.document_content_digest.as_ref() else {
        return Err("managed-test-run-document-content-unbound");
    };
    if saved_digest != &overlay_digest {
        return Err("managed-test-run-document-content-stale");
    }
    scope.document_content_digest = Some(overlay_digest);
    Ok(())
}

impl OperationEventTestRunProjection {
    fn expand_stored(
        &self,
        root: AdmittedRoot,
        stored_record: String,
    ) -> LspRuntimeFuture<ContextExpansionOutcome> {
        let projection = self.clone();
        Box::pin(async move {
            let Ok(record) = serde_json::from_str::<StoredLspTestRunExpansionV1>(&stored_record)
            else {
                return ContextExpansionOutcome::Denied;
            };
            let observed_at = now_micros();
            if record.schema_version != LSP_TEST_RUN_EXPANSION_HANDLE_SCHEMA_VERSION
                || record.revision != TRACEDECAY_CONTEXT_REVISION
                || record.root_uri != root.uri()
                || record.issued_at >= record.expires_at
                || observed_at < record.issued_at
                || observed_at >= record.expires_at
                || record.page_size == 0
                || record.page_size > MAX_CONTEXT_PROJECTION_ITEMS as u32
            {
                return ContextExpansionOutcome::Denied;
            }
            let scope = match projection
                .project
                .resolve(root.clone(), record.document_uri.clone())
                .await
            {
                Ok(scope) => scope,
                Err(_) => return ContextExpansionOutcome::Denied,
            };
            if projection.project.feedback.scope().scope_digest.as_str() != record.scope_digest {
                return ContextExpansionOutcome::Denied;
            }
            if scope.generation != record.generation
                || scope.projection_identity() != record.identity
            {
                return ContextExpansionOutcome::Ready(context_expansion_envelope_for_test_run(
                    record,
                    ContextCoverage::Partial,
                    None,
                    Some("stale-generation".to_owned()),
                    None,
                ));
            }
            let current = ManagedTestRunCurrentScope {
                root_uri: root.uri().to_owned(),
                head_commit_id: Some(scope.head_commit_id.clone()),
                code_generation_id: Some(scope.code_generation_id.clone()),
                document_uri: record.document_uri.clone(),
                document_content_digest: scope.document_content_digest.clone(),
            };
            let ManagedTestRunReadOutcome::Current(snapshot) =
                projection.reader.latest_current(&current).await
            else {
                return ContextExpansionOutcome::Denied;
            };
            if snapshot.operation_id.to_string() != record.operation_id
                || snapshot.generation != record.operation_generation
                || snapshot.completed != record.operation_completed
                || snapshot.total != record.operation_total
                || snapshot.termination != record.operation_termination
                || snapshot.results.len() != record.available_results
            {
                return ContextExpansionOutcome::Ready(context_expansion_envelope_for_test_run(
                    record,
                    ContextCoverage::Partial,
                    None,
                    Some("stale-generation".to_owned()),
                    None,
                ));
            }
            let end = record
                .result_offset
                .saturating_add(record.page_size as usize)
                .min(snapshot.results.len());
            if record.result_offset >= snapshot.results.len() {
                return ContextExpansionOutcome::Denied;
            }
            let results = snapshot.results[record.result_offset..end]
                .iter()
                .map(|result| {
                    serde_json::json!({
                        "test": result.test,
                        "passed": result.passed,
                    })
                })
                .collect::<Vec<_>>();
            let result_offset = record.result_offset;
            let next_retrieval_handle = if end < snapshot.results.len() {
                match projection.store_expansion(
                    &root,
                    record.document_uri.as_deref(),
                    &scope,
                    record.stable_id.clone(),
                    LspTestRunExpansionContext {
                        operation_id: record.operation_id.clone(),
                        operation_generation: record.operation_generation,
                        operation_completed: record.operation_completed,
                        operation_total: record.operation_total,
                        operation_termination: record.operation_termination,
                        available_results: record.available_results,
                        result_offset: end,
                        page_size: MAX_CONTEXT_PROJECTION_ITEMS as u32,
                    },
                ) {
                    Ok(handle) => Some(handle),
                    Err(_) => return ContextExpansionOutcome::Denied,
                }
            } else {
                None
            };
            let coverage = if next_retrieval_handle.is_some() {
                ContextCoverage::Partial
            } else {
                ContextCoverage::Complete
            };
            let omission_reason = next_retrieval_handle
                .as_ref()
                .map(|_| "bounded-projection-items".to_owned());
            ContextExpansionOutcome::Ready(context_expansion_envelope_for_test_run(
                record,
                coverage,
                Some(serde_json::json!({
                    "results": results,
                    "result_offset": result_offset,
                    "available_results": snapshot.results.len(),
                    "next_retrieval_handle": next_retrieval_handle,
                })),
                omission_reason,
                next_retrieval_handle,
            ))
        })
    }
}

/// Shared feedback source mounted as both `FeedbackCyclePort` and the managed
/// diagnostics/context authority in the daemon LSP runtime adapters.
#[derive(Clone)]
pub struct ConcreteFeedbackLspSource {
    runtime: Arc<FeedbackRuntime>,
    owner: Arc<ConcreteFeedbackOwner>,
    publications: ProjectFeedbackStore,
    cycle: Arc<dyn FeedbackCycleRuntimePort>,
    scope: Arc<dyn LspFeedbackProjectionScopePort>,
    diagnostic_projection: Arc<dyn LspFeedbackDiagnosticProjectionPort>,
    test_runs: Arc<dyn LspTestRunProjectionPort>,
    changes: ProjectionChangeQueue,
}

impl ConcreteFeedbackLspSource {
    pub fn new<F>(
        runtime: Arc<FeedbackRuntime>,
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
            changes: ProjectionChangeQueue::default(),
        }
    }

    /// The exact store clone supplied to the Plan 09 cycle dedupe/publication
    /// boundary. Exposing it lets the daemon composition root prove both
    /// surfaces use one authority.
    pub fn publication_store(&self) -> ProjectFeedbackStore {
        self.publications.clone()
    }

    async fn queue_feedback_changes(&self, request: &FeedbackCycleRequest) {
        let Ok(current) = self
            .current_cycle(
                AdmittedRoot::new(request.root_uri.clone()),
                Some(request.document_uri.clone()),
                None,
            )
            .await
        else {
            return;
        };
        let identity = current.scope.projection_identity();
        let generation = current.scope.generation;
        let (source_revision, changes) = match current.result.as_ref() {
            Some(result) => {
                let cycle = &result.cycle;
                let aggregate_state = producer_state_for_cycle(cycle);
                let github =
                    advisory_projection_status(cycle, FeedbackDiagnosticProducerV1::GitHubReview);
                let ci =
                    advisory_projection_status(cycle, FeedbackDiagnosticProducerV1::CiLocalization);
                let proximity =
                    advisory_projection_status(cycle, FeedbackDiagnosticProducerV1::Proximity);
                (
                    cycle.result_id.as_str().to_owned(),
                    vec![
                        (
                            ContextProjectionKind::diagnostics(),
                            cycle_coverage(cycle),
                            aggregate_state,
                        ),
                        (
                            ContextProjectionKind::post_edit_impact(),
                            impact_projection(cycle).0,
                            aggregate_state,
                        ),
                        (
                            ContextProjectionKind::affected_tests(),
                            affected_test_projection(cycle).0,
                            aggregate_state,
                        ),
                        (ContextProjectionKind::github_review(), github.0, github.1),
                        (ContextProjectionKind::ci_failure_localization(), ci.0, ci.1),
                        (
                            ContextProjectionKind::agent_proximity(),
                            proximity.0,
                            proximity.1,
                        ),
                    ],
                )
            }
            // No cycle to name a revision with. Keying the notification on the
            // termination collapses a run of identical degraded reads into one
            // change instead of re-announcing the same absence per read.
            None => {
                let (coverage, producer_state) = incomplete_read_projection(current.termination);
                (
                    format!("incomplete-read.{producer_state:?}"),
                    [
                        ContextProjectionKind::diagnostics(),
                        ContextProjectionKind::post_edit_impact(),
                        ContextProjectionKind::affected_tests(),
                        ContextProjectionKind::github_review(),
                        ContextProjectionKind::ci_failure_localization(),
                        ContextProjectionKind::agent_proximity(),
                    ]
                    .map(|kind| (kind, coverage, producer_state))
                    .to_vec(),
                )
            }
        };
        for (kind, coverage, producer_state) in changes {
            self.changes.offer(
                source_revision.clone(),
                ContextProjectionChange {
                    root_uri: request.root_uri.clone(),
                    document_uri: Some(request.document_uri.clone()),
                    kind,
                    generation,
                    identity: identity.clone(),
                    freshness: ContextFreshness::Current,
                    producer_state,
                    coverage,
                    revision: TRACEDECAY_CONTEXT_REVISION,
                    retrieval_handle: None,
                },
            );
        }
    }

    async fn current_cycle(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
        document_content_digest: Option<ContentDigest>,
    ) -> Result<CurrentFeedbackCycle, LspRuntimeFailure> {
        let mut scope = self.scope.resolve(root, document_uri).await?;
        if document_content_digest.is_some() {
            scope.document_content_digest = document_content_digest;
        }
        let observed_at = now_micros();
        let expires_at = self
            .runtime
            .request_expiry_at(observed_at)
            .map_err(|_| LspRuntimeFailure::new("feedback-request-expiry-unavailable"))?;
        let request_id = mint_global_request_id(GlobalRequestSurface::LspFeedbackDiagnostics)
            .map_err(|_| LspRuntimeFailure::new("feedback-request-identity-unavailable"))?;
        let handle = self
            .runtime
            .mint_diagnostics(
                request_id.as_str(),
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
        // A read that did not complete, or that completed with no cycle to
        // report, is the ordinary state of a project that has ingested nothing
        // yet. Refusing it here denied every first-run project any context
        // projection at all, even though the projection envelope already
        // carries typed coverage and producer state for exactly this case.
        let termination = evidence.execution.termination;
        let Some(payload) = evidence.payload else {
            return Ok(CurrentFeedbackCycle {
                scope,
                result: None,
                termination,
                canonical_handle: handle,
                observed_at,
                expires_at,
            });
        };
        if !feedback_content_is_current(payload.cycle.content_identity.as_ref(), &scope) {
            return Err(LspRuntimeFailure::new("feedback-source-identity-stale"));
        }
        Ok(CurrentFeedbackCycle {
            scope,
            result: Some(payload),
            termination,
            canonical_handle: handle,
            observed_at,
            expires_at,
        })
    }

    fn current_finding_items<'a>(
        &self,
        target: FindingContextTarget<'_>,
        scope: &LspFeedbackProjectionScope,
        impact_target_file: Option<&tracedecay_domain::FileOccurrenceId>,
        kind: ContextProjectionKind,
        findings: impl Iterator<Item = &'a FeedbackFindingV1>,
        maximum_items: usize,
    ) -> Result<Vec<ContextProjectionItem>, LspRuntimeFailure> {
        let observed_at = now_micros();
        let expires_at = self
            .runtime
            .request_expiry_at(observed_at)
            .map_err(|_| LspRuntimeFailure::new("feedback-finding-expiry-unavailable"))?;
        findings
            .filter(|finding| finding_matches_document(finding, scope, impact_target_file))
            .filter_map(|finding| finding_item(finding).map(|item| (finding, item)))
            .take(maximum_items)
            .map(|(finding, item)| {
                let (canonical_operation, canonical_handle) = if let Some(anchor) =
                    finding.retrieval_anchor_id.as_ref()
                {
                    let page = PageRequest::first(MAX_CONTEXT_PROJECTION_ITEMS as u32)
                        .map_err(|_| LspRuntimeFailure::new("feedback-expand-page-invalid"))?;
                    let handle = self
                        .runtime
                        .mint_expand(
                            mint_global_request_id(GlobalRequestSurface::LspFeedbackExpand)
                                .map_err(|_| {
                                    LspRuntimeFailure::new("feedback-request-identity-unavailable")
                                })?
                                .as_str(),
                            FeedbackExpandRequestV1 {
                                finding_id: finding.finding_id.clone(),
                                expansion: AnchorExpandRequest {
                                    anchor: anchor.clone(),
                                    meta: RetrievalRequestMeta::current(
                                        page,
                                        ResultProjection::ReferencesOnly,
                                        RetrievalOrder::StableIdentity,
                                    ),
                                },
                            },
                            observed_at,
                        )
                        .map_err(|_| LspRuntimeFailure::new("feedback-expand-mint-failed"))?;
                    (FeedbackReadOperationV1::Expand, handle)
                } else {
                    let handle = self
                        .runtime
                        .mint_get(
                            mint_global_request_id(GlobalRequestSurface::LspFeedbackGet)
                                .map_err(|_| {
                                    LspRuntimeFailure::new("feedback-request-identity-unavailable")
                                })?
                                .as_str(),
                            finding.finding_id.clone(),
                            observed_at,
                        )
                        .map_err(|_| LspRuntimeFailure::new("feedback-get-mint-failed"))?;
                    (FeedbackReadOperationV1::Get, handle)
                };
                self.attach_context_handle(
                    target.root,
                    target.document_uri,
                    kind.clone(),
                    scope,
                    observed_at,
                    expires_at,
                    canonical_operation,
                    &canonical_handle,
                    item,
                )
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
            identity: scope.projection_identity(),
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
        let content = match retrieve_response_handle(
            self.runtime.project_root(),
            &request.retrieval_handle,
            micros_to_seconds(observed_at),
        ) {
            Ok(ResponseHandleLookup::Found(record)) => record.content,
            Ok(ResponseHandleLookup::Missing | ResponseHandleLookup::Expired { .. }) => {
                return ContextExpansionOutcome::Denied;
            }
            Err(_) => {
                return ContextExpansionOutcome::Failed {
                    reason: "context-expansion-handle-unavailable".to_owned(),
                };
            }
        };
        let record = match serde_json::from_str::<StoredLspContextExpansionV1>(&content) {
            Ok(record) => record,
            Err(_) => {
                return self.test_runs.expand(root, content).await;
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
            Err(FeedbackReadOwnerErrorV1::Unavailable)
            | Err(FeedbackReadOwnerErrorV1::Contract(_)) => {
                return ContextExpansionOutcome::Ready(context_expansion_envelope(
                    record,
                    ContextCoverage::Partial,
                    None,
                    Some("canonical-feedback-unavailable".to_owned()),
                ));
            }
        };
        let Ok((complete, evidence)) =
            canonical_feedback_value(record.canonical_operation, invocation)
        else {
            return ContextExpansionOutcome::Failed {
                reason: "context-expansion-kind-mismatch".to_owned(),
            };
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

impl FeedbackCycleRuntimePort for ConcreteFeedbackLspSource {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>> {
        let source = self.clone();
        Box::pin(async move {
            source.cycle.execute(request.clone()).await?;
            source.queue_feedback_changes(&request).await;
            Ok(())
        })
    }
}

impl ManagedDiagnosticSnapshotPort for ConcreteFeedbackLspSource {
    fn snapshot(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<ManagedDiagnosticSnapshot, LspRuntimeFailure>> {
        let source = self.clone();
        Box::pin(async move {
            source
                .execute(FeedbackCycleRequest {
                    root_uri: request.root.uri().to_owned(),
                    document_uri: request.document_uri.clone(),
                    trigger: DiagnosticTrigger::ExplicitDocumentDiagnostics,
                })
                .await?;
            let current = source
                .current_cycle(
                    request.root.clone(),
                    Some(request.document_uri.clone()),
                    None,
                )
                .await?;
            let scope = current.scope;
            crate::lsp_support::validate_managed_diagnostic_scope(&request, &scope)?;
            let Some(result) = current.result else {
                return Err(LspRuntimeFailure::new("feedback-read-incomplete"));
            };
            let cycle = result.cycle;
            let expansion_handles = source
                .current_finding_items(
                    FindingContextTarget {
                        root: &request.root,
                        document_uri: Some(&request.document_uri),
                    },
                    &scope,
                    cycle.impact.as_ref().map(|impact| &impact.target.file),
                    ContextProjectionKind::diagnostics(),
                    cycle.findings.iter(),
                    MAX_CONTEXT_PROJECTION_ITEMS,
                )?
                .into_iter()
                .filter_map(|item| item.retrieval_handle.map(|handle| (item.stable_id, handle)))
                .collect();
            let diagnostics = source
                .diagnostic_projection
                .project(
                    request.root,
                    request.document_uri,
                    scope.clone(),
                    cycle,
                    expansion_handles,
                )
                .await?;
            Ok(ManagedDiagnosticSnapshot {
                generation: scope.generation,
                code_generation_id: scope.code_generation_id.clone(),
                snapshot_digest: scope.snapshot_digest.clone(),
                authority_digest: crate::lsp_support::managed_diagnostic_authority_digest(&scope)?,
                diagnostics,
            })
        })
    }
}

impl CanonicalContextProjectionAuthority for ConcreteFeedbackLspSource {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        [
            ContextProjectionKind::diagnostics(),
            ContextProjectionKind::post_edit_impact(),
            ContextProjectionKind::affected_tests(),
            ContextProjectionKind::test_run_results(),
            ContextProjectionKind::github_review(),
            ContextProjectionKind::ci_failure_localization(),
            ContextProjectionKind::agent_proximity(),
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
        _request_id: LspRequestId,
        request: ContextProjectionRequest,
    ) -> LspRuntimeFuture<ContextProjectionOutcome> {
        if request.kind == ContextProjectionKind::test_run_results() {
            let document_content_digest = request.document_content_digest().cloned();
            return self
                .test_runs
                .snapshot(root, request.document_uri, document_content_digest);
        }
        let source = self.clone();
        Box::pin(async move {
            let current = match source
                .current_cycle(
                    root.clone(),
                    request.document_uri.clone(),
                    request.document_content_digest().cloned(),
                )
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
                termination,
                canonical_handle,
                observed_at,
                expires_at,
            } = current;
            let Some(result) = result else {
                if request.kind != ContextProjectionKind::diagnostics()
                    && request.kind != ContextProjectionKind::post_edit_impact()
                    && request.kind != ContextProjectionKind::affected_tests()
                    && advisory_projection_producer(&request.kind).is_none()
                {
                    return ContextProjectionOutcome::Unsupported;
                }
                let (coverage, producer_state) = incomplete_read_projection(termination);
                return ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
                    root_uri: root.uri().to_owned(),
                    document_uri: request.document_uri,
                    kind: request.kind,
                    generation: scope.generation,
                    identity: scope.projection_identity(),
                    freshness: ContextFreshness::Current,
                    producer_state,
                    coverage,
                    revision: TRACEDECAY_CONTEXT_REVISION,
                    items: Vec::new(),
                    omitted_count: 0,
                    omission_reasons: projection_omission_reasons(coverage, 0, producer_state),
                    retrieval_handle: None,
                });
            };
            let cycle = result.cycle;
            let kind = request.kind.clone();
            let (coverage, mut items, omitted_count) =
                if request.kind == ContextProjectionKind::diagnostics() {
                    let items = match source.current_finding_items(
                        FindingContextTarget {
                            root: &root,
                            document_uri: request.document_uri.as_deref(),
                        },
                        &scope,
                        cycle.impact.as_ref().map(|impact| &impact.target.file),
                        ContextProjectionKind::diagnostics(),
                        cycle.findings.iter(),
                        MAX_CONTEXT_PROJECTION_ITEMS,
                    ) {
                        Ok(items) => items,
                        Err(error) => {
                            return ContextProjectionOutcome::Deferred {
                                reason: error.class().to_owned(),
                            };
                        }
                    };
                    let active_findings = cycle
                        .findings
                        .iter()
                        .filter(|finding| {
                            finding_matches_document(
                                finding,
                                &scope,
                                cycle.impact.as_ref().map(|impact| &impact.target.file),
                            )
                        })
                        .filter(|finding| finding.lifecycle == FeedbackFindingLifecycleV1::Active)
                        .count();
                    let omitted_count = usize::try_from(cycle.omitted_findings)
                        .unwrap_or(usize::MAX)
                        .saturating_add(active_findings.saturating_sub(items.len()));
                    let coverage = match (cycle_coverage(&cycle), omitted_count) {
                        (ContextCoverage::Complete, 1..) => ContextCoverage::Partial,
                        (coverage, _) => coverage,
                    };
                    (coverage, items, omitted_count)
                } else if request.kind == ContextProjectionKind::post_edit_impact() {
                    impact_projection(&cycle)
                } else if request.kind == ContextProjectionKind::affected_tests() {
                    affected_test_projection(&cycle)
                } else if let Some(producer) = advisory_projection_producer(&request.kind) {
                    let projected_item_count = cycle
                        .findings
                        .iter()
                        .filter(|finding| {
                            finding_matches_document(
                                finding,
                                &scope,
                                cycle.impact.as_ref().map(|impact| &impact.target.file),
                            )
                        })
                        .filter(|finding| advisory_finding_matches(finding, producer))
                        .filter_map(finding_item)
                        .count();
                    let items = match source.current_finding_items(
                        FindingContextTarget {
                            root: &root,
                            document_uri: request.document_uri.as_deref(),
                        },
                        &scope,
                        cycle.impact.as_ref().map(|impact| &impact.target.file),
                        kind.clone(),
                        cycle
                            .findings
                            .iter()
                            .filter(|finding| advisory_finding_matches(finding, producer)),
                        MAX_CONTEXT_PROJECTION_ITEMS,
                    ) {
                        Ok(items) => items,
                        Err(error) => {
                            return ContextProjectionOutcome::Deferred {
                                reason: error.class().to_owned(),
                            };
                        }
                    };
                    // Cycle omissions are aggregate and carry no producer
                    // attribution. Surface that uncertainty through status,
                    // but count only the bounded items attributable to this
                    // projection so every advisory lane cannot claim the
                    // same omitted finding.
                    let omitted_count =
                        bounded_advisory_item_omissions(projected_item_count, items.len());
                    let coverage = match (
                        advisory_projection_status(&cycle, producer).0,
                        omitted_count,
                    ) {
                        (ContextCoverage::Complete, 1..) => ContextCoverage::Partial,
                        (coverage, _) => coverage,
                    };
                    (coverage, items, omitted_count)
                } else {
                    return ContextProjectionOutcome::Unsupported;
                };
            // Advisory finding items already carry per-finding canonical
            // expand/get handles from `current_finding_items`; replacing them
            // with a cycle-level diagnostics handle would discard the exact
            // authority that expansion must reauthorize.
            if kind != ContextProjectionKind::diagnostics()
                && advisory_projection_producer(&kind).is_none()
            {
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
            let mut producer_state = advisory_projection_producer(&kind)
                .map(|producer| advisory_projection_status(&cycle, producer).1)
                .unwrap_or_else(|| producer_state_for_cycle(&cycle));
            if coverage == ContextCoverage::Partial
                && producer_state == ContextProducerState::Complete
            {
                producer_state = ContextProducerState::Partial;
            }
            let mut omission_reasons =
                projection_omission_reasons(coverage, omitted_count, producer_state);
            if advisory_projection_producer(&kind).is_some() && cycle.omitted_findings > 0 {
                omission_reasons.retain(|reason| reason != "producer-partial");
                omission_reasons.push("cycle-omissions-unattributed".to_owned());
            }
            ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
                root_uri: root.uri().to_owned(),
                document_uri: request.document_uri,
                kind,
                generation: scope.generation,
                identity: scope.projection_identity(),
                freshness: ContextFreshness::Current,
                producer_state,
                coverage,
                revision: TRACEDECAY_CONTEXT_REVISION,
                items,
                omitted_count,
                omission_reasons,
                retrieval_handle,
            })
        })
    }

    fn expand(
        &self,
        root: AdmittedRoot,
        _request_id: LspRequestId,
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
        ordered_context_changes(
            self.changes.snapshot(root, subscriptions),
            self.test_runs.poll_changes(root, subscriptions),
        )
    }
}

/// Feedback-cycle changes describe the saved edit that triggered the cycle;
/// managed test-run changes are a distinct later execution result. Preserve
/// that production chronology after the bounded feedback lanes have been
/// ordered, rather than giving the test run an artificial earlier rank.
fn ordered_context_changes(
    mut feedback_changes: Vec<ContextProjectionChange>,
    test_run_changes: Vec<ContextProjectionChange>,
) -> Vec<ContextProjectionChange> {
    feedback_changes.extend(test_run_changes);
    feedback_changes
}

fn feedback_content_is_current(
    content_identity: Option<&FeedbackContentIdentityV1>,
    scope: &LspFeedbackProjectionScope,
) -> bool {
    matches!(
        content_identity,
        Some(FeedbackContentIdentityV1::SavedContent {
            generation_digest,
            file_digest,
        }) if generation_digest == &scope.snapshot_digest
            && scope
                .document_content_digest
                .as_ref()
                .is_none_or(|digest| digest.as_str() == file_digest.as_str())
    )
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
        && valid_projection_identity(&record.identity)
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
    current.projection_identity() == record.identity && current.generation == record.generation
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
            identity: record.identity,
        },
        expires_at: record.expires_at.0,
        coverage,
        revision: TRACEDECAY_CONTEXT_REVISION,
        evidence,
        omission_reason,
        next_retrieval_handle: None,
    }
}

fn context_expansion_envelope_for_test_run(
    record: StoredLspTestRunExpansionV1,
    coverage: ContextCoverage,
    evidence: Option<serde_json::Value>,
    omission_reason: Option<String>,
    next_retrieval_handle: Option<String>,
) -> ContextExpansionEnvelope {
    ContextExpansionEnvelope {
        root_uri: record.root_uri,
        document_uri: record.document_uri,
        kind: ContextProjectionKind::test_run_results(),
        stable_id: record.stable_id,
        generation: record.generation,
        scope: ContextExpansionScope {
            scope_digest: record.scope_digest,
            identity: record.identity,
        },
        expires_at: record.expires_at.0,
        coverage,
        revision: record.revision,
        evidence,
        omission_reason,
        next_retrieval_handle,
    }
}

fn valid_projection_identity(identity: &ContextProjectionIdentity) -> bool {
    CommitId::new(identity.head_commit_id.clone()).is_ok()
        && CodeGenerationId::new(identity.code_generation_id.clone()).is_ok()
        && ManifestDigest::new(identity.snapshot_digest.clone()).is_ok()
        && ManifestDigest::new(identity.invalidation_digest.clone()).is_ok()
        && ContentDigest::new(identity.snapshot_content_digest.clone()).is_ok()
        && identity
            .document_content_digest
            .as_ref()
            .is_none_or(|digest| ContentDigest::new(digest.clone()).is_ok())
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
pub fn lsp_session_factory<F>(
    runtime: tokio::runtime::Handle,
    feedback_runtime: Arc<FeedbackRuntime>,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    workspace_index: Arc<dyn crate::lsp_support::LspWorkspaceDocumentIndexPort>,
    diagnostic_records: Arc<dyn LspFeedbackDiagnosticRecordPort>,
    feedback_cycle: F,
    semantics: Arc<dyn SemanticProviderPort + Send + Sync>,
    diagnostic_broker: Arc<AsyncMutex<DiagnosticBroker>>,
    diagnostics_quiet_window: Duration,
    cancellation: Arc<dyn LspAnalyzerCancellationAuthority>,
    gateway_capabilities: GatewayCapabilities,
    upstream_capabilities: UpstreamCapabilities,
) -> Result<DaemonLspSessionFactory, LspRuntimeFailure>
where
    F: FnOnce(ProjectFeedbackStore) -> Arc<dyn FeedbackCycleRuntimePort>,
{
    let project = Arc::new(RegisteredProjectLspAuthority::new(
        feedback_runtime.clone(),
        code_index,
        workspace_index,
    )?);
    let test_runs = lsp_test_result_port(project.clone());
    let diagnostic_projection = Arc::new(DiagnosticsStoreLspFeedbackProjection::new(
        diagnostic_records,
        project.clone(),
    ));
    let feedback = Arc::new(ConcreteFeedbackLspSource::new(
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
    Ok(DaemonLspSessionFactory::new(
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
    scope: LspFeedbackProjectionScope,
    snapshot: ManagedTestRunSnapshot,
) -> ContextProjectionOutcome {
    let termination = match snapshot.termination {
        Some(termination) => termination,
        None if snapshot.deadline.is_elapsed_at(now_micros()) => OperationTermination::TimedOut,
        None => return ContextProjectionOutcome::Pending,
    };
    let Some(head_commit_id) = snapshot.head_commit_id.as_ref() else {
        return ContextProjectionOutcome::Deferred {
            reason: "managed-test-run-head-unbound".to_owned(),
        };
    };
    let Some(code_generation_id) = snapshot.code_generation_id.as_ref() else {
        return ContextProjectionOutcome::Deferred {
            reason: "managed-test-run-code-generation-unbound".to_owned(),
        };
    };
    if head_commit_id != &scope.head_commit_id || code_generation_id != &scope.code_generation_id {
        return ContextProjectionOutcome::Deferred {
            reason: "managed-test-run-source-identity-stale".to_owned(),
        };
    }
    if let Some(document_uri) = document_uri.as_ref() {
        let Some(current_digest) = scope.document_content_digest.as_ref() else {
            return ContextProjectionOutcome::Deferred {
                reason: "managed-test-run-document-content-unbound".to_owned(),
            };
        };
        let Some(retained_digest) = snapshot.document_content_digests.get(document_uri) else {
            return ContextProjectionOutcome::Deferred {
                reason: "managed-test-run-document-content-unbound".to_owned(),
            };
        };
        if retained_digest != current_digest {
            return ContextProjectionOutcome::Deferred {
                reason: "managed-test-run-document-content-stale".to_owned(),
            };
        }
    }
    let missing_results = snapshot
        .completed
        .saturating_sub(snapshot.available_results as u64);
    let bounded_omissions = snapshot.available_results.saturating_sub(
        snapshot
            .result_offset
            .saturating_add(snapshot.results.len()),
    ) as u64;
    let mut omitted_count =
        usize::try_from(missing_results.saturating_add(bounded_omissions)).unwrap_or(usize::MAX);
    let completed_with_full_results = snapshot.total == Some(snapshot.completed)
        && snapshot.results.len() as u64 == snapshot.completed
        && omitted_count == 0;
    let (coverage, producer_state, include_results) = match termination {
        OperationTermination::Completed if completed_with_full_results => (
            ContextCoverage::Complete,
            ContextProducerState::Complete,
            true,
        ),
        OperationTermination::Completed | OperationTermination::Partial => (
            ContextCoverage::Partial,
            ContextProducerState::Partial,
            true,
        ),
        OperationTermination::Cancelled => (
            ContextCoverage::Unavailable,
            ContextProducerState::Cancelled,
            false,
        ),
        OperationTermination::TimedOut => (
            ContextCoverage::Unavailable,
            ContextProducerState::TimedOut,
            false,
        ),
        OperationTermination::Failed => {
            (ContextCoverage::Failed, ContextProducerState::Failed, false)
        }
        OperationTermination::Unavailable => (
            ContextCoverage::Unavailable,
            ContextProducerState::Unavailable,
            false,
        ),
        OperationTermination::EffectUnknown => (
            ContextCoverage::Unavailable,
            ContextProducerState::Unavailable,
            false,
        ),
    };
    let operation_id = snapshot.operation_id.to_string();
    let items = if include_results {
        snapshot
            .results
            .into_iter()
            .enumerate()
            .map(|(index, result)| ContextProjectionItem {
                stable_id: format!(
                    "{operation_id}.{}",
                    snapshot.result_offset.saturating_add(index)
                ),
                summary: bounded_test_run_summary(&result.test, result.passed),
                retrieval_handle: None,
            })
            .collect()
    } else {
        omitted_count = omitted_count.saturating_add(snapshot.results.len());
        Vec::new()
    };
    let omission_reasons = projection_omission_reasons(coverage, omitted_count, producer_state);
    ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
        root_uri: root.uri().to_owned(),
        document_uri,
        kind: ContextProjectionKind::test_run_results(),
        generation: scope.generation,
        identity: scope.projection_identity(),
        freshness: ContextFreshness::Current,
        producer_state,
        coverage,
        revision: TRACEDECAY_CONTEXT_REVISION,
        items,
        omitted_count,
        omission_reasons,
        retrieval_handle: None,
    })
}

fn projection_omission_reasons(
    coverage: ContextCoverage,
    omitted_count: usize,
    producer_state: ContextProducerState,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if omitted_count > 0 {
        reasons.push("bounded-projection-items".to_owned());
    }
    if coverage != ContextCoverage::Complete {
        reasons.push(
            match producer_state {
                ContextProducerState::Complete => "projection-incomplete",
                ContextProducerState::Partial => "producer-partial",
                ContextProducerState::Indexing => "producer-indexing",
                ContextProducerState::Unavailable => "producer-unavailable",
                ContextProducerState::Failed => "producer-failed",
                ContextProducerState::Cancelled => "producer-cancelled",
                ContextProducerState::TimedOut => "producer-timed-out",
            }
            .to_owned(),
        );
    }
    reasons
}

fn bounded_test_run_summary(test: &str, passed: bool) -> String {
    let prefix = if passed { "passed: " } else { "failed: " };
    let truncated = tracedecay_runtime_core::text::utf8_prefix_at_or_before(
        test,
        MAX_CONTEXT_SUMMARY_BYTES.saturating_sub(prefix.len()),
    );
    format!("{prefix}{truncated}")
}

fn finding_item(finding: &FeedbackFindingV1) -> Option<ContextProjectionItem> {
    if finding.lifecycle != FeedbackFindingLifecycleV1::Active {
        return None;
    }
    projection_item(
        finding.finding_id.as_str(),
        finding
            .safe_bounded_preview
            .clone()
            .unwrap_or_else(|| "feedback finding".to_owned()),
    )
}

/// Maps a negotiated advisory projection to the canonical producer that owns
/// its findings. No gateway-local source or inferred contributor is involved.
fn advisory_projection_producer(
    kind: &ContextProjectionKind,
) -> Option<FeedbackDiagnosticProducerV1> {
    match kind.as_str() {
        ContextProjectionKind::GITHUB_REVIEW => Some(FeedbackDiagnosticProducerV1::GitHubReview),
        ContextProjectionKind::CI_FAILURE_LOCALIZATION => {
            Some(FeedbackDiagnosticProducerV1::CiLocalization)
        }
        ContextProjectionKind::AGENT_PROXIMITY => Some(FeedbackDiagnosticProducerV1::Proximity),
        _ => None,
    }
}

/// A finding is visible in exactly the advisory projection backed by its
/// canonical diagnostic producer. Lifecycle filtering remains centralized in
/// `finding_item`, so a saved cycle that clears the finding clears the LSP
/// projection as well.
fn advisory_finding_matches(
    finding: &FeedbackFindingV1,
    producer: FeedbackDiagnosticProducerV1,
) -> bool {
    finding
        .diagnostic_projection
        .as_ref()
        .is_some_and(|projection| projection.producer == producer)
}

/// Checks document membership before an LSP context handle can be bound. An
/// advisory finding names its own file; ordinary canonical findings are
/// document-scoped through the cycle impact target. If neither can establish
/// that relationship, this document cannot safely expose the finding.
fn finding_matches_document(
    finding: &FeedbackFindingV1,
    scope: &LspFeedbackProjectionScope,
    impact_target_file: Option<&tracedecay_domain::FileOccurrenceId>,
) -> bool {
    let finding_file = finding
        .diagnostic_projection
        .as_ref()
        .map(|projection| &projection.file)
        .or(impact_target_file);
    if let Some(document_file) = scope.document_file_occurrence_id.as_ref() {
        return finding_file == Some(document_file);
    }
    let Some(document_relative_path) = scope.document_relative_path.as_deref() else {
        return true;
    };
    finding_file.is_some_and(|file| file.as_str() == document_relative_path)
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

fn advisory_provider_state(
    providers: &[tracedecay_domain::feedback::FeedbackAdvisoryProviderStateV1],
    producer: FeedbackDiagnosticProducerV1,
) -> Option<ProviderEvaluationStateV1> {
    providers
        .iter()
        .find(|provider| provider.producer == producer)
        .map(|provider| provider.state)
}

/// Translate one canonical provider evaluation, without allowing an unrelated
/// provider's incomplete coverage to downgrade this projection.
fn advisory_projection_status(
    cycle: &FeedbackCycleResultV1,
    producer: FeedbackDiagnosticProducerV1,
) -> (ContextCoverage, ContextProducerState) {
    let (coverage, state) = advisory_provider_status(advisory_provider_state(
        &cycle.advisory_provider_states,
        producer,
    ));
    advisory_coverage(coverage, state, cycle.omitted_findings)
}

fn advisory_coverage(
    coverage: ContextCoverage,
    producer_state: ContextProducerState,
    omitted_findings: u64,
) -> (ContextCoverage, ContextProducerState) {
    if omitted_findings > 0
        && coverage == ContextCoverage::Complete
        && producer_state == ContextProducerState::Complete
    {
        // The aggregate result does not identify which advisory producer lost
        // the finding. It is therefore partiality of the cycle, not a
        // producer-specific omitted count.
        (ContextCoverage::Partial, ContextProducerState::Partial)
    } else {
        (coverage, producer_state)
    }
}

fn bounded_advisory_item_omissions(projected_item_count: usize, returned_items: usize) -> usize {
    projected_item_count.saturating_sub(returned_items)
}

fn advisory_provider_status(
    provider_state: Option<ProviderEvaluationStateV1>,
) -> (ContextCoverage, ContextProducerState) {
    match provider_state {
        Some(ProviderEvaluationStateV1::SupportedCompletedComplete) => {
            (ContextCoverage::Complete, ContextProducerState::Complete)
        }
        Some(
            ProviderEvaluationStateV1::Unsupported
            | ProviderEvaluationStateV1::Absent
            | ProviderEvaluationStateV1::Unavailable,
        )
        | None => (
            ContextCoverage::Unavailable,
            ContextProducerState::Unavailable,
        ),
        Some(ProviderEvaluationStateV1::Indexing) => {
            (ContextCoverage::Partial, ContextProducerState::Indexing)
        }
        Some(ProviderEvaluationStateV1::Stale | ProviderEvaluationStateV1::Partial) => {
            (ContextCoverage::Partial, ContextProducerState::Partial)
        }
        Some(ProviderEvaluationStateV1::Cancelled) => (
            ContextCoverage::Unavailable,
            ContextProducerState::Cancelled,
        ),
        Some(ProviderEvaluationStateV1::TimedOut) => {
            (ContextCoverage::Unavailable, ContextProducerState::TimedOut)
        }
        Some(ProviderEvaluationStateV1::Failed) => {
            (ContextCoverage::Failed, ContextProducerState::Failed)
        }
    }
}

fn producer_state_for_cycle(cycle: &FeedbackCycleResultV1) -> ContextProducerState {
    use tracedecay_domain::feedback::FeedbackCycleTerminationV1;

    match cycle.termination {
        FeedbackCycleTerminationV1::Clean => {
            if cycle_coverage(cycle) == ContextCoverage::Complete {
                ContextProducerState::Complete
            } else {
                ContextProducerState::Partial
            }
        }
        FeedbackCycleTerminationV1::DuplicateNoop => ContextProducerState::Complete,
        FeedbackCycleTerminationV1::IncompleteCoverage
        | FeedbackCycleTerminationV1::StaleReplanRequired => ContextProducerState::Partial,
        FeedbackCycleTerminationV1::BudgetExceeded => ContextProducerState::TimedOut,
        FeedbackCycleTerminationV1::Cancelled | FeedbackCycleTerminationV1::UserStop => {
            ContextProducerState::Cancelled
        }
        FeedbackCycleTerminationV1::Blocked | FeedbackCycleTerminationV1::DaemonUnavailable => {
            ContextProducerState::Unavailable
        }
    }
}

fn gateway_diagnostic_coverage(coverage: ContextCoverage) -> GatewayDiagnosticCoverage {
    match coverage {
        ContextCoverage::Complete => GatewayDiagnosticCoverage::Complete,
        ContextCoverage::Partial => GatewayDiagnosticCoverage::Partial,
        ContextCoverage::Unavailable => GatewayDiagnosticCoverage::Unavailable,
        ContextCoverage::Failed => GatewayDiagnosticCoverage::Failed,
    }
}

fn gateway_diagnostic_lifecycle(
    lifecycle: FeedbackFindingLifecycleV1,
) -> GatewayDiagnosticLifecycle {
    match lifecycle {
        FeedbackFindingLifecycleV1::Active => GatewayDiagnosticLifecycle::Active,
        FeedbackFindingLifecycleV1::Superseded => GatewayDiagnosticLifecycle::Superseded,
        FeedbackFindingLifecycleV1::Resolved => GatewayDiagnosticLifecycle::Resolved,
        FeedbackFindingLifecycleV1::Cleared => GatewayDiagnosticLifecycle::Cleared,
    }
}

fn gateway_diagnostic_provider_state(
    state: ProviderEvaluationStateV1,
) -> GatewayDiagnosticProviderState {
    match state {
        ProviderEvaluationStateV1::SupportedCompletedComplete => {
            GatewayDiagnosticProviderState::SupportedCompletedComplete
        }
        ProviderEvaluationStateV1::Unsupported => GatewayDiagnosticProviderState::Unsupported,
        ProviderEvaluationStateV1::Absent => GatewayDiagnosticProviderState::Absent,
        ProviderEvaluationStateV1::Indexing => GatewayDiagnosticProviderState::Indexing,
        ProviderEvaluationStateV1::Stale => GatewayDiagnosticProviderState::Stale,
        ProviderEvaluationStateV1::Cancelled => GatewayDiagnosticProviderState::Cancelled,
        ProviderEvaluationStateV1::TimedOut => GatewayDiagnosticProviderState::TimedOut,
        ProviderEvaluationStateV1::Failed => GatewayDiagnosticProviderState::Failed,
        ProviderEvaluationStateV1::Partial => GatewayDiagnosticProviderState::Partial,
        ProviderEvaluationStateV1::Unavailable => GatewayDiagnosticProviderState::Unavailable,
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

fn adapter_for_path(path: &Path) -> Option<LspAdapterDefinition> {
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

fn validated_document_path(
    project_root: &Path,
    root_uri: &Url,
    project_dir: &Dir,
    document_uri: &str,
) -> Result<ValidatedDocumentPath, LspRuntimeFailure> {
    let url = strict_file_url(document_uri)
        .ok_or_else(|| LspRuntimeFailure::new("document-uri-invalid"))?;
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
        .map_err(|()| LspRuntimeFailure::new("document-uri-invalid"))?;
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

    // The symlink-escape test that exercises open_project_file is unix-only.
    #[cfg(unix)]
    use super::open_project_file;
    use super::validated_document_path;

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
}

#[cfg(test)]
mod context_expansion_tests {
    use super::{
        LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION, LspFeedbackProjectionScope,
        StoredLspContextExpansionV1, context_expansion_scope_is_current,
        valid_context_expansion_record,
    };
    use crate::feedback::owner::FeedbackReadOperationV1;
    use tracedecay_domain::{CodeGenerationId, CommitId, ContentDigest, ManifestDigest, UtcMicros};
    use tracedecay_lsp::{AdmittedRoot, ContextProjectionIdentity, ContextProjectionKind};

    fn record() -> StoredLspContextExpansionV1 {
        StoredLspContextExpansionV1 {
            schema_version: LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION,
            root_uri: "file:///root".to_owned(),
            document_uri: Some("file:///root/src/lib.rs".to_owned()),
            kind: ContextProjectionKind::diagnostics(),
            stable_id: "finding.1".to_owned(),
            scope_digest: "sha256:scope".to_owned(),
            identity: ContextProjectionIdentity {
                head_commit_id: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                code_generation_id: "generation.v1.aaaaaaaa.00000001".to_owned(),
                snapshot_digest: format!("sha256:{}", "a".repeat(64)),
                invalidation_digest: format!("sha256:{}", "b".repeat(64)),
                snapshot_content_digest: format!("sha256:{}", "c".repeat(64)),
                document_content_digest: Some(format!("sha256:{}", "d".repeat(64))),
            },
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
            head_commit_id: CommitId::new(record.identity.head_commit_id.clone()).expect("commit"),
            code_generation_id: CodeGenerationId::new(record.identity.code_generation_id.clone())
                .expect("generation"),
            snapshot_digest: ManifestDigest::new(record.identity.snapshot_digest.clone())
                .expect("snapshot digest"),
            invalidation_digest: ManifestDigest::new(record.identity.invalidation_digest.clone())
                .expect("invalidation digest"),
            snapshot_content_digest: ContentDigest::new(
                record.identity.snapshot_content_digest.clone(),
            )
            .expect("snapshot content digest"),
            document_file_occurrence_id: None,
            document_content_digest: record
                .identity
                .document_content_digest
                .as_ref()
                .map(|digest| ContentDigest::new(digest.clone()).expect("document content digest")),
            document_relative_path: None,
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

#[cfg(test)]
mod projection_tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        LspFeedbackProjectionScope, ProjectionChangeQueue, advisory_coverage,
        advisory_finding_matches, advisory_projection_producer, advisory_provider_state,
        advisory_provider_status, bind_test_run_document_content, bounded_advisory_item_omissions,
        feedback_content_is_current, finding_item, finding_matches_document,
        ordered_context_changes, test_run_projection,
    };
    use crate::operation_stream::{ManagedTestRunResult, ManagedTestRunSnapshot, OperationId};
    use tracedecay_application::{Deadline, OperationTermination, RequestId};
    use tracedecay_domain::feedback::{
        FeedbackAdvisoryProviderStateV1, FeedbackContentIdentityV1,
        FeedbackDiagnosticClassificationV1, FeedbackDiagnosticProducerV1,
        FeedbackDiagnosticProjectionV1, FeedbackFindingId, FeedbackFindingLifecycleV1,
        FeedbackFindingV1, ProviderEvaluationStateV1,
    };
    use tracedecay_domain::{
        CodeGenerationId, CommitId, ContentDigest, DiagnosticSeverityV1, FileOccurrenceId,
        ManifestDigest, SourceSpan, UtcMicros,
    };
    use tracedecay_lsp::{
        AdmittedRoot, ContextCoverage, ContextFreshness, ContextProducerState,
        ContextProjectionChange, ContextProjectionKind, ContextProjectionOutcome,
        ContextProjectionRegistration, MAX_CONTEXT_PROJECTION_ITEMS, TRACEDECAY_CONTEXT_REVISION,
    };

    fn finding(lifecycle: FeedbackFindingLifecycleV1) -> FeedbackFindingV1 {
        FeedbackFindingV1 {
            finding_id: FeedbackFindingId::new("finding.lifecycle").expect("finding"),
            classification: FeedbackDiagnosticClassificationV1::New,
            lifecycle,
            retrieval_anchor_id: None,
            provider_state: ProviderEvaluationStateV1::SupportedCompletedComplete,
            safe_bounded_preview: Some("bounded finding".to_owned()),
            diagnostic_projection: None,
        }
    }

    fn advisory_finding(
        producer: FeedbackDiagnosticProducerV1,
        lifecycle: FeedbackFindingLifecycleV1,
    ) -> FeedbackFindingV1 {
        FeedbackFindingV1 {
            diagnostic_projection: Some(FeedbackDiagnosticProjectionV1 {
                file: FileOccurrenceId::new("src/lib.rs").expect("file"),
                span: SourceSpan {
                    start_byte: 0,
                    end_byte: 1,
                },
                symbol: None,
                code: "advisory".to_owned(),
                severity: DiagnosticSeverityV1::Warning,
                safe_bounded_message: "bounded advisory finding".to_owned(),
                producer,
                code_description_uri: None,
            }),
            ..finding(lifecycle)
        }
    }

    fn projection_scope() -> LspFeedbackProjectionScope {
        LspFeedbackProjectionScope {
            head_commit_id: CommitId::new("0123456789abcdef0123456789abcdef01234567")
                .expect("commit"),
            code_generation_id: CodeGenerationId::new("generation.v1.aaaaaaaa.00000001")
                .expect("code generation"),
            snapshot_digest: ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("snapshot digest"),
            invalidation_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("invalidation digest"),
            snapshot_content_digest: ContentDigest::new(format!("sha256:{}", "c".repeat(64)))
                .expect("snapshot content digest"),
            document_file_occurrence_id: Some(
                FileOccurrenceId::new("src/lib.rs").expect("document file"),
            ),
            document_content_digest: None,
            document_relative_path: Some("src/lib.rs".to_owned()),
            generation: 42,
        }
    }

    fn change(kind: ContextProjectionKind, generation: u64) -> ContextProjectionChange {
        ContextProjectionChange {
            root_uri: "file:///root".to_owned(),
            document_uri: Some("file:///root/src/lib.rs".to_owned()),
            kind,
            generation,
            identity: projection_scope().projection_identity(),
            freshness: ContextFreshness::Current,
            producer_state: ContextProducerState::Complete,
            coverage: ContextCoverage::Complete,
            revision: TRACEDECAY_CONTEXT_REVISION,
            retrieval_handle: None,
        }
    }

    #[test]
    fn feedback_change_queue_replays_latest_advisory_state_in_delivery_order() {
        let root = AdmittedRoot::new("file:///root");
        let queue = ProjectionChangeQueue::default();
        queue.offer(
            "before-subscription".to_owned(),
            change(ContextProjectionKind::diagnostics(), 1),
        );
        assert!(queue.snapshot(&root, &BTreeSet::new()).is_empty());

        let subscriptions = [
            ContextProjectionKind::diagnostics(),
            ContextProjectionKind::post_edit_impact(),
            ContextProjectionKind::affected_tests(),
            ContextProjectionKind::test_run_results(),
            ContextProjectionKind::github_review(),
            ContextProjectionKind::ci_failure_localization(),
            ContextProjectionKind::agent_proximity(),
        ]
        .into_iter()
        .map(|kind| ContextProjectionRegistration {
            kind,
            revision: TRACEDECAY_CONTEXT_REVISION,
        })
        .collect::<BTreeSet<_>>();
        queue.offer(
            "diagnostics-1".to_owned(),
            change(ContextProjectionKind::diagnostics(), 1),
        );
        queue.offer(
            "diagnostics-2".to_owned(),
            change(ContextProjectionKind::diagnostics(), 2),
        );
        for (revision, kind) in [
            ("affected-1", ContextProjectionKind::affected_tests()),
            ("impact-1", ContextProjectionKind::post_edit_impact()),
            ("github-1", ContextProjectionKind::github_review()),
            ("ci-1", ContextProjectionKind::ci_failure_localization()),
            ("proximity-1", ContextProjectionKind::agent_proximity()),
        ] {
            queue.offer(revision.to_owned(), change(kind, 1));
        }

        let changes = queue.snapshot(&root, &subscriptions);
        assert_eq!(
            changes
                .iter()
                .map(|change| change.kind.as_str())
                .collect::<Vec<_>>(),
            vec![
                "diagnostics",
                "postEditImpact",
                "affectedTests",
                "githubReview",
                "ciFailureLocalization",
                "agentProximity",
            ]
        );
        assert_eq!(changes[0].generation, 2);
        assert_eq!(queue.snapshot(&root, &subscriptions), changes);

        queue.offer(
            "diagnostics-2".to_owned(),
            change(ContextProjectionKind::diagnostics(), 2),
        );
        assert_eq!(queue.snapshot(&root, &subscriptions), changes);
        queue.offer(
            "diagnostics-3".to_owned(),
            change(ContextProjectionKind::diagnostics(), 3),
        );
        let latest = queue.snapshot(&root, &subscriptions);
        assert_eq!(latest.len(), 6);
        assert_eq!(latest[0].generation, 3);

        let delivered = ordered_context_changes(
            latest,
            vec![change(ContextProjectionKind::test_run_results(), 1)],
        );
        assert_eq!(
            delivered
                .iter()
                .map(|change| change.kind.as_str())
                .collect::<Vec<_>>(),
            vec![
                "diagnostics",
                "postEditImpact",
                "affectedTests",
                "githubReview",
                "ciFailureLocalization",
                "agentProximity",
                "testRunResults",
            ],
            "the source appends a test execution only after its saved-edit feedback cycle"
        );
    }

    #[test]
    fn context_projection_emits_only_active_findings() {
        assert!(finding_item(&finding(FeedbackFindingLifecycleV1::Active)).is_some());
        for lifecycle in [
            FeedbackFindingLifecycleV1::Superseded,
            FeedbackFindingLifecycleV1::Resolved,
            FeedbackFindingLifecycleV1::Cleared,
        ] {
            assert!(
                finding_item(&finding(lifecycle)).is_none(),
                "{lifecycle:?} finding remained visible"
            );
        }
    }

    #[test]
    fn advisory_projection_keeps_only_its_active_canonical_producer_findings() {
        let findings = [
            advisory_finding(
                FeedbackDiagnosticProducerV1::GitHubReview,
                FeedbackFindingLifecycleV1::Active,
            ),
            advisory_finding(
                FeedbackDiagnosticProducerV1::CiLocalization,
                FeedbackFindingLifecycleV1::Active,
            ),
            advisory_finding(
                FeedbackDiagnosticProducerV1::Proximity,
                FeedbackFindingLifecycleV1::Cleared,
            ),
        ];

        assert!(advisory_finding_matches(
            &findings[0],
            FeedbackDiagnosticProducerV1::GitHubReview
        ));
        assert!(!advisory_finding_matches(
            &findings[1],
            FeedbackDiagnosticProducerV1::GitHubReview
        ));
        assert!(finding_item(&findings[0]).is_some());
        assert!(
            finding_item(&findings[2]).is_none(),
            "a cleared advisory finding must clear its LSP projection"
        );
        assert_eq!(
            advisory_projection_producer(&ContextProjectionKind::github_review()),
            Some(FeedbackDiagnosticProducerV1::GitHubReview)
        );
        assert_eq!(
            advisory_projection_producer(&ContextProjectionKind::ci_failure_localization()),
            Some(FeedbackDiagnosticProducerV1::CiLocalization)
        );
        assert_eq!(
            advisory_projection_producer(&ContextProjectionKind::agent_proximity()),
            Some(FeedbackDiagnosticProducerV1::Proximity)
        );
    }

    #[test]
    fn advisory_projection_uses_its_canonical_provider_state_and_document() {
        let github = advisory_finding(
            FeedbackDiagnosticProducerV1::GitHubReview,
            FeedbackFindingLifecycleV1::Active,
        );
        let mut other_file = github.clone();
        other_file
            .diagnostic_projection
            .as_mut()
            .expect("advisory projection")
            .file = FileOccurrenceId::new("src/other.rs").expect("other file");
        let scope = projection_scope();

        assert!(finding_matches_document(&github, &scope, None));
        assert!(
            !finding_matches_document(&other_file, &scope, None),
            "a finding for another document must not receive this document's handle"
        );
        let states = [
            FeedbackAdvisoryProviderStateV1 {
                producer: FeedbackDiagnosticProducerV1::CiLocalization,
                state: ProviderEvaluationStateV1::Unavailable,
            },
            FeedbackAdvisoryProviderStateV1 {
                producer: FeedbackDiagnosticProducerV1::GitHubReview,
                state: ProviderEvaluationStateV1::SupportedCompletedComplete,
            },
            FeedbackAdvisoryProviderStateV1 {
                producer: FeedbackDiagnosticProducerV1::Proximity,
                state: ProviderEvaluationStateV1::Failed,
            },
        ];
        assert_eq!(
            advisory_provider_state(&states, FeedbackDiagnosticProducerV1::GitHubReview),
            Some(ProviderEvaluationStateV1::SupportedCompletedComplete)
        );
        assert_eq!(
            advisory_provider_state(&states, FeedbackDiagnosticProducerV1::CiLocalization),
            Some(ProviderEvaluationStateV1::Unavailable)
        );
        assert_eq!(
            advisory_provider_state(&states, FeedbackDiagnosticProducerV1::Proximity),
            Some(ProviderEvaluationStateV1::Failed)
        );
        assert_eq!(
            advisory_provider_state(
                &[FeedbackAdvisoryProviderStateV1 {
                    producer: FeedbackDiagnosticProducerV1::CiLocalization,
                    state: ProviderEvaluationStateV1::SupportedCompletedComplete,
                }],
                FeedbackDiagnosticProducerV1::GitHubReview,
            ),
            None,
            "an untyped aggregate provider vector must not be relabelled as GitHub"
        );
        assert_eq!(
            advisory_provider_status(advisory_provider_state(
                &states,
                FeedbackDiagnosticProducerV1::GitHubReview,
            )),
            (ContextCoverage::Complete, ContextProducerState::Complete)
        );
        assert_eq!(
            advisory_provider_status(advisory_provider_state(
                &states,
                FeedbackDiagnosticProducerV1::CiLocalization,
            )),
            (
                ContextCoverage::Unavailable,
                ContextProducerState::Unavailable
            )
        );
        assert_eq!(
            advisory_provider_status(advisory_provider_state(
                &states,
                FeedbackDiagnosticProducerV1::Proximity,
            )),
            (ContextCoverage::Failed, ContextProducerState::Failed)
        );
    }

    #[test]
    fn aggregate_advisory_omissions_are_unattributed_and_do_not_inflate_lanes() {
        assert_eq!(
            advisory_coverage(
                ContextCoverage::Complete,
                ContextProducerState::Complete,
                u64::MAX,
            ),
            (ContextCoverage::Partial, ContextProducerState::Partial),
            "cycle-wide omissions must be unattributed partiality, not a complete producer projection"
        );
        assert_eq!(
            bounded_advisory_item_omissions(7, 3),
            4,
            "only the lane's bounded items belong in its omitted count"
        );
        assert_eq!(
            bounded_advisory_item_omissions(3, 7),
            0,
            "an unattributed aggregate omission never inflates an individual lane"
        );
        assert_eq!(
            advisory_coverage(ContextCoverage::Complete, ContextProducerState::Complete, 0),
            (ContextCoverage::Complete, ContextProducerState::Complete)
        );
    }

    #[test]
    fn feedback_projection_requires_exact_saved_generation_and_file_identity() {
        let file_digest =
            ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("file digest");
        let scope = LspFeedbackProjectionScope {
            document_content_digest: Some(
                ContentDigest::new(file_digest.as_str().to_owned()).expect("document digest"),
            ),
            ..projection_scope()
        };
        let current = FeedbackContentIdentityV1::SavedContent {
            generation_digest: scope.snapshot_digest.clone(),
            file_digest: file_digest.clone(),
        };
        let stale_generation = FeedbackContentIdentityV1::SavedContent {
            generation_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
                .expect("stale generation"),
            file_digest: file_digest.clone(),
        };
        let stale_file = FeedbackContentIdentityV1::SavedContent {
            generation_digest: scope.snapshot_digest.clone(),
            file_digest: ManifestDigest::new(format!("sha256:{}", "f".repeat(64)))
                .expect("stale file"),
        };

        assert!(feedback_content_is_current(Some(&current), &scope));
        assert!(!feedback_content_is_current(
            Some(&stale_generation),
            &scope
        ));
        assert!(!feedback_content_is_current(Some(&stale_file), &scope));
        assert!(!feedback_content_is_current(None, &scope));
    }

    #[test]
    fn root_latest_test_run_is_not_relabelled_as_current_code_scope() {
        let scope = projection_scope();
        let snapshot = ManagedTestRunSnapshot {
            operation_id: OperationId::from_request(
                RequestId::new("request.test-run.unbound").expect("request"),
            ),
            generation: 7,
            source_revision: 1,
            head_commit_id: None,
            code_generation_id: None,
            document_content_digests: BTreeMap::new(),
            deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            results: Vec::new(),
            result_offset: 0,
            available_results: 0,
            next_cursor: None,
            completed: 0,
            total: Some(0),
            termination: Some(OperationTermination::Completed),
            receipt: None,
        };

        assert_eq!(
            test_run_projection(AdmittedRoot::new("file:///root"), None, scope, snapshot),
            ContextProjectionOutcome::Deferred {
                reason: "managed-test-run-head-unbound".to_owned(),
            }
        );
    }

    #[test]
    fn current_complete_test_run_projects_ready_results() {
        let scope = projection_scope();
        let snapshot = ManagedTestRunSnapshot {
            operation_id: OperationId::from_request(
                RequestId::new("request.test-run.current").expect("request"),
            ),
            generation: 7,
            source_revision: 1,
            head_commit_id: Some(scope.head_commit_id.clone()),
            code_generation_id: Some(scope.code_generation_id.clone()),
            document_content_digests: BTreeMap::new(),
            deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            results: vec![ManagedTestRunResult {
                test: "suite::passes".to_owned(),
                passed: true,
            }],
            result_offset: 0,
            available_results: 1,
            next_cursor: None,
            completed: 1,
            total: Some(1),
            termination: Some(OperationTermination::Completed),
            receipt: None,
        };

        let ContextProjectionOutcome::Ready(envelope) =
            test_run_projection(AdmittedRoot::new("file:///root"), None, scope, snapshot)
        else {
            panic!("current complete run must be ready");
        };
        assert_eq!(envelope.coverage, ContextCoverage::Complete);
        assert_eq!(envelope.producer_state, ContextProducerState::Complete);
        assert_eq!(envelope.items.len(), 1);
        assert_eq!(envelope.items[0].summary, "passed: suite::passes");
    }

    #[test]
    fn preexisting_dirty_overlay_cannot_relabel_saved_test_results() {
        let saved_digest =
            ContentDigest::new(format!("sha256:{}", "d".repeat(64))).expect("saved digest");
        let overlay_digest =
            ContentDigest::new(format!("sha256:{}", "e".repeat(64))).expect("overlay digest");
        let mut scope = LspFeedbackProjectionScope {
            document_content_digest: Some(saved_digest),
            ..projection_scope()
        };

        assert_eq!(
            bind_test_run_document_content(&mut scope, Some(overlay_digest)),
            Err("managed-test-run-document-content-stale")
        );
    }

    #[test]
    fn current_test_run_projection_reports_the_canonical_page_boundary() {
        let scope = projection_scope();
        let operation_id =
            OperationId::from_request(RequestId::new("request.test-run.bounded").expect("request"));
        let snapshot = ManagedTestRunSnapshot {
            operation_id: operation_id.clone(),
            generation: 7,
            source_revision: 1,
            head_commit_id: Some(scope.head_commit_id.clone()),
            code_generation_id: Some(scope.code_generation_id.clone()),
            document_content_digests: BTreeMap::new(),
            deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            results: (0..MAX_CONTEXT_PROJECTION_ITEMS)
                .map(|index| ManagedTestRunResult {
                    test: format!("suite::test_{index}"),
                    passed: true,
                })
                .collect(),
            result_offset: 0,
            available_results: MAX_CONTEXT_PROJECTION_ITEMS + 1,
            next_cursor: None,
            completed: (MAX_CONTEXT_PROJECTION_ITEMS + 1) as u64,
            total: Some((MAX_CONTEXT_PROJECTION_ITEMS + 1) as u64),
            termination: Some(OperationTermination::Completed),
            receipt: None,
        };

        let ContextProjectionOutcome::Ready(envelope) =
            test_run_projection(AdmittedRoot::new("file:///root"), None, scope, snapshot)
        else {
            panic!("bounded current run must be ready");
        };
        assert_eq!(envelope.coverage, ContextCoverage::Partial);
        assert_eq!(envelope.items.len(), MAX_CONTEXT_PROJECTION_ITEMS);
        assert_eq!(envelope.omitted_count, 1);
        assert_eq!(
            envelope.items[MAX_CONTEXT_PROJECTION_ITEMS - 1].stable_id,
            format!("{operation_id}.{}", MAX_CONTEXT_PROJECTION_ITEMS - 1)
        );
    }

    #[test]
    fn overlay_digest_drift_invalidates_saved_test_result_currentness() {
        let saved_digest =
            ContentDigest::new(format!("sha256:{}", "d".repeat(64))).expect("saved digest");
        let drifted_digest =
            ContentDigest::new(format!("sha256:{}", "e".repeat(64))).expect("drifted digest");
        let mut scope = LspFeedbackProjectionScope {
            document_content_digest: Some(saved_digest.clone()),
            ..projection_scope()
        };

        assert_eq!(
            bind_test_run_document_content(&mut scope, Some(saved_digest.clone())),
            Ok(())
        );
        assert_eq!(
            bind_test_run_document_content(&mut scope, Some(drifted_digest)),
            Err("managed-test-run-document-content-stale")
        );
        assert_eq!(scope.document_content_digest, Some(saved_digest));
    }

    #[test]
    fn saved_document_drift_rejects_stale_test_run_results() {
        let document_uri = "file:///root/src/lib.rs";
        let current_digest =
            ContentDigest::new(format!("sha256:{}", "d".repeat(64))).expect("current digest");
        let stale_digest =
            ContentDigest::new(format!("sha256:{}", "e".repeat(64))).expect("stale digest");
        let scope = LspFeedbackProjectionScope {
            document_content_digest: Some(current_digest),
            ..projection_scope()
        };
        let snapshot = ManagedTestRunSnapshot {
            operation_id: OperationId::from_request(
                RequestId::new("request.test-run.saved-drift").expect("request"),
            ),
            generation: 7,
            source_revision: 1,
            head_commit_id: Some(scope.head_commit_id.clone()),
            code_generation_id: Some(scope.code_generation_id.clone()),
            document_content_digests: BTreeMap::from([(document_uri.to_owned(), stale_digest)]),
            deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            results: Vec::new(),
            result_offset: 0,
            available_results: 0,
            next_cursor: None,
            completed: 0,
            total: Some(0),
            termination: Some(OperationTermination::Completed),
            receipt: None,
        };

        assert_eq!(
            test_run_projection(
                AdmittedRoot::new("file:///root"),
                Some(document_uri.to_owned()),
                scope,
                snapshot,
            ),
            ContextProjectionOutcome::Deferred {
                reason: "managed-test-run-document-content-stale".to_owned(),
            }
        );
    }

    #[test]
    fn overlay_projection_requires_test_run_document_identity() {
        let document_uri = "file:///root/src/lib.rs";
        let scope = LspFeedbackProjectionScope {
            document_content_digest: Some(
                ContentDigest::new(format!("sha256:{}", "d".repeat(64))).expect("document digest"),
            ),
            ..projection_scope()
        };
        let snapshot = ManagedTestRunSnapshot {
            operation_id: OperationId::from_request(
                RequestId::new("request.test-run.overlay-unbound").expect("request"),
            ),
            generation: 7,
            source_revision: 1,
            head_commit_id: Some(scope.head_commit_id.clone()),
            code_generation_id: Some(scope.code_generation_id.clone()),
            document_content_digests: BTreeMap::new(),
            deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            results: Vec::new(),
            result_offset: 0,
            available_results: 0,
            next_cursor: None,
            completed: 0,
            total: Some(0),
            termination: Some(OperationTermination::Completed),
            receipt: None,
        };

        assert_eq!(
            test_run_projection(
                AdmittedRoot::new("file:///root"),
                Some(document_uri.to_owned()),
                scope,
                snapshot,
            ),
            ContextProjectionOutcome::Deferred {
                reason: "managed-test-run-document-content-unbound".to_owned(),
            }
        );
    }

    #[test]
    fn expired_unfinished_test_run_projects_timed_out_unavailable() {
        let scope = projection_scope();
        let snapshot = ManagedTestRunSnapshot {
            operation_id: OperationId::from_request(
                RequestId::new("request.test-run.expired").expect("request"),
            ),
            generation: 7,
            source_revision: 1,
            head_commit_id: Some(scope.head_commit_id.clone()),
            code_generation_id: Some(scope.code_generation_id.clone()),
            document_content_digests: BTreeMap::new(),
            deadline: Deadline::new(UtcMicros(1)).expect("deadline"),
            results: Vec::new(),
            result_offset: 0,
            available_results: 0,
            next_cursor: None,
            completed: 0,
            total: Some(1),
            termination: None,
            receipt: None,
        };

        let ContextProjectionOutcome::Ready(envelope) =
            test_run_projection(AdmittedRoot::new("file:///root"), None, scope, snapshot)
        else {
            panic!("expired run must produce a terminal projection");
        };
        assert_eq!(envelope.coverage, ContextCoverage::Unavailable);
        assert_eq!(envelope.producer_state, ContextProducerState::TimedOut);
        assert!(envelope.items.is_empty());
    }

    #[test]
    fn noncomplete_test_terminations_use_protocol_valid_state_pairs() {
        for (termination, coverage, producer_state) in [
            (
                OperationTermination::Partial,
                ContextCoverage::Partial,
                ContextProducerState::Partial,
            ),
            (
                OperationTermination::Cancelled,
                ContextCoverage::Unavailable,
                ContextProducerState::Cancelled,
            ),
            (
                OperationTermination::EffectUnknown,
                ContextCoverage::Unavailable,
                ContextProducerState::Unavailable,
            ),
        ] {
            let scope = projection_scope();
            let snapshot = ManagedTestRunSnapshot {
                operation_id: OperationId::from_request(
                    RequestId::new(format!("request.test-run.{termination:?}")).expect("request"),
                ),
                generation: 7,
                source_revision: 1,
                head_commit_id: Some(scope.head_commit_id.clone()),
                code_generation_id: Some(scope.code_generation_id.clone()),
                document_content_digests: BTreeMap::new(),
                deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
                results: Vec::new(),
                result_offset: 0,
                available_results: 0,
                next_cursor: None,
                completed: 0,
                total: Some(1),
                termination: Some(termination),
                receipt: None,
            };
            let ContextProjectionOutcome::Ready(envelope) =
                test_run_projection(AdmittedRoot::new("file:///root"), None, scope, snapshot)
            else {
                panic!("{termination:?} run must be ready");
            };
            assert_eq!(envelope.coverage, coverage);
            assert_eq!(envelope.producer_state, producer_state);
        }
    }
}

#[cfg(test)]
#[path = "lsp_runtime/projection_identity_tests.rs"]
mod projection_identity_tests;

#[cfg(test)]
#[path = "lsp_runtime/overlay_admission_tests.rs"]
mod overlay_admission_tests;

#[cfg(test)]
mod diagnostic_admission_tests {
    use super::{FeedbackDiagnosticProjectionSkipV1, classify_feedback_diagnostic_admission};
    use crate::diagnostics_publication::{
        CleanGenerationDiagnosticScopeV1, CleanGenerationDiagnosticSnapshotBuilderV1,
        DiagnosticContributionV1, DiagnosticPillarV1,
    };
    use tracedecay_domain::{
        CodeGenerationId, CommitId, ContentDigest, DiagnosticRecordStateV1, DiagnosticSeverityV1,
        FileOccurrenceId, GenerationDiagnosticV1, SourceSpan, UtcMicros,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    /// Builds a record through the real production publication builder so the
    /// admission rules are exercised against records shaped exactly like the
    /// ones a pillar publishes.
    fn record(pillar: DiagnosticPillarV1) -> GenerationDiagnosticV1 {
        let mut builder =
            CleanGenerationDiagnosticSnapshotBuilderV1::new(CleanGenerationDiagnosticScopeV1 {
                generation_id: id("generation.admission.1"),
                repository: id("repository.fixture"),
                worktree: Some(id("worktree.fixture")),
                reference: Some(id("ref.main")),
                source_revision: Some(id("commit.head")),
                analyzer_revision: id("analyzer.v1"),
                configuration_revision: id("config.v1"),
                collected_at: UtcMicros(1_700_000_000_000_000),
            });
        builder
            .contribute(
                pillar,
                DiagnosticContributionV1 {
                    anchor: id("anchor.admission.1"),
                    file_occurrence_id: id("src/lib.rs"),
                    content_digest: id(&digest('a')),
                    span: SourceSpan {
                        start_byte: 0,
                        end_byte: 4,
                    },
                    symbol_occurrence_id: None,
                    code: "E0308".to_owned(),
                    severity: DiagnosticSeverityV1::Error,
                    message: "mismatched types".to_owned(),
                },
            )
            .expect("contribution accepted");
        builder.records().pop().expect("one record")
    }

    fn admit(
        record: &GenerationDiagnosticV1,
        target: Option<&FileOccurrenceId>,
    ) -> Result<(), FeedbackDiagnosticProjectionSkipV1> {
        classify_feedback_diagnostic_admission(
            record,
            target,
            &id::<CodeGenerationId>("generation.admission.1"),
            &id::<ContentDigest>(&digest('a')),
            &id::<CommitId>("commit.head"),
        )
    }

    #[test]
    fn every_pillar_record_is_admitted_when_identity_matches() {
        for pillar in [
            DiagnosticPillarV1::Compiler,
            DiagnosticPillarV1::GitHubReview,
            DiagnosticPillarV1::CiLocalization,
            DiagnosticPillarV1::Proximity,
        ] {
            let record = record(pillar);
            let target: FileOccurrenceId = id("src/lib.rs");
            assert_eq!(
                admit(&record, Some(&target)),
                Ok(()),
                "{pillar:?} record was refused despite exact identity"
            );
        }
    }

    /// The formerly silent case: the record attaches to a different file than
    /// the cycle's impact target. It must now be a named refusal.
    #[test]
    fn impact_target_file_mismatch_is_named_not_silent() {
        let record = record(DiagnosticPillarV1::Compiler);
        let other: FileOccurrenceId = id("src/other.rs");
        assert_eq!(
            admit(&record, Some(&other)),
            Err(FeedbackDiagnosticProjectionSkipV1::ImpactTargetFileMismatch)
        );
    }

    #[test]
    fn absent_impact_target_is_named_separately_from_mismatch() {
        let record = record(DiagnosticPillarV1::Proximity);
        assert_eq!(
            admit(&record, None),
            Err(FeedbackDiagnosticProjectionSkipV1::ImpactTargetAbsent)
        );
    }

    #[test]
    fn generation_content_state_and_revision_drift_each_have_a_reason() {
        let target: FileOccurrenceId = id("src/lib.rs");

        let mut wrong_generation = record(DiagnosticPillarV1::GitHubReview);
        wrong_generation.generation_id = id("generation.admission.2");
        assert_eq!(
            admit(&wrong_generation, Some(&target)),
            Err(FeedbackDiagnosticProjectionSkipV1::GenerationMismatch)
        );

        let mut wrong_content = record(DiagnosticPillarV1::CiLocalization);
        wrong_content.content_digest = id(&digest('b'));
        assert_eq!(
            admit(&wrong_content, Some(&target)),
            Err(FeedbackDiagnosticProjectionSkipV1::ContentDigestMismatch)
        );

        let mut cleared = record(DiagnosticPillarV1::Compiler);
        cleared.state = DiagnosticRecordStateV1::Cleared {
            cleared_in_generation: id("generation.admission.2"),
        };
        assert_eq!(
            admit(&cleared, Some(&target)),
            Err(FeedbackDiagnosticProjectionSkipV1::RecordNotCurrent)
        );

        let mut drifted = record(DiagnosticPillarV1::Compiler);
        drifted.source_revision = Some(id("commit.other"));
        assert_eq!(
            admit(&drifted, Some(&target)),
            Err(FeedbackDiagnosticProjectionSkipV1::SourceRevisionDrift)
        );
    }

    #[test]
    fn every_skip_reason_has_a_distinct_label() {
        let labels = [
            FeedbackDiagnosticProjectionSkipV1::LifecycleNotActive,
            FeedbackDiagnosticProjectionSkipV1::NoRetrievalAnchor,
            FeedbackDiagnosticProjectionSkipV1::AnchorNotPublished,
            FeedbackDiagnosticProjectionSkipV1::ImpactTargetFileMismatch,
            FeedbackDiagnosticProjectionSkipV1::ImpactTargetAbsent,
            FeedbackDiagnosticProjectionSkipV1::GenerationMismatch,
            FeedbackDiagnosticProjectionSkipV1::ContentDigestMismatch,
            FeedbackDiagnosticProjectionSkipV1::RecordNotCurrent,
            FeedbackDiagnosticProjectionSkipV1::SourceRevisionDrift,
        ]
        .map(FeedbackDiagnosticProjectionSkipV1::label);
        let unique: std::collections::BTreeSet<&str> = labels.into_iter().collect();
        assert_eq!(unique.len(), labels.len());
    }
}

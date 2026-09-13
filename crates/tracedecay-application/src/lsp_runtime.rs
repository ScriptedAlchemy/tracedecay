//! Production LSP composition over the canonical feedback runtime.
//!
//! The adapter mints authorized reads through [`FeedbackRuntime`] and
//! invokes its daemon owner. The cloned [`ProjectFeedbackStore`] is the same
//! durable publication/dedupe authority used by the feedback cycle; this
//! module creates no feedback store, cache, cursor codec, or diagnostic
//! authority.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use tracedecay_contracts::OperationTermination;
use tracedecay_contracts::feedback::FeedbackDiagnosticsReadResultV1;
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, FileOccurrenceId, ManifestDigest, UtcMicros,
};
use tracedecay_lsp::analyzer::broker::DiagnosticBroker;
use tracedecay_lsp::{
    AdmittedRoot, ContextCoverage, ContextProducerState, ContextProjectionChange,
    ContextProjectionIdentity, ContextProjectionKind, ContextProjectionRegistration,
    FeedbackCycleRuntimePort, GatewayCapabilities, LspAnalyzerCancellationAuthority, LspPosition,
    LspRuntimeFailure, SemanticProviderPort, TRACEDECAY_CONTEXT_REVISION, UpstreamCapabilities,
    byte_offset_to_utf16_position,
};

use crate::feedback::concrete::{FeedbackRuntime, ProjectFeedbackStore};
use crate::feedback::owner::FeedbackReadOperationV1;
pub use crate::lsp_support::{
    BrokerDiagnosticSnapshotAuthority, DaemonLspSessionFactory, DaemonSemanticProviderAdapter,
    LspDiagnosticDocumentPort, LspSemanticRequestAuthority, LspWorkspaceDocumentIndexPort,
    UpstreamCapabilityInitializationAuthority,
};
const LSP_CONTEXT_EXPANSION_HANDLE_SCHEMA_VERSION: u16 = 1;
const LSP_TEST_RUN_EXPANSION_HANDLE_SCHEMA_VERSION: u16 = 1;

fn byte_offsets_to_utf16_range(
    text: &str,
    start: usize,
    end: usize,
) -> Result<(LspPosition, LspPosition), LspRuntimeFailure> {
    let start_pos = byte_offset_to_utf16_position(text, start)
        .map_err(|_| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
    if start == end {
        return Ok((start_pos, start_pos));
    }
    if start > end {
        return Err(LspRuntimeFailure::new("diagnostic-span-invalid"));
    }
    let between = text
        .get(start..end)
        .ok_or_else(|| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
    if !between.contains('\n') && !between.contains('\r') {
        let extra = between
            .chars()
            .map(|value| value.len_utf16() as u32)
            .fold(0u32, u32::saturating_add);
        return Ok((
            start_pos,
            LspPosition {
                line: start_pos.line,
                character: start_pos.character.saturating_add(extra),
            },
        ));
    }
    let mut line = start_pos.line;
    let mut character = start_pos.character;
    let bytes = text.as_bytes();
    let mut index = start;
    while index < end {
        match bytes.get(index) {
            Some(b'\n') => {
                line = line.saturating_add(1);
                character = 0;
                index += 1;
            }
            Some(b'\r') => {
                if bytes.get(index + 1) == Some(&b'\n') {
                    if index + 1 == end {
                        return Err(LspRuntimeFailure::new("diagnostic-span-invalid"));
                    }
                    index += 2;
                } else {
                    index += 1;
                }
                line = line.saturating_add(1);
                character = 0;
            }
            Some(_) => {
                let ch = text[index..]
                    .chars()
                    .next()
                    .ok_or_else(|| LspRuntimeFailure::new("diagnostic-span-invalid"))?;
                character = character.saturating_add(ch.len_utf16() as u32);
                index += ch.len_utf8();
            }
            None => return Err(LspRuntimeFailure::new("diagnostic-span-invalid")),
        }
    }
    Ok((start_pos, LspPosition { line, character }))
}
const LSP_TEST_RUN_EXPANSION_TTL_MICROS: i64 = 15 * 60 * 1_000_000;

mod projection_identity;
pub use projection_identity::{
    LspCodeIndexProjectionIdentity, LspCodeIndexProjectionIdentityPort,
    LspCodeIndexWorktreeGraphScope,
};
mod diagnostic_records;
mod overlay_admission;
pub use diagnostic_records::LspFeedbackDiagnosticRecordPort;
mod semantic;
pub use semantic::{ProductionSemanticAuthorities, production_semantic_authorities};
#[cfg(test)]
mod advisory_source_tests;
#[cfg(test)]
mod context_expansion_tests;
mod context_projection;
#[cfg(test)]
mod diagnostic_admission_tests;
mod diagnostic_projection;
mod document_paths;
mod feedback_source;
mod managed_test_runs;
#[cfg(test)]
mod path_tests;
#[cfg(test)]
mod projection_tests;
mod registered_authority;

pub use diagnostic_projection::{
    DiagnosticsStoreLspFeedbackProjection, FeedbackDiagnosticProjectionSkipV1,
    LspFeedbackDiagnosticProjectionPort, LspFeedbackDocumentSnapshot,
    LspFeedbackDocumentSnapshotPort, classify_feedback_diagnostic_admission,
};
pub use feedback_source::ConcreteFeedbackLspSource;
pub use managed_test_runs::LspTestRunProjectionPort;
pub(crate) use managed_test_runs::lsp_test_result_port;
pub use registered_authority::{LspFeedbackProjectionScopePort, RegisteredProjectLspAuthority};

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

#[cfg(test)]
#[path = "lsp_runtime/projection_identity_tests.rs"]
mod projection_identity_tests;

#[cfg(test)]
#[path = "lsp_runtime/overlay_admission_tests.rs"]
mod overlay_admission_tests;

//! Daemon-owned scheduling and reconciliation for production code generations.
//!
//! Hook events are bounded wake-up hints only. Every run reconstructs its
//! source snapshot from gix's HEAD-tree/index/worktree status before content
//! digests decide whether publication is necessary.
use std::time::Duration;

use tracedecay_domain::{
    ContentDigest, FileOccurrenceId, ManifestDigest, ProjectionKeyV1, ProjectionKindV1,
    RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, SnapshotFileDispositionV1,
    WorktreeId, canonical_text::sha256_hex,
};

use crate::code_index::chunks::content_digest;

// Reached only as `super::…` by `registry`, `ignored_dependencies`,
// `branch_generations`, and `branch_publication`.
use self::freshness_witness::SourceContentManifestV1;
use crate::code_index::{
    languages::StaticLanguageRegistry, production::CodeIndexPublishedGenerationV1,
};
use tracedecay_application::code_index::DaemonCodeIndexControlV1;
use tracedecay_contracts::now_micros;

/// Std mutex wrapped for Hotpath lock-contention accounting. Condvar-paired
/// mutexes (the generation-decode barrier and the text-projection slot)
/// cannot use this wrapper because `Condvar::wait` requires the exact std
/// guard type; those measure lock-wait and parked wait with explicit spans
/// instead.
type ProfiledStdMutex<T> = hotpath::mutexes::Mutex<T>;

/// Freshness contract for non-git-mediated mutations (raw file writes, rsync,
/// out-of-agent saves): a query admitted after this bound since the last
/// reconciliation re-checks gix truth before serving. Git-mediated changes are
/// caught immediately by the tier-1 metadata check regardless of this bound.
const DEFAULT_STALENESS_THRESHOLD: Duration = Duration::from_secs(30);
pub use tracedecay_code_index_retention::code_index_generations::scoped_code_index_store_root;

/// How the scheduler is hinted about changes.
///
/// `TraceDecay`'s edits are agent-driven, so the daemon already learns about
/// touched paths through host after-file-edit hooks; those are the primary
/// hint source and require no standing filesystem watches. gix status remains
/// the sole truth, reconciled lazily (on open, on hook receipt, and on the
/// query-admission freshness ladder).
#[derive(Clone, Copy, Debug)]
pub struct CodeIndexHintPolicyV1 {
    /// Tier-2 bounded-staleness reconcile threshold for non-git mutations.
    pub staleness_threshold: Duration,
}

impl Default for CodeIndexHintPolicyV1 {
    fn default() -> Self {
        Self {
            staleness_threshold: DEFAULT_STALENESS_THRESHOLD,
        }
    }
}

fn id<T>(value: &str) -> Result<T, CodeIndexSchedulerErrorV1>
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value.to_owned())
        .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))
}

fn file_occurrence_id(
    repository: &RepositoryId,
    worktree: &WorktreeId,
    logical_path: &str,
    digest: &ContentDigest,
    receipt: &SanitizationReceiptId,
) -> Result<FileOccurrenceId, CodeIndexSchedulerErrorV1> {
    id(&format!(
        "file.daemon.{}",
        sha256_hex(
            format!(
                "{}\0{}\0{logical_path}\0{}\0{}",
                repository.as_str(),
                worktree.as_str(),
                digest.as_str(),
                receipt.as_str(),
            )
            .as_bytes()
        )
    ))
}

fn omitted_file_occurrence_id(
    repository: &RepositoryId,
    worktree: &WorktreeId,
    logical_path: &str,
    digest: &ContentDigest,
    disposition: SnapshotFileDispositionV1,
) -> Result<FileOccurrenceId, CodeIndexSchedulerErrorV1> {
    let disposition = match disposition {
        SnapshotFileDispositionV1::Ignored => "ignored",
        SnapshotFileDispositionV1::Binary => "binary",
        SnapshotFileDispositionV1::Generated => "generated",
        SnapshotFileDispositionV1::UnsupportedLanguage => "unsupported_language",
        SnapshotFileDispositionV1::Present
        | SnapshotFileDispositionV1::Deleted
        | SnapshotFileDispositionV1::Renamed => {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "omitted file occurrence requires an omitted disposition".to_owned(),
            ));
        }
    };
    id(&format!(
        "file.daemon.omitted.{}",
        sha256_hex(
            format!(
                "{}\0{}\0{logical_path}\0{}\0{disposition}",
                repository.as_str(),
                worktree.as_str(),
                digest.as_str(),
            )
            .as_bytes()
        )
    ))
}

fn projection_key() -> Result<ProjectionKeyV1, CodeIndexSchedulerErrorV1> {
    Ok(ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.daemon.v1".to_owned(),
        profile_digest: id::<ManifestDigest>(&format!("sha256:{}", "d".repeat(64)))?,
    })
}

fn snapshot_content_identity(
    files: &[SanitizedCodeFileV1],
    sanitization_receipts: &[SanitizationReceiptId],
) -> ContentDigest {
    let mut bytes = Vec::new();
    for file in files {
        bytes.extend_from_slice(file.logical_path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(file.content_digest.as_str().as_bytes());
        bytes.push(0xff);
    }
    for receipt in sanitization_receipts {
        bytes.extend_from_slice(receipt.as_str().as_bytes());
        bytes.push(0xfe);
    }
    content_digest(&bytes)
}

#[cfg(test)]
mod activation_tests;
#[cfg(test)]
mod memory_tests;
#[cfg(test)]
mod overlay_ephemerality_tests;
#[cfg(test)]
mod tests;

mod activation;
pub mod branch_generations;
pub mod branch_publication;
mod cadence;
mod classification;
mod freshness_witness;
mod git_tree_capture;
pub use git_tree_capture::{
    ExactGitTreeSourceV1, NativeCandidateGenerationBindingsV1, NativeCandidateGenerationIdentityV1,
    NativeCandidateGenerationSourcesV1,
};
mod graph_activation;
pub mod identity;
pub mod ignored_dependencies;
pub mod observability;
mod privacy;
mod publication_store;
pub mod queries;
pub mod query_runtime;
mod reconcile;
mod reconcile_panic_guard;
mod registry;
pub mod semantic_query_runtime;
pub mod semantic_vector_graph;
mod serving;

// The registry surface lives in `registry.rs`; re-export it so its public path
// (`code_index_scheduler::CodeIndexSchedulerRegistryV1`) and method signatures
// stay stable for the daemon and MCP server that mount and query worktrees.
pub use crate::code_graph_seat::CodeGraphReplayBindingV1;
pub use activation::{
    CodeIndexActivationHintSinkV1, CodeIndexActivationMountV1, CodeIndexActivationV1,
    CodeIndexAutomaticAdmissionV1,
};
#[cfg(test)]
pub use cadence::CodeIndexCadenceReadModelV1;
pub use cadence::{
    CodeIndexArrivalV1, CodeIndexCadenceOutcomeV1, CodeIndexCadenceTelemetryV1,
    CodeIndexCadenceTriggerV1, CodeIndexEventToReadyReceiptV1, newly_eligible_percentile,
};
pub use graph_activation::{CodeGraphActivationAuthorityV1, CodeGraphActivationPolicyV1};
pub use ignored_dependencies::{
    CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexIgnoredDependencyRefusalV1,
    CodeIndexIgnoredDependencyRequestV1,
};
pub use registry::CodeIndexSchedulerRegistryV1;
pub use registry::watch_ingress::GitStateChangeRequestV1;
pub use registry::{
    ScopedFeedbackDocumentIdentityV1, ServingGenerationInstallationOutcomeV1,
    ServingGenerationRollbackOutcomeV1, feedback_document_identity_from_generation,
};
pub type CodeIndexGenerationPublishedV1 = registry::CodeIndexGenerationPublishedV1;

// The scheduler body is split by seam: `publication_store` (durable sealed
// generations), `serving` (the latest complete generation's query owners and
// text artifact), and `reconcile` (the per-worktree scheduler). Re-export
// every item the siblings, the tests, and the crate reach through this
// module so no path outside the seam files changes.
use publication_store::DaemonProjectionSinkV1;
#[cfg(test)]
pub use publication_store::{CodeIndexBytePoolStatsV1, HeldActiveDecodeV1};
#[cfg(test)]
use publication_store::{DECODED_GENERATION_CACHE_CAPACITY, TemporaryEvidencePackV1};
pub use publication_store::{
    DaemonCodeIndexPublicationStoreV1, GenerationDecodeAdmissionV1, SharedCodeIndexBytePoolV1,
};
#[cfg(test)]
use reconcile::CODE_INDEX_WORKER_RESIDENT_COMPONENT_V1;
use reconcile::{
    CapturedCandidateV1, CapturedSnapshotV1, FreshnessProbeVerdictV1, PendingHintsV1,
    RetainedTextGenerationRestoreV1, cancelled_code_index_reconcile,
};
pub use reconcile::{
    CodeIndexNoopEvidenceV1, CodeIndexPublishEvidenceV1, CodeIndexReconcileOutcomeV1,
    CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1, HistoricalCodeIndexGenerationOwnerV1,
    ReconcilePassGuard,
};
pub(crate) use reconcile::{ServingSourceWitnessV1, SourceFreshnessFenceV1};
use serving::{
    CodeGraphActivationStateV1, CodeGraphServingAuthorityV1, CodeIndexBuildProgressStateV1,
    CodeTextProjectionStateV1, DurableActiveSealedGenerationBindingV1, GenerationServingCachesV1,
    GenerationTextControlV1, TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1, try_publish_build_progress,
};
pub use serving::{
    CodeIndexBuildProgressSlotStateV1, CodeIndexBuildProgressSlotV1, DaemonCodeTextArtifactStoreV1,
    LatestCodeTextGenerationV1, LatestCompleteCodeIndexV1, ProductionCodeIndexQueryOwnersV1,
    SemanticEvaluationCodeSnapshotV1,
};
#[cfg(test)]
use serving::{
    CodeIndexCommittedProgressSampleV1, CodeTextProjectionSlotV1, TEXT_ARTIFACT_PAGE_CHUNKS_V1,
    map_sealed_page_source_error, sha256_private_file_and_size, text_artifact_builder_budget,
    text_artifact_resident_memory_charges, text_artifact_source_batch_limits,
};

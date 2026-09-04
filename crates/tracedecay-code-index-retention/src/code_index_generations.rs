//! Liveness-based retention for immutable code-index generations.
//!
//! The code-index store is derived, but a generation can still be live while
//! the active code pointer or a readable vector inventory names it. Collection
//! therefore uses conservative mark-and-sweep rather than refcounts: a missed
//! mark costs disk space, while a miscount could silently remove readable code
//! evidence. The mark set is every generation addressable through the durable
//! publication pointer and every vector-readable source. Callers may request a
//! rollback floor explicitly, but the production default adds no unbounded
//! evidence beyond the pointer's byte-, time-, and count-bounded history.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_private_fs::framed_log::DirectorySyncPolicy;
// The census gates on the exact revision the publisher writes. A second copy of
// that number here let the writer be versioned to 3 while retention still
// demanded 1: every real sealed file was refused as "incompatible" and the store
// became uncollectable.
use tracedecay_code_index::production::{
    SEALED_GENERATION_FORMAT_REVISION_V1, sealed_generation_format_revision_is_compatible,
};
use tracedecay_domain::canonical_text::encode_lowercase_hex;
/// Only the generation fixtures build tagged digests; production here works in
/// untagged hex, so importing this unconditionally is an unused-import error.
#[cfg(test)]
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
use tracedecay_domain::canonical_text::{is_lowercase_hex, sha256_hex};
use tracedecay_domain::{CodeGenerationId, ManifestDigest, UtcMicros, canonical_sha256};

mod generation_scan;
mod generation_transactions;
mod graph_replay_release;
mod journal;
mod locking;
mod receipt_store;
mod scope_quarantine;
mod scope_roots;
mod text_artifacts;
pub use graph_replay_release::{
    CodeGenerationGraphReplayReleasePageV1, CodeGenerationGraphReplayReleaseV1,
    code_generation_graph_replay_release_page, complete_code_generation_graph_replay_release,
};
pub use locking::{
    CodeGenerationStoreLockV1, acquire_code_generation_store_lock,
    try_acquire_code_generation_store_lock,
};
pub use scope_roots::{
    RefusedCodeIndexScopeV1, ScopeRootAuthorityReceiptV1, ScopeRootBindingCleanupReplayV1,
    ScopeRootCandidateBindingV1, ScopeRootLivenessProofV1, ScopeRootRetentionPlanV1,
    ScopeRootRetentionReceiptV1, ScopeRootRetentionReportV1, StrandedCodeIndexScopeV1,
    StrandedScopeRefusalV1, complete_scope_root_binding_cleanup, execute_scope_root_retention,
    plan_scope_root_retention, plan_scope_root_retention_with_liveness_proof,
    prepare_scope_root_binding_cleanup, recover_scope_root_binding_cleanup,
    recover_scope_root_retention,
};
pub use text_artifacts::{
    attach_verified_text_artifact_under_lock, withdraw_verified_text_artifact_under_lock,
};

use generation_transactions::{
    GENERATION_RECEIPT_STORE, acquire_graph_replay_pool_lock_checked,
    cleanup_committed_transaction, cleanup_committed_transaction_under_graph_replay_pool_lock,
    clear_transaction, expose_staged_generations_under_graph_replay_pool_lock, load_transaction,
    open_file_sha256_hex_cancellable, path_still_names_open_file, persist_transaction,
    receipt_is_durable, regular_file_exists, remove_empty_stage_root, rollback_staged_transaction,
    stage_collectable_generations, transaction_path, write_receipt,
};
#[cfg(test)]
use generation_transactions::{
    acquire_graph_replay_pool_lock, transaction_stage_root, verify_existing_graph_replay_pool_entry,
};
use receipt_store::receipt_digest_file_component;
use scope_roots::is_code_index_scope_hash;
#[cfg(test)]
use scope_roots::{
    ScopeRootRetentionTransactionV1, build_scope_receipt, persist_scope_transaction,
    scope_receipt_digest, scope_receipt_path, scope_stage_root, scope_transaction_path,
    validate_scope_transaction, write_scope_receipt,
};
#[cfg(test)]
use text_artifacts::{
    build_text_artifact_receipt, persist_text_artifact_transaction,
    stage_collectable_text_artifacts, total_text_artifact_bytes, write_text_artifact_receipt,
};
use text_artifacts::{
    execute_text_artifact_retention_under_store_lock, plan_collectable_text_artifacts_cancellable,
    recover_pending_text_artifact_transaction_unlocked, text_artifact_transaction_path,
};

use generation_scan::{read_generation_format_revision, read_generation_metadata};
#[cfg(test)]
use scope_quarantine::ScopeQuarantineAuthority;

pub const DEFAULT_SUPERSEDED_GENERATION_FLOOR: usize = 0;
pub const MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1: usize = 32;
pub const MAX_DURABLE_GENERATION_INDEX_BYTES_V1: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_DURABLE_GENERATION_INDEX_TTL_MICROS_V1: i64 = 7 * 24 * 60 * 60 * 1_000_000;
pub const MAX_DURABLE_PUBLICATION_POINTER_BYTES_V1: u64 = 512 * 1024;
pub const ACTIVE_CODE_TEXT_ARTIFACT_FILE_V1: &str = "active-code-text-artifact-v1.json";
pub const CODE_TEXT_ARTIFACT_HEAD_SCHEMA_V1: &str = "tracedecay.code-text-artifact-head.v1";
pub const CODE_TEXT_ARTIFACTS_DIRECTORY_V1: &str = "code-text-artifacts-v1";

/// How long a code-index scope root must have been untouched before it can be
/// classified as stranded and collected. A worktree can be unmounted, moved, or
/// temporarily unavailable; only a scope that has been quiet for this long is
/// treated as abandoned rather than idle.
pub const DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS: i64 = 7 * 24 * 60 * 60;

/// How long collection may poll a held graph-replay pool before deferring.
///
/// Publication hashes multi-GiB seals under this lock. Waiting that out
/// behind the daemon writer gate is the remaining TOCTOU after the outer
/// non-blocking probe: the executor must bound its own acquire instead of
/// falling back to a deadline-free blocking flock.
pub const GRAPH_REPLAY_POOL_ACQUIRE_BUDGET: Duration = Duration::from_millis(50);

/// Pause between non-blocking pool-lock probes while the acquire budget remains.
pub const GRAPH_REPLAY_POOL_ACQUIRE_POLL: Duration = Duration::from_millis(5);

const ACTIVE_POINTER_FILE: &str = "active-code-generation-v1.json";
const GENERATIONS_DIRECTORY: &str = "code-generations-v1";
const GENERATION_SEGMENTS_DIRECTORY: &str = "code-generation-segments-v1";
const RECEIPTS_DIRECTORY: &str = "code-generation-retention-receipts-v1";
const QUARANTINE_DIRECTORY: &str = ".code-generation-retention-quarantine-v1";
const STORE_LOCK_FILE: &str = ".code-generation-retention.lock";
const RECEIPT_SCHEMA: &str = "tracedecay.code-generation-retention-receipt.v1";
const TRANSACTION_FILE: &str = ".code-generation-retention-transaction-v1.json";
const TRANSACTION_SCHEMA: &str = "tracedecay.code-generation-retention-transaction.v1";
const TEXT_ARTIFACT_RECEIPTS_DIRECTORY: &str = "code-text-artifact-retention-receipts-v1";
const TEXT_ARTIFACT_QUARANTINE_DIRECTORY: &str = ".code-text-artifact-retention-quarantine-v1";
const TEXT_ARTIFACT_TRANSACTION_FILE: &str = ".code-text-artifact-retention-transaction-v1.json";
const TEXT_ARTIFACT_RECEIPT_SCHEMA: &str = "tracedecay.code-text-artifact-retention-receipt.v1";
const TEXT_ARTIFACT_TRANSACTION_SCHEMA: &str =
    "tracedecay.code-text-artifact-retention-transaction.v1";
const GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY: &str = "graph-replay-release-queue";
const GRAPH_REPLAY_RELEASE_SCHEMA: &str = "tracedecay.graph-replay-release.v1";

/// Scope-root reconciliation artifacts. They deliberately live in the *parent*
/// `code-index-v1/` directory rather than inside a scope: the scope directory is
/// what gets collected, so a receipt written inside it would vanish with the
/// evidence it certifies.
const SCOPE_RETENTION_LOCK_FILE: &str = ".code-index-scope-retention.lock";
const SCOPE_RETENTION_TRANSACTION_FILE: &str = ".code-index-scope-retention-transaction-v1.json";
const SCOPE_RETENTION_QUARANTINE_DIRECTORY: &str = ".code-index-scope-retention-quarantine-v1";
const SCOPE_RETENTION_RECEIPTS_DIRECTORY: &str = "code-index-scope-retention-receipts-v1";
const SCOPE_RETENTION_RECEIPT_SCHEMA: &str = "tracedecay.code-index-scope-retention-receipt.v1";
const SCOPE_RETENTION_TRANSACTION_SCHEMA: &str =
    "tracedecay.code-index-scope-retention-transaction.v1";
const SCOPE_BINDING_CLEANUP_INTENT_FILE: &str = ".code-index-scope-binding-cleanup-intent-v1.json";
const SCOPE_BINDING_CLEANUP_INTENT_SCHEMA: &str =
    "tracedecay.code-index-scope-binding-cleanup-intent.v1";
const SCOPE_ROOT_LIVENESS_PROOF_SCHEMA: &str = "tracedecay.code-index-scope-liveness-proof.v1";
const MAX_SCOPE_TRANSACTION_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SCOPE_BINDING_CLEANUP_INTENT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SCOPE_ROOTS_PER_INVENTORY: usize = 4_096;

const MAX_GENERATION_METADATA_PREFIX_BYTES: usize = 16 * 1024 * 1024;
const MAX_TRANSACTION_BYTES: u64 = 1024 * 1024;
pub const MAX_CODE_GENERATION_RETENTION_BATCH_V1: usize = 32;
/// One maintenance pass removes at most this many derived text-artifact files.
///
/// The bounded durable index can protect at most 32 completed artifacts and
/// the active generation can own one resumable staging file. The inventory
/// reads that fixed liveness window plus one removal page, so a restart reaches
/// later debris without ever materializing an unbounded directory listing.
const MAX_CODE_TEXT_ARTIFACT_RETENTION_BATCH_V1: usize = 32;
const MAX_CODE_TEXT_ARTIFACT_INVENTORY_ENTRIES_V1: usize =
    MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1 + 1 + MAX_CODE_TEXT_ARTIFACT_RETENTION_BATCH_V1;

#[inline]
fn observe_cancel(is_cancelled: &dyn Fn() -> bool) -> bool {
    let cancelled = is_cancelled();
    if cancelled {
        crate::hotpath_observe::retention_cancelled();
    }
    cancelled
}

#[derive(Deserialize)]
struct SealedGenerationManifestMetadataV1 {
    generation_id: CodeGenerationId,
    seal: SealedGenerationSealMetadataV1,
}

#[derive(Deserialize)]
struct SealedGenerationSealMetadataV1 {
    sealed_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DurableSealedCodeGenerationIdentityV1 {
    pub locator: String,
    pub digest: ManifestDigest,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DurableCodeTextArtifactDescriptorV1 {
    pub generation_id: CodeGenerationId,
    pub artifact_file: String,
    pub artifact_digest: ManifestDigest,
    pub artifact_size_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DurableGenerationCardinalityV1 {
    pub file_count: u64,
    pub chunk_count: u64,
    pub symbol_count: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DurableGenerationIndexEntryV1 {
    pub generation_id: String,
    pub snapshot_content_identity: String,
    pub sealed_at_micros: i64,
    pub size_bytes: u64,
    #[serde(default)]
    pub segment_bytes: u64,
    pub generation_file: String,
    pub state_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_reference: Option<String>,
    pub source_revision: Option<String>,
    pub source_tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<DurableGenerationCardinalityV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_artifact: Option<DurableCodeTextArtifactDescriptorV1>,
}

/// Apply the durable exact-generation history bounds in canonical oldest-first
/// order. The active generation is never evicted; every other generation,
/// including dirty snapshots without Git evidence, is subject to the same TTL,
/// byte, and count limits.
pub fn retain_bounded_generation_index(
    entries: &mut Vec<DurableGenerationIndexEntryV1>,
    active_generation_id: &str,
) -> usize {
    retain_bounded_generation_index_with_text_head(entries, active_generation_id, None)
}

/// Apply durable history bounds while preserving both independently published
/// heads. A newer sealed generation may become active before its text artifact
/// is rebuilt, so the generation named by the incumbent text head remains live
/// until that head advances.
pub fn retain_bounded_generation_index_with_text_head(
    entries: &mut Vec<DurableGenerationIndexEntryV1>,
    active_generation_id: &str,
    active_text_head_generation_id: Option<&str>,
) -> usize {
    entries.sort_by(|left, right| {
        (left.sealed_at_micros, left.generation_id.as_str())
            .cmp(&(right.sealed_at_micros, right.generation_id.as_str()))
    });
    let original_len = entries.len();
    let active_sealed_at = entries
        .iter()
        .find(|entry| entry.generation_id == active_generation_id)
        .map_or(i64::MIN, |entry| entry.sealed_at_micros);
    let oldest_retained =
        active_sealed_at.saturating_sub(MAX_DURABLE_GENERATION_INDEX_TTL_MICROS_V1);
    entries.retain(|entry| {
        entry.generation_id == active_generation_id
            || active_text_head_generation_id == Some(entry.generation_id.as_str())
            || entry.sealed_at_micros >= oldest_retained
    });

    loop {
        let total_bytes = durable_generation_index_bytes(entries);
        if entries.len() <= MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1
            && total_bytes <= MAX_DURABLE_GENERATION_INDEX_BYTES_V1
        {
            break;
        }
        let Some(index) = entries.iter().position(|entry| {
            entry.generation_id != active_generation_id
                && active_text_head_generation_id != Some(entry.generation_id.as_str())
        }) else {
            break;
        };
        entries.remove(index);
    }
    original_len.saturating_sub(entries.len())
}

fn durable_generation_index_bytes(entries: &[DurableGenerationIndexEntryV1]) -> u64 {
    let generation_bytes = entries.iter().fold(0_u64, |total, entry| {
        total
            .saturating_add(entry.size_bytes)
            .saturating_add(entry.segment_bytes)
    });
    let mut artifacts = BTreeSet::new();
    entries.iter().fold(generation_bytes, |total, entry| {
        let Some(artifact) = entry.text_artifact.as_ref() else {
            return total;
        };
        if artifacts.insert(artifact.artifact_file.as_str()) {
            total.saturating_add(artifact.artifact_size_bytes)
        } else {
            total
        }
    })
}

pub fn durable_generation_index_digest(
    entries: &[DurableGenerationIndexEntryV1],
    truncated: bool,
) -> Result<String, CodeGenerationRetentionErrorV1> {
    canonical_sha256(&(entries, truncated))
        .map(|digest| digest.as_str().to_owned())
        .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DurablePublicationPointerV1 {
    pub generation_id: String,
    pub snapshot_content_identity: String,
    pub publication_digest: String,
    pub sealed_at_micros: i64,
    pub generation_file: String,
    pub state_digest: String,
    #[serde(default)]
    pub generation_index: Vec<DurableGenerationIndexEntryV1>,
    #[serde(default)]
    pub generation_index_truncated: bool,
    #[serde(default)]
    pub generation_index_digest: Option<String>,
}

#[derive(Debug, Error)]
pub enum CodeGenerationRetentionErrorV1 {
    #[error("code-generation retention storage failure: {0}")]
    Storage(String),
    #[error("code-generation retention refused unsafe state: {0}")]
    UnsafeState(String),
    #[error("code-generation retention conflict: {0}")]
    Conflict(String),
    #[error("code-generation retention cancelled")]
    Cancelled,
    #[error("code-generation retention deferred: graph replay pool is busy")]
    GraphReplayPoolBusy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeGenerationRetentionModeV1 {
    DryRun,
    Apply,
}

/// How hard a census proves that a sealed generation file still matches the
/// content digest encoded in its name.
///
/// A single generation is routinely ~1 GiB, so [`Self::Full`] costs a whole-file
/// SHA-256 per generation. That is correct — and mandatory — before unlinking
/// anything, but it is far too expensive for an observability read, which is why
/// every byte-budget gate in front of Doctor and the storage report used to fail
/// closed on real profiles and report nothing at all. [`Self::MetadataOnly`]
/// reads the bounded manifest prefix plus `stat`, and takes the content digest
/// from the file name instead of recomputing it. It answers "how many superseded
/// generations, how many bytes, which are collectable" exactly; it does not
/// prove file integrity, so it can never authorize a deletion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationDigestVerificationV1 {
    /// Hash every byte of every generation and prove it matches its file name.
    Full,
    /// Read only the bounded metadata prefix; trust the name for the digest.
    MetadataOnly,
}

/// What a census learned about content-addressed generation segments.
///
/// Marking live segments means streaming every retained manifest end to end —
/// the same multi-gigabyte read [`GenerationDigestVerificationV1::MetadataOnly`]
/// exists to avoid, and one that fails closed when a manifest no longer matches
/// its content-addressed file name. A metadata-only census therefore refuses to
/// guess: it reports [`Self::NoneFound`] only from the bounded directory listing
/// that proves no segment file exists at all, and [`Self::Unknown`] otherwise.
/// `Unknown` counts as collectable work so the segment sweep is still reached,
/// never skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationSegmentCensusV1 {
    /// The mark-and-sweep proved no unreferenced segment exists, or the store
    /// holds no segment files at all.
    NoneFound,
    /// The mark-and-sweep found at least one unreferenced segment.
    Present,
    /// Segment files exist and this census did not pay the mark phase that
    /// would classify them. Only a full-verification census resolves this.
    Unknown,
}

impl GenerationSegmentCensusV1 {
    /// Whether a retention pass still has segment work to reach.
    #[must_use]
    pub const fn may_have_collectable_segments(self) -> bool {
        matches!(self, Self::Present | Self::Unknown)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeGenerationRetentionGenerationV1 {
    pub generation_id: CodeGenerationId,
    pub generation_file: String,
    pub sealed_at_micros: i64,
    pub size_bytes: u64,
}

/// One derived text-artifact path collected by the retention transaction.
///
/// These are filesystem names below `code-text-artifacts-v1/`, never caller
/// supplied paths. A receipt retains the exact candidate kind and byte size so
/// recovery can roll back a staged unlink without widening the namespace.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeTextArtifactRetentionKindV1 {
    Completed,
    Staging,
    Corrupt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeTextArtifactRetentionCandidateV1 {
    artifact_file: String,
    kind: CodeTextArtifactRetentionKindV1,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeGenerationRetentionPlanV1 {
    /// `None` plans an unpublished store: sealed files exist but no active
    /// publication pointer was ever written. The sealer holds the store lock
    /// across the sealed-file write and the pointer write, so under that same
    /// lock this state is crash debris from an interrupted publish, never a
    /// mid-flight seal.
    pub active_generation_id: Option<CodeGenerationId>,
    pub vector_readable_sources: BTreeSet<CodeGenerationId>,
    pub rollback_floor: usize,
    pub superseded_generations: Vec<CodeGenerationRetentionGenerationV1>,
    pub collectable_generations: Vec<CodeGenerationRetentionGenerationV1>,
    /// Derived text-artifact debris selected from one bounded canonical
    /// inventory. Descriptor-referenced and still-in-progress staging files
    /// are deliberately absent.
    collectable_text_artifacts: Vec<CodeTextArtifactRetentionCandidateV1>,
    /// Whether a fully content-addressed segment exists outside every retained
    /// generation manifest (and, when configured, the graph replay pool).
    /// This is a mark-phase wake signal only: execution recomputes the exact
    /// live set under the canonical store lock before unlinking anything.
    collectable_generation_segments: GenerationSegmentCensusV1,
    /// Unique bytes seen in the bounded text-artifact inventory: durable
    /// descriptor targets, the one resumable active staging file, and this
    /// pass's selected debris candidates. A descriptor shared by retained
    /// generations is counted once by its canonical artifact path.
    text_artifact_inventory_bytes: u64,
    /// How thoroughly this plan proved generation integrity. Apply-mode
    /// execution refuses anything but [`GenerationDigestVerificationV1::Full`].
    pub verification: GenerationDigestVerificationV1,
    /// Present exactly when [`Self::active_generation_id`] is present; the
    /// execute-time compare-and-swap re-reads the pointer under the store
    /// lock and requires this exact value (including "still absent").
    active_pointer: Option<DurablePublicationPointerV1>,
}

impl CodeGenerationRetentionPlanV1 {
    #[must_use]
    pub fn active_generation_file(&self) -> Option<&str> {
        self.active_pointer
            .as_ref()
            .map(|pointer| pointer.generation_file.as_str())
    }

    #[must_use]
    pub fn superseded_generation_bytes(&self) -> u64 {
        total_bytes(&self.superseded_generations)
    }

    #[must_use]
    pub fn collectable_generation_bytes(&self) -> u64 {
        total_bytes(&self.collectable_generations)
    }

    #[must_use]
    pub fn has_collectable_work(&self) -> bool {
        !self.collectable_generations.is_empty()
            || !self.collectable_text_artifacts.is_empty()
            || self
                .collectable_generation_segments
                .may_have_collectable_segments()
    }

    /// What this plan proved about unreferenced generation segments.
    #[must_use]
    pub const fn generation_segment_census(&self) -> GenerationSegmentCensusV1 {
        self.collectable_generation_segments
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeGenerationRetentionReceiptV1 {
    pub schema: String,
    pub receipt_digest: String,
    /// `None` records an unpublished-store sweep (crash debris collected from
    /// a store whose pointer was never written). Canonical serialization is
    /// transparent over `Some`, so receipts written before this field became
    /// optional keep their exact digests.
    pub active_generation_id: Option<CodeGenerationId>,
    pub vector_readable_sources: BTreeSet<CodeGenerationId>,
    pub rollback_floor: usize,
    pub deleted_generations: Vec<CodeGenerationRetentionGenerationV1>,
    pub reclaimed_bytes: u64,
    pub completed_at_micros: i64,
}

/// Durable proof for a text-artifact-only sweep. It is intentionally separate
/// from generation deletion receipts: graph replay consumes only sealed
/// generation releases and must never mistake an artifact cleanup for one.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeTextArtifactRetentionReceiptV1 {
    schema: String,
    receipt_digest: String,
    /// Both are `None` exactly when the sweep ran against an unpublished
    /// store (no active pointer, so no durable index digest exists).
    active_generation_id: Option<CodeGenerationId>,
    active_generation_index_digest: Option<String>,
    deleted_artifacts: Vec<CodeTextArtifactRetentionCandidateV1>,
    inventory_bytes_before_collection: u64,
    pub reclaimed_bytes: u64,
    completed_at_micros: i64,
}

#[derive(Serialize)]
struct CodeGenerationRetentionReceiptMaterialV1<'a> {
    schema: &'static str,
    active_generation_id: Option<&'a CodeGenerationId>,
    vector_readable_sources: &'a BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    deleted_generations: &'a [CodeGenerationRetentionGenerationV1],
    reclaimed_bytes: u64,
    completed_at_micros: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CodeGenerationRetentionTransactionV1 {
    schema: String,
    /// `None` journals an unpublished-store sweep; recovery re-proves the
    /// pointer is still absent before completing or rolling back.
    active_pointer: Option<DurablePublicationPointerV1>,
    receipt: CodeGenerationRetentionReceiptV1,
}

#[derive(Serialize)]
struct CodeTextArtifactRetentionReceiptMaterialV1<'a> {
    schema: &'static str,
    active_generation_id: Option<&'a CodeGenerationId>,
    active_generation_index_digest: Option<&'a str>,
    deleted_artifacts: &'a [CodeTextArtifactRetentionCandidateV1],
    inventory_bytes_before_collection: u64,
    reclaimed_bytes: u64,
    completed_at_micros: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CodeTextArtifactRetentionTransactionV1 {
    schema: String,
    /// `None` journals an unpublished-store sweep; recovery re-proves the
    /// pointer is still absent before completing or rolling back.
    active_pointer: Option<DurablePublicationPointerV1>,
    receipt: CodeTextArtifactRetentionReceiptV1,
}

#[derive(Debug)]
struct CodeTextArtifactRetentionInventoryV1 {
    candidates: Vec<CodeTextArtifactRetentionCandidateV1>,
    unique_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeGenerationRetentionReportV1 {
    pub plan: CodeGenerationRetentionPlanV1,
    pub deleted_generations: Vec<CodeGenerationRetentionGenerationV1>,
    pub receipt: Option<CodeGenerationRetentionReceiptV1>,
    pub deleted_text_artifacts: Vec<CodeTextArtifactRetentionCandidateV1>,
    pub text_artifact_receipt: Option<CodeTextArtifactRetentionReceiptV1>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeGenerationRetentionObservationV1 {
    pub superseded_generation_count: u64,
    pub superseded_generation_bytes: u64,
}

#[must_use]
pub fn scoped_code_index_store_root(store_root: &Path, canonical_project_root: &Path) -> PathBuf {
    store_root.join(code_index_scope_hash(canonical_project_root))
}

#[must_use]
pub fn code_text_artifacts_root(store_root: &Path) -> PathBuf {
    store_root.join(CODE_TEXT_ARTIFACTS_DIRECTORY_V1)
}

pub fn code_text_artifact_path(
    store_root: &Path,
    descriptor: &DurableCodeTextArtifactDescriptorV1,
) -> Result<PathBuf, CodeGenerationRetentionErrorV1> {
    validate_text_artifact_descriptor(descriptor)?;
    Ok(code_text_artifacts_root(store_root).join(&descriptor.artifact_file))
}

/// The directory name `code-index-v1/` uses for one canonical project root.
///
/// Scope-root reconciliation compares directory names against this exact
/// derivation, so it must never diverge from
/// [`scoped_code_index_store_root`] — a divergence would classify a live scope
/// as stranded.
#[must_use]
pub fn code_index_scope_hash(canonical_project_root: &Path) -> String {
    sha256_hex(canonical_project_root.to_string_lossy().as_bytes())
}

/// Plan retention with full digest verification. This is the only planner a
/// collection may be built from.
pub fn plan_code_generation_retention(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
) -> Result<CodeGenerationRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    plan_code_generation_retention_with_verification(
        store_root,
        vector_readable_sources,
        rollback_floor,
        GenerationDigestVerificationV1::Full,
    )
}

/// The same exact liveness census at a caller-chosen verification cost.
///
/// Observability callers pass [`GenerationDigestVerificationV1::MetadataOnly`]:
/// the counts, byte totals, and collectable set are identical, but no
/// multi-gigabyte file is re-hashed to produce them.
pub fn plan_code_generation_retention_with_verification(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    verification: GenerationDigestVerificationV1,
) -> Result<CodeGenerationRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    plan_code_generation_retention_with_verification_cancellable(
        store_root,
        vector_readable_sources,
        rollback_floor,
        verification,
        None,
        &|| false,
    )
}

#[hotpath::measure(label = "usecases.retention.plan_next")]
pub fn plan_next_code_generation_retention_cancellable(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeGenerationRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    let mut plan = plan_code_generation_retention_with_verification_cancellable(
        store_root,
        vector_readable_sources,
        rollback_floor,
        GenerationDigestVerificationV1::Full,
        None,
        is_cancelled,
    )?;
    plan.collectable_generations.truncate(1);
    Ok(plan)
}

/// Recover any bounded prior apply, then build the next fully verified
/// collection unit while preserving the caller's cancellation authority.
///
/// Daemon maintenance performs this preparation before it acquires the graph
/// writer transaction. Full verification checks `is_cancelled` between bounded
/// read chunks, so shutdown never waits for every byte in a multi-GiB store
/// while that transaction is held.
#[hotpath::measure(label = "usecases.retention.prepare")]
pub fn prepare_next_code_generation_retention_cancellable(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    is_cancelled: &dyn Fn() -> bool,
    graph_replay_pool_root: Option<&Path>,
) -> Result<CodeGenerationRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    if observe_cancel(is_cancelled) {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    recover_code_generation_retention_cancellable(
        store_root,
        vector_readable_sources,
        graph_replay_pool_root,
        is_cancelled,
    )?;
    // Most maintenance ticks have no collectable work. Inventory those ticks
    // from bounded manifest metadata first, and pay the full digest cost only
    // when this exact census found bytes that may be unlinked. The executor
    // still refuses metadata-only plans, so no deletion can cross this gate
    // without the canonical full verification below.
    let mut census = plan_code_generation_retention_with_verification_cancellable(
        store_root,
        vector_readable_sources,
        rollback_floor,
        GenerationDigestVerificationV1::MetadataOnly,
        graph_replay_pool_root,
        is_cancelled,
    )?;
    // The bounded census leaves segments typed unknown rather than paying the
    // mark phase. Resolve exactly that question with the sweep alone: a store
    // whose only possible work is segments must still reach them, and running
    // a whole full-digest plan to find out would cost a second pass over every
    // sealed byte on every quiet maintenance tick.
    if census.collectable_generations.is_empty()
        && census.collectable_text_artifacts.is_empty()
        && census.collectable_generation_segments == GenerationSegmentCensusV1::Unknown
    {
        census.collectable_generation_segments = if has_unreferenced_generation_segments(
            store_root,
            graph_replay_pool_root,
            is_cancelled,
        )? {
            GenerationSegmentCensusV1::Present
        } else {
            GenerationSegmentCensusV1::NoneFound
        };
    }
    if !census.has_collectable_work() {
        return Ok(census);
    }
    let mut plan = plan_code_generation_retention_with_verification_cancellable(
        store_root,
        vector_readable_sources,
        rollback_floor,
        GenerationDigestVerificationV1::Full,
        graph_replay_pool_root,
        is_cancelled,
    )?;
    plan.collectable_generations.truncate(1);
    Ok(plan)
}

#[hotpath::measure(label = "usecases.retention.plan")]
fn plan_code_generation_retention_with_verification_cancellable(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    verification: GenerationDigestVerificationV1,
    graph_replay_pool_root: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeGenerationRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    if observe_cancel(is_cancelled) {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    if transaction_path(store_root).exists() || text_artifact_transaction_path(store_root).exists()
    {
        crate::hotpath_observe::retention_recovery_pending();
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-generation retention recovery is pending".to_owned(),
        ));
    }
    // An absent pointer is a typed unpublished store, not an error: the
    // sealer writes the sealed file and the pointer under one store-lock
    // hold, so a store with sealed bytes and no pointer is crash debris from
    // an interrupted publish and everything in it is collectable (modulo
    // vector pins, which are re-proven below like any other plan).
    let active_pointer = read_optional_active_pointer(store_root)?;
    let active_generation_id = active_pointer
        .as_ref()
        .map(|pointer| {
            validate_generation_file(&pointer.generation_file)?;
            validate_durable_generation_index(pointer)?;
            CodeGenerationId::new(pointer.generation_id.clone())
                .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))
        })
        .transpose()?;
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let entries = match std::fs::read_dir(&generations_root) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && active_pointer.is_none() => {
            None
        }
        Err(error) => return Err(storage(error)),
    };
    let mut generations = BTreeMap::new();
    let mut active_state_digest = None;

    for entry in entries.into_iter().flatten() {
        if observe_cancel(is_cancelled) {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        let entry = entry.map_err(storage)?;
        let path = entry.path();
        let Some(file_name) = generation_file_name(&path) else {
            continue;
        };
        let (format_revision, manifest, raw_state_digest, size_bytes) =
            read_generation_metadata(&path, verification, is_cancelled)?;
        let expected_file = format!(
            "generation-{}.json",
            raw_state_digest
                .strip_prefix("sha256:")
                .unwrap_or(&raw_state_digest)
        );
        if file_name != expected_file {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "generation file '{}' does not match its content digest",
                path.display()
            )));
        }
        if !sealed_generation_format_revision_is_compatible(format_revision) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "generation file '{}' has an incompatible format revision",
                path.display()
            )));
        }
        let generation_id = manifest.generation_id;
        let metadata = CodeGenerationRetentionGenerationV1 {
            generation_id: generation_id.clone(),
            generation_file: file_name.clone(),
            sealed_at_micros: manifest.seal.sealed_at.0,
            size_bytes,
        };
        if generations
            .insert(generation_id.clone(), metadata)
            .is_some()
        {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "generation identity '{}' appears in more than one sealed file",
                generation_id.as_str()
            )));
        }
        if let Some(pointer) = active_pointer.as_ref()
            && file_name == pointer.generation_file
        {
            active_state_digest = Some(raw_state_digest);
            if Some(&generation_id) != active_generation_id.as_ref() {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                    "active pointer generation id does not match its sealed file".to_owned(),
                ));
            }
        }
    }

    let mut pointer_generations = BTreeSet::new();
    if let Some(pointer) = active_pointer.as_ref() {
        if active_state_digest.as_deref() != Some(pointer.state_digest.as_str()) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "active generation file is missing or does not match the pointer digest".to_owned(),
            ));
        }
        pointer_generations = pointer
            .generation_index
            .iter()
            .map(|entry| {
                CodeGenerationId::new(entry.generation_id.clone())
                    .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let missing_pointer_generations = pointer_generations
            .iter()
            .filter(|generation| !generations.contains_key(*generation))
            .map(CodeGenerationId::as_str)
            .collect::<Vec<_>>();
        if !missing_pointer_generations.is_empty() {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "publication-pointer generations are missing: {}",
                missing_pointer_generations.join(", ")
            )));
        }
        for entry in &pointer.generation_index {
            let generation_id = CodeGenerationId::new(entry.generation_id.clone())
                .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
            let Some(generation) = generations.get(&generation_id) else {
                continue;
            };
            if generation.size_bytes != entry.size_bytes {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "publication-pointer generation '{}' has a mismatched byte size",
                    generation_id.as_str()
                )));
            }
        }
    }
    let missing_sources = vector_readable_sources
        .iter()
        .filter(|source| !generations.contains_key(*source))
        .map(tracedecay_domain::CodeGenerationId::as_str)
        .collect::<Vec<_>>();
    if !missing_sources.is_empty() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "vector-readable source generations are missing: {}",
            missing_sources.join(", ")
        )));
    }

    let mut superseded_generations = generations
        .into_values()
        .filter(|generation| Some(&generation.generation_id) != active_generation_id.as_ref())
        .collect::<Vec<_>>();
    superseded_generations.sort_by(|left, right| {
        right
            .sealed_at_micros
            .cmp(&left.sealed_at_micros)
            .then_with(|| right.generation_id.cmp(&left.generation_id))
            .then_with(|| right.generation_file.cmp(&left.generation_file))
    });

    // Mark before sweeping. An omitted mark retains a derived file and costs
    // space; unlike refcounting, no accounting drift can silently delete a live
    // generation. Pointer-addressable and vector-readable marks are exact
    // liveness, while the newest superseded floor is the bounded rollback
    // reserve.
    let mut marked = pointer_generations;
    marked.extend(vector_readable_sources.iter().cloned());
    marked.extend(active_generation_id.clone());
    marked.extend(
        superseded_generations
            .iter()
            .take(rollback_floor)
            .map(|generation| generation.generation_id.clone()),
    );
    let collectable_generations = superseded_generations
        .iter()
        .filter(|generation| !marked.contains(&generation.generation_id))
        .take(MAX_CODE_GENERATION_RETENTION_BATCH_V1)
        .cloned()
        .collect::<Vec<_>>();
    let text_artifact_inventory = plan_collectable_text_artifacts_cancellable(
        store_root,
        active_pointer.as_ref(),
        verification,
        is_cancelled,
    )?;
    // The segment mark phase streams every retained manifest end to end and
    // fails closed when one no longer matches its content-addressed name. That
    // is exactly the multi-gigabyte read a metadata-only census promises not to
    // pay, and paying it here made a bounded observability census error out on
    // stores whose generations are individually larger than any byte budget.
    // Metadata-only callers get the bounded directory answer plus a typed
    // unknown; `prepare_next_code_generation_retention_cancellable` resolves
    // that unknown with the sweep alone, so maintenance still reaches segments
    // without a second full-digest pass.
    let collectable_generation_segments = match verification {
        GenerationDigestVerificationV1::Full => {
            if has_unreferenced_generation_segments(
                store_root,
                graph_replay_pool_root,
                is_cancelled,
            )? {
                GenerationSegmentCensusV1::Present
            } else {
                GenerationSegmentCensusV1::NoneFound
            }
        }
        GenerationDigestVerificationV1::MetadataOnly => {
            if store_may_hold_generation_segments(store_root, is_cancelled)? {
                GenerationSegmentCensusV1::Unknown
            } else {
                GenerationSegmentCensusV1::NoneFound
            }
        }
    };
    #[cfg(feature = "hotpath")]
    {
        let planned_bytes = total_bytes(&collectable_generations).saturating_add(
            text_artifact_inventory
                .candidates
                .iter()
                .map(|candidate| candidate.size_bytes)
                .sum::<u64>(),
        );
        crate::hotpath_observe::retention_plan(
            collectable_generations
                .len()
                .saturating_add(text_artifact_inventory.candidates.len()),
            planned_bytes,
        );
    }

    Ok(CodeGenerationRetentionPlanV1 {
        active_generation_id,
        vector_readable_sources: vector_readable_sources.clone(),
        rollback_floor,
        superseded_generations,
        collectable_generations,
        collectable_text_artifacts: text_artifact_inventory.candidates,
        collectable_generation_segments,
        text_artifact_inventory_bytes: text_artifact_inventory.unique_bytes,
        verification,
        active_pointer,
    })
}

fn generation_file_digest(file_name: &str) -> Option<&str> {
    file_name
        .strip_prefix("generation-")?
        .strip_suffix(".json")
        .filter(|digest| is_lowercase_hex(digest, 64))
}

struct CancellableGenerationManifestReaderV1<'a> {
    file: File,
    hasher: Sha256,
    is_cancelled: &'a dyn Fn() -> bool,
    cancelled: bool,
}

impl Read for CancellableGenerationManifestReaderV1<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if (self.is_cancelled)() {
            self.cancelled = true;
            crate::hotpath_observe::retention_cancelled();
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "generation segment mark cancelled",
            ));
        }
        let read = self.file.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        crate::hotpath_observe::retention_inspected(read as u64);
        Ok(read)
    }
}

impl CancellableGenerationManifestReaderV1<'_> {
    fn digest_hex(&self) -> String {
        encode_lowercase_hex(&self.hasher.clone().finalize())
    }
}

#[hotpath::measure(label = "usecases.retention.segment_mark_sweep")]
fn sweep_unreferenced_generation_segments(
    store_root: &Path,
    graph_replay_pool_root: Option<&Path>,
    apply: bool,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(bool, u64), CodeGenerationRetentionErrorV1> {
    let segments_root = store_root.join(GENERATION_SEGMENTS_DIRECTORY);
    let entries = match std::fs::read_dir(&segments_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((false, 0)),
        Err(error) => return Err(storage(error)),
    };
    let mut live_segments = BTreeSet::new();
    let mut mark_root = |root: &Path,
                         replay_pool: bool|
     -> Result<(), CodeGenerationRetentionErrorV1> {
        let manifests = match std::fs::read_dir(root) {
            Ok(manifests) => manifests,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(storage(error)),
        };
        for manifest in manifests {
            if observe_cancel(is_cancelled) {
                return Err(CodeGenerationRetentionErrorV1::Cancelled);
            }
            let manifest = manifest.map_err(storage)?;
            let path = manifest.path();
            let expected_digest = if replay_pool {
                let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                    return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                        "graph replay pool entry '{}' has no UTF-8 file name",
                        path.display()
                    )));
                };
                if file_name == STORE_LOCK_FILE {
                    continue;
                }
                replay_generation_file_digest(file_name)
                    .ok_or_else(|| {
                        CodeGenerationRetentionErrorV1::UnsafeState(format!(
                            "graph replay pool entry '{}' is not a recognized generation",
                            path.display()
                        ))
                    })?
                    .to_owned()
            } else {
                let Some(file_name) = generation_file_name(&path) else {
                    continue;
                };
                generation_file_digest(&file_name)
                    .ok_or_else(|| {
                        CodeGenerationRetentionErrorV1::UnsafeState(format!(
                            "generation manifest '{}' has no content-addressed file name",
                            path.display()
                        ))
                    })?
                    .to_owned()
            };
            if read_generation_format_revision(&path, is_cancelled)?
                != SEALED_GENERATION_FORMAT_REVISION_V1
            {
                continue;
            }
            let mut reader = CancellableGenerationManifestReaderV1 {
                file: File::open(&path).map_err(storage)?,
                hasher: Sha256::new(),
                is_cancelled,
                cancelled: false,
            };
            let identities = {
                let buffered = BufReader::with_capacity(1024 * 1024, &mut reader);
                tracedecay_code_index::production::CodeIndexPublishedGenerationV1::partitioned_segment_identities_from_reader(
                    buffered,
                )
            };
            if reader.cancelled {
                return Err(CodeGenerationRetentionErrorV1::Cancelled);
            }
            if reader.digest_hex() != expected_digest {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "generation manifest '{}' does not match its content-addressed file name",
                    path.display()
                )));
            }
            let identities = identities.map_err(|error| {
                CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "generation manifest '{}' cannot mark its segments: {error}",
                    path.display()
                ))
            })?;
            if let Some(identities) = identities {
                live_segments.extend(
                    identities
                        .into_iter()
                        .map(|identity| identity.digest.as_str().to_owned()),
                );
            }
        }
        Ok(())
    };
    mark_root(&store_root.join(GENERATIONS_DIRECTORY), false)?;
    if let Some(pool_root) = graph_replay_pool_root {
        mark_root(pool_root, true)?;
    }

    let mut found = false;
    let mut reclaimed = 0_u64;
    for entry in entries {
        if observe_cancel(is_cancelled) {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        let entry = entry.map_err(storage)?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(digest) = file_name
            .strip_prefix("segment-")
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|digest| is_lowercase_hex(digest, 64))
        else {
            continue;
        };
        if live_segments.contains(&format!("sha256:{digest}")) {
            continue;
        }
        let metadata = path.symlink_metadata().map_err(storage)?;
        if !metadata.file_type().is_file() {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "generation segment '{}' is not a regular file",
                path.display()
            )));
        }
        found = true;
        if !apply {
            return Ok((true, 0));
        }
        std::fs::remove_file(&path).map_err(storage)?;
        reclaimed = reclaimed.saturating_add(metadata.len());
    }
    if reclaimed > 0 {
        sync_directory(&segments_root)?;
    }
    Ok((found, reclaimed))
}

fn replay_generation_file_digest(file_name: &str) -> Option<&str> {
    generation_file_digest(file_name).or_else(|| {
        let (digest, suffix) = file_name
            .strip_prefix(".generation-")?
            .split_once(".unlink-")?;
        (!suffix.is_empty() && is_lowercase_hex(digest, 64)).then_some(digest)
    })
}

/// Whether the store may hold content-addressed segment work.
///
/// A one-entry directory observation: no manifest is opened, so this is the
/// only segment question a metadata-only census may answer. An empty or absent
/// directory proves there is nothing to collect; any observed entry, including
/// unrecognized crash debris, stays typed unknown until the mark-and-sweep
/// runs. Classifying names here would let arbitrary debris turn an operator
/// diagnostic into an exhaustive directory scan.
fn store_may_hold_generation_segments(
    store_root: &Path,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    let mut entries = match std::fs::read_dir(store_root.join(GENERATION_SEGMENTS_DIRECTORY)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(storage(error)),
    };
    if observe_cancel(is_cancelled) {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    entries
        .next()
        .transpose()
        .map(|entry| entry.is_some())
        .map_err(storage)
}

fn has_unreferenced_generation_segments(
    store_root: &Path,
    graph_replay_pool_root: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    sweep_unreferenced_generation_segments(store_root, graph_replay_pool_root, false, is_cancelled)
        .map(|(found, _)| found)
}

fn collect_unreferenced_generation_segments(
    store_root: &Path,
    graph_replay_pool_root: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<u64, CodeGenerationRetentionErrorV1> {
    sweep_unreferenced_generation_segments(store_root, graph_replay_pool_root, true, is_cancelled)
        .map(|(_, reclaimed)| reclaimed)
}

/// `graph_replay_pool_root` is the project graph's replay pool. When present,
/// every retired generation survives retention as a hard-linked pool entry
/// until the graph projection durably confirms it is no longer needed (the
/// replay release queue's existing contract); `None` deletes retired files
/// outright and is only sound for stores with no graph projection.
pub fn execute_code_generation_retention(
    store_root: &Path,
    plan: CodeGenerationRetentionPlanV1,
    mode: CodeGenerationRetentionModeV1,
    completed_at: UtcMicros,
    graph_replay_pool_root: Option<&Path>,
) -> Result<CodeGenerationRetentionReportV1, CodeGenerationRetentionErrorV1> {
    execute_code_generation_retention_cancellable(
        store_root,
        plan,
        mode,
        completed_at,
        graph_replay_pool_root,
        &|| false,
    )
}

/// Apply a fully verified retention plan while preserving the caller's
/// cancellation authority through the bounded artifact re-verification step.
///
/// The plan is immutable evidence, but its content-addressed artifact files
/// are verified again under the store lock immediately before quarantine. A
/// shutdown must be able to stop that full-file read before any candidate is
/// renamed or any deletion receipt is published. Existing callers retain the
/// non-cancellable wrapper above until their control path is wired through.
#[hotpath::measure(label = "usecases.retention.execute")]
pub fn execute_code_generation_retention_cancellable(
    store_root: &Path,
    plan: CodeGenerationRetentionPlanV1,
    mode: CodeGenerationRetentionModeV1,
    completed_at: UtcMicros,
    graph_replay_pool_root: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeGenerationRetentionReportV1, CodeGenerationRetentionErrorV1> {
    if observe_cancel(is_cancelled) {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    if mode == CodeGenerationRetentionModeV1::DryRun {
        return Ok(CodeGenerationRetentionReportV1 {
            plan,
            deleted_generations: Vec::new(),
            receipt: None,
            deleted_text_artifacts: Vec::new(),
            text_artifact_receipt: None,
        });
    }
    // A metadata-only census trusts file names for content digests. That is
    // fine for reporting and never sufficient to unlink evidence.
    if plan.verification != GenerationDigestVerificationV1::Full {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "applied retention requires a fully digest-verified plan".to_owned(),
        ));
    }

    let vector_readable_sources = plan.vector_readable_sources.clone();
    let _store_lock = acquire_code_generation_store_lock(store_root)?;
    if observe_cancel(is_cancelled) {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    if transaction_path(store_root).exists() || text_artifact_transaction_path(store_root).exists()
    {
        crate::hotpath_observe::retention_recovery_pending();
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-generation retention recovery is pending".to_owned(),
        ));
    }
    if read_optional_active_pointer(store_root)? != plan.active_pointer {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "active generation changed after the retention mark phase".to_owned(),
        ));
    }
    let mut reclaimed_segment_bytes = if plan.collectable_generations.is_empty()
        && plan.collectable_generation_segments == GenerationSegmentCensusV1::Present
    {
        let graph_replay_pool_lock = match graph_replay_pool_root {
            Some(pool_root) => Some(acquire_graph_replay_pool_lock_checked(
                pool_root,
                Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
                is_cancelled,
            )?),
            None => None,
        };
        let reclaimed = collect_unreferenced_generation_segments(
            store_root,
            graph_replay_pool_root,
            is_cancelled,
        )?;
        drop(graph_replay_pool_lock);
        reclaimed
    } else {
        0
    };
    let (deleted_generations, receipt) = if plan.collectable_generations.is_empty() {
        (Vec::new(), None)
    } else {
        let generations_root = store_root.join(GENERATIONS_DIRECTORY);
        for generation in &plan.collectable_generations {
            validate_generation_file(&generation.generation_file)?;
            let path = generations_root.join(&generation.generation_file);
            let metadata = std::fs::metadata(&path).map_err(storage)?;
            if metadata.len() != generation.size_bytes {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "collectable generation '{}' changed after the mark phase",
                    generation.generation_file
                )));
            }
        }

        let deleted_generations = plan.collectable_generations.clone();
        let receipt = build_receipt(&plan, deleted_generations.clone(), completed_at)?;
        let transaction = CodeGenerationRetentionTransactionV1 {
            schema: TRANSACTION_SCHEMA.to_owned(),
            active_pointer: plan.active_pointer.clone(),
            receipt: receipt.clone(),
        };
        // Canonical order is code-generation store first, then graph replay
        // pool. Hold the pool lock through durable release publication and
        // committed cleanup so the reconciler cannot race an orphaning unlink.
        // Acquire is checked and budget-capped: a publisher that took the
        // pool after the outer non-blocking probe must not park this
        // executor on a deadline-free flock while the daemon writer gate
        // stays held.
        let graph_replay_pool_lock = match graph_replay_pool_root {
            Some(pool_root) => Some(acquire_graph_replay_pool_lock_checked(
                pool_root,
                Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
                is_cancelled,
            )?),
            None => None,
        };
        persist_transaction(store_root, &transaction)?;

        let result = (|| {
            stage_collectable_generations(store_root, &transaction)?;
            if read_optional_active_pointer(store_root)? != transaction.active_pointer {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                    "active generation changed while retention candidates were quarantined"
                        .to_owned(),
                ));
            }
            if let Some(pool_lock) = graph_replay_pool_lock.as_ref() {
                expose_staged_generations_under_graph_replay_pool_lock(
                    store_root,
                    &transaction,
                    pool_lock,
                )?;
            }
            // Release events must be durable before the deletion receipt so
            // the replay reconciler never observes a receipt whose pool
            // survival events are missing.
            graph_replay_release::write_events(store_root, &receipt)?;
            write_receipt(store_root, &receipt)?;
            cleanup_committed_transaction_under_graph_replay_pool_lock(
                store_root,
                &transaction,
                &vector_readable_sources,
                graph_replay_pool_lock.as_ref(),
            )?;
            reclaimed_segment_bytes = collect_unreferenced_generation_segments(
                store_root,
                graph_replay_pool_root,
                is_cancelled,
            )?;
            clear_transaction(store_root)
        })();
        if let Err(error) = result {
            drop(graph_replay_pool_lock);
            if !receipt_is_durable(store_root, &receipt)? {
                rollback_staged_transaction(store_root, &transaction, graph_replay_pool_root)?;
                clear_transaction(store_root)?;
            }
            return Err(error);
        }
        (deleted_generations, Some(receipt))
    };

    let (deleted_text_artifacts, text_artifact_receipt) =
        if plan.collectable_text_artifacts.is_empty() {
            (Vec::new(), None)
        } else {
            execute_text_artifact_retention_under_store_lock(
                store_root,
                &plan,
                completed_at,
                is_cancelled,
            )?
        };

    let reclaimed_bytes = receipt
        .as_ref()
        .map(|receipt| receipt.reclaimed_bytes)
        .unwrap_or(0)
        .saturating_add(
            text_artifact_receipt
                .as_ref()
                .map(|receipt| receipt.reclaimed_bytes)
                .unwrap_or(0),
        )
        .saturating_add(reclaimed_segment_bytes);
    crate::hotpath_observe::retention_reclaimed(reclaimed_bytes);
    crate::hotpath_observe::retention_recovery_idle();

    Ok(CodeGenerationRetentionReportV1 {
        plan,
        deleted_generations,
        receipt,
        deleted_text_artifacts,
        text_artifact_receipt,
    })
}

#[cfg(test)]
fn recover_code_generation_retention(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    graph_replay_pool_root: Option<&Path>,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    recover_code_generation_retention_cancellable(
        store_root,
        vector_readable_sources,
        graph_replay_pool_root,
        &|| false,
    )
}

/// Recover a prior retention transaction without converting cancellation into
/// a successful maintenance pass. Recovery is journaled, so a cancellation
/// before either transaction family starts leaves the durable journal for the
/// next attempt rather than clearing partial evidence.
#[hotpath::measure(label = "usecases.retention.recover")]
fn recover_code_generation_retention_cancellable(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    graph_replay_pool_root: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    crate::hotpath_observe::retention_recovery_running();
    if observe_cancel(is_cancelled) {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    let _store_lock = acquire_code_generation_store_lock(store_root)?;
    if observe_cancel(is_cancelled) {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    recover_pending_transaction_unlocked(
        store_root,
        vector_readable_sources,
        graph_replay_pool_root,
        is_cancelled,
    )?;
    if observe_cancel(is_cancelled) {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    recover_pending_text_artifact_transaction_unlocked(store_root)?;
    crate::hotpath_observe::retention_recovery_idle();
    Ok(())
}

pub fn run_code_generation_retention(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    mode: CodeGenerationRetentionModeV1,
    completed_at: UtcMicros,
    graph_replay_pool_root: Option<&Path>,
) -> Result<CodeGenerationRetentionReportV1, CodeGenerationRetentionErrorV1> {
    run_code_generation_retention_cancellable(
        store_root,
        vector_readable_sources,
        rollback_floor,
        mode,
        completed_at,
        graph_replay_pool_root,
        &|| false,
    )
}

/// Plan, recover, and apply with one cancellation authority. The old wrapper
/// preserves current callers while daemon maintenance is integrated with this
/// control boundary.
#[hotpath::measure(label = "usecases.retention.run")]
fn run_code_generation_retention_cancellable(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    mode: CodeGenerationRetentionModeV1,
    completed_at: UtcMicros,
    graph_replay_pool_root: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeGenerationRetentionReportV1, CodeGenerationRetentionErrorV1> {
    // Apply must sweep the same census dry-run reports (bounded by the batch
    // cap), not the single-unit "next" plan: that truncation exists for daemon
    // maintenance, which calls `prepare_next_…` directly so one graph writer
    // transaction never holds more than one collection unit.
    let plan = match mode {
        CodeGenerationRetentionModeV1::Apply => {
            recover_code_generation_retention_cancellable(
                store_root,
                vector_readable_sources,
                graph_replay_pool_root,
                is_cancelled,
            )?;
            plan_code_generation_retention_with_verification_cancellable(
                store_root,
                vector_readable_sources,
                rollback_floor,
                GenerationDigestVerificationV1::Full,
                graph_replay_pool_root,
                is_cancelled,
            )?
        }
        CodeGenerationRetentionModeV1::DryRun => {
            plan_code_generation_retention_with_verification_cancellable(
                store_root,
                vector_readable_sources,
                rollback_floor,
                GenerationDigestVerificationV1::Full,
                graph_replay_pool_root,
                is_cancelled,
            )?
        }
    };
    execute_code_generation_retention_cancellable(
        store_root,
        plan,
        mode,
        completed_at,
        graph_replay_pool_root,
        is_cancelled,
    )
}

#[hotpath::measure(label = "usecases.retention.observe")]
pub fn observe_code_generation_retention(
    store_root: &Path,
) -> Result<CodeGenerationRetentionObservationV1, CodeGenerationRetentionErrorV1> {
    // A store without a publication pointer is a typed unpublished store, not
    // an error: every sealed file in it is crash debris and counts as
    // superseded. Every present pointer goes through the one canonical reader
    // so corruption reporting cannot drift.
    let active_pointer = read_optional_active_pointer(store_root)?;
    if let Some(pointer) = active_pointer.as_ref() {
        validate_generation_file(&pointer.generation_file)?;
    }
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let entries = match std::fs::read_dir(&generations_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if active_pointer.is_none() {
                return Ok(CodeGenerationRetentionObservationV1::default());
            }
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "active pointer exists without a generation directory".to_owned(),
            ));
        }
        Err(error) => return Err(storage(error)),
    };
    let mut active_present = false;
    let mut observation = CodeGenerationRetentionObservationV1::default();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_SCOPE_ROOTS_PER_INVENTORY {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "code-index scope inventory exceeds its bounded authority".to_owned(),
            ));
        }
        let entry = entry.map_err(storage)?;
        let path = entry.path();
        let Some(file_name) = generation_file_name(&path) else {
            continue;
        };
        if let Some(pointer) = active_pointer.as_ref()
            && file_name == pointer.generation_file
        {
            active_present = true;
            continue;
        }
        observation.superseded_generation_count =
            observation.superseded_generation_count.saturating_add(1);
        observation.superseded_generation_bytes = observation
            .superseded_generation_bytes
            .saturating_add(entry.metadata().map_err(storage)?.len());
    }
    if active_pointer.is_some() && !active_present {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "active pointer target is missing from the generation directory".to_owned(),
        ));
    }
    Ok(observation)
}

fn recover_pending_transaction_unlocked(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    graph_replay_pool_root: Option<&Path>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let Some(transaction) = load_transaction(store_root)? else {
        return Ok(());
    };

    if receipt_is_durable(store_root, &transaction.receipt)? {
        cleanup_committed_transaction(
            store_root,
            &transaction,
            vector_readable_sources,
            graph_replay_pool_root,
            is_cancelled,
        )?;
    } else {
        rollback_staged_transaction(store_root, &transaction, graph_replay_pool_root)?;
    }
    clear_transaction(store_root)
}

fn read_active_pointer(
    store_root: &Path,
) -> Result<DurablePublicationPointerV1, CodeGenerationRetentionErrorV1> {
    let path = store_root.join(ACTIVE_POINTER_FILE);
    let bytes = std::fs::read(&path).map_err(storage)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "active pointer '{}' is corrupt: {error}",
            path.display()
        ))
    })
}

/// The one reader that distinguishes "no pointer was ever published" from a
/// readable pointer. Every other failure (corruption, permissions) stays an
/// error; only `NotFound` is the typed unpublished state.
fn read_optional_active_pointer(
    store_root: &Path,
) -> Result<Option<DurablePublicationPointerV1>, CodeGenerationRetentionErrorV1> {
    match std::fs::symlink_metadata(store_root.join(ACTIVE_POINTER_FILE)) {
        Ok(_) => read_active_pointer(store_root).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

fn validate_durable_generation_index(
    pointer: &DurablePublicationPointerV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let expected_digest = durable_generation_index_digest(
        &pointer.generation_index,
        pointer.generation_index_truncated,
    )?;
    if pointer.generation_index_digest.as_deref() != Some(expected_digest.as_str()) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "publication-pointer generation index digest does not match its entries".to_owned(),
        ));
    }
    let mut generation_ids = BTreeSet::new();
    let mut text_artifacts = BTreeMap::new();
    for entry in &pointer.generation_index {
        validate_generation_file(&entry.generation_file)?;
        if entry.size_bytes == 0 || !generation_ids.insert(entry.generation_id.as_str()) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "publication-pointer generation index contains an invalid or duplicate entry"
                    .to_owned(),
            ));
        }
        if let Some(artifact) = entry.text_artifact.as_ref() {
            validate_text_artifact_descriptor(artifact)?;
            if artifact.generation_id.as_str() != entry.generation_id {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                    "publication-pointer text artifact names a different generation".to_owned(),
                ));
            }
            let identity = (
                artifact.artifact_digest.as_str(),
                artifact.artifact_size_bytes,
            );
            if text_artifacts
                .insert(artifact.artifact_file.as_str(), identity)
                .is_some_and(|prior| prior != identity)
            {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                    "publication-pointer text artifact path has conflicting identity".to_owned(),
                ));
            }
        }
    }
    let Some(active_entry) = pointer
        .generation_index
        .iter()
        .find(|entry| entry.generation_id == pointer.generation_id)
    else {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "publication-pointer generation index does not contain its active generation"
                .to_owned(),
        ));
    };
    if active_entry.snapshot_content_identity != pointer.snapshot_content_identity
        || active_entry.sealed_at_micros != pointer.sealed_at_micros
        || active_entry.generation_file != pointer.generation_file
        || active_entry.state_digest != pointer.state_digest
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "publication-pointer active generation index entry does not match its pointer"
                .to_owned(),
        ));
    }
    let mut bounded = pointer.generation_index.clone();
    if retain_bounded_generation_index(&mut bounded, &pointer.generation_id) > 0
        || bounded != pointer.generation_index
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "publication-pointer generation index exceeds its retention bounds".to_owned(),
        ));
    }
    Ok(())
}

fn validate_sealed_generation_identity(
    identity: &DurableSealedCodeGenerationIdentityV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    validate_generation_file(&identity.locator)?;
    if identity.size_bytes == 0 {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "sealed generation identity has a zero byte size".to_owned(),
        ));
    }
    let digest = sha256_file_component(&identity.digest, "sealed generation")?;
    if identity.locator != format!("generation-{digest}.json") {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "sealed generation locator does not match its digest".to_owned(),
        ));
    }
    Ok(())
}

fn validate_text_artifact_descriptor(
    descriptor: &DurableCodeTextArtifactDescriptorV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if descriptor.artifact_size_bytes == 0 {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "text artifact descriptor has a zero byte size".to_owned(),
        ));
    }
    let digest = sha256_file_component(&descriptor.artifact_digest, "text artifact")?;
    if descriptor.artifact_file != format!("text-artifact-{digest}.bin") {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "text artifact filename does not match its digest".to_owned(),
        ));
    }
    Ok(())
}

fn sha256_file_component<'a>(
    digest: &'a ManifestDigest,
    resource: &str,
) -> Result<&'a str, CodeGenerationRetentionErrorV1> {
    let Some(value) = digest.as_str().strip_prefix("sha256:") else {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{resource} digest is not SHA-256"
        )));
    };
    if !is_lowercase_hex(value, 64) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{resource} digest is not lowercase SHA-256"
        )));
    }
    Ok(value)
}

fn generation_file_name(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    (path.is_file()
        && file_name.starts_with("generation-")
        && file_name.ends_with(".json")
        && validate_generation_file(file_name).is_ok())
    .then(|| file_name.to_owned())
}

fn validate_generation_file(value: &str) -> Result<(), CodeGenerationRetentionErrorV1> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains(['/', '\\'])
        || path.file_name().and_then(|name| name.to_str()) != Some(value)
        || !value.starts_with("generation-")
        || !value.ends_with(".json")
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "generation file name is not a direct immutable generation artifact".to_owned(),
        ));
    }
    Ok(())
}

fn build_receipt(
    plan: &CodeGenerationRetentionPlanV1,
    deleted_generations: Vec<CodeGenerationRetentionGenerationV1>,
    completed_at: UtcMicros,
) -> Result<CodeGenerationRetentionReceiptV1, CodeGenerationRetentionErrorV1> {
    let reclaimed_bytes = total_bytes(&deleted_generations);
    let material = CodeGenerationRetentionReceiptMaterialV1 {
        schema: RECEIPT_SCHEMA,
        active_generation_id: plan.active_generation_id.as_ref(),
        vector_readable_sources: &plan.vector_readable_sources,
        rollback_floor: plan.rollback_floor,
        deleted_generations: &deleted_generations,
        reclaimed_bytes,
        completed_at_micros: completed_at.0,
    };
    let digest = canonical_sha256(&material)
        .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
    let receipt_digest = receipt_digest_file_component(&GENERATION_RECEIPT_STORE, digest.as_str())?;
    Ok(CodeGenerationRetentionReceiptV1 {
        schema: RECEIPT_SCHEMA.to_owned(),
        receipt_digest,
        active_generation_id: plan.active_generation_id.clone(),
        vector_readable_sources: plan.vector_readable_sources.clone(),
        rollback_floor: plan.rollback_floor,
        deleted_generations,
        reclaimed_bytes,
        completed_at_micros: completed_at.0,
    })
}

fn sync_directory(path: &Path) -> Result<(), CodeGenerationRetentionErrorV1> {
    tracedecay_private_fs::framed_log::sync_directory(path, DirectorySyncPolicy::Strict)
        .map_err(storage)
}

fn total_bytes(generations: &[CodeGenerationRetentionGenerationV1]) -> u64 {
    generations.iter().fold(0_u64, |total, generation| {
        total.saturating_add(generation.size_bytes)
    })
}

fn storage(error: impl std::fmt::Display) -> CodeGenerationRetentionErrorV1 {
    CodeGenerationRetentionErrorV1::Storage(error.to_string())
}

#[cfg(test)]
mod tests;

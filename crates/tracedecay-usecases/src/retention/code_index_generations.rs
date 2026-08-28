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
#[cfg(test)]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
#[cfg(test)]
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_private_fs::framed_log::DirectorySyncPolicy;
// The census gates on the exact revision the publisher writes. A second copy of
// that number here let the writer be versioned to 3 while retention still
// demanded 1: every real sealed file was refused as "incompatible" and the store
// became uncollectable.
#[cfg(test)]
use tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1;
use tracedecay_code_index::production::sealed_generation_format_revision_is_compatible;
/// Only the generation fixtures build tagged digests; production here works in
/// untagged hex, so importing this unconditionally is an unused-import error.
#[cfg(test)]
use tracedecay_domain::canonical_text::encode_tagged_lowercase_hex;
#[cfg(test)]
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::{CodeGenerationId, ManifestDigest, UtcMicros, canonical_sha256};

mod generation_scan;
mod graph_replay_release;
mod locking;
mod scope_quarantine;
mod generation_transactions;
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
    acquire_graph_replay_pool_lock, cleanup_committed_transaction,
    cleanup_committed_transaction_under_graph_replay_pool_lock, clear_transaction,
    expose_staged_generations_under_graph_replay_pool_lock, load_transaction,
    open_file_sha256_hex_cancellable, path_still_names_open_file, persist_transaction,
    receipt_is_durable, regular_file_exists, remove_empty_stage_root,
    rollback_staged_transaction, stage_collectable_generations, transaction_path,
};
#[cfg(test)]
use generation_transactions::{
    transaction_stage_root, verify_existing_graph_replay_pool_entry,
};
use scope_roots::is_code_index_scope_hash;
#[cfg(test)]
use scope_roots::{
    ScopeRootRetentionTransactionV1, build_scope_receipt, persist_scope_transaction,
    scope_receipt_digest, scope_receipt_path, scope_stage_root, scope_transaction_path,
    validate_scope_transaction, write_scope_receipt,
};
use text_artifacts::{
    execute_text_artifact_retention_under_store_lock, plan_collectable_text_artifacts_cancellable,
    recover_pending_text_artifact_transaction_unlocked, text_artifact_transaction_path,
};
#[cfg(test)]
use text_artifacts::{
    build_text_artifact_receipt, persist_text_artifact_transaction,
    stage_collectable_text_artifacts, total_text_artifact_bytes, write_text_artifact_receipt,
};

use generation_scan::read_generation_metadata;
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

const ACTIVE_POINTER_FILE: &str = "active-code-generation-v1.json";
const GENERATIONS_DIRECTORY: &str = "code-generations-v1";
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
    let generation_bytes = entries
        .iter()
        .fold(0_u64, |total, entry| total.saturating_add(entry.size_bytes));
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
    pub active_generation_id: CodeGenerationId,
    pub vector_readable_sources: BTreeSet<CodeGenerationId>,
    pub rollback_floor: usize,
    pub superseded_generations: Vec<CodeGenerationRetentionGenerationV1>,
    pub collectable_generations: Vec<CodeGenerationRetentionGenerationV1>,
    /// Derived text-artifact debris selected from one bounded canonical
    /// inventory. Descriptor-referenced and still-in-progress staging files
    /// are deliberately absent.
    collectable_text_artifacts: Vec<CodeTextArtifactRetentionCandidateV1>,
    /// Unique bytes seen in the bounded text-artifact inventory: durable
    /// descriptor targets, the one resumable active staging file, and this
    /// pass's selected debris candidates. A descriptor shared by retained
    /// generations is counted once by its canonical artifact path.
    text_artifact_inventory_bytes: u64,
    /// How thoroughly this plan proved generation integrity. Apply-mode
    /// execution refuses anything but [`GenerationDigestVerificationV1::Full`].
    pub verification: GenerationDigestVerificationV1,
    active_pointer: DurablePublicationPointerV1,
}

impl CodeGenerationRetentionPlanV1 {
    #[must_use]
    pub fn active_generation_file(&self) -> &str {
        &self.active_pointer.generation_file
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
        !self.collectable_generations.is_empty() || !self.collectable_text_artifacts.is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeGenerationRetentionReceiptV1 {
    pub schema: String,
    pub receipt_digest: String,
    pub active_generation_id: CodeGenerationId,
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
    active_generation_id: CodeGenerationId,
    active_generation_index_digest: String,
    deleted_artifacts: Vec<CodeTextArtifactRetentionCandidateV1>,
    inventory_bytes_before_collection: u64,
    pub reclaimed_bytes: u64,
    completed_at_micros: i64,
}

#[derive(Serialize)]
struct CodeGenerationRetentionReceiptMaterialV1<'a> {
    schema: &'static str,
    active_generation_id: &'a CodeGenerationId,
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
    active_pointer: DurablePublicationPointerV1,
    receipt: CodeGenerationRetentionReceiptV1,
}

#[derive(Serialize)]
struct CodeTextArtifactRetentionReceiptMaterialV1<'a> {
    schema: &'static str,
    active_generation_id: &'a CodeGenerationId,
    active_generation_index_digest: &'a str,
    deleted_artifacts: &'a [CodeTextArtifactRetentionCandidateV1],
    inventory_bytes_before_collection: u64,
    reclaimed_bytes: u64,
    completed_at_micros: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CodeTextArtifactRetentionTransactionV1 {
    schema: String,
    active_pointer: DurablePublicationPointerV1,
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
    let census = plan_code_generation_retention_with_verification_cancellable(
        store_root,
        vector_readable_sources,
        rollback_floor,
        GenerationDigestVerificationV1::MetadataOnly,
        is_cancelled,
    )?;
    if !census.has_collectable_work() {
        return Ok(census);
    }
    plan_next_code_generation_retention_cancellable(
        store_root,
        vector_readable_sources,
        rollback_floor,
        is_cancelled,
    )
}

#[hotpath::measure(label = "usecases.retention.plan")]
fn plan_code_generation_retention_with_verification_cancellable(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    verification: GenerationDigestVerificationV1,
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
    let active_pointer = read_active_pointer(store_root)?;
    validate_generation_file(&active_pointer.generation_file)?;
    let active_generation_id = CodeGenerationId::new(active_pointer.generation_id.clone())
        .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
    validate_durable_generation_index(&active_pointer)?;
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let entries = std::fs::read_dir(&generations_root).map_err(storage)?;
    let mut generations = BTreeMap::new();
    let mut active_state_digest = None;

    for entry in entries {
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
        if file_name == active_pointer.generation_file {
            active_state_digest = Some(raw_state_digest);
            if generation_id != active_generation_id {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                    "active pointer generation id does not match its sealed file".to_owned(),
                ));
            }
        }
    }

    if active_state_digest.as_deref() != Some(active_pointer.state_digest.as_str()) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "active generation file is missing or does not match the pointer digest".to_owned(),
        ));
    }
    let pointer_generations = active_pointer
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
    for entry in &active_pointer.generation_index {
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
        .filter(|generation| generation.generation_id != active_generation_id)
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
    marked.insert(active_generation_id.clone());
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
        &active_pointer,
        verification,
        is_cancelled,
    )?;
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
        text_artifact_inventory_bytes: text_artifact_inventory.unique_bytes,
        verification,
        active_pointer,
    })
}


fn generation_file_digest(file_name: &str) -> Option<&str> {
    file_name
        .strip_prefix("generation-")?
        .strip_suffix(".json")
        .filter(|digest| is_lowercase_sha256(digest))
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
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
    if !plan.has_collectable_work() {
        return Ok(CodeGenerationRetentionReportV1 {
            plan,
            deleted_generations: Vec::new(),
            receipt: None,
            deleted_text_artifacts: Vec::new(),
            text_artifact_receipt: None,
        });
    }
    if read_active_pointer(store_root)? != plan.active_pointer {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "active generation changed after the retention mark phase".to_owned(),
        ));
    }
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
        let graph_replay_pool_lock = graph_replay_pool_root
            .map(acquire_graph_replay_pool_lock)
            .transpose()?;
        persist_transaction(store_root, &transaction)?;

        let result = (|| {
            stage_collectable_generations(store_root, &transaction)?;
            if read_active_pointer(store_root)? != transaction.active_pointer {
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
            write_receipt(store_root, &receipt)?;
            cleanup_committed_transaction_under_graph_replay_pool_lock(
                store_root,
                &transaction,
                &vector_readable_sources,
                graph_replay_pool_lock.as_ref(),
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
        );
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
                is_cancelled,
            )?
        }
        CodeGenerationRetentionModeV1::DryRun => {
            plan_code_generation_retention_with_verification_cancellable(
                store_root,
                vector_readable_sources,
                rollback_floor,
                GenerationDigestVerificationV1::Full,
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
    let active_path = store_root.join(ACTIVE_POINTER_FILE);
    let active_pointer = match std::fs::read(&active_path) {
        Ok(bytes) => {
            serde_json::from_slice::<DurablePublicationPointerV1>(&bytes).map_err(|error| {
                CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "active pointer '{}' is corrupt: {error}",
                    active_path.display()
                ))
            })?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CodeGenerationRetentionObservationV1::default());
        }
        Err(error) => return Err(storage(error)),
    };
    validate_generation_file(&active_pointer.generation_file)?;
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let entries = match std::fs::read_dir(&generations_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
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
        if file_name == active_pointer.generation_file {
            active_present = true;
            continue;
        }
        observation.superseded_generation_count =
            observation.superseded_generation_count.saturating_add(1);
        observation.superseded_generation_bytes = observation
            .superseded_generation_bytes
            .saturating_add(entry.metadata().map_err(storage)?.len());
    }
    if !active_present {
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
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
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
        active_generation_id: &plan.active_generation_id,
        vector_readable_sources: &plan.vector_readable_sources,
        rollback_floor: plan.rollback_floor,
        deleted_generations: &deleted_generations,
        reclaimed_bytes,
        completed_at_micros: completed_at.0,
    };
    let digest = canonical_sha256(&material)
        .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
    let receipt_digest = digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(digest.as_str())
        .to_owned();
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


fn write_receipt(
    store_root: &Path,
    receipt: &CodeGenerationRetentionReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    graph_replay_release::write_events(store_root, receipt)?;
    let receipts_root = store_root.join(RECEIPTS_DIRECTORY);
    std::fs::create_dir_all(&receipts_root).map_err(storage)?;
    let final_path = receipts_root.join(format!("receipt-{}.json", receipt.receipt_digest));
    let bytes = serde_json::to_vec(receipt).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "retention receipt serialization failed: {error}"
        ))
    })?;
    if final_path.exists() {
        let existing = std::fs::read(&final_path).map_err(storage)?;
        if existing == bytes {
            return Ok(());
        }
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention receipt digest collides with different bytes".to_owned(),
        ));
    }
    let temporary = receipts_root.join(format!(
        ".receipt-{}.{}.tmp",
        receipt.receipt_digest,
        std::process::id()
    ));
    if temporary.exists() {
        std::fs::remove_file(&temporary).map_err(storage)?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(storage)?;
    file.write_all(&bytes).map_err(storage)?;
    file.sync_all().map_err(storage)?;
    std::fs::rename(&temporary, &final_path).map_err(storage)?;
    sync_directory(&receipts_root)
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

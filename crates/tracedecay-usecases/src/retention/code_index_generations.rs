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
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_application::{DirectorySyncPolicy, atomic_write};
// The census gates on the exact revision the publisher writes. A second copy of
// that number here let the writer be versioned to 3 while retention still
// demanded 1: every real sealed file was refused as "incompatible" and the store
// became uncollectable.
use tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1;
use tracedecay_domain::{CodeGenerationId, ManifestDigest, UtcMicros, canonical_sha256};

mod generation_scan;
mod graph_replay_release;
mod locking;
pub use graph_replay_release::{
    CodeGenerationGraphReplayReleasePageV1, CodeGenerationGraphReplayReleaseV1,
    code_generation_graph_replay_release_page, complete_code_generation_graph_replay_release,
};
pub use locking::{
    CodeGenerationStoreLockV1, acquire_code_generation_store_lock,
    try_acquire_code_generation_store_lock,
};

use generation_scan::read_generation_metadata;
use locking::acquire_scope_retention_lock;

pub const DEFAULT_SUPERSEDED_GENERATION_FLOOR: usize = 0;
pub const MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1: usize = 32;
pub const MAX_DURABLE_GENERATION_INDEX_BYTES_V1: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_DURABLE_GENERATION_INDEX_TTL_MICROS_V1: i64 = 7 * 24 * 60 * 60 * 1_000_000;

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
}

/// Apply the durable exact-generation history bounds in canonical oldest-first
/// order. The active generation is never evicted; every other generation,
/// including dirty snapshots without Git evidence, is subject to the same TTL,
/// byte, and count limits.
pub fn retain_bounded_generation_index(
    entries: &mut Vec<DurableGenerationIndexEntryV1>,
    active_generation_id: &str,
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
        entry.generation_id == active_generation_id || entry.sealed_at_micros >= oldest_retained
    });

    loop {
        let total_bytes = entries
            .iter()
            .fold(0_u64, |total, entry| total.saturating_add(entry.size_bytes));
        if entries.len() <= MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1
            && total_bytes <= MAX_DURABLE_GENERATION_INDEX_BYTES_V1
        {
            break;
        }
        let Some(index) = entries
            .iter()
            .position(|entry| entry.generation_id != active_generation_id)
        else {
            break;
        };
        entries.remove(index);
    }
    original_len.saturating_sub(entries.len())
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeGenerationRetentionPlanV1 {
    pub active_generation_id: CodeGenerationId,
    pub vector_readable_sources: BTreeSet<CodeGenerationId>,
    pub rollback_floor: usize,
    pub superseded_generations: Vec<CodeGenerationRetentionGenerationV1>,
    pub collectable_generations: Vec<CodeGenerationRetentionGenerationV1>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeGenerationRetentionReportV1 {
    pub plan: CodeGenerationRetentionPlanV1,
    pub deleted_generations: Vec<CodeGenerationRetentionGenerationV1>,
    pub receipt: Option<CodeGenerationRetentionReceiptV1>,
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

/// The directory name `code-index-v1/` uses for one canonical project root.
///
/// Scope-root reconciliation compares directory names against this exact
/// derivation, so it must never diverge from
/// [`scoped_code_index_store_root`] — a divergence would classify a live scope
/// as stranded.
#[must_use]
pub fn code_index_scope_hash(canonical_project_root: &Path) -> String {
    hex::encode(Sha256::digest(
        canonical_project_root.to_string_lossy().as_bytes(),
    ))
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
pub fn prepare_next_code_generation_retention_cancellable(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeGenerationRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    if is_cancelled() {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    recover_code_generation_retention(store_root, vector_readable_sources)?;
    plan_next_code_generation_retention_cancellable(
        store_root,
        vector_readable_sources,
        rollback_floor,
        is_cancelled,
    )
}

fn plan_code_generation_retention_with_verification_cancellable(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    verification: GenerationDigestVerificationV1,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeGenerationRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    if is_cancelled() {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    if transaction_path(store_root).exists() {
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
        if is_cancelled() {
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
        if format_revision != SEALED_GENERATION_FORMAT_REVISION_V1 {
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
        .collect();

    Ok(CodeGenerationRetentionPlanV1 {
        active_generation_id,
        vector_readable_sources: vector_readable_sources.clone(),
        rollback_floor,
        superseded_generations,
        collectable_generations,
        verification,
        active_pointer,
    })
}

pub fn execute_code_generation_retention(
    store_root: &Path,
    plan: CodeGenerationRetentionPlanV1,
    mode: CodeGenerationRetentionModeV1,
    completed_at: UtcMicros,
) -> Result<CodeGenerationRetentionReportV1, CodeGenerationRetentionErrorV1> {
    if mode == CodeGenerationRetentionModeV1::DryRun {
        return Ok(CodeGenerationRetentionReportV1 {
            plan,
            deleted_generations: Vec::new(),
            receipt: None,
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
    if transaction_path(store_root).exists() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-generation retention recovery is pending".to_owned(),
        ));
    }
    if plan.collectable_generations.is_empty() {
        return Ok(CodeGenerationRetentionReportV1 {
            plan,
            deleted_generations: Vec::new(),
            receipt: None,
        });
    }
    if read_active_pointer(store_root)? != plan.active_pointer {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "active generation changed after the retention mark phase".to_owned(),
        ));
    }
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
    persist_transaction(store_root, &transaction)?;

    let result = (|| {
        stage_collectable_generations(store_root, &transaction)?;
        if read_active_pointer(store_root)? != transaction.active_pointer {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "active generation changed while retention candidates were quarantined".to_owned(),
            ));
        }
        write_receipt(store_root, &receipt)?;
        cleanup_committed_transaction(store_root, &transaction, &vector_readable_sources)?;
        clear_transaction(store_root)
    })();
    if let Err(error) = result {
        if !receipt_is_durable(store_root, &receipt)? {
            rollback_staged_transaction(store_root, &transaction)?;
            clear_transaction(store_root)?;
        }
        return Err(error);
    }

    Ok(CodeGenerationRetentionReportV1 {
        plan,
        deleted_generations,
        receipt: Some(receipt),
    })
}

pub fn recover_code_generation_retention(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let _store_lock = acquire_code_generation_store_lock(store_root)?;
    recover_pending_transaction_unlocked(store_root, vector_readable_sources)
}

pub fn run_code_generation_retention(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
    mode: CodeGenerationRetentionModeV1,
    completed_at: UtcMicros,
) -> Result<CodeGenerationRetentionReportV1, CodeGenerationRetentionErrorV1> {
    let plan = match mode {
        CodeGenerationRetentionModeV1::Apply => prepare_next_code_generation_retention_cancellable(
            store_root,
            vector_readable_sources,
            rollback_floor,
            &|| false,
        )?,
        CodeGenerationRetentionModeV1::DryRun => {
            plan_code_generation_retention(store_root, vector_readable_sources, rollback_floor)?
        }
    };
    execute_code_generation_retention(store_root, plan, mode, completed_at)
}

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
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let Some(transaction) = load_transaction(store_root)? else {
        return Ok(());
    };

    if receipt_is_durable(store_root, &transaction.receipt)? {
        cleanup_committed_transaction(store_root, &transaction, vector_readable_sources)?;
    } else {
        rollback_staged_transaction(store_root, &transaction)?;
    }
    clear_transaction(store_root)
}

fn transaction_path(store_root: &Path) -> PathBuf {
    store_root.join(TRANSACTION_FILE)
}

fn transaction_stage_root(
    store_root: &Path,
    receipt: &CodeGenerationRetentionReceiptV1,
) -> PathBuf {
    store_root
        .join(QUARANTINE_DIRECTORY)
        .join(&receipt.receipt_digest)
}

fn persist_transaction(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    validate_transaction(transaction)?;
    let bytes = serde_json::to_vec(transaction).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "retention transaction serialization failed: {error}"
        ))
    })?;
    atomic_write(
        &transaction_path(store_root),
        "code-generation-retention-transaction",
        &bytes,
        DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(storage)
}

fn load_transaction(
    store_root: &Path,
) -> Result<Option<CodeGenerationRetentionTransactionV1>, CodeGenerationRetentionErrorV1> {
    let path = transaction_path(store_root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage(error)),
    };
    if bytes.len() as u64 > MAX_TRANSACTION_BYTES {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "retention transaction '{}' exceeds the bounded journal size",
            path.display()
        )));
    }
    let transaction = serde_json::from_slice(&bytes).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "retention transaction '{}' is unreadable: {error}",
            path.display()
        ))
    })?;
    validate_transaction(&transaction)?;
    Ok(Some(transaction))
}

fn validate_transaction(
    transaction: &CodeGenerationRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if transaction.schema != TRANSACTION_SCHEMA || transaction.receipt.schema != RECEIPT_SCHEMA {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention transaction has an incompatible schema".to_owned(),
        ));
    }
    if transaction.receipt.receipt_digest.len() != 64
        || !transaction
            .receipt
            .receipt_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention transaction receipt digest is not a SHA-256 file component".to_owned(),
        ));
    }
    validate_generation_file(&transaction.active_pointer.generation_file)?;
    let pointer_generation = CodeGenerationId::new(
        transaction.active_pointer.generation_id.clone(),
    )
    .map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "retention transaction active generation id is invalid: {error}"
        ))
    })?;
    if pointer_generation != transaction.receipt.active_generation_id {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention transaction active pointer does not match its receipt".to_owned(),
        ));
    }
    let mut generation_ids = BTreeSet::new();
    let mut generation_files = BTreeSet::new();
    for generation in &transaction.receipt.deleted_generations {
        validate_generation_file(&generation.generation_file)?;
        if !generation_ids.insert(generation.generation_id.clone())
            || !generation_files.insert(generation.generation_file.clone())
        {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "retention transaction has duplicate generation identities".to_owned(),
            ));
        }
    }
    if generation_ids.is_empty()
        || generation_ids.contains(&transaction.receipt.active_generation_id)
        || !transaction
            .receipt
            .vector_readable_sources
            .is_disjoint(&generation_ids)
        || transaction.receipt.reclaimed_bytes
            != total_bytes(&transaction.receipt.deleted_generations)
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention transaction violates exact liveness or byte invariants".to_owned(),
        ));
    }
    Ok(())
}

fn receipt_path(store_root: &Path, receipt: &CodeGenerationRetentionReceiptV1) -> PathBuf {
    store_root
        .join(RECEIPTS_DIRECTORY)
        .join(format!("receipt-{}.json", receipt.receipt_digest))
}

fn receipt_bytes(
    receipt: &CodeGenerationRetentionReceiptV1,
) -> Result<Vec<u8>, CodeGenerationRetentionErrorV1> {
    serde_json::to_vec(receipt).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "retention receipt serialization failed: {error}"
        ))
    })
}

fn receipt_is_durable(
    store_root: &Path,
    receipt: &CodeGenerationRetentionReceiptV1,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    let path = receipt_path(store_root, receipt);
    if !regular_file_exists(&path)? {
        return Ok(false);
    }
    let existing = std::fs::read(&path).map_err(storage)?;
    if existing != receipt_bytes(receipt)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention receipt digest collides with different bytes".to_owned(),
        ));
    }
    Ok(true)
}

fn stage_collectable_generations(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let stage_root = transaction_stage_root(store_root, &transaction.receipt);
    std::fs::create_dir_all(&stage_root).map_err(storage)?;
    sync_directory(stage_root.parent().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState("retention quarantine has no parent".to_owned())
    })?)?;

    for generation in &transaction.receipt.deleted_generations {
        let source = generations_root.join(&generation.generation_file);
        let staged = stage_root.join(&generation.generation_file);
        match (regular_file_exists(&source)?, regular_file_exists(&staged)?) {
            (true, false) => {
                let metadata = std::fs::metadata(&source).map_err(storage)?;
                if metadata.len() != generation.size_bytes {
                    return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                        "collectable generation '{}' changed after the mark phase",
                        generation.generation_file
                    )));
                }
                std::fs::rename(&source, &staged).map_err(storage)?;
                sync_directory(&generations_root)?;
                sync_directory(&stage_root)?;
            }
            (false, false) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "collectable generation '{}' is missing before quarantine",
                    generation.generation_file
                )));
            }
            (false, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "collectable generation '{}' was already quarantined",
                    generation.generation_file
                )));
            }
            (true, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "collectable generation '{}' exists in both source and quarantine",
                    generation.generation_file
                )));
            }
        }
    }
    Ok(())
}

fn rollback_staged_transaction(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let stage_root = transaction_stage_root(store_root, &transaction.receipt);
    for generation in &transaction.receipt.deleted_generations {
        let source = generations_root.join(&generation.generation_file);
        let staged = stage_root.join(&generation.generation_file);
        match (regular_file_exists(&source)?, regular_file_exists(&staged)?) {
            (true, false) => {}
            (false, true) => {
                std::fs::rename(&staged, &source).map_err(storage)?;
                sync_directory(&generations_root)?;
                sync_directory(&stage_root)?;
            }
            (false, false) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "retention rollback cannot find '{}'",
                    generation.generation_file
                )));
            }
            (true, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "retention rollback found duplicate '{}'",
                    generation.generation_file
                )));
            }
        }
    }
    graph_replay_release::remove_events(store_root, &transaction.receipt)?;
    remove_empty_stage_root(&stage_root)
}

fn cleanup_committed_transaction(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    ensure_transaction_liveness(store_root, transaction, vector_readable_sources)?;
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let stage_root = transaction_stage_root(store_root, &transaction.receipt);
    for generation in &transaction.receipt.deleted_generations {
        let source = generations_root.join(&generation.generation_file);
        if regular_file_exists(&source)? {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "retention receipt is durable but '{}' returned to the generation directory",
                generation.generation_file
            )));
        }
        let staged = stage_root.join(&generation.generation_file);
        if regular_file_exists(&staged)? {
            std::fs::remove_file(&staged).map_err(storage)?;
            sync_directory(&stage_root)?;
        }
    }
    remove_empty_stage_root(&stage_root)
}

fn ensure_transaction_liveness(
    store_root: &Path,
    transaction: &CodeGenerationRetentionTransactionV1,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let current = read_active_pointer(store_root)?;
    let current_generation =
        CodeGenerationId::new(current.generation_id.clone()).map_err(|error| {
            CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "current active generation id is invalid during retention recovery: {error}"
            ))
        })?;
    let deleted_ids = transaction
        .receipt
        .deleted_generations
        .iter()
        .map(|generation| generation.generation_id.clone())
        .collect::<BTreeSet<_>>();
    if deleted_ids.contains(&current_generation)
        || transaction
            .receipt
            .deleted_generations
            .iter()
            .any(|generation| generation.generation_file == current.generation_file)
        || !deleted_ids.is_disjoint(vector_readable_sources)
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "retention recovery would remove an active or vector-readable generation".to_owned(),
        ));
    }
    Ok(())
}

fn clear_transaction(store_root: &Path) -> Result<(), CodeGenerationRetentionErrorV1> {
    let path = transaction_path(store_root);
    match std::fs::remove_file(&path) {
        Ok(()) => sync_directory(store_root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage(error)),
    }
}

fn remove_empty_stage_root(stage_root: &Path) -> Result<(), CodeGenerationRetentionErrorV1> {
    let mut entries = match std::fs::read_dir(stage_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage(error)),
    };
    if entries.next().is_some() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "retention quarantine '{}' contains unexpected files",
            stage_root.display()
        )));
    }
    std::fs::remove_dir(stage_root).map_err(storage)?;
    sync_directory(stage_root.parent().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState("retention quarantine has no parent".to_owned())
    })?)
}

fn regular_file_exists(path: &Path) -> Result<bool, CodeGenerationRetentionErrorV1> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "retention path '{}' is not a regular file",
            path.display()
        ))),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(storage(error)),
    }
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
    for entry in &pointer.generation_index {
        validate_generation_file(&entry.generation_file)?;
        if entry.size_bytes == 0 || !generation_ids.insert(entry.generation_id.as_str()) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "publication-pointer generation index contains an invalid or duplicate entry"
                    .to_owned(),
            ));
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
    tracedecay_application::sync_directory(path, DirectorySyncPolicy::Strict).map_err(storage)
}

fn total_bytes(generations: &[CodeGenerationRetentionGenerationV1]) -> u64 {
    generations.iter().fold(0_u64, |total, generation| {
        total.saturating_add(generation.size_bytes)
    })
}

fn storage(error: impl std::fmt::Display) -> CodeGenerationRetentionErrorV1 {
    CodeGenerationRetentionErrorV1::Storage(error.to_string())
}

// ---------------------------------------------------------------------------
// Scope-root reconciliation
// ---------------------------------------------------------------------------
//
// Generation retention above is *within* one scope root
// (`code-index-v1/<sha256(canonical_project_root)>/`). Every caller derives
// exactly one scope from the project root it was handed, so nothing has ever
// enumerated the siblings. A profile therefore accumulates whole scope roots
// belonging to project roots that no longer exist — deleted agent worktrees are
// the common source — and those bytes are unreachable by any retention pass and
// uncounted by any report.
//
// This section closes that gap under the same discipline as generation
// retention: journal, quarantine, durable receipt, then unlink. It is
// deliberately harder to trigger than generation retention, because the unit of
// collection is an entire directory tree rather than one superseded file.

/// One `code-index-v1/` scope directory whose scope hash matches no live
/// canonical project root.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StrandedCodeIndexScopeV1 {
    /// The directory name, which is `hex(sha256(canonical_project_root))`.
    pub scope_hash: String,
    /// Total payload bytes under the scope, excluding retention lock files.
    pub size_bytes: u64,
    /// Newest mtime anywhere in the scope, in unix seconds. Drives the age gate.
    pub newest_mtime_secs: i64,
}

/// Why a stranded scope was left alone even though nothing live names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrandedScopeRefusalV1 {
    /// The scope has an unfinished generation-retention journal. Recovering it
    /// belongs to that scope's own owner; collecting it here would destroy the
    /// evidence that recovery needs.
    PendingGenerationRetention,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefusedCodeIndexScopeV1 {
    pub scope: StrandedCodeIndexScopeV1,
    pub refusal: StrandedScopeRefusalV1,
}

/// The reconciliation decision for one `code-index-v1/` store root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRootRetentionPlanV1 {
    /// Scope hashes derived from the live canonical roots the caller proved.
    pub live_scope_hashes: BTreeSet<String>,
    /// Scope directories on disk that matched a live root.
    pub live_scope_count: usize,
    /// Directory entries that are not scope roots at all (receipts, quarantine,
    /// lock files). Reported so an unexpected layout is visible rather than
    /// silently swept into "stranded".
    pub unrecognized_entry_count: usize,
    pub minimum_stranding_age_secs: i64,
    /// Stranded, past the age gate, and free of a pending journal.
    pub collectable_scopes: Vec<StrandedCodeIndexScopeV1>,
    /// Stranded but touched too recently to be called abandoned.
    pub retained_immature_scopes: Vec<StrandedCodeIndexScopeV1>,
    /// Stranded but structurally refused.
    pub refused_scopes: Vec<RefusedCodeIndexScopeV1>,
    /// Present only when the canonical production authorities sealed this plan
    /// for Apply. Raw-root observation plans deliberately leave it absent.
    liveness_proof: Option<ScopeRootLivenessProofV1>,
}

impl ScopeRootRetentionPlanV1 {
    #[must_use]
    pub fn liveness_proof(&self) -> Option<&ScopeRootLivenessProofV1> {
        self.liveness_proof.as_ref()
    }

    /// Every scope no live root names, whatever this pass decided to do about
    /// it. This is the number a storage report or Doctor finding must publish:
    /// the gap is "unreachable bytes", not "bytes we happened to collect".
    #[must_use]
    pub fn stranded_scope_count(&self) -> u64 {
        (self.collectable_scopes.len()
            + self.retained_immature_scopes.len()
            + self.refused_scopes.len()) as u64
    }

    #[must_use]
    pub fn stranded_scope_bytes(&self) -> u64 {
        self.collectable_scopes
            .iter()
            .chain(self.retained_immature_scopes.iter())
            .chain(self.refused_scopes.iter().map(|refused| &refused.scope))
            .fold(0_u64, |total, scope| total.saturating_add(scope.size_bytes))
    }

    #[must_use]
    pub fn collectable_scope_bytes(&self) -> u64 {
        total_scope_bytes(&self.collectable_scopes)
    }
}

/// Terminal receipt for one bounded liveness authority. `revision` identifies
/// the exact snapshot while `digest` covers every row in that snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootAuthorityReceiptV1 {
    pub revision: String,
    pub terminal_count: u64,
    pub digest: String,
}

/// Exact relational source bound to one physical code-index scope at the
/// vector census revision recorded by the proof.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootCandidateBindingV1 {
    pub scope_hash: String,
    pub source_scope: tracedecay_store::StoreShardIdV1,
    pub vector_census_revision: String,
    pub live: bool,
}

/// Complete, revision-bound proof used by scope collection. Every authority is
/// explicit so adding a new liveness source cannot silently omit it from the
/// digest or from crash replay.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootLivenessProofV1 {
    pub schema: String,
    pub proof_digest: String,
    pub live_scope_hashes: BTreeSet<String>,
    pub registered_roots: ScopeRootAuthorityReceiptV1,
    pub git_worktrees: ScopeRootAuthorityReceiptV1,
    pub mounted_leases: ScopeRootAuthorityReceiptV1,
    pub configuration_roots: ScopeRootAuthorityReceiptV1,
    pub vector_census: ScopeRootAuthorityReceiptV1,
    pub vector_dependencies: ScopeRootAuthorityReceiptV1,
    pub candidate_binding: ScopeRootCandidateBindingV1,
}

#[derive(Serialize)]
struct ScopeRootLivenessProofMaterialV1<'a> {
    schema: &'static str,
    live_scope_hashes: &'a BTreeSet<String>,
    registered_roots: &'a ScopeRootAuthorityReceiptV1,
    git_worktrees: &'a ScopeRootAuthorityReceiptV1,
    mounted_leases: &'a ScopeRootAuthorityReceiptV1,
    configuration_roots: &'a ScopeRootAuthorityReceiptV1,
    vector_census: &'a ScopeRootAuthorityReceiptV1,
    vector_dependencies: &'a ScopeRootAuthorityReceiptV1,
    candidate_binding: &'a ScopeRootCandidateBindingV1,
}

impl ScopeRootLivenessProofV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        live_scope_hashes: BTreeSet<String>,
        registered_roots: ScopeRootAuthorityReceiptV1,
        git_worktrees: ScopeRootAuthorityReceiptV1,
        mounted_leases: ScopeRootAuthorityReceiptV1,
        configuration_roots: ScopeRootAuthorityReceiptV1,
        vector_census: ScopeRootAuthorityReceiptV1,
        vector_dependencies: ScopeRootAuthorityReceiptV1,
        candidate_binding: ScopeRootCandidateBindingV1,
    ) -> Result<Self, CodeGenerationRetentionErrorV1> {
        let mut proof = Self {
            schema: SCOPE_ROOT_LIVENESS_PROOF_SCHEMA.to_owned(),
            proof_digest: String::new(),
            live_scope_hashes,
            registered_roots,
            git_worktrees,
            mounted_leases,
            configuration_roots,
            vector_census,
            vector_dependencies,
            candidate_binding,
        };
        proof.refresh_digest()?;
        validate_scope_root_liveness_proof(&proof)?;
        Ok(proof)
    }

    fn refresh_digest(&mut self) -> Result<(), CodeGenerationRetentionErrorV1> {
        self.proof_digest = scope_root_liveness_proof_digest(self)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootBindingCleanupReplayV1 {
    pub scope_hash: String,
    pub source_scope: tracedecay_store::StoreShardIdV1,
    pub liveness_proof: ScopeRootLivenessProofV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootRetentionReceiptV1 {
    pub schema: String,
    pub receipt_digest: String,
    /// The exact live set the decision was made against, so a receipt can be
    /// audited without re-deriving it.
    pub live_scope_hashes: BTreeSet<String>,
    pub liveness_proof: ScopeRootLivenessProofV1,
    pub minimum_stranding_age_secs: i64,
    pub collected_scopes: Vec<StrandedCodeIndexScopeV1>,
    pub reclaimed_bytes: u64,
    pub completed_at_micros: i64,
}

#[derive(Serialize)]
struct ScopeReceiptMaterial<'a> {
    schema: &'static str,
    live_scope_hashes: &'a BTreeSet<String>,
    liveness_proof: &'a ScopeRootLivenessProofV1,
    minimum_stranding_age_secs: i64,
    collected_scopes: &'a [StrandedCodeIndexScopeV1],
    reclaimed_bytes: u64,
    completed_at_micros: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ScopeRootRetentionTransactionV1 {
    schema: String,
    receipt: ScopeRootRetentionReceiptV1,
}

/// A durable promise to remove one semantic source-scope binding only after
/// the corresponding scope-root receipt has committed. This lives beside the
/// filesystem transaction because deleting the source scope would otherwise
/// erase the only place an interrupted relational cleanup could be recovered.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ScopeRootBindingCleanupIntentV1 {
    schema: String,
    scope_hash: String,
    source_scope: tracedecay_store::StoreShardIdV1,
    liveness_proof: ScopeRootLivenessProofV1,
    receipt: ScopeRootRetentionReceiptV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRootRetentionReportV1 {
    pub plan: ScopeRootRetentionPlanV1,
    pub collected_scopes: Vec<StrandedCodeIndexScopeV1>,
    pub receipt: Option<ScopeRootRetentionReceiptV1>,
}

/// Read-only classification of every scope directory under one
/// `code-index-v1/` store root against caller-supplied roots.
///
/// This API never seals an Apply-capable plan. Production collection uses
/// [`plan_scope_root_retention_with_liveness_proof`] after the canonical
/// authorities have produced a complete revision-bound receipt.
pub fn plan_scope_root_retention(
    store_root: &Path,
    live_canonical_roots: &BTreeSet<PathBuf>,
    minimum_stranding_age_secs: i64,
    now_secs: i64,
) -> Result<ScopeRootRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    if live_canonical_roots.is_empty() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope reconciliation refused an empty live-root set".to_owned(),
        ));
    }
    let live_scope_hashes = live_canonical_roots
        .iter()
        .map(|root| code_index_scope_hash(root))
        .collect::<BTreeSet<_>>();
    plan_scope_root_retention_from_hashes(
        store_root,
        &live_scope_hashes,
        minimum_stranding_age_secs,
        now_secs,
    )
}

/// Plan an Apply-capable scope pass from the complete canonical liveness proof.
/// The physical executor will require the exact proof again immediately before
/// quarantine, which turns every authority revision into a compare-and-swap.
pub fn plan_scope_root_retention_with_liveness_proof(
    store_root: &Path,
    liveness_proof: ScopeRootLivenessProofV1,
    minimum_stranding_age_secs: i64,
    now_secs: i64,
) -> Result<ScopeRootRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    validate_scope_root_liveness_proof(&liveness_proof)?;
    let mut plan = plan_scope_root_retention_from_hashes(
        store_root,
        &liveness_proof.live_scope_hashes,
        minimum_stranding_age_secs,
        now_secs,
    )?;
    if liveness_proof.candidate_binding.live
        || liveness_proof
            .live_scope_hashes
            .contains(&liveness_proof.candidate_binding.scope_hash)
        || !plan
            .collectable_scopes
            .iter()
            .any(|scope| scope.scope_hash == liveness_proof.candidate_binding.scope_hash)
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope liveness proof does not authorize its exact collection candidate".to_owned(),
        ));
    }
    plan.collectable_scopes
        .retain(|scope| scope.scope_hash == liveness_proof.candidate_binding.scope_hash);
    plan.liveness_proof = Some(liveness_proof);
    Ok(plan)
}

fn plan_scope_root_retention_from_hashes(
    store_root: &Path,
    live_scope_hashes: &BTreeSet<String>,
    minimum_stranding_age_secs: i64,
    now_secs: i64,
) -> Result<ScopeRootRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    if live_scope_hashes.is_empty() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope reconciliation refused an empty live-root set".to_owned(),
        ));
    }
    if scope_transaction_path(store_root).exists() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope reconciliation recovery is pending".to_owned(),
        ));
    }

    let entries = std::fs::read_dir(store_root).map_err(storage)?;
    let mut plan = ScopeRootRetentionPlanV1 {
        live_scope_hashes: live_scope_hashes.clone(),
        live_scope_count: 0,
        unrecognized_entry_count: 0,
        minimum_stranding_age_secs,
        collectable_scopes: Vec::new(),
        retained_immature_scopes: Vec::new(),
        refused_scopes: Vec::new(),
        liveness_proof: None,
    };

    for entry in entries {
        let entry = entry.map_err(storage)?;
        let file_type = entry.file_type().map_err(storage)?;
        if !file_type.is_dir() {
            continue;
        }
        let Some(scope_hash) = entry.file_name().to_str().map(str::to_owned) else {
            plan.unrecognized_entry_count = plan.unrecognized_entry_count.saturating_add(1);
            continue;
        };
        // Only a directory literally named `hex(sha256(root))` is a scope. This
        // is what keeps the receipts and quarantine directories — and anything
        // else a future layout adds — structurally uncollectable.
        if !is_code_index_scope_hash(&scope_hash) {
            plan.unrecognized_entry_count = plan.unrecognized_entry_count.saturating_add(1);
            continue;
        }
        if live_scope_hashes.contains(&scope_hash) {
            plan.live_scope_count = plan.live_scope_count.saturating_add(1);
            continue;
        }

        let scope_root = entry.path();
        let (size_bytes, newest_mtime_secs) = measure_scope_tree(&scope_root)?;
        let scope = StrandedCodeIndexScopeV1 {
            scope_hash,
            size_bytes,
            newest_mtime_secs,
        };
        if scope_root.join(TRANSACTION_FILE).exists() {
            plan.refused_scopes.push(RefusedCodeIndexScopeV1 {
                scope,
                refusal: StrandedScopeRefusalV1::PendingGenerationRetention,
            });
            continue;
        }
        if now_secs.saturating_sub(newest_mtime_secs) < minimum_stranding_age_secs {
            plan.retained_immature_scopes.push(scope);
            continue;
        }
        plan.collectable_scopes.push(scope);
    }

    plan.collectable_scopes
        .sort_by(|left, right| left.scope_hash.cmp(&right.scope_hash));
    plan.retained_immature_scopes
        .sort_by(|left, right| left.scope_hash.cmp(&right.scope_hash));
    plan.refused_scopes
        .sort_by(|left, right| left.scope.scope_hash.cmp(&right.scope.scope_hash));
    Ok(plan)
}

/// Collect the one stranded scope whose exact semantic binding-cleanup intent
/// was durably recorded, under the journal → quarantine → durable receipt →
/// unlink ordering generation retention uses.
pub fn execute_scope_root_retention(
    store_root: &Path,
    plan: ScopeRootRetentionPlanV1,
    revalidated_liveness_proof: &ScopeRootLivenessProofV1,
    mode: CodeGenerationRetentionModeV1,
    now_secs: i64,
    completed_at: UtcMicros,
) -> Result<ScopeRootRetentionReportV1, CodeGenerationRetentionErrorV1> {
    if mode == CodeGenerationRetentionModeV1::DryRun || plan.collectable_scopes.is_empty() {
        return Ok(ScopeRootRetentionReportV1 {
            plan,
            collected_scopes: Vec::new(),
            receipt: None,
        });
    }

    validate_scope_root_liveness_proof(revalidated_liveness_proof)?;
    let planned_liveness_proof = plan.liveness_proof.as_ref().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope Apply requires a canonical proof-bound plan".to_owned(),
        )
    })?;
    if planned_liveness_proof != revalidated_liveness_proof {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope liveness authority changed before quarantine".to_owned(),
        ));
    }
    let candidate = plan.collectable_scopes.first().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup requires a singleton collection plan".to_owned(),
        )
    })?;
    let expected_binding_cleanup_intent = ScopeRootBindingCleanupIntentV1 {
        schema: SCOPE_BINDING_CLEANUP_INTENT_SCHEMA.to_owned(),
        scope_hash: candidate.scope_hash.clone(),
        source_scope: revalidated_liveness_proof
            .candidate_binding
            .source_scope
            .clone(),
        liveness_proof: revalidated_liveness_proof.clone(),
        receipt: binding_cleanup_receipt(&plan, &candidate.scope_hash, completed_at)?,
    };
    validate_scope_binding_cleanup_intent(&expected_binding_cleanup_intent)?;

    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    recover_pending_scope_transaction_unlocked(store_root)?;
    if plan.liveness_proof.as_ref() != Some(revalidated_liveness_proof) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope liveness authority changed at the quarantine boundary".to_owned(),
        ));
    }
    match load_scope_binding_cleanup_intent(store_root)? {
        Some(intent) if intent == expected_binding_cleanup_intent => {}
        Some(_) => {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "scope binding cleanup intent does not match the collection plan".to_owned(),
            ));
        }
        None => {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "scope collection requires a durable binding cleanup intent".to_owned(),
            ));
        }
    }

    // Re-verify every candidate under the pass lock, and hold each scope's own
    // generation-retention lock while doing so, so a concurrent generation pass
    // in that scope cannot be running.
    let mut scope_locks = Vec::with_capacity(plan.collectable_scopes.len());
    let mut collected = Vec::with_capacity(plan.collectable_scopes.len());
    for scope in &plan.collectable_scopes {
        if plan.live_scope_hashes.contains(&scope.scope_hash) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "code-index scope reconciliation planned a live scope for collection".to_owned(),
            ));
        }
        let scope_root = scope_root_path(store_root, &scope.scope_hash)?;
        if !scope_directory_exists(&scope_root)? {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "stranded scope '{}' disappeared after the reconciliation mark phase",
                scope.scope_hash
            )));
        }
        scope_locks.push(acquire_code_generation_store_lock(&scope_root)?);
        if scope_root.join(TRANSACTION_FILE).exists() {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "stranded scope '{}' has a pending generation-retention journal",
                scope.scope_hash
            )));
        }
        let (size_bytes, newest_mtime_secs) = measure_scope_tree(&scope_root)?;
        if size_bytes != scope.size_bytes || newest_mtime_secs != scope.newest_mtime_secs {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "stranded scope '{}' changed after the reconciliation mark phase",
                scope.scope_hash
            )));
        }
        if now_secs.saturating_sub(newest_mtime_secs) < plan.minimum_stranding_age_secs {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "stranded scope '{}' is younger than the minimum stranding age",
                scope.scope_hash
            )));
        }
        collected.push(scope.clone());
    }

    let receipt = expected_binding_cleanup_intent.receipt;
    let transaction = ScopeRootRetentionTransactionV1 {
        schema: SCOPE_RETENTION_TRANSACTION_SCHEMA.to_owned(),
        receipt: receipt.clone(),
    };
    persist_scope_transaction(store_root, &transaction)?;

    let result = (|| {
        stage_stranded_scopes(store_root, &transaction)?;
        write_scope_receipt(store_root, &receipt)?;
        cleanup_committed_scope_transaction(store_root, &transaction)?;
        clear_scope_transaction(store_root)
    })();
    if let Err(error) = result {
        if !scope_receipt_is_durable(store_root, &receipt)? {
            rollback_staged_scopes(store_root, &transaction)?;
            clear_scope_transaction(store_root)?;
        }
        return Err(error);
    }

    drop(scope_locks);
    Ok(ScopeRootRetentionReportV1 {
        plan,
        collected_scopes: collected,
        receipt: Some(receipt),
    })
}

/// Finish or undo an interrupted scope-reconciliation transaction.
pub fn recover_scope_root_retention(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if !store_root.is_dir() {
        return Ok(());
    }
    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    recover_pending_scope_transaction_unlocked(store_root)
}

/// Persist the relational cleanup that must follow one exact scope-root
/// collection before the scope can be physically quarantined.
///
/// The receipt is derived from the same singleton plan and timestamp that
/// `execute_scope_root_retention` will use. That makes a later replay depend
/// on the durable filesystem decision, rather than on a newly derived plan.
pub fn prepare_scope_root_binding_cleanup(
    store_root: &Path,
    plan: &ScopeRootRetentionPlanV1,
    scope_hash: &str,
    source_scope: &tracedecay_store::StoreShardIdV1,
    revalidated_liveness_proof: &ScopeRootLivenessProofV1,
    completed_at: UtcMicros,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    validate_scope_root_liveness_proof(revalidated_liveness_proof)?;
    if plan.liveness_proof.as_ref() != Some(revalidated_liveness_proof)
        || revalidated_liveness_proof.candidate_binding.scope_hash != scope_hash
        || revalidated_liveness_proof.candidate_binding.source_scope != *source_scope
        || revalidated_liveness_proof.candidate_binding.live
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope cleanup intent does not match its revalidated liveness proof".to_owned(),
        ));
    }
    let receipt = binding_cleanup_receipt(plan, scope_hash, completed_at)?;
    let intent = ScopeRootBindingCleanupIntentV1 {
        schema: SCOPE_BINDING_CLEANUP_INTENT_SCHEMA.to_owned(),
        scope_hash: scope_hash.to_owned(),
        source_scope: source_scope.clone(),
        liveness_proof: revalidated_liveness_proof.clone(),
        receipt,
    };
    validate_scope_binding_cleanup_intent(&intent)?;

    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    if scope_transaction_path(store_root).exists() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup cannot begin while filesystem recovery is pending".to_owned(),
        ));
    }
    match load_scope_binding_cleanup_intent(store_root)? {
        None => persist_scope_binding_cleanup_intent(store_root, &intent),
        Some(existing) if existing == intent => Ok(()),
        Some(_) => Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "a different scope binding cleanup intent is already pending".to_owned(),
        )),
    }
}

/// Return the exact binding whose cleanup must be replayed after filesystem
/// transaction recovery. A rolled-back filesystem transaction clears its
/// intent; every other state that cannot prove either outcome is unsafe.
pub fn recover_scope_root_binding_cleanup(
    store_root: &Path,
) -> Result<Option<ScopeRootBindingCleanupReplayV1>, CodeGenerationRetentionErrorV1> {
    if !store_root.is_dir() {
        return Ok(None);
    }
    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    if scope_transaction_path(store_root).exists() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup requires filesystem transaction recovery first".to_owned(),
        ));
    }
    let Some(intent) = load_scope_binding_cleanup_intent(store_root)? else {
        return Ok(None);
    };
    let source_exists = scope_directory_exists(&scope_root_path(store_root, &intent.scope_hash)?)?;
    if scope_receipt_is_durable(store_root, &intent.receipt)? {
        if source_exists {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "scope binding cleanup receipt is durable but its source scope remains".to_owned(),
            ));
        }
        return Ok(Some(ScopeRootBindingCleanupReplayV1 {
            scope_hash: intent.scope_hash,
            source_scope: intent.source_scope,
            liveness_proof: intent.liveness_proof,
        }));
    }
    if source_exists {
        clear_scope_binding_cleanup_intent(store_root)?;
        return Ok(None);
    }
    Err(CodeGenerationRetentionErrorV1::UnsafeState(
        "scope binding cleanup cannot prove whether its source scope was collected".to_owned(),
    ))
}

/// Clear a replayed binding-cleanup intent only after the exact receipt is
/// durable and its exact source scope is absent.
pub fn complete_scope_root_binding_cleanup(
    store_root: &Path,
    replay: &ScopeRootBindingCleanupReplayV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    if scope_transaction_path(store_root).exists() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup cannot complete while filesystem recovery is pending".to_owned(),
        ));
    }
    let intent = load_scope_binding_cleanup_intent(store_root)?.ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup completion has no pending intent".to_owned(),
        )
    })?;
    if intent.scope_hash != replay.scope_hash
        || intent.source_scope != replay.source_scope
        || intent.liveness_proof != replay.liveness_proof
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup completion does not match its pending intent".to_owned(),
        ));
    }
    if !scope_receipt_is_durable(store_root, &intent.receipt)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup completion has no durable filesystem receipt".to_owned(),
        ));
    }
    if scope_directory_exists(&scope_root_path(store_root, &replay.scope_hash)?)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup completion found its source scope present".to_owned(),
        ));
    }
    clear_scope_binding_cleanup_intent(store_root)
}

fn recover_pending_scope_transaction_unlocked(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let Some(transaction) = load_scope_transaction(store_root)? else {
        return Ok(());
    };
    if scope_receipt_is_durable(store_root, &transaction.receipt)? {
        cleanup_committed_scope_transaction(store_root, &transaction)?;
    } else {
        rollback_staged_scopes(store_root, &transaction)?;
    }
    clear_scope_transaction(store_root)
}

fn scope_transaction_path(store_root: &Path) -> PathBuf {
    store_root.join(SCOPE_RETENTION_TRANSACTION_FILE)
}

fn scope_binding_cleanup_intent_path(store_root: &Path) -> PathBuf {
    store_root.join(SCOPE_BINDING_CLEANUP_INTENT_FILE)
}

fn scope_stage_root(store_root: &Path, receipt: &ScopeRootRetentionReceiptV1) -> PathBuf {
    store_root
        .join(SCOPE_RETENTION_QUARANTINE_DIRECTORY)
        .join(&receipt.receipt_digest)
}

fn scope_receipt_path(store_root: &Path, receipt: &ScopeRootRetentionReceiptV1) -> PathBuf {
    store_root
        .join(SCOPE_RETENTION_RECEIPTS_DIRECTORY)
        .join(format!("receipt-{}.json", receipt.receipt_digest))
}

/// Join a scope hash onto the store root, refusing anything that is not a bare
/// 64-character hex component. Every destructive path in this section is built
/// through here, so no journal value can escape the store root.
fn scope_root_path(
    store_root: &Path,
    scope_hash: &str,
) -> Result<PathBuf, CodeGenerationRetentionErrorV1> {
    if !is_code_index_scope_hash(scope_hash) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope name is not a SHA-256 path component".to_owned(),
        ));
    }
    Ok(store_root.join(scope_hash))
}

fn is_code_index_scope_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Payload bytes and newest mtime for one scope tree.
///
/// Retention lock files and the scope root's own directory mtime are excluded
/// deliberately: acquiring the scope lock creates that file and stamps that
/// directory, so including them would make the execution-time "nothing changed
/// since the mark phase" fence unsatisfiable. Symlinks are refused outright —
/// nothing in a code-index scope creates them, and a tree that is about to be
/// renamed and unlinked is the wrong place to start interpreting them.
fn measure_scope_tree(scope_root: &Path) -> Result<(u64, i64), CodeGenerationRetentionErrorV1> {
    let mut total_bytes = 0_u64;
    let mut newest_mtime = i64::MIN;
    let mut pending = vec![scope_root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        if directory != scope_root {
            newest_mtime = newest_mtime.max(directory_mtime_secs(&directory)?);
        }
        for entry in std::fs::read_dir(&directory).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            let file_type = entry.file_type().map_err(storage)?;
            if file_type.is_symlink() {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "code-index scope '{}' contains a symlink",
                    scope_root.display()
                )));
            }
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "code-index scope '{}' contains a non-regular file",
                    scope_root.display()
                )));
            }
            if entry.file_name().to_str() == Some(STORE_LOCK_FILE) {
                continue;
            }
            let metadata = entry.metadata().map_err(storage)?;
            total_bytes = total_bytes.saturating_add(metadata.len());
            newest_mtime = newest_mtime.max(mtime_secs(&metadata)?);
        }
    }
    Ok((
        total_bytes,
        if newest_mtime == i64::MIN {
            0
        } else {
            newest_mtime
        },
    ))
}

fn directory_mtime_secs(path: &Path) -> Result<i64, CodeGenerationRetentionErrorV1> {
    mtime_secs(&std::fs::symlink_metadata(path).map_err(storage)?)
}

fn mtime_secs(metadata: &std::fs::Metadata) -> Result<i64, CodeGenerationRetentionErrorV1> {
    let modified = metadata.modified().map_err(storage)?;
    let seconds = match modified.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    };
    Ok(seconds)
}

fn total_scope_bytes(scopes: &[StrandedCodeIndexScopeV1]) -> u64 {
    scopes
        .iter()
        .fold(0_u64, |total, scope| total.saturating_add(scope.size_bytes))
}

fn build_scope_receipt(
    plan: &ScopeRootRetentionPlanV1,
    collected_scopes: Vec<StrandedCodeIndexScopeV1>,
    completed_at: UtcMicros,
) -> Result<ScopeRootRetentionReceiptV1, CodeGenerationRetentionErrorV1> {
    let liveness_proof = plan.liveness_proof.clone().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope receipt requires a canonical liveness proof".to_owned(),
        )
    })?;
    validate_scope_root_liveness_proof(&liveness_proof)?;
    let reclaimed_bytes = total_scope_bytes(&collected_scopes);
    let mut receipt = ScopeRootRetentionReceiptV1 {
        schema: SCOPE_RETENTION_RECEIPT_SCHEMA.to_owned(),
        receipt_digest: String::new(),
        live_scope_hashes: plan.live_scope_hashes.clone(),
        liveness_proof,
        minimum_stranding_age_secs: plan.minimum_stranding_age_secs,
        collected_scopes,
        reclaimed_bytes,
        completed_at_micros: completed_at.0,
    };
    receipt.receipt_digest = scope_receipt_digest(&receipt)?;
    Ok(receipt)
}

fn scope_receipt_digest(
    receipt: &ScopeRootRetentionReceiptV1,
) -> Result<String, CodeGenerationRetentionErrorV1> {
    let material = ScopeReceiptMaterial {
        schema: SCOPE_RETENTION_RECEIPT_SCHEMA,
        live_scope_hashes: &receipt.live_scope_hashes,
        liveness_proof: &receipt.liveness_proof,
        minimum_stranding_age_secs: receipt.minimum_stranding_age_secs,
        collected_scopes: &receipt.collected_scopes,
        reclaimed_bytes: receipt.reclaimed_bytes,
        completed_at_micros: receipt.completed_at_micros,
    };
    let digest = canonical_sha256(&material)
        .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
    let Some(receipt_digest) = digest.as_str().strip_prefix("sha256:") else {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt digest lacks its SHA-256 prefix".to_owned(),
        ));
    };
    Ok(receipt_digest.to_owned())
}

fn binding_cleanup_receipt(
    plan: &ScopeRootRetentionPlanV1,
    scope_hash: &str,
    completed_at: UtcMicros,
) -> Result<ScopeRootRetentionReceiptV1, CodeGenerationRetentionErrorV1> {
    if plan.collectable_scopes.len() != 1 {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup requires a singleton collection plan".to_owned(),
        ));
    }
    let candidate = plan.collectable_scopes.first().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup singleton plan has no candidate".to_owned(),
        )
    })?;
    if candidate.scope_hash != scope_hash {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup candidate does not match its collection plan".to_owned(),
        ));
    }
    build_scope_receipt(plan, vec![candidate.clone()], completed_at)
}

fn validate_scope_receipt(
    receipt: &ScopeRootRetentionReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if receipt.schema != SCOPE_RETENTION_RECEIPT_SCHEMA {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt has an incompatible schema".to_owned(),
        ));
    }
    if receipt.receipt_digest.len() != 64
        || !receipt
            .receipt_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt digest is not a SHA-256 file component".to_owned(),
        ));
    }
    if receipt.receipt_digest != scope_receipt_digest(receipt)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt digest does not match its contents".to_owned(),
        ));
    }
    if receipt.live_scope_hashes.is_empty() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation transaction records an empty live-root set".to_owned(),
        ));
    }
    validate_scope_root_liveness_proof(&receipt.liveness_proof)?;
    if receipt.live_scope_hashes != receipt.liveness_proof.live_scope_hashes {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt liveness set does not match its proof".to_owned(),
        ));
    }
    let mut seen = BTreeSet::new();
    for scope in &receipt.collected_scopes {
        if !is_code_index_scope_hash(&scope.scope_hash) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "scope reconciliation transaction names a non-scope directory".to_owned(),
            ));
        }
        if !seen.insert(scope.scope_hash.clone()) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "scope reconciliation transaction has duplicate scopes".to_owned(),
            ));
        }
    }
    if seen.is_empty()
        || !seen.is_disjoint(&receipt.live_scope_hashes)
        || receipt.reclaimed_bytes != total_scope_bytes(&receipt.collected_scopes)
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation transaction violates liveness or byte invariants".to_owned(),
        ));
    }
    Ok(())
}

fn validate_scope_transaction(
    transaction: &ScopeRootRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if transaction.schema != SCOPE_RETENTION_TRANSACTION_SCHEMA {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation transaction has an incompatible schema".to_owned(),
        ));
    }
    validate_scope_receipt(&transaction.receipt)
}

fn scope_root_liveness_proof_digest(
    proof: &ScopeRootLivenessProofV1,
) -> Result<String, CodeGenerationRetentionErrorV1> {
    canonical_sha256(&ScopeRootLivenessProofMaterialV1 {
        schema: SCOPE_ROOT_LIVENESS_PROOF_SCHEMA,
        live_scope_hashes: &proof.live_scope_hashes,
        registered_roots: &proof.registered_roots,
        git_worktrees: &proof.git_worktrees,
        mounted_leases: &proof.mounted_leases,
        configuration_roots: &proof.configuration_roots,
        vector_census: &proof.vector_census,
        vector_dependencies: &proof.vector_dependencies,
        candidate_binding: &proof.candidate_binding,
    })
    .map(|digest| digest.as_str().to_owned())
    .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))
}

fn validate_scope_root_authority_receipt(
    name: &str,
    receipt: &ScopeRootAuthorityReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if receipt.revision.is_empty() || ManifestDigest::new(receipt.digest.clone()).is_err() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{name} liveness authority receipt has an invalid revision or digest"
        )));
    }
    Ok(())
}

fn validate_scope_root_liveness_proof(
    proof: &ScopeRootLivenessProofV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if proof.schema != SCOPE_ROOT_LIVENESS_PROOF_SCHEMA
        || proof.live_scope_hashes.is_empty()
        || proof
            .live_scope_hashes
            .iter()
            .any(|scope_hash| !is_code_index_scope_hash(scope_hash))
        || !is_code_index_scope_hash(&proof.candidate_binding.scope_hash)
        || proof.candidate_binding.vector_census_revision != proof.vector_census.revision
        || (!proof.candidate_binding.live
            && proof
                .live_scope_hashes
                .contains(&proof.candidate_binding.scope_hash))
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope liveness proof violates its structural authority contract".to_owned(),
        ));
    }
    for (name, receipt) in [
        ("registered-root", &proof.registered_roots),
        ("git-worktree", &proof.git_worktrees),
        ("mounted-lease", &proof.mounted_leases),
        ("configuration-root", &proof.configuration_roots),
        ("vector-census", &proof.vector_census),
        ("vector-dependency", &proof.vector_dependencies),
    ] {
        validate_scope_root_authority_receipt(name, receipt)?;
    }
    if proof.registered_roots.terminal_count == 0
        || proof.git_worktrees.terminal_count == 0
        || ManifestDigest::new(proof.proof_digest.clone()).is_err()
        || proof.proof_digest != scope_root_liveness_proof_digest(proof)?
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope liveness proof is incomplete or its digest does not match".to_owned(),
        ));
    }
    Ok(())
}

fn validate_scope_binding_cleanup_intent(
    intent: &ScopeRootBindingCleanupIntentV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if intent.schema != SCOPE_BINDING_CLEANUP_INTENT_SCHEMA {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup intent has an incompatible schema".to_owned(),
        ));
    }
    if !is_code_index_scope_hash(&intent.scope_hash) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup intent names a non-scope directory".to_owned(),
        ));
    }
    validate_scope_root_liveness_proof(&intent.liveness_proof)?;
    validate_scope_receipt(&intent.receipt)?;
    if intent.receipt.collected_scopes.len() != 1
        || intent.receipt.collected_scopes[0].scope_hash != intent.scope_hash
        || intent.liveness_proof.candidate_binding.scope_hash != intent.scope_hash
        || intent.liveness_proof.candidate_binding.source_scope != intent.source_scope
        || intent.liveness_proof.candidate_binding.live
        || intent.receipt.liveness_proof != intent.liveness_proof
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup intent does not bind exactly one matching scope".to_owned(),
        ));
    }
    Ok(())
}

fn persist_scope_transaction(
    store_root: &Path,
    transaction: &ScopeRootRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    validate_scope_transaction(transaction)?;
    let bytes = serde_json::to_vec(transaction).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "scope reconciliation transaction serialization failed: {error}"
        ))
    })?;
    atomic_write(
        &scope_transaction_path(store_root),
        "code-index-scope-retention-transaction",
        &bytes,
        DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(storage)
}

fn load_scope_transaction(
    store_root: &Path,
) -> Result<Option<ScopeRootRetentionTransactionV1>, CodeGenerationRetentionErrorV1> {
    let path = scope_transaction_path(store_root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage(error)),
    };
    if bytes.len() as u64 > MAX_SCOPE_TRANSACTION_BYTES {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "scope reconciliation transaction '{}' exceeds the bounded journal size",
            path.display()
        )));
    }
    let transaction = serde_json::from_slice(&bytes).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "scope reconciliation transaction '{}' is unreadable: {error}",
            path.display()
        ))
    })?;
    validate_scope_transaction(&transaction)?;
    Ok(Some(transaction))
}

fn clear_scope_transaction(store_root: &Path) -> Result<(), CodeGenerationRetentionErrorV1> {
    match std::fs::remove_file(scope_transaction_path(store_root)) {
        Ok(()) => sync_directory(store_root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage(error)),
    }
}

fn persist_scope_binding_cleanup_intent(
    store_root: &Path,
    intent: &ScopeRootBindingCleanupIntentV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    validate_scope_binding_cleanup_intent(intent)?;
    let bytes = serde_json::to_vec(intent).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "scope binding cleanup intent serialization failed: {error}"
        ))
    })?;
    atomic_write(
        &scope_binding_cleanup_intent_path(store_root),
        "code-index-scope-binding-cleanup-intent",
        &bytes,
        DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(storage)
}

fn load_scope_binding_cleanup_intent(
    store_root: &Path,
) -> Result<Option<ScopeRootBindingCleanupIntentV1>, CodeGenerationRetentionErrorV1> {
    let path = scope_binding_cleanup_intent_path(store_root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage(error)),
    };
    if bytes.len() as u64 > MAX_SCOPE_BINDING_CLEANUP_INTENT_BYTES {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "scope binding cleanup intent '{}' exceeds the bounded journal size",
            path.display()
        )));
    }
    let intent = serde_json::from_slice(&bytes).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "scope binding cleanup intent '{}' is unreadable: {error}",
            path.display()
        ))
    })?;
    validate_scope_binding_cleanup_intent(&intent)?;
    Ok(Some(intent))
}

fn clear_scope_binding_cleanup_intent(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    match std::fs::remove_file(scope_binding_cleanup_intent_path(store_root)) {
        Ok(()) => sync_directory(store_root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage(error)),
    }
}

fn scope_receipt_bytes(
    receipt: &ScopeRootRetentionReceiptV1,
) -> Result<Vec<u8>, CodeGenerationRetentionErrorV1> {
    serde_json::to_vec(receipt).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "scope reconciliation receipt serialization failed: {error}"
        ))
    })
}

fn scope_receipt_is_durable(
    store_root: &Path,
    receipt: &ScopeRootRetentionReceiptV1,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    let path = scope_receipt_path(store_root, receipt);
    if !regular_file_exists(&path)? {
        return Ok(false);
    }
    if std::fs::read(&path).map_err(storage)? != scope_receipt_bytes(receipt)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt digest collides with different bytes".to_owned(),
        ));
    }
    Ok(true)
}

fn write_scope_receipt(
    store_root: &Path,
    receipt: &ScopeRootRetentionReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let receipts_root = store_root.join(SCOPE_RETENTION_RECEIPTS_DIRECTORY);
    std::fs::create_dir_all(&receipts_root).map_err(storage)?;
    let final_path = receipts_root.join(format!("receipt-{}.json", receipt.receipt_digest));
    let bytes = scope_receipt_bytes(receipt)?;
    if final_path.exists() {
        if std::fs::read(&final_path).map_err(storage)? == bytes {
            return Ok(());
        }
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt digest collides with different bytes".to_owned(),
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

fn stage_stranded_scopes(
    store_root: &Path,
    transaction: &ScopeRootRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let stage_root = scope_stage_root(store_root, &transaction.receipt);
    std::fs::create_dir_all(&stage_root).map_err(storage)?;
    sync_directory(stage_root.parent().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation quarantine has no parent".to_owned(),
        )
    })?)?;

    for scope in &transaction.receipt.collected_scopes {
        let source = scope_root_path(store_root, &scope.scope_hash)?;
        let staged = scope_root_path(&stage_root, &scope.scope_hash)?;
        match (
            scope_directory_exists(&source)?,
            scope_directory_exists(&staged)?,
        ) {
            (true, false) => {
                std::fs::rename(&source, &staged).map_err(storage)?;
                sync_directory(store_root)?;
                sync_directory(&stage_root)?;
            }
            (false, false) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "stranded scope '{}' is missing before quarantine",
                    scope.scope_hash
                )));
            }
            (false, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "stranded scope '{}' was already quarantined",
                    scope.scope_hash
                )));
            }
            (true, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "stranded scope '{}' exists in both source and quarantine",
                    scope.scope_hash
                )));
            }
        }
    }
    Ok(())
}

fn rollback_staged_scopes(
    store_root: &Path,
    transaction: &ScopeRootRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let stage_root = scope_stage_root(store_root, &transaction.receipt);
    for scope in &transaction.receipt.collected_scopes {
        let source = scope_root_path(store_root, &scope.scope_hash)?;
        let staged = scope_root_path(&stage_root, &scope.scope_hash)?;
        match (
            scope_directory_exists(&source)?,
            scope_directory_exists(&staged)?,
        ) {
            (true, false) => {}
            (false, true) => {
                std::fs::rename(&staged, &source).map_err(storage)?;
                sync_directory(store_root)?;
                sync_directory(&stage_root)?;
            }
            (false, false) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "scope reconciliation rollback cannot find '{}'",
                    scope.scope_hash
                )));
            }
            (true, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "scope reconciliation rollback found duplicate '{}'",
                    scope.scope_hash
                )));
            }
        }
    }
    remove_empty_scope_stage_root(&stage_root)
}

fn cleanup_committed_scope_transaction(
    store_root: &Path,
    transaction: &ScopeRootRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let stage_root = scope_stage_root(store_root, &transaction.receipt);
    for scope in &transaction.receipt.collected_scopes {
        let source = scope_root_path(store_root, &scope.scope_hash)?;
        if scope_directory_exists(&source)? {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "scope reconciliation receipt is durable but '{}' returned to the store root",
                scope.scope_hash
            )));
        }
        let staged = scope_root_path(&stage_root, &scope.scope_hash)?;
        if scope_directory_exists(&staged)? {
            // The only recursive removal in this module. Its path is
            // `<store_root>/<quarantine>/<receipt digest>/<scope hash>`, every
            // component of which is a validated hex string from the durable
            // journal, and the tree only reaches that path by a rename this
            // transaction performed.
            std::fs::remove_dir_all(&staged).map_err(storage)?;
            sync_directory(&stage_root)?;
        }
    }
    remove_empty_scope_stage_root(&stage_root)
}

fn remove_empty_scope_stage_root(stage_root: &Path) -> Result<(), CodeGenerationRetentionErrorV1> {
    let mut entries = match std::fs::read_dir(stage_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage(error)),
    };
    if entries.next().is_some() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "scope reconciliation quarantine '{}' contains unexpected entries",
            stage_root.display()
        )));
    }
    std::fs::remove_dir(stage_root).map_err(storage)?;
    sync_directory(stage_root.parent().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation quarantine has no parent".to_owned(),
        )
    })?)
}

fn scope_directory_exists(path: &Path) -> Result<bool, CodeGenerationRetentionErrorV1> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(true),
        Ok(_) => Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "code-index scope path '{}' is not a directory",
            path.display()
        ))),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(storage(error)),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    mod graph_replay_release_tests;

    const TEST_ROLLBACK_FLOOR: usize = 3;

    fn indexed_generation(
        sequence: usize,
        sealed_at_micros: i64,
        size_bytes: u64,
        exact: bool,
    ) -> DurableGenerationIndexEntryV1 {
        DurableGenerationIndexEntryV1 {
            generation_id: format!("generation.v1.retention.{sequence:08}"),
            snapshot_content_identity: format!("sha256:{sequence:064x}"),
            sealed_at_micros,
            size_bytes,
            generation_file: format!("generation-{sequence:064x}.json"),
            state_digest: format!("sha256:{sequence:064x}"),
            source_reference: exact.then(|| format!("refs/heads/branch-{sequence}")),
            source_revision: exact.then(|| format!("{sequence:040x}")),
            source_tree: exact.then(|| format!("{:040x}", sequence + 1)),
        }
    }

    #[test]
    fn durable_index_bounds_clean_and_dirty_history_by_ttl_bytes_and_count() {
        let now = MAX_DURABLE_GENERATION_INDEX_TTL_MICROS_V1 * 2;
        let active = indexed_generation(99, now, 32, true);
        let mut entries = vec![
            indexed_generation(
                0,
                now - MAX_DURABLE_GENERATION_INDEX_TTL_MICROS_V1 - 1,
                1,
                false,
            ),
            indexed_generation(1, now - 3, MAX_DURABLE_GENERATION_INDEX_BYTES_V1, true),
            indexed_generation(2, now - 2, 32, false),
            active.clone(),
        ];
        entries.extend(
            (3..=MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1 + 2)
                .map(|sequence| indexed_generation(sequence, now - 1, 1, sequence % 2 == 0)),
        );

        let removed = retain_bounded_generation_index(&mut entries, &active.generation_id);

        assert!(
            removed >= 3,
            "TTL, byte, and count pressure must evict history"
        );
        assert!(entries.iter().any(|entry| entry == &active));
        assert!(entries.len() <= MAX_DURABLE_GENERATION_INDEX_ENTRIES_V1);
        assert!(
            entries.iter().map(|entry| entry.size_bytes).sum::<u64>()
                <= MAX_DURABLE_GENERATION_INDEX_BYTES_V1
        );
        assert!(
            entries.iter().all(|entry| {
                entry.generation_id == active.generation_id
                    || entry.sealed_at_micros >= now - MAX_DURABLE_GENERATION_INDEX_TTL_MICROS_V1
            }),
            "dirty generations are not exempt from the TTL"
        );
    }

    #[derive(Clone)]
    struct FixtureGeneration {
        id: CodeGenerationId,
        file: String,
        state_digest: String,
        size_bytes: u64,
    }

    fn fixture_store(count: usize) -> (tempfile::TempDir, Vec<FixtureGeneration>) {
        let store = tempfile::TempDir::new().expect("create generation store");
        let generations_root = store.path().join(GENERATIONS_DIRECTORY);
        std::fs::create_dir_all(&generations_root).expect("create generation directory");
        let mut generations = Vec::with_capacity(count);

        for sequence in 0..count {
            let generation_id =
                CodeGenerationId::new(format!("generation.v1.fixture.{sequence:08}"))
                    .expect("valid generation id");
            let sealed_at = i64::try_from(sequence).expect("fixture sequence fits i64");
            let bytes = serde_json::to_vec(&serde_json::json!({
                "format_revision": SEALED_GENERATION_FORMAT_REVISION_V1,
                "manifest": {
                    "generation_id": generation_id.as_str(),
                    "seal": { "sealed_at": sealed_at },
                },
                "chunks": [],
            }))
            .expect("serialize generation fixture");
            let state_digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
            let file = format!(
                "generation-{}.json",
                state_digest.strip_prefix("sha256:").expect("digest prefix")
            );
            let size_bytes = u64::try_from(bytes.len()).expect("fixture size fits u64");
            std::fs::write(generations_root.join(&file), bytes).expect("write generation fixture");
            generations.push(FixtureGeneration {
                id: generation_id,
                file,
                state_digest,
                size_bytes,
            });
        }

        let active = generations.last().expect("at least one generation");
        let active_entry = DurableGenerationIndexEntryV1 {
            generation_id: active.id.as_str().to_owned(),
            snapshot_content_identity: "snapshot.fixture".to_owned(),
            sealed_at_micros: i64::try_from(count - 1).expect("fixture sequence fits i64"),
            size_bytes: active.size_bytes,
            generation_file: active.file.clone(),
            state_digest: active.state_digest.clone(),
            source_reference: None,
            source_revision: None,
            source_tree: None,
        };
        let generation_index = vec![active_entry];
        let generation_index_digest =
            durable_generation_index_digest(&generation_index, true).expect("index digest");
        let pointer = DurablePublicationPointerV1 {
            generation_id: active.id.as_str().to_owned(),
            snapshot_content_identity: "snapshot.fixture".to_owned(),
            publication_digest: "sha256:publication".to_owned(),
            sealed_at_micros: i64::try_from(count - 1).expect("fixture sequence fits i64"),
            generation_file: active.file.clone(),
            state_digest: active.state_digest.clone(),
            generation_index,
            generation_index_truncated: true,
            generation_index_digest: Some(generation_index_digest),
        };
        std::fs::write(
            store.path().join(ACTIVE_POINTER_FILE),
            serde_json::to_vec(&pointer).expect("serialize active pointer"),
        )
        .expect("write active pointer");

        (store, generations)
    }

    fn pad_generation_file(
        store: &tempfile::TempDir,
        generation: &mut FixtureGeneration,
        padding_bytes: usize,
        active: bool,
    ) {
        let generations_root = store.path().join(GENERATIONS_DIRECTORY);
        let old_path = generations_root.join(&generation.file);
        let mut bytes = std::fs::read(&old_path).expect("read generation fixture");
        bytes.extend(std::iter::repeat_n(b' ', padding_bytes));
        let state_digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
        let file = format!(
            "generation-{}.json",
            state_digest.strip_prefix("sha256:").expect("digest prefix")
        );
        std::fs::write(generations_root.join(&file), bytes).expect("write padded generation");
        std::fs::remove_file(old_path).expect("remove unpadded generation");
        generation.file = file.clone();
        generation.state_digest = state_digest.clone();
        if active {
            let pointer_path = store.path().join(ACTIVE_POINTER_FILE);
            let mut pointer: DurablePublicationPointerV1 =
                serde_json::from_slice(&std::fs::read(&pointer_path).expect("read active pointer"))
                    .expect("decode active pointer");
            pointer.generation_file = file;
            pointer.state_digest = state_digest;
            std::fs::write(
                pointer_path,
                serde_json::to_vec(&pointer).expect("serialize active pointer"),
            )
            .expect("write active pointer");
        }
    }

    #[test]
    fn next_retention_plan_limits_collection_to_one_generation() {
        let (store, _generations) = fixture_store(8);

        let plan = plan_next_code_generation_retention_cancellable(
            store.path(),
            &BTreeSet::new(),
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
            &|| false,
        )
        .expect("plan one retention unit");

        assert_eq!(plan.collectable_generations.len(), 1);
        assert_eq!(plan.superseded_generations.len(), 7);
    }

    #[test]
    fn cancellable_maintenance_preparation_stops_during_generation_verification() {
        let (store, mut generations) = fixture_store(1);
        pad_generation_file(&store, &mut generations[0], 3 * 1024 * 1024, true);
        let checks = std::sync::atomic::AtomicUsize::new(0);

        let error = prepare_next_code_generation_retention_cancellable(
            store.path(),
            &BTreeSet::new(),
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
            &|| checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2,
        )
        .expect_err("cancellation must interrupt full-file verification");

        assert!(matches!(error, CodeGenerationRetentionErrorV1::Cancelled));
        assert!(checks.load(std::sync::atomic::Ordering::SeqCst) >= 3);
        assert!(!transaction_path(store.path()).exists());
        assert!(!store.path().join(RECEIPTS_DIRECTORY).exists());
    }

    #[test]
    fn executing_a_prevalidated_unit_collects_only_that_generation() {
        let (store, _generations) = fixture_store(8);
        let plan = plan_next_code_generation_retention_cancellable(
            store.path(),
            &BTreeSet::new(),
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
            &|| false,
        )
        .expect("plan one retention unit");

        let report = execute_code_generation_retention(
            store.path(),
            plan,
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(99),
        )
        .expect("execute one retention unit");

        assert_eq!(report.deleted_generations.len(), 1);
        assert_eq!(
            std::fs::read_dir(store.path().join(GENERATIONS_DIRECTORY))
                .expect("generation directory")
                .count(),
            7
        );
    }

    #[test]
    fn apply_preserves_collectable_generations_when_receipt_commit_fails() {
        let (store, _generations) = fixture_store(5);
        let plan =
            plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
                .expect("plan retention");
        assert_eq!(plan.collectable_generations.len(), 1);
        let collectable = plan.collectable_generations[0].clone();
        let active_file = plan.active_generation_file().to_owned();
        let rollback_files = plan
            .superseded_generations
            .iter()
            .take(plan.rollback_floor)
            .map(|generation| generation.generation_file.clone())
            .collect::<Vec<_>>();
        let generations_root = store.path().join(GENERATIONS_DIRECTORY);

        std::fs::write(store.path().join(RECEIPTS_DIRECTORY), b"not a directory")
            .expect("block receipt directory");
        let error = execute_code_generation_retention(
            store.path(),
            plan,
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(100),
        )
        .expect_err("receipt commit must fail");

        assert!(matches!(error, CodeGenerationRetentionErrorV1::Storage(_)));
        assert!(
            generations_root
                .join(&collectable.generation_file)
                .is_file(),
            "a failed receipt commit must not unlink collectable evidence"
        );
        assert!(
            generations_root.join(active_file).is_file(),
            "retention must preserve the active generation"
        );
        for rollback_file in rollback_files {
            assert!(
                generations_root.join(rollback_file).is_file(),
                "retention must preserve the rollback floor"
            );
        }
    }

    #[test]
    fn recovery_restores_quarantined_generations_without_a_durable_receipt() {
        let (store, _generations) = fixture_store(5);
        let vector_readable_sources = BTreeSet::new();
        let plan = plan_code_generation_retention(
            store.path(),
            &vector_readable_sources,
            TEST_ROLLBACK_FLOOR,
        )
        .expect("plan retention");
        let collectable = plan.collectable_generations[0].clone();
        let receipt = build_receipt(&plan, plan.collectable_generations.clone(), UtcMicros(101))
            .expect("build retention receipt");
        let transaction = CodeGenerationRetentionTransactionV1 {
            schema: TRANSACTION_SCHEMA.to_owned(),
            active_pointer: plan.active_pointer.clone(),
            receipt: receipt.clone(),
        };
        let generations_root = store.path().join(GENERATIONS_DIRECTORY);
        let staged_root = transaction_stage_root(store.path(), &receipt);

        persist_transaction(store.path(), &transaction).expect("persist transaction journal");
        stage_collectable_generations(store.path(), &transaction).expect("stage generation");
        assert!(!generations_root.join(&collectable.generation_file).exists());
        assert!(staged_root.join(&collectable.generation_file).is_file());

        recover_code_generation_retention(store.path(), &vector_readable_sources)
            .expect("recover uncommitted transaction");

        assert!(
            generations_root
                .join(&collectable.generation_file)
                .is_file()
        );
        assert!(!transaction_path(store.path()).exists());
        assert!(!staged_root.exists());
    }

    #[test]
    fn plan_keeps_active_vector_pinned_and_rollback_generations() {
        let (store, generations) = fixture_store(7);
        let vector_readable_sources = [generations[0].id.clone()].into_iter().collect();

        let plan = plan_code_generation_retention(
            store.path(),
            &vector_readable_sources,
            TEST_ROLLBACK_FLOOR,
        )
        .expect("plan retention");

        assert_eq!(plan.active_generation_id, generations[6].id);
        assert!(
            plan.collectable_generations
                .iter()
                .all(|generation| generation.generation_id != generations[0].id),
            "a vector-readable generation remains pinned even outside the rollback floor"
        );
        let collectable_ids = plan
            .collectable_generations
            .iter()
            .map(|generation| generation.generation_id.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            collectable_ids,
            [generations[1].id.clone(), generations[2].id.clone()]
                .into_iter()
                .collect()
        );
    }

    // --- Scope-root reconciliation -----------------------------------------

    const LIVE_ROOT: &str = "/repos/live-checkout";
    const STRANDED_ROOT: &str = "/repos/.claude/worktrees/agent-deleted";
    const AGED_NOW_SECS: i64 = 4_000_000_000;

    /// A `code-index-v1/` parent holding one live scope and one stranded scope,
    /// each with a payload file so the census has bytes to measure.
    fn fixture_scope_store() -> (tempfile::TempDir, String, String) {
        let store = tempfile::TempDir::new().expect("create code-index store");
        let mut hashes = Vec::new();
        for (root, payload) in [(LIVE_ROOT, "live"), (STRANDED_ROOT, "stranded")] {
            let hash = code_index_scope_hash(Path::new(root));
            let scope = store.path().join(&hash);
            std::fs::create_dir_all(scope.join(GENERATIONS_DIRECTORY))
                .expect("create scope generations directory");
            std::fs::write(
                scope.join(GENERATIONS_DIRECTORY).join("generation-fixture"),
                payload.as_bytes(),
            )
            .expect("write scope payload");
            hashes.push(hash);
        }
        let (live, stranded) = (hashes[0].clone(), hashes[1].clone());
        (store, live, stranded)
    }

    fn live_root_set() -> BTreeSet<PathBuf> {
        [PathBuf::from(LIVE_ROOT)].into_iter().collect()
    }

    fn authority_receipt(
        revision: &str,
        terminal_count: u64,
        digest_byte: char,
    ) -> ScopeRootAuthorityReceiptV1 {
        ScopeRootAuthorityReceiptV1 {
            revision: revision.to_owned(),
            terminal_count,
            digest: format!("sha256:{}", digest_byte.to_string().repeat(64)),
        }
    }

    fn fixture_scope_liveness_proof(
        live_scope_hash: String,
        candidate_scope_hash: String,
    ) -> ScopeRootLivenessProofV1 {
        let source_scope = tracedecay_store::StoreShardIdV1::project(
            tracedecay_domain::BrainId::new("brain.scope-retention").expect("fixture brain"),
            tracedecay_domain::UserProfileId::new("profile.scope-retention")
                .expect("fixture profile"),
            tracedecay_domain::ProjectId::new("project.scope-retention").expect("fixture project"),
        );
        ScopeRootLivenessProofV1::new(
            [live_scope_hash].into_iter().collect(),
            authority_receipt("registry-r1", 1, '1'),
            authority_receipt("git-r1", 1, '2'),
            authority_receipt("mount-r1", 1, '3'),
            authority_receipt("config-r1", 1, '4'),
            authority_receipt("vector-r1", 2, '5'),
            authority_receipt("dependency-r1", 1, '6'),
            ScopeRootCandidateBindingV1 {
                scope_hash: candidate_scope_hash,
                source_scope,
                vector_census_revision: "vector-r1".to_owned(),
                live: false,
            },
        )
        .expect("valid fixture liveness proof")
    }

    #[test]
    fn scope_apply_refuses_a_changed_terminal_authority_receipt() {
        let (store, live, stranded) = fixture_scope_store();
        let proof = fixture_scope_liveness_proof(live, stranded.clone());
        let plan = plan_scope_root_retention_with_liveness_proof(
            store.path(),
            proof.clone(),
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            AGED_NOW_SECS,
        )
        .expect("plan proof-bound scope reconciliation");
        let completed_at = UtcMicros(10);
        prepare_scope_root_binding_cleanup(
            store.path(),
            &plan,
            &stranded,
            &proof.candidate_binding.source_scope,
            &proof,
            completed_at,
        )
        .expect("persist exact proof-bound cleanup intent");
        let mut changed = proof;
        changed.mounted_leases.revision = "mount-r2".to_owned();
        changed
            .refresh_digest()
            .expect("refresh changed proof digest");

        let error = execute_scope_root_retention(
            store.path(),
            plan,
            &changed,
            CodeGenerationRetentionModeV1::Apply,
            AGED_NOW_SECS,
            completed_at,
        )
        .expect_err("pre-quarantine CAS must reject a changed root authority");

        assert!(matches!(
            error,
            CodeGenerationRetentionErrorV1::UnsafeState(_)
        ));
        assert!(store.path().join(stranded).is_dir());
        assert!(!scope_transaction_path(store.path()).exists());
    }

    #[test]
    fn cleanup_replay_preserves_exact_source_shard_and_liveness_proof() {
        let (store, live, stranded) = fixture_scope_store();
        let proof = fixture_scope_liveness_proof(live, stranded.clone());
        let plan = plan_scope_root_retention_with_liveness_proof(
            store.path(),
            proof.clone(),
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            AGED_NOW_SECS,
        )
        .expect("plan proof-bound scope reconciliation");
        let completed_at = UtcMicros(11);
        prepare_scope_root_binding_cleanup(
            store.path(),
            &plan,
            &stranded,
            &proof.candidate_binding.source_scope,
            &proof,
            completed_at,
        )
        .expect("persist proof-bound cleanup intent");
        execute_scope_root_retention(
            store.path(),
            plan,
            &proof,
            CodeGenerationRetentionModeV1::Apply,
            AGED_NOW_SECS,
            completed_at,
        )
        .expect("collect proof-bound stranded scope");

        let replay = recover_scope_root_binding_cleanup(store.path())
            .expect("read cleanup replay")
            .expect("pending cleanup replay");
        assert_eq!(replay.scope_hash, stranded);
        assert_eq!(replay.source_scope, proof.candidate_binding.source_scope);
        assert_eq!(replay.liveness_proof, proof);
    }

    #[test]
    fn scope_plan_refuses_an_unproven_live_root_set() {
        let (store, _live, stranded) = fixture_scope_store();

        let error = plan_scope_root_retention(
            store.path(),
            &BTreeSet::new(),
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            AGED_NOW_SECS,
        )
        .expect_err("an empty live-root set must never be interpreted");

        assert!(matches!(
            error,
            CodeGenerationRetentionErrorV1::UnsafeState(_)
        ));
        assert!(store.path().join(stranded).is_dir());
    }

    #[test]
    fn scope_recovery_restores_quarantined_scopes_without_a_durable_receipt() {
        let (store, live, stranded) = fixture_scope_store();
        let proof = fixture_scope_liveness_proof(live.clone(), stranded.clone());
        let plan = plan_scope_root_retention_with_liveness_proof(
            store.path(),
            proof,
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            AGED_NOW_SECS,
        )
        .expect("plan scope reconciliation");
        assert_eq!(plan.collectable_scopes.len(), 1);
        assert_eq!(plan.collectable_scopes[0].scope_hash, stranded);

        let receipt = build_scope_receipt(&plan, plan.collectable_scopes.clone(), UtcMicros(11))
            .expect("build reconciliation receipt");
        let transaction = ScopeRootRetentionTransactionV1 {
            schema: SCOPE_RETENTION_TRANSACTION_SCHEMA.to_owned(),
            receipt: receipt.clone(),
        };
        let staged_root = scope_stage_root(store.path(), &receipt);

        // Crash exactly between quarantine and the durable receipt.
        persist_scope_transaction(store.path(), &transaction).expect("persist journal");
        stage_stranded_scopes(store.path(), &transaction).expect("quarantine stranded scope");
        assert!(!store.path().join(&stranded).exists());
        assert!(staged_root.join(&stranded).is_dir());

        recover_scope_root_retention(store.path()).expect("recover uncommitted reconciliation");

        assert!(
            store.path().join(&stranded).is_dir(),
            "without a durable receipt the scope must come back intact"
        );
        assert!(store.path().join(&live).is_dir());
        assert!(!scope_transaction_path(store.path()).exists());
        assert!(!staged_root.exists());
    }

    #[test]
    fn scope_recovery_completes_collection_once_the_receipt_is_durable() {
        let (store, live, stranded) = fixture_scope_store();
        let proof = fixture_scope_liveness_proof(live.clone(), stranded.clone());
        let plan = plan_scope_root_retention_with_liveness_proof(
            store.path(),
            proof,
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            AGED_NOW_SECS,
        )
        .expect("plan scope reconciliation");
        let receipt = build_scope_receipt(&plan, plan.collectable_scopes.clone(), UtcMicros(12))
            .expect("build reconciliation receipt");
        let transaction = ScopeRootRetentionTransactionV1 {
            schema: SCOPE_RETENTION_TRANSACTION_SCHEMA.to_owned(),
            receipt: receipt.clone(),
        };
        let staged_root = scope_stage_root(store.path(), &receipt);

        // Crash after the receipt is durable but before the quarantine is
        // unlinked: the decision is committed, so recovery rolls forward.
        persist_scope_transaction(store.path(), &transaction).expect("persist journal");
        stage_stranded_scopes(store.path(), &transaction).expect("quarantine stranded scope");
        write_scope_receipt(store.path(), &receipt).expect("commit reconciliation receipt");

        recover_scope_root_retention(store.path()).expect("recover committed reconciliation");

        assert!(!store.path().join(&stranded).exists());
        assert!(!staged_root.exists());
        assert!(store.path().join(&live).is_dir());
        assert!(!scope_transaction_path(store.path()).exists());
        assert!(scope_receipt_path(store.path(), &receipt).is_file());
    }

    #[test]
    fn scope_apply_refuses_collection_without_exact_binding_cleanup_intent() {
        let (store, live, stranded) = fixture_scope_store();
        let proof = fixture_scope_liveness_proof(live, stranded.clone());
        let plan = plan_scope_root_retention_with_liveness_proof(
            store.path(),
            proof.clone(),
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            AGED_NOW_SECS,
        )
        .expect("plan scope reconciliation");

        let error = execute_scope_root_retention(
            store.path(),
            plan,
            &proof,
            CodeGenerationRetentionModeV1::Apply,
            AGED_NOW_SECS,
            UtcMicros(13),
        )
        .expect_err("physical collection must require a durable relational cleanup intent");

        assert!(matches!(
            error,
            CodeGenerationRetentionErrorV1::UnsafeState(_)
        ));
        assert!(store.path().join(stranded).is_dir());
        assert!(!scope_transaction_path(store.path()).exists());
    }

    #[test]
    fn scope_binding_cleanup_intent_replays_after_filesystem_collection_restart() {
        let (store, live, stranded) = fixture_scope_store();
        let proof = fixture_scope_liveness_proof(live.clone(), stranded.clone());
        let plan = plan_scope_root_retention_with_liveness_proof(
            store.path(),
            proof.clone(),
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            AGED_NOW_SECS,
        )
        .expect("plan scope reconciliation");
        let completed_at = UtcMicros(14);

        prepare_scope_root_binding_cleanup(
            store.path(),
            &plan,
            &stranded,
            &proof.candidate_binding.source_scope,
            &proof,
            completed_at,
        )
        .expect("journal relational cleanup before filesystem collection");
        let report = execute_scope_root_retention(
            store.path(),
            plan,
            &proof,
            CodeGenerationRetentionModeV1::Apply,
            AGED_NOW_SECS,
            completed_at,
        )
        .expect("complete filesystem collection");
        assert_eq!(report.collected_scopes[0].scope_hash, stranded);
        assert!(!store.path().join(&stranded).exists());
        assert!(store.path().join(&live).is_dir());

        // Simulate restart exactly after durable filesystem completion and
        // before the caller removes the semantic source-scope binding.
        recover_scope_root_retention(store.path()).expect("recover filesystem transaction");
        let replay = recover_scope_root_binding_cleanup(store.path())
            .expect("replay binding cleanup intent")
            .expect("pending replay");
        assert_eq!(replay.scope_hash, stranded);
        assert_eq!(replay.source_scope, proof.candidate_binding.source_scope);
        assert_eq!(replay.liveness_proof, proof);
        complete_scope_root_binding_cleanup(store.path(), &replay)
            .expect("complete exact binding cleanup intent");
        assert_eq!(
            recover_scope_root_binding_cleanup(store.path())
                .expect("completed cleanup stays complete"),
            None
        );
    }

    #[test]
    fn scope_transaction_never_journals_a_live_scope() {
        let (_store, live, stranded) = fixture_scope_store();
        let proof = fixture_scope_liveness_proof(live.clone(), stranded.clone());
        let mut receipt = ScopeRootRetentionReceiptV1 {
            schema: SCOPE_RETENTION_RECEIPT_SCHEMA.to_owned(),
            receipt_digest: String::new(),
            live_scope_hashes: [live.clone()].into_iter().collect(),
            liveness_proof: proof,
            minimum_stranding_age_secs: DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            collected_scopes: vec![
                StrandedCodeIndexScopeV1 {
                    scope_hash: stranded,
                    size_bytes: 8,
                    newest_mtime_secs: 1,
                },
                StrandedCodeIndexScopeV1 {
                    scope_hash: live,
                    size_bytes: 4,
                    newest_mtime_secs: 1,
                },
            ],
            reclaimed_bytes: 12,
            completed_at_micros: 13,
        };
        receipt.receipt_digest =
            scope_receipt_digest(&receipt).expect("calculate malformed receipt digest");

        let error = validate_scope_transaction(&ScopeRootRetentionTransactionV1 {
            schema: SCOPE_RETENTION_TRANSACTION_SCHEMA.to_owned(),
            receipt,
        })
        .expect_err("a live scope in the collected set must be rejected");

        assert!(matches!(
            error,
            CodeGenerationRetentionErrorV1::UnsafeState(_)
        ));
    }

    #[test]
    fn scope_reconciliation_never_treats_its_own_artifacts_as_scopes() {
        let (store, _live, _stranded) = fixture_scope_store();
        std::fs::create_dir_all(store.path().join(SCOPE_RETENTION_RECEIPTS_DIRECTORY))
            .expect("create receipts directory");
        std::fs::create_dir_all(store.path().join(SCOPE_RETENTION_QUARANTINE_DIRECTORY))
            .expect("create quarantine directory");

        let plan = plan_scope_root_retention(
            store.path(),
            &live_root_set(),
            DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
            AGED_NOW_SECS,
        )
        .expect("plan scope reconciliation");

        assert_eq!(plan.collectable_scopes.len(), 1);
        assert_eq!(
            plan.unrecognized_entry_count, 2,
            "reconciliation's own directories are never candidates"
        );
    }

    #[test]
    fn metadata_only_census_matches_full_verification() {
        let (store, _generations) = fixture_store(5);

        let full =
            plan_code_generation_retention(store.path(), &BTreeSet::new(), TEST_ROLLBACK_FLOOR)
                .expect("full census");
        let metadata_only = plan_code_generation_retention_with_verification(
            store.path(),
            &BTreeSet::new(),
            TEST_ROLLBACK_FLOOR,
            GenerationDigestVerificationV1::MetadataOnly,
        )
        .expect("metadata-only census");

        assert_eq!(
            full.superseded_generations,
            metadata_only.superseded_generations
        );
        assert_eq!(
            full.collectable_generations,
            metadata_only.collectable_generations
        );
    }

    #[test]
    fn applied_retention_refuses_a_metadata_only_plan() {
        let (store, _generations) = fixture_store(5);
        let plan = plan_code_generation_retention_with_verification(
            store.path(),
            &BTreeSet::new(),
            TEST_ROLLBACK_FLOOR,
            GenerationDigestVerificationV1::MetadataOnly,
        )
        .expect("metadata-only census");

        let error = execute_code_generation_retention(
            store.path(),
            plan,
            CodeGenerationRetentionModeV1::Apply,
            UtcMicros(14),
        )
        .expect_err("unlinking evidence requires proven content digests");

        assert!(matches!(
            error,
            CodeGenerationRetentionErrorV1::UnsafeState(_)
        ));
    }
}

//! Liveness-based retention for immutable code-index generations.
//!
//! The code-index store is derived, but a generation can still be live while
//! the active code pointer or a readable vector inventory names it. Collection
//! therefore uses conservative mark-and-sweep rather than refcounts: a missed
//! mark costs disk space, while a miscount could silently remove readable code
//! evidence. The mark set is the active generation, every vector-readable source,
//! and a small newest-superseded rollback floor.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_application::{DirectorySyncPolicy, atomic_write};
// The census gates on the exact revision the publisher writes. A second copy of
// that number here let the writer be versioned to 3 while retention still
// demanded 1: every real sealed file was refused as "incompatible" and the store
// became uncollectable.
use tracedecay_code_index::production::SEALED_GENERATION_FORMAT_REVISION_V1;
use tracedecay_domain::{CodeGenerationId, UtcMicros, canonical_sha256};

pub const DEFAULT_SUPERSEDED_GENERATION_FLOOR: usize = 3;

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
const MAX_SCOPE_TRANSACTION_BYTES: u64 = 4 * 1024 * 1024;

const MAX_GENERATION_METADATA_PREFIX_BYTES: usize = 16 * 1024 * 1024;
const MAX_TRANSACTION_BYTES: u64 = 1024 * 1024;

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
pub struct DurablePublicationPointerV1 {
    pub generation_id: String,
    pub snapshot_content_identity: String,
    pub publication_digest: String,
    pub sealed_at_micros: i64,
    pub generation_file: String,
    pub state_digest: String,
}

#[derive(Debug, Error)]
pub enum CodeGenerationRetentionErrorV1 {
    #[error("code-generation retention storage failure: {0}")]
    Storage(String),
    #[error("code-generation retention refused unsafe state: {0}")]
    UnsafeState(String),
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

pub struct CodeGenerationStoreLockV1(File);

impl Drop for CodeGenerationStoreLockV1 {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub fn acquire_code_generation_store_lock(
    store_root: &Path,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    let lock_path = store_root.join(STORE_LOCK_FILE);
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(storage)?;
    lock.lock_exclusive().map_err(storage)?;
    Ok(CodeGenerationStoreLockV1(lock))
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
    if transaction_path(store_root).exists() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-generation retention recovery is pending".to_owned(),
        ));
    }
    let active_pointer = read_active_pointer(store_root)?;
    validate_generation_file(&active_pointer.generation_file)?;
    let active_generation_id = CodeGenerationId::new(active_pointer.generation_id.clone())
        .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
    let generations_root = store_root.join(GENERATIONS_DIRECTORY);
    let entries = std::fs::read_dir(&generations_root).map_err(storage)?;
    let mut generations = BTreeMap::new();
    let mut active_state_digest = None;

    for entry in entries {
        let entry = entry.map_err(storage)?;
        let path = entry.path();
        let Some(file_name) = generation_file_name(&path) else {
            continue;
        };
        let (format_revision, manifest, raw_state_digest, size_bytes) =
            read_generation_metadata(&path, verification)?;
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
    // generation. Active and vector-readable marks are exact liveness, while
    // the newest superseded floor is the bounded rollback reserve.
    let mut marked = vector_readable_sources.clone();
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

fn read_generation_metadata(
    path: &Path,
    verification: GenerationDigestVerificationV1,
) -> Result<(u32, SealedGenerationManifestMetadataV1, String, u64), CodeGenerationRetentionErrorV1>
{
    let mut file = File::open(path).map_err(storage)?;
    let size_bytes = file.metadata().map_err(storage)?.len();
    let mut hasher = Sha256::new();
    let mut prefix = Vec::with_capacity(MAX_GENERATION_METADATA_PREFIX_BYTES);
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let bytes_read = file.read(&mut buffer).map_err(storage)?;
        if bytes_read == 0 {
            break;
        }
        if verification == GenerationDigestVerificationV1::Full {
            hasher.update(&buffer[..bytes_read]);
        }
        let remaining = MAX_GENERATION_METADATA_PREFIX_BYTES.saturating_sub(prefix.len());
        prefix.extend_from_slice(&buffer[..bytes_read.min(remaining)]);
        // A metadata-only census never needs a byte past the manifest prefix.
        // This is the entire difference between a cheap Doctor read and
        // re-hashing every gigabyte the store holds.
        if verification == GenerationDigestVerificationV1::MetadataOnly
            && prefix.len() >= MAX_GENERATION_METADATA_PREFIX_BYTES
        {
            break;
        }
    }
    let format_revision = parse_json_u32_field(&prefix, b"format_revision").ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "generation file '{}' has no readable format revision in its metadata prefix",
            path.display()
        ))
    })?;
    let manifest_bytes = extract_json_object_field(&prefix, b"manifest").ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "generation file '{}' has no complete manifest within its bounded metadata prefix",
            path.display()
        ))
    })?;
    let manifest = serde_json::from_slice(manifest_bytes).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "generation file '{}' has unreadable manifest metadata: {error}",
            path.display()
        ))
    })?;
    let state_digest = match verification {
        GenerationDigestVerificationV1::Full => {
            format!("sha256:{}", hex::encode(hasher.finalize()))
        }
        GenerationDigestVerificationV1::MetadataOnly => named_state_digest(path)?,
    };
    Ok((format_revision, manifest, state_digest, size_bytes))
}

/// The content digest a sealed generation file *claims*, read from its name.
///
/// Only valid under [`GenerationDigestVerificationV1::MetadataOnly`]: it makes
/// the file-name/digest cross-check a tautology for that file, so it proves
/// nothing about content. The active-pointer digest comparison still holds,
/// because the pointer's digest is compared against the named file.
fn named_state_digest(path: &Path) -> Result<String, CodeGenerationRetentionErrorV1> {
    let named = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("generation-"))
        .and_then(|name| name.strip_suffix(".json"))
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or_else(|| {
            CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "generation file '{}' does not name a SHA-256 content digest",
                path.display()
            ))
        })?;
    Ok(format!("sha256:{named}"))
}

fn parse_json_u32_field(prefix: &[u8], field: &[u8]) -> Option<u32> {
    let start = json_field_value_start(prefix, field)?;
    let end = prefix[start..]
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .map_or(prefix.len(), |offset| start + offset);
    std::str::from_utf8(&prefix[start..end]).ok()?.parse().ok()
}

fn extract_json_object_field<'a>(prefix: &'a [u8], field: &[u8]) -> Option<&'a [u8]> {
    let start = json_field_value_start(prefix, field)?;
    if prefix.get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in prefix[start..].iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&prefix[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn json_field_value_start(prefix: &[u8], field: &[u8]) -> Option<usize> {
    let quoted = [b"\"".as_slice(), field, b"\"".as_slice()].concat();
    let key_start = prefix
        .windows(quoted.len())
        .position(|window| window == quoted)?;
    let mut cursor = key_start + quoted.len();
    while prefix.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    if prefix.get(cursor) != Some(&b':') {
        return None;
    }
    cursor += 1;
    while prefix.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    Some(cursor)
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
    let rollback_floor = plan.rollback_floor;
    let _store_lock = acquire_code_generation_store_lock(store_root)?;
    recover_pending_transaction_unlocked(store_root, &vector_readable_sources)?;
    let plan =
        plan_code_generation_retention(store_root, &vector_readable_sources, rollback_floor)?;
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
    if mode == CodeGenerationRetentionModeV1::Apply {
        recover_code_generation_retention(store_root, vector_readable_sources)?;
    }
    let plan = plan_code_generation_retention(store_root, vector_readable_sources, rollback_floor)?;
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
    for entry in entries {
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
    #[derive(Serialize)]
    struct ReceiptMaterial<'a> {
        schema: &'static str,
        active_generation_id: &'a CodeGenerationId,
        vector_readable_sources: &'a BTreeSet<CodeGenerationId>,
        rollback_floor: usize,
        deleted_generations: &'a [CodeGenerationRetentionGenerationV1],
        reclaimed_bytes: u64,
        completed_at_micros: i64,
    }

    let reclaimed_bytes = total_bytes(&deleted_generations);
    let material = ReceiptMaterial {
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
}

impl ScopeRootRetentionPlanV1 {
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootRetentionReceiptV1 {
    pub schema: String,
    pub receipt_digest: String,
    /// The exact live set the decision was made against, so a receipt can be
    /// audited without re-deriving it.
    pub live_scope_hashes: BTreeSet<String>,
    pub minimum_stranding_age_secs: i64,
    pub collected_scopes: Vec<StrandedCodeIndexScopeV1>,
    pub reclaimed_bytes: u64,
    pub completed_at_micros: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ScopeRootRetentionTransactionV1 {
    schema: String,
    receipt: ScopeRootRetentionReceiptV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRootRetentionReportV1 {
    pub plan: ScopeRootRetentionPlanV1,
    pub collected_scopes: Vec<StrandedCodeIndexScopeV1>,
    pub receipt: Option<ScopeRootRetentionReceiptV1>,
}

/// Classify every scope directory under one `code-index-v1/` store root against
/// the caller's proven-live canonical project roots.
///
/// `live_canonical_roots` must be the *complete* live set. An empty set is
/// rejected rather than interpreted, because "I could not read the registry" and
/// "this profile has no live roots" are indistinguishable at this layer and one
/// of those readings would delete the entire store.
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

/// Collect the stranded scopes a plan named, under the same
/// journal → quarantine → durable receipt → unlink ordering generation
/// retention uses.
pub fn execute_scope_root_retention(
    store_root: &Path,
    plan: ScopeRootRetentionPlanV1,
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

    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    recover_pending_scope_transaction_unlocked(store_root)?;

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

    let receipt = build_scope_receipt(&plan, collected.clone(), completed_at)?;
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

/// Recover, plan, and execute one scope-root reconciliation pass.
pub fn run_scope_root_retention(
    store_root: &Path,
    live_canonical_roots: &BTreeSet<PathBuf>,
    minimum_stranding_age_secs: i64,
    mode: CodeGenerationRetentionModeV1,
    now_secs: i64,
    completed_at: UtcMicros,
) -> Result<ScopeRootRetentionReportV1, CodeGenerationRetentionErrorV1> {
    if mode == CodeGenerationRetentionModeV1::Apply {
        recover_scope_root_retention(store_root)?;
    }
    let plan = plan_scope_root_retention(
        store_root,
        live_canonical_roots,
        minimum_stranding_age_secs,
        now_secs,
    )?;
    execute_scope_root_retention(store_root, plan, mode, now_secs, completed_at)
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

fn acquire_scope_retention_lock(
    store_root: &Path,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    let lock_path = store_root.join(SCOPE_RETENTION_LOCK_FILE);
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(storage)?;
    lock.lock_exclusive().map_err(storage)?;
    Ok(CodeGenerationStoreLockV1(lock))
}

fn scope_transaction_path(store_root: &Path) -> PathBuf {
    store_root.join(SCOPE_RETENTION_TRANSACTION_FILE)
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
    #[derive(Serialize)]
    struct ScopeReceiptMaterial<'a> {
        schema: &'static str,
        live_scope_hashes: &'a BTreeSet<String>,
        minimum_stranding_age_secs: i64,
        collected_scopes: &'a [StrandedCodeIndexScopeV1],
        reclaimed_bytes: u64,
        completed_at_micros: i64,
    }

    let reclaimed_bytes = total_scope_bytes(&collected_scopes);
    let material = ScopeReceiptMaterial {
        schema: SCOPE_RETENTION_RECEIPT_SCHEMA,
        live_scope_hashes: &plan.live_scope_hashes,
        minimum_stranding_age_secs: plan.minimum_stranding_age_secs,
        collected_scopes: &collected_scopes,
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
    Ok(ScopeRootRetentionReceiptV1 {
        schema: SCOPE_RETENTION_RECEIPT_SCHEMA.to_owned(),
        receipt_digest,
        live_scope_hashes: plan.live_scope_hashes.clone(),
        minimum_stranding_age_secs: plan.minimum_stranding_age_secs,
        collected_scopes,
        reclaimed_bytes,
        completed_at_micros: completed_at.0,
    })
}

fn validate_scope_transaction(
    transaction: &ScopeRootRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let receipt = &transaction.receipt;
    if transaction.schema != SCOPE_RETENTION_TRANSACTION_SCHEMA
        || receipt.schema != SCOPE_RETENTION_RECEIPT_SCHEMA
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation transaction has an incompatible schema".to_owned(),
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
    if receipt.live_scope_hashes.is_empty() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation transaction records an empty live-root set".to_owned(),
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

    #[derive(Clone)]
    struct FixtureGeneration {
        id: CodeGenerationId,
        file: String,
        state_digest: String,
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
            std::fs::write(generations_root.join(&file), bytes).expect("write generation fixture");
            generations.push(FixtureGeneration {
                id: generation_id,
                file,
                state_digest,
            });
        }

        let active = generations.last().expect("at least one generation");
        let pointer = DurablePublicationPointerV1 {
            generation_id: active.id.as_str().to_owned(),
            snapshot_content_identity: "snapshot.fixture".to_owned(),
            publication_digest: "sha256:publication".to_owned(),
            sealed_at_micros: i64::try_from(count - 1).expect("fixture sequence fits i64"),
            generation_file: active.file.clone(),
            state_digest: active.state_digest.clone(),
        };
        std::fs::write(
            store.path().join(ACTIVE_POINTER_FILE),
            serde_json::to_vec(&pointer).expect("serialize active pointer"),
        )
        .expect("write active pointer");

        (store, generations)
    }

    #[test]
    fn apply_preserves_collectable_generations_when_receipt_commit_fails() {
        let (store, _generations) = fixture_store(5);
        let plan = plan_code_generation_retention(
            store.path(),
            &BTreeSet::new(),
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        )
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
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
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
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
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
        let plan = plan_scope_root_retention(
            store.path(),
            &live_root_set(),
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
        let plan = plan_scope_root_retention(
            store.path(),
            &live_root_set(),
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
    fn scope_transaction_never_journals_a_live_scope() {
        let (_store, live, stranded) = fixture_scope_store();
        let receipt = ScopeRootRetentionReceiptV1 {
            schema: SCOPE_RETENTION_RECEIPT_SCHEMA.to_owned(),
            receipt_digest: "a".repeat(64),
            live_scope_hashes: [live.clone()].into_iter().collect(),
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

        let full = plan_code_generation_retention(
            store.path(),
            &BTreeSet::new(),
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        )
        .expect("full census");
        let metadata_only = plan_code_generation_retention_with_verification(
            store.path(),
            &BTreeSet::new(),
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
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
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
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

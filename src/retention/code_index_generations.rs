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

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_application::{DirectorySyncPolicy, atomic_write};
use tracedecay_domain::{CodeGenerationId, UtcMicros, canonical_sha256};

pub const DEFAULT_SUPERSEDED_GENERATION_FLOOR: usize = 3;

const ACTIVE_POINTER_FILE: &str = "active-code-generation-v1.json";
const GENERATIONS_DIRECTORY: &str = "code-generations-v1";
const RECEIPTS_DIRECTORY: &str = "code-generation-retention-receipts-v1";
const QUARANTINE_DIRECTORY: &str = ".code-generation-retention-quarantine-v1";
const STORE_LOCK_FILE: &str = ".code-generation-retention.lock";
const RECEIPT_SCHEMA: &str = "tracedecay.code-generation-retention-receipt.v1";
const TRANSACTION_FILE: &str = ".code-generation-retention-transaction-v1.json";
const TRANSACTION_SCHEMA: &str = "tracedecay.code-generation-retention-transaction.v1";
const SEALED_GENERATION_FORMAT_REVISION_V1: u32 = 1;
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
pub(crate) struct DurablePublicationPointerV1 {
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
    store_root.join(hex::encode(Sha256::digest(
        canonical_project_root.to_string_lossy().as_bytes(),
    )))
}

pub(crate) struct CodeGenerationStoreLockV1(File);

impl Drop for CodeGenerationStoreLockV1 {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub(crate) fn acquire_code_generation_store_lock(
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

pub fn plan_code_generation_retention(
    store_root: &Path,
    vector_readable_sources: &BTreeSet<CodeGenerationId>,
    rollback_floor: usize,
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
            read_generation_metadata(&path)?;
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
        active_pointer,
    })
}

fn read_generation_metadata(
    path: &Path,
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
        hasher.update(&buffer[..bytes_read]);
        let remaining = MAX_GENERATION_METADATA_PREFIX_BYTES.saturating_sub(prefix.len());
        prefix.extend_from_slice(&buffer[..bytes_read.min(remaining)]);
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
    Ok((
        format_revision,
        manifest,
        format!("sha256:{}", hex::encode(hasher.finalize())),
        size_bytes,
    ))
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
    File::open(path)
        .and_then(|directory| directory.sync_all())
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
}

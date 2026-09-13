//! Journaled retention for derived code-text artifacts.
//!
//! The durable generation index is the only completed-artifact liveness authority; collection uses a bounded quarantine journal so recovery can roll back or finish without guessing from wall-clock age.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

use tracedecay_domain::canonical_text::is_lowercase_hex;
use tracedecay_domain::{CodeGenerationId, UtcMicros, canonical_sha256};
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, atomic_write};

use super::journal::{
    BoundedJournalSpec, clear_journal, journal_path, load_journal, persist_journal,
};
use super::receipt_store;
use super::receipt_store::{ReceiptStoreSpec, receipt_digest_file_component};
use super::{
    ACTIVE_POINTER_FILE, CodeGenerationRetentionErrorV1, CodeGenerationRetentionPlanV1,
    CodeGenerationStoreLockV1, CodeTextArtifactRetentionCandidateV1,
    CodeTextArtifactRetentionInventoryV1, CodeTextArtifactRetentionKindV1,
    CodeTextArtifactRetentionReceiptMaterialV1, CodeTextArtifactRetentionReceiptV1,
    CodeTextArtifactRetentionTransactionV1, DurableCodeTextArtifactDescriptorV1,
    DurablePublicationPointerV1, DurableSealedCodeGenerationIdentityV1,
    GenerationDigestVerificationV1, MAX_CODE_TEXT_ARTIFACT_INVENTORY_ENTRIES_V1,
    MAX_CODE_TEXT_ARTIFACT_RETENTION_BATCH_V1, MAX_DURABLE_PUBLICATION_POINTER_BYTES_V1,
    MAX_TRANSACTION_BYTES, TEXT_ARTIFACT_QUARANTINE_DIRECTORY, TEXT_ARTIFACT_RECEIPT_SCHEMA,
    TEXT_ARTIFACT_RECEIPTS_DIRECTORY, TEXT_ARTIFACT_TRANSACTION_FILE,
    TEXT_ARTIFACT_TRANSACTION_SCHEMA, code_text_artifacts_root, durable_generation_index_digest,
    generation_file_digest, observe_cancel, open_file_sha256_hex_cancellable,
    path_still_names_open_file, read_active_pointer, read_optional_active_pointer,
    regular_file_exists, remove_empty_stage_root, retain_bounded_generation_index_with_text_head,
    sha256_file_component, storage, sync_directory, validate_durable_generation_index,
    validate_sealed_generation_identity, validate_text_artifact_descriptor,
};

const TEXT_ARTIFACT_TRANSACTION_JOURNAL: BoundedJournalSpec<
    CodeTextArtifactRetentionTransactionV1,
> = BoundedJournalSpec {
    file_name: TEXT_ARTIFACT_TRANSACTION_FILE,
    max_bytes: MAX_TRANSACTION_BYTES,
    label: "text-artifact retention transaction",
    write_context: "code-text-artifact-retention-transaction",
    validate: validate_text_artifact_transaction,
};

const TEXT_ARTIFACT_RECEIPT_STORE: ReceiptStoreSpec = ReceiptStoreSpec {
    directory: TEXT_ARTIFACT_RECEIPTS_DIRECTORY,
    label: "text-artifact retention receipt",
};

/// Durably attach a verified text artifact to its sealed generation entry.
///
/// The lock is root-bound, so the caller can keep this exact guard while it
/// advances the independent text head. Returning success proves the updated
/// generation pointer and its parent directory were fsynced first.
pub fn attach_verified_text_artifact_under_lock(
    lock: &CodeGenerationStoreLockV1,
    expected_pointer: &DurablePublicationPointerV1,
    sealed_identity: &DurableSealedCodeGenerationIdentityV1,
    descriptor: DurableCodeTextArtifactDescriptorV1,
) -> Result<DurablePublicationPointerV1, CodeGenerationRetentionErrorV1> {
    let store_root = lock.generation_store_root()?;
    validate_sealed_generation_identity(sealed_identity)?;
    validate_text_artifact_descriptor(&descriptor)?;
    let mut pointer = read_active_pointer(store_root)?;
    if &pointer != expected_pointer {
        return Err(CodeGenerationRetentionErrorV1::Conflict(
            "active generation pointer changed before text-artifact attachment".to_owned(),
        ));
    }
    validate_durable_generation_index(&pointer)?;
    let attached_generation_id = descriptor.generation_id.clone();
    let entry = pointer
        .generation_index
        .iter_mut()
        .find(|entry| entry.generation_id == descriptor.generation_id.as_str())
        .ok_or_else(|| {
            CodeGenerationRetentionErrorV1::Conflict(
                "text-artifact generation is no longer retained by the durable index".to_owned(),
            )
        })?;
    if entry.generation_file != sealed_identity.locator
        || entry.state_digest != sealed_identity.digest.as_str()
        || entry.size_bytes != sealed_identity.size_bytes
    {
        return Err(CodeGenerationRetentionErrorV1::Conflict(
            "text artifact does not match the retained sealed generation".to_owned(),
        ));
    }
    match entry.text_artifact.as_ref() {
        Some(existing) if existing == &descriptor => return Ok(pointer),
        Some(_) => {
            return Err(CodeGenerationRetentionErrorV1::Conflict(
                "sealed generation already names a different text artifact".to_owned(),
            ));
        }
        None => entry.text_artifact = Some(descriptor),
    }
    let active_generation_id = pointer.generation_id.clone();
    let removed = retain_bounded_generation_index_with_text_head(
        &mut pointer.generation_index,
        &active_generation_id,
        Some(attached_generation_id.as_str()),
    );
    pointer.generation_index_truncated |= removed > 0;
    pointer.generation_index_digest = Some(durable_generation_index_digest(
        &pointer.generation_index,
        pointer.generation_index_truncated,
    )?);
    validate_durable_generation_index(&pointer)?;
    let bytes = serde_json::to_vec(&pointer).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "publication pointer serialization failed: {error}"
        ))
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_DURABLE_PUBLICATION_POINTER_BYTES_V1 {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "publication pointer exceeds its durable byte bound".to_owned(),
        ));
    }
    atomic_write(
        &store_root.join(ACTIVE_POINTER_FILE),
        "code-generation-text-artifact-attachment",
        &bytes,
        DirectorySyncPolicy::Strict,
    )
    .map_err(storage)?;
    Ok(pointer)
}

/// Withdraw one exact derived text-artifact attachment under the canonical
/// generation-store lock.
///
/// Missing or corrupt artifact bytes are recoverable because the sealed code
/// generation remains authoritative. The exact descriptor is the CAS token:
/// a caller may clear only the attachment it failed to open, never a newer
/// artifact published by a concurrent repair.
pub fn withdraw_verified_text_artifact_under_lock(
    lock: &CodeGenerationStoreLockV1,
    expected_pointer: &DurablePublicationPointerV1,
    descriptor: &DurableCodeTextArtifactDescriptorV1,
) -> Result<DurablePublicationPointerV1, CodeGenerationRetentionErrorV1> {
    let store_root = lock.generation_store_root()?;
    validate_text_artifact_descriptor(descriptor)?;
    let mut pointer = read_active_pointer(store_root)?;
    if &pointer != expected_pointer {
        return Err(CodeGenerationRetentionErrorV1::Conflict(
            "active generation pointer changed before text-artifact withdrawal".to_owned(),
        ));
    }
    validate_durable_generation_index(&pointer)?;
    let entry = pointer
        .generation_index
        .iter_mut()
        .find(|entry| entry.generation_id == descriptor.generation_id.as_str())
        .ok_or_else(|| {
            CodeGenerationRetentionErrorV1::Conflict(
                "text-artifact generation is no longer retained by the durable index".to_owned(),
            )
        })?;
    match entry.text_artifact.as_ref() {
        Some(existing) if existing == descriptor => entry.text_artifact = None,
        Some(_) => {
            return Err(CodeGenerationRetentionErrorV1::Conflict(
                "sealed generation names a newer text artifact".to_owned(),
            ));
        }
        None => return Ok(pointer),
    }
    pointer.generation_index_digest = Some(durable_generation_index_digest(
        &pointer.generation_index,
        pointer.generation_index_truncated,
    )?);
    validate_durable_generation_index(&pointer)?;
    let bytes = serde_json::to_vec(&pointer).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "publication pointer serialization failed: {error}"
        ))
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_DURABLE_PUBLICATION_POINTER_BYTES_V1 {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "publication pointer exceeds its durable byte bound".to_owned(),
        ));
    }
    atomic_write(
        &store_root.join(ACTIVE_POINTER_FILE),
        "code-generation-text-artifact-withdrawal",
        &bytes,
        DirectorySyncPolicy::Strict,
    )
    .map_err(storage)?;
    Ok(pointer)
}

/// Select one bounded page of derived text-artifact debris from the canonical
/// artifact root. The durable generation index is the only completed-artifact
/// liveness authority. An in-progress builder names its staging database with
/// the sealed generation digest, so every staging path whose source digest is
/// still retained is preserved rather than guessed dead by wall-clock age.
pub(super) fn plan_collectable_text_artifacts_cancellable(
    store_root: &Path,
    active_pointer: Option<&DurablePublicationPointerV1>,
    verification: GenerationDigestVerificationV1,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeTextArtifactRetentionInventoryV1, CodeGenerationRetentionErrorV1> {
    if observe_cancel(is_cancelled) {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    // An unpublished store (`None`) has no durable index and no resumable
    // build authority: every completed, staging, sidecar, and corrupt file
    // under its artifact root is crash debris and therefore a candidate.
    let mut referenced = BTreeMap::new();
    for entry in active_pointer
        .map(|pointer| pointer.generation_index.as_slice())
        .unwrap_or_default()
    {
        if let Some(descriptor) = entry.text_artifact.as_ref() {
            validate_text_artifact_descriptor(descriptor)?;
            if referenced
                .insert(descriptor.artifact_file.as_str(), descriptor)
                .is_some_and(|prior| prior != descriptor)
            {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                    "publication-pointer text artifact path has conflicting identity".to_owned(),
                ));
            }
        }
    }
    let active_staging_source = active_pointer
        .map(|pointer| {
            generation_file_digest(&pointer.generation_file).ok_or_else(|| {
                CodeGenerationRetentionErrorV1::UnsafeState(
                    "active publication-pointer generation filename has no SHA-256 digest"
                        .to_owned(),
                )
            })
        })
        .transpose()?;

    let root = code_text_artifacts_root(store_root);
    let root_metadata = match std::fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if referenced.is_empty() {
                return Ok(CodeTextArtifactRetentionInventoryV1 {
                    candidates: Vec::new(),
                    unique_bytes: 0,
                });
            }
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "durable publication pointer references text artifacts but their root is missing"
                    .to_owned(),
            ));
        }
        Err(error) => return Err(storage(error)),
    };
    if !root_metadata.file_type().is_dir() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "code text artifact root '{}' is not a directory",
            root.display()
        )));
    }

    // The index can name at most 32 completed artifacts, and only the active
    // generation has a resumable build authority. Verify the completed
    // liveness set directly before scanning debris, so an early candidate page
    // never certifies deletion while a durable descriptor is corrupt or
    // missing.
    let mut inventory = BTreeMap::new();
    for descriptor in referenced.values() {
        if observe_cancel(is_cancelled) {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        verify_completed_text_artifact(
            &root.join(&descriptor.artifact_file),
            descriptor,
            verification,
            is_cancelled,
        )?;
        inventory.insert(
            descriptor.artifact_file.clone(),
            descriptor.artifact_size_bytes,
        );
    }
    if let Some(active_staging_source) = active_staging_source {
        let active_staging_file = format!(".text-artifact-{active_staging_source}.staging");
        let active_staging_path = root.join(&active_staging_file);
        match std::fs::symlink_metadata(&active_staging_path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                inventory.insert(active_staging_file, metadata.len());
            }
            Ok(_) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "active text-artifact staging path '{}' is not a regular file",
                    active_staging_path.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
    }

    let mut entries = std::fs::read_dir(&root).map_err(storage)?;
    let mut candidates = BTreeMap::new();
    for _ in 0..MAX_CODE_TEXT_ARTIFACT_INVENTORY_ENTRIES_V1 {
        if observe_cancel(is_cancelled) {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        let Some(entry) = entries.next() else {
            break;
        };
        let entry = entry.map_err(storage)?;
        let file_name = entry.file_name().into_string().map_err(|_| {
            CodeGenerationRetentionErrorV1::UnsafeState(
                "code text artifact inventory filename is not UTF-8".to_owned(),
            )
        })?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(storage)?;
        if !metadata.file_type().is_file() {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "code text artifact inventory path '{}' is not a regular file",
                path.display()
            )));
        }

        let candidate = if let Some(digest) = completed_text_artifact_digest(&file_name) {
            if referenced.contains_key(file_name.as_str()) {
                None
            } else {
                // A completed SQLite artifact can never be empty. A zero-byte
                // file at its final content-addressed path is the only state
                // left when publication created the destination but failed
                // before writing any bytes. It contains no recoverable data,
                // so retain the regular-file/inode/size checks while allowing
                // retention to collect that publish-crash placeholder. Every
                // non-empty candidate still requires its full content proof.
                let candidate_verification = if metadata.len() == 0 {
                    GenerationDigestVerificationV1::MetadataOnly
                } else {
                    verification
                };
                verify_unreferenced_completed_text_artifact(
                    &path,
                    digest,
                    metadata.len(),
                    candidate_verification,
                    is_cancelled,
                )?;
                Some(CodeTextArtifactRetentionCandidateV1 {
                    artifact_file: file_name,
                    kind: CodeTextArtifactRetentionKindV1::Completed,
                    size_bytes: metadata.len(),
                })
            }
        } else if let Some(source_digest) = staging_text_artifact_source_digest(&file_name) {
            if Some(source_digest) == active_staging_source {
                None
            } else {
                Some(CodeTextArtifactRetentionCandidateV1 {
                    artifact_file: file_name,
                    kind: CodeTextArtifactRetentionKindV1::Staging,
                    size_bytes: metadata.len(),
                })
            }
        } else if let Some(source_digest) = staging_sidecar_text_artifact_source_digest(&file_name)
        {
            // SQLite sidecars of the staging database (`-journal`, `-wal`,
            // `-shm`). They live and die with their staging file: the active
            // build's sidecars are the builder's property, while an orphaned
            // staging file's sidecars are the same crash debris it is. Before
            // this arm they were "unrecognized regular file" failures that
            // poisoned every retention plan for the scope.
            if Some(source_digest) == active_staging_source {
                None
            } else {
                Some(CodeTextArtifactRetentionCandidateV1 {
                    artifact_file: file_name,
                    kind: CodeTextArtifactRetentionKindV1::Staging,
                    size_bytes: metadata.len(),
                })
            }
        } else if is_corrupt_text_artifact_file(&file_name) {
            Some(CodeTextArtifactRetentionCandidateV1 {
                artifact_file: file_name,
                kind: CodeTextArtifactRetentionKindV1::Corrupt,
                size_bytes: metadata.len(),
            })
        } else {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "code text artifact inventory contains unrecognized regular file '{}'",
                path.display()
            )));
        };
        if let Some(candidate) = candidate {
            inventory.insert(candidate.artifact_file.clone(), candidate.size_bytes);
            candidates.insert(candidate.artifact_file.clone(), candidate);
            if candidates.len() == MAX_CODE_TEXT_ARTIFACT_RETENTION_BATCH_V1 {
                break;
            }
        }
    }
    Ok(CodeTextArtifactRetentionInventoryV1 {
        candidates: candidates.into_values().collect(),
        unique_bytes: inventory
            .values()
            .fold(0_u64, |total, bytes| total.saturating_add(*bytes)),
    })
}

pub(super) fn completed_text_artifact_digest(file_name: &str) -> Option<&str> {
    file_name
        .strip_prefix("text-artifact-")?
        .strip_suffix(".bin")
        .filter(|digest| is_lowercase_hex(digest, 64))
}

pub(super) fn staging_text_artifact_source_digest(file_name: &str) -> Option<&str> {
    file_name
        .strip_prefix(".text-artifact-")?
        .strip_suffix(".staging")
        .filter(|digest| is_lowercase_hex(digest, 64))
}

/// The SQLite sidecar files a staging database leaves beside itself
/// (`.staging-journal`, `.staging-wal`, `.staging-shm`). They carry the same
/// source-generation digest as their staging file and share its liveness.
pub(super) fn staging_sidecar_text_artifact_source_digest(file_name: &str) -> Option<&str> {
    let value = file_name.strip_prefix(".text-artifact-")?;
    let digest = value
        .strip_suffix(".staging-journal")
        .or_else(|| value.strip_suffix(".staging-wal"))
        .or_else(|| value.strip_suffix(".staging-shm"))?;
    is_lowercase_hex(digest, 64).then_some(digest)
}

pub(super) fn is_corrupt_text_artifact_file(file_name: &str) -> bool {
    let Some(value) = file_name.strip_prefix("text-artifact-") else {
        return false;
    };
    let Some((digest, suffix)) = value.split_once(".corrupt-") else {
        return false;
    };
    let digest = digest.strip_suffix(".bin").unwrap_or(digest);
    !suffix.is_empty() && is_lowercase_hex(digest, 64)
}

pub(super) fn verify_completed_text_artifact(
    path: &Path,
    descriptor: &DurableCodeTextArtifactDescriptorV1,
    verification: GenerationDigestVerificationV1,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let digest = sha256_file_component(&descriptor.artifact_digest, "text artifact")?;
    verify_unreferenced_completed_text_artifact(
        path,
        digest,
        descriptor.artifact_size_bytes,
        verification,
        is_cancelled,
    )
}

/// A content-addressed path is trusted only after the open file and its path
/// still name the same regular inode. Full retention hashes that stable file;
/// metadata-only observation deliberately stops at bounded type/size/name
/// identity and can never authorize an unlink.
pub(super) fn verify_unreferenced_completed_text_artifact(
    path: &Path,
    expected_digest: &str,
    expected_size_bytes: u64,
    verification: GenerationDigestVerificationV1,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let before = std::fs::symlink_metadata(path).map_err(storage)?;
    if !before.file_type().is_file() || before.len() != expected_size_bytes {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "code text artifact '{}' has an invalid regular-file identity",
            path.display()
        )));
    }
    let file = File::open(path).map_err(storage)?;
    if !path_still_names_open_file(path, &file, &before)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "code text artifact '{}' changed while its identity was being verified",
            path.display()
        )));
    }
    if verification == GenerationDigestVerificationV1::Full
        && open_file_sha256_hex_cancellable(&file, is_cancelled)? != expected_digest
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "code text artifact '{}' does not match its content address",
            path.display()
        )));
    }
    if !path_still_names_open_file(path, &file, &before)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "code text artifact '{}' changed while its content was being verified",
            path.display()
        )));
    }
    Ok(())
}

/// `active_pointer` is the pointer the store carries *now*, which is not
/// `plan.active_pointer` when generation retention rewrote the durable index
/// earlier in the same pass. The compare-and-swap below and the receipt's
/// index digest must both read that current value.
pub(super) fn execute_text_artifact_retention_under_store_lock(
    store_root: &Path,
    plan: &CodeGenerationRetentionPlanV1,
    active_pointer: Option<&DurablePublicationPointerV1>,
    completed_at: UtcMicros,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<
    (
        Vec<CodeTextArtifactRetentionCandidateV1>,
        Option<CodeTextArtifactRetentionReceiptV1>,
    ),
    CodeGenerationRetentionErrorV1,
> {
    if observe_cancel(is_cancelled) {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    let deleted_artifacts = plan.collectable_text_artifacts.clone();
    let receipt = build_text_artifact_receipt(
        plan,
        active_pointer,
        deleted_artifacts.clone(),
        completed_at,
    )?;
    let transaction = CodeTextArtifactRetentionTransactionV1 {
        schema: TEXT_ARTIFACT_TRANSACTION_SCHEMA.to_owned(),
        active_pointer: active_pointer.cloned(),
        receipt: receipt.clone(),
    };
    persist_text_artifact_transaction(store_root, &transaction)?;
    let result = (|| {
        if observe_cancel(is_cancelled) {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        stage_collectable_text_artifacts_cancellable(store_root, &transaction, is_cancelled)?;
        if observe_cancel(is_cancelled) {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        if read_optional_active_pointer(store_root)? != transaction.active_pointer {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "active generation changed while text-artifact candidates were quarantined"
                    .to_owned(),
            ));
        }
        if observe_cancel(is_cancelled) {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        write_text_artifact_receipt(store_root, &receipt)?;
        cleanup_committed_text_artifact_transaction(store_root, &transaction)?;
        clear_text_artifact_transaction(store_root)
    })();
    if let Err(error) = result {
        if !text_artifact_receipt_is_durable(store_root, &receipt)? {
            rollback_staged_text_artifact_transaction(store_root, &transaction)?;
            clear_text_artifact_transaction(store_root)?;
        }
        return Err(error);
    }
    Ok((deleted_artifacts, Some(receipt)))
}

pub(super) fn recover_pending_text_artifact_transaction_unlocked(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let Some(transaction) = load_text_artifact_transaction(store_root)? else {
        return Ok(());
    };
    if text_artifact_receipt_is_durable(store_root, &transaction.receipt)? {
        cleanup_committed_text_artifact_transaction(store_root, &transaction)?;
    } else {
        rollback_staged_text_artifact_transaction(store_root, &transaction)?;
    }
    clear_text_artifact_transaction(store_root)
}

pub(super) fn text_artifact_transaction_path(store_root: &Path) -> PathBuf {
    journal_path(store_root, &TEXT_ARTIFACT_TRANSACTION_JOURNAL)
}

pub(super) fn text_artifact_transaction_stage_root(
    store_root: &Path,
    receipt: &CodeTextArtifactRetentionReceiptV1,
) -> PathBuf {
    store_root
        .join(TEXT_ARTIFACT_QUARANTINE_DIRECTORY)
        .join(&receipt.receipt_digest)
}

pub(super) fn persist_text_artifact_transaction(
    store_root: &Path,
    transaction: &CodeTextArtifactRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    persist_journal(store_root, &TEXT_ARTIFACT_TRANSACTION_JOURNAL, transaction)
}

pub(super) fn load_text_artifact_transaction(
    store_root: &Path,
) -> Result<Option<CodeTextArtifactRetentionTransactionV1>, CodeGenerationRetentionErrorV1> {
    load_journal(store_root, &TEXT_ARTIFACT_TRANSACTION_JOURNAL)
}

pub(super) fn validate_text_artifact_transaction(
    transaction: &CodeTextArtifactRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if transaction.schema != TEXT_ARTIFACT_TRANSACTION_SCHEMA
        || transaction.receipt.schema != TEXT_ARTIFACT_RECEIPT_SCHEMA
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "text-artifact retention transaction has an incompatible schema".to_owned(),
        ));
    }
    let pointer_identity = transaction
        .active_pointer
        .as_ref()
        .map(|pointer| {
            validate_durable_generation_index(pointer)?;
            let generation = CodeGenerationId::new(pointer.generation_id.clone())
                .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
            let index_digest = pointer.generation_index_digest.clone().ok_or_else(|| {
                CodeGenerationRetentionErrorV1::UnsafeState(
                    "text-artifact transaction active pointer has no index digest".to_owned(),
                )
            })?;
            Ok::<_, CodeGenerationRetentionErrorV1>((generation, index_digest))
        })
        .transpose()?;
    let (pointer_generation, index_digest) = match pointer_identity {
        Some((generation, digest)) => (Some(generation), Some(digest)),
        None => (None, None),
    };
    if transaction.receipt.active_generation_id != pointer_generation
        || transaction.receipt.active_generation_index_digest != index_digest
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "text-artifact transaction active pointer does not match its receipt".to_owned(),
        ));
    }
    if !is_lowercase_hex(&transaction.receipt.receipt_digest, 64) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "text-artifact transaction receipt digest is not a SHA-256 file component".to_owned(),
        ));
    }
    let mut artifact_files = BTreeSet::new();
    for candidate in &transaction.receipt.deleted_artifacts {
        validate_text_artifact_candidate(candidate)?;
        if !artifact_files.insert(candidate.artifact_file.as_str()) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "text-artifact transaction contains duplicate candidate paths".to_owned(),
            ));
        }
    }
    if artifact_files.is_empty()
        || transaction.receipt.reclaimed_bytes
            != total_text_artifact_bytes(&transaction.receipt.deleted_artifacts)
        || transaction.receipt.reclaimed_bytes
            > transaction.receipt.inventory_bytes_before_collection
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "text-artifact transaction violates exact candidate or byte invariants".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_text_artifact_candidate(
    candidate: &CodeTextArtifactRetentionCandidateV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let direct_name = Path::new(&candidate.artifact_file)
        .file_name()
        .and_then(|name| name.to_str())
        == Some(candidate.artifact_file.as_str());
    let kind_matches_name = match candidate.kind {
        CodeTextArtifactRetentionKindV1::Completed => {
            completed_text_artifact_digest(&candidate.artifact_file).is_some()
        }
        CodeTextArtifactRetentionKindV1::Staging => {
            staging_text_artifact_source_digest(&candidate.artifact_file).is_some()
                || staging_sidecar_text_artifact_source_digest(&candidate.artifact_file).is_some()
        }
        CodeTextArtifactRetentionKindV1::Corrupt => {
            is_corrupt_text_artifact_file(&candidate.artifact_file)
        }
    };
    if !direct_name || candidate.artifact_file.contains(['/', '\\']) || !kind_matches_name {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "text-artifact transaction candidate is outside the canonical namespace".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn text_artifact_receipt_is_durable(
    store_root: &Path,
    receipt: &CodeTextArtifactRetentionReceiptV1,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    receipt_store::receipt_is_durable(
        store_root,
        &TEXT_ARTIFACT_RECEIPT_STORE,
        &receipt.receipt_digest,
        receipt,
    )
}

#[cfg(test)]
pub(super) fn stage_collectable_text_artifacts(
    store_root: &Path,
    transaction: &CodeTextArtifactRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    stage_collectable_text_artifacts_cancellable(store_root, transaction, &|| false)
}

pub(super) fn stage_collectable_text_artifacts_cancellable(
    store_root: &Path,
    transaction: &CodeTextArtifactRetentionTransactionV1,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let artifacts_root = code_text_artifacts_root(store_root);
    let stage_root = text_artifact_transaction_stage_root(store_root, &transaction.receipt);
    std::fs::create_dir_all(&stage_root).map_err(storage)?;
    sync_directory(stage_root.parent().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "text-artifact retention quarantine has no parent".to_owned(),
        )
    })?)?;
    for candidate in &transaction.receipt.deleted_artifacts {
        if observe_cancel(is_cancelled) {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        validate_text_artifact_candidate(candidate)?;
        let source = artifacts_root.join(&candidate.artifact_file);
        let staged = stage_root.join(&candidate.artifact_file);
        match (regular_file_exists(&source)?, regular_file_exists(&staged)?) {
            (true, false) => {
                let metadata = std::fs::symlink_metadata(&source).map_err(storage)?;
                if metadata.len() != candidate.size_bytes {
                    return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                        "text-artifact candidate '{}' changed after the mark phase",
                        candidate.artifact_file
                    )));
                }
                if let CodeTextArtifactRetentionKindV1::Completed = candidate.kind {
                    let digest = completed_text_artifact_digest(&candidate.artifact_file)
                        .ok_or_else(|| {
                            CodeGenerationRetentionErrorV1::UnsafeState(
                                "completed text-artifact candidate lost its content address"
                                    .to_owned(),
                            )
                        })?;
                    let candidate_verification = if candidate.size_bytes == 0 {
                        GenerationDigestVerificationV1::MetadataOnly
                    } else {
                        GenerationDigestVerificationV1::Full
                    };
                    verify_unreferenced_completed_text_artifact(
                        &source,
                        digest,
                        candidate.size_bytes,
                        candidate_verification,
                        is_cancelled,
                    )?;
                }
                if observe_cancel(is_cancelled) {
                    return Err(CodeGenerationRetentionErrorV1::Cancelled);
                }
                std::fs::rename(&source, &staged).map_err(storage)?;
                sync_directory(&artifacts_root)?;
                sync_directory(&stage_root)?;
            }
            (false, false) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "text-artifact candidate '{}' is missing before quarantine",
                    candidate.artifact_file
                )));
            }
            (false, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "text-artifact candidate '{}' was already quarantined",
                    candidate.artifact_file
                )));
            }
            (true, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "text-artifact candidate '{}' exists in source and quarantine",
                    candidate.artifact_file
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn rollback_staged_text_artifact_transaction(
    store_root: &Path,
    transaction: &CodeTextArtifactRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let artifacts_root = code_text_artifacts_root(store_root);
    let stage_root = text_artifact_transaction_stage_root(store_root, &transaction.receipt);
    for candidate in &transaction.receipt.deleted_artifacts {
        let source = artifacts_root.join(&candidate.artifact_file);
        let staged = stage_root.join(&candidate.artifact_file);
        match (regular_file_exists(&source)?, regular_file_exists(&staged)?) {
            (true, false) => {}
            (false, true) => {
                std::fs::rename(&staged, &source).map_err(storage)?;
                sync_directory(&artifacts_root)?;
                sync_directory(&stage_root)?;
            }
            (false, false) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "text-artifact rollback cannot find '{}'",
                    candidate.artifact_file
                )));
            }
            (true, true) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "text-artifact rollback found duplicate '{}'",
                    candidate.artifact_file
                )));
            }
        }
    }
    remove_empty_stage_root(&stage_root)
}

pub(super) fn cleanup_committed_text_artifact_transaction(
    store_root: &Path,
    transaction: &CodeTextArtifactRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    ensure_text_artifact_transaction_liveness(store_root, transaction)?;
    let artifacts_root = code_text_artifacts_root(store_root);
    let stage_root = text_artifact_transaction_stage_root(store_root, &transaction.receipt);
    for candidate in &transaction.receipt.deleted_artifacts {
        let source = artifacts_root.join(&candidate.artifact_file);
        if regular_file_exists(&source)? {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "text-artifact receipt is durable but '{}' returned to its source root",
                candidate.artifact_file
            )));
        }
        let staged = stage_root.join(&candidate.artifact_file);
        if regular_file_exists(&staged)? {
            std::fs::remove_file(&staged).map_err(storage)?;
            sync_directory(&stage_root)?;
        }
    }
    remove_empty_stage_root(&stage_root)
}

pub(super) fn ensure_text_artifact_transaction_liveness(
    store_root: &Path,
    transaction: &CodeTextArtifactRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    // Liveness is proven against the *current* pointer: a publish may have
    // landed since the transaction was staged (including the first publish
    // into a previously unpublished store), and no durable descriptor target
    // it names may be removed.
    let Some(current) = read_optional_active_pointer(store_root)? else {
        return Ok(());
    };
    validate_durable_generation_index(&current)?;
    let deleted = transaction
        .receipt
        .deleted_artifacts
        .iter()
        .map(|candidate| candidate.artifact_file.as_str())
        .collect::<BTreeSet<_>>();
    if current
        .generation_index
        .iter()
        .filter_map(|entry| entry.text_artifact.as_ref())
        .any(|descriptor| deleted.contains(descriptor.artifact_file.as_str()))
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "text-artifact retention recovery would remove a durable descriptor target".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn clear_text_artifact_transaction(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    clear_journal(store_root, &TEXT_ARTIFACT_TRANSACTION_JOURNAL)
}

pub(super) fn build_text_artifact_receipt(
    plan: &CodeGenerationRetentionPlanV1,
    active_pointer: Option<&DurablePublicationPointerV1>,
    deleted_artifacts: Vec<CodeTextArtifactRetentionCandidateV1>,
    completed_at: UtcMicros,
) -> Result<CodeTextArtifactRetentionReceiptV1, CodeGenerationRetentionErrorV1> {
    let active_generation_index_digest = active_pointer
        .map(|pointer| {
            pointer.generation_index_digest.as_deref().ok_or_else(|| {
                CodeGenerationRetentionErrorV1::UnsafeState(
                    "active publication pointer has no generation index digest".to_owned(),
                )
            })
        })
        .transpose()?;
    let reclaimed_bytes = total_text_artifact_bytes(&deleted_artifacts);
    let material = CodeTextArtifactRetentionReceiptMaterialV1 {
        schema: TEXT_ARTIFACT_RECEIPT_SCHEMA,
        active_generation_id: plan.active_generation_id.as_ref(),
        active_generation_index_digest,
        deleted_artifacts: &deleted_artifacts,
        inventory_bytes_before_collection: plan.text_artifact_inventory_bytes,
        reclaimed_bytes,
        completed_at_micros: completed_at.0,
    };
    let digest = canonical_sha256(&material)
        .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
    let receipt_digest =
        receipt_digest_file_component(&TEXT_ARTIFACT_RECEIPT_STORE, digest.as_str())?;
    Ok(CodeTextArtifactRetentionReceiptV1 {
        schema: TEXT_ARTIFACT_RECEIPT_SCHEMA.to_owned(),
        receipt_digest,
        active_generation_id: plan.active_generation_id.clone(),
        active_generation_index_digest: active_generation_index_digest.map(str::to_owned),
        deleted_artifacts,
        inventory_bytes_before_collection: plan.text_artifact_inventory_bytes,
        reclaimed_bytes,
        completed_at_micros: completed_at.0,
    })
}

pub(super) fn write_text_artifact_receipt(
    store_root: &Path,
    receipt: &CodeTextArtifactRetentionReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    receipt_store::write_receipt(
        store_root,
        &TEXT_ARTIFACT_RECEIPT_STORE,
        &receipt.receipt_digest,
        receipt,
    )
}

pub(super) fn total_text_artifact_bytes(artifacts: &[CodeTextArtifactRetentionCandidateV1]) -> u64 {
    artifacts.iter().fold(0_u64, |total, artifact| {
        total.saturating_add(artifact.size_bytes)
    })
}

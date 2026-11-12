//! Reference retirement in the registered project's canonical SQL authority.

use std::collections::BTreeSet;

use tracedecay_domain::{CodeGenerationId, WorktreeId};
use tracedecay_runtime_core::db::{
    CodeIndexPayloadIdentity, CodeIndexSqliteStorage, CodeIndexStorageError,
};

use super::{
    CodeGenerationRetentionErrorV1, CodeGenerationRetentionGenerationV1,
    DurablePublicationPointerV1, build_generation_receipt,
};

pub fn retain_registered_code_index(
    storage: &CodeIndexSqliteStorage,
    worktree: &WorktreeId,
    protected: &BTreeSet<CodeGenerationId>,
    check: &dyn Fn() -> bool,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    if check() {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    let mut changed = storage.collect_abandoned_stage().map_err(failure)?;
    let head = storage.read_head(worktree).map_err(failure)?;
    let Some(head) = head else {
        return storage.collect_unreferenced_payload().map_err(failure)
            .map(|collected| changed || collected);
    };
    let mut pointer: DurablePublicationPointerV1 =
        serde_json::from_slice(&head).map_err(|error| {
            CodeGenerationRetentionErrorV1::UnsafeState(error.to_string())
        })?;
    super::validate_durable_generation_index(&pointer)?;
    // Retention is a liveness decision, not generation_index membership.
    // SQL separately protects durable graph replay and active encoded readers.
    let active = CodeGenerationId::new(pointer.generation_id.clone()).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(error.to_string())
    })?;
    let entries = storage.generation_page(worktree, None).map_err(failure)?;
    let mut expected_head = head;
    for entry in entries.records() {
        if check() {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        let generation = &entry.key.generation_id;
        if generation == &active || protected.contains(generation) {
            continue;
        }
        let manifest = storage.read_manifest(&entry.key, &entry.manifest.digest).map_err(failure)?;
        let metadata = tracedecay_code_index::production::CodeIndexPublishedGenerationV1::partitioned_text_metadata(&manifest)
            .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
        if metadata.manifest().generation_id != *generation {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "retirement manifest names another generation".to_owned(),
            ));
        }
        let identities = tracedecay_code_index::production::CodeIndexPublishedGenerationV1::partitioned_segment_identities(&manifest)
            .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
        for identity in identities {
            if check() {
                return Err(CodeGenerationRetentionErrorV1::Cancelled);
            }
            storage.verify_payload(
                &entry.key,
                &CodeIndexPayloadIdentity {
                    digest: identity.digest,
                    size_bytes: identity.size_bytes,
                },
                &|| if check() {
                    Err(CodeIndexStorageError::Infrastructure("retirement cancelled".to_owned()))
                } else {
                    Ok(())
                },
            ).map_err(|error| if check() {
                CodeGenerationRetentionErrorV1::Cancelled
            } else {
                failure(error)
            })?;
        }
        let pending = storage.pending_retirement(&entry.key).map_err(failure)?;
        let receipt = build_generation_receipt(
            Some(&active), protected,
            vec![CodeGenerationRetentionGenerationV1 {
                generation_id: generation.clone(),
                generation_file: format!("generation-{}.json",
                    tracedecay_domain::sha256_hex_suffix(entry.manifest.digest.as_str())
                        .ok_or_else(|| CodeGenerationRetentionErrorV1::UnsafeState(
                            "retirement manifest digest is not canonical".to_owned(),
                        ))?,
                ),
                sealed_at_micros: metadata.manifest().seal.sealed_at.0,
                size_bytes: entry.manifest.size_bytes,
            }],
            tracedecay_runtime_core::tracedecay::saturating_utc_now(),
        )?;
        let operation = tracedecay_runtime_core::db::CodeIndexStorageOperationId::new(format!(
            "retire:{}:{}", worktree, receipt.receipt_digest,
        )).map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
        let receipt = storage.encode_json(&receipt).map_err(failure)?;
        let (operation, receipt) = pending.unwrap_or((operation, receipt));
        let mut next_pointer = pointer.clone();
        next_pointer.generation_index.retain(|entry| entry.generation_id != generation.as_str());
        next_pointer.generation_index_digest = Some(super::durable_generation_index_digest(
            &next_pointer.generation_index, next_pointer.generation_index_truncated,
        )?);
        let next_head = storage.encode_json(&next_pointer).map_err(failure)?;
        storage.prepare_retirement(
            &entry.key, &operation, &entry.manifest.digest, &receipt, &expected_head, &next_head,
        ).map_err(failure)?;
        pointer = next_pointer;
        expected_head = next_head;
        match storage.complete_retirement(&entry.key, &operation) {
            Ok(()) => {
                changed = true;
            }
            Err(CodeIndexStorageError::Retained) => changed = true,
            Err(error) => return Err(failure(error)),
        }
    }
    if !check() && storage.collect_unreferenced_payload().map_err(failure)? {
        changed = true;
    }
    Ok(changed)
}

fn failure(error: CodeIndexStorageError) -> CodeGenerationRetentionErrorV1 {
    CodeGenerationRetentionErrorV1::UnsafeState(error.to_string())
}


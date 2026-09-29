//! One content-addressed durable receipt store shared by every retention
//! receipt family (generation, text artifact, scope).
//!
//! A receipt file name embeds the receipt's own digest, so a same-name
//! collision is only acceptable when the bytes are identical (an idempotent
//! replay); different bytes under one digest mean corrupted or forged
//! evidence and fail closed before anything is certified durable.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use tracedecay_domain::sha256_hex_suffix;

use serde::Serialize;

use super::{CodeGenerationRetentionErrorV1, regular_file_exists, storage, sync_directory};

/// Static description of one receipt family: where its receipts live and the
/// label that prefixes its error strings.
pub(super) struct ReceiptStoreSpec {
    pub(super) directory: &'static str,
    pub(super) label: &'static str,
}

pub(super) fn receipt_path(
    store_root: &Path,
    spec: &ReceiptStoreSpec,
    receipt_digest: &str,
) -> PathBuf {
    store_root
        .join(spec.directory)
        .join(format!("receipt-{receipt_digest}.json"))
}

fn receipt_bytes<T: Serialize>(
    spec: &ReceiptStoreSpec,
    receipt: &T,
) -> Result<Vec<u8>, CodeGenerationRetentionErrorV1> {
    serde_json::to_vec(receipt).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} serialization failed: {error}",
            spec.label
        ))
    })
}

/// Whether the exact receipt is already committed. A present file with
/// different bytes is not "durable with drift", it is a digest collision and
/// therefore corrupt evidence.
pub(super) fn receipt_is_durable<T: Serialize>(
    store_root: &Path,
    spec: &ReceiptStoreSpec,
    receipt_digest: &str,
    receipt: &T,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    let path = receipt_path(store_root, spec, receipt_digest);
    if !regular_file_exists(&path)? {
        return Ok(false);
    }
    if std::fs::read(&path).map_err(storage)? != receipt_bytes(spec, receipt)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} digest collides with different bytes",
            spec.label
        )));
    }
    Ok(true)
}

/// Durably publish one receipt: write to a private temporary, fsync the file,
/// rename onto the content-addressed name, then fsync the directory. An
/// identical existing receipt is an idempotent success; different bytes fail
/// closed.
pub(super) fn write_receipt<T: Serialize>(
    store_root: &Path,
    spec: &ReceiptStoreSpec,
    receipt_digest: &str,
    receipt: &T,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let receipts_root = store_root.join(spec.directory);
    std::fs::create_dir_all(&receipts_root).map_err(storage)?;
    let final_path = receipt_path(store_root, spec, receipt_digest);
    let bytes = receipt_bytes(spec, receipt)?;
    if final_path.exists() {
        if std::fs::read(&final_path).map_err(storage)? == bytes {
            return Ok(());
        }
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} digest collides with different bytes",
            spec.label
        )));
    }
    let temporary = receipts_root.join(format!(
        ".receipt-{receipt_digest}.{}.tmp",
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

/// Remove every committed receipt of this family whose digest `keep` does not
/// name, and return how many were removed. The caller holds the store lock and
/// has settled every journal, so `keep` is the complete set of readers.
pub(super) fn prune_receipts(
    store_root: &Path,
    spec: &ReceiptStoreSpec,
    keep: &BTreeSet<String>,
) -> Result<usize, CodeGenerationRetentionErrorV1> {
    let receipts_root = store_root.join(spec.directory);
    let entries = match std::fs::read_dir(&receipts_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(storage(error)),
    };
    let mut removed = 0usize;
    for entry in entries {
        let entry = entry.map_err(storage)?;
        let name = entry.file_name();
        let Some(digest) = name
            .to_str()
            .and_then(|name| name.strip_prefix("receipt-"))
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if keep.contains(digest) {
            continue;
        }
        if !entry.file_type().map_err(storage)?.is_file() {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "{} '{digest}' is not a regular file",
                spec.label
            )));
        }
        std::fs::remove_file(entry.path()).map_err(storage)?;
        removed += 1;
    }
    if removed > 0 {
        sync_directory(&receipts_root)?;
    }
    Ok(removed)
}

/// Strip the canonical `sha256:` tag from a receipt digest for use as a file
/// component. Every canonical digest carries the tag; a missing tag is corrupt
/// input and fails closed rather than being passed through untagged.
pub(super) fn receipt_digest_file_component(
    spec: &ReceiptStoreSpec,
    digest: &str,
) -> Result<String, CodeGenerationRetentionErrorV1> {
    match sha256_hex_suffix(digest) {
        Some(value) => Ok(value.to_owned()),
        None => Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} digest lacks its SHA-256 prefix",
            spec.label
        ))),
    }
}

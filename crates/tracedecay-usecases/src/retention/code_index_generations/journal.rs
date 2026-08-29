//! One bounded, schema-validated crash journal shared by every retention
//! transaction family.
//!
//! Each family (generation, text artifact, scope, binding-cleanup intent)
//! describes itself with a [`BoundedJournalSpec`]; the persist/load/clear
//! machinery is written once so a hardening fix can never drift between
//! copies again.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, atomic_write};

use super::{CodeGenerationRetentionErrorV1, storage, sync_directory};

/// Static description of one journal family. `label` prefixes every error so
/// a failure names its exact journal; `validate` is the family's schema and
/// invariant authority, enforced on both persist and load so a journal that
/// round-trips is always internally consistent.
pub(super) struct BoundedJournalSpec<T> {
    pub(super) file_name: &'static str,
    pub(super) max_bytes: u64,
    pub(super) label: &'static str,
    pub(super) write_context: &'static str,
    pub(super) validate: fn(&T) -> Result<(), CodeGenerationRetentionErrorV1>,
}

pub(super) fn journal_path<T>(store_root: &Path, spec: &BoundedJournalSpec<T>) -> PathBuf {
    store_root.join(spec.file_name)
}

pub(super) fn persist_journal<T: Serialize>(
    store_root: &Path,
    spec: &BoundedJournalSpec<T>,
    value: &T,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    (spec.validate)(value)?;
    let bytes = serde_json::to_vec(value).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} serialization failed: {error}",
            spec.label
        ))
    })?;
    atomic_write(
        &journal_path(store_root, spec),
        spec.write_context,
        &bytes,
        DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(storage)
}

/// Load a journal without ever trusting the path by name. The identity is
/// probed with `symlink_metadata` first so a planted symlink or directory is
/// refused instead of followed; the byte bound is enforced from that metadata
/// before any read so an oversized file is never materialized in memory; and
/// the length is re-verified after the read so a file swapped mid-read fails
/// closed rather than parsing a hybrid.
pub(super) fn load_journal<T: DeserializeOwned>(
    store_root: &Path,
    spec: &BoundedJournalSpec<T>,
) -> Result<Option<T>, CodeGenerationRetentionErrorV1> {
    let path = journal_path(store_root, spec);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage(error)),
    };
    if !metadata.file_type().is_file() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} '{}' is not a bounded regular file",
            spec.label,
            path.display()
        )));
    }
    if metadata.len() > spec.max_bytes {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} '{}' exceeds the bounded journal size",
            spec.label,
            path.display()
        )));
    }
    let bytes = std::fs::read(&path).map_err(storage)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} '{}' changed during read",
            spec.label,
            path.display()
        )));
    }
    let value = serde_json::from_slice::<T>(&bytes).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} '{}' is unreadable: {error}",
            spec.label,
            path.display()
        ))
    })?;
    (spec.validate)(&value)?;
    Ok(Some(value))
}

pub(super) fn clear_journal<T>(
    store_root: &Path,
    spec: &BoundedJournalSpec<T>,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    match std::fs::remove_file(journal_path(store_root, spec)) {
        Ok(()) => sync_directory(store_root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage(error)),
    }
}

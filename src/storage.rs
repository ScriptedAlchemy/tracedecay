//! Root shim for the kernel `storage` module.
//!
//! The layout resolver moved to `tracedecay_runtime_core::storage` in the
//! one-shot crate split. Two thin adapters could not follow it: they take
//! `global_db` types (`RegisteredGlobalDb`, `StoreInstanceRecord`) that still
//! live above the kernel, so they stay here and call the kernel's
//! field-shaped classifier. See `crates/tracedecay-runtime-core/SEAMS.md`.

use std::path::Path;

pub use tracedecay_runtime_core::storage::*;

use crate::errors::Result;

/// Classifies project storage, falling back to the registry alias lookup when
/// the on-disk markers alone report a stale project.
pub(crate) async fn try_classify_project_storage_with_registry(
    project_root: &Path,
    global_db: &crate::global_db::RegisteredGlobalDb,
    profile_root: &Path,
) -> Result<ProjectStorageLocation> {
    let location = classify_project_storage(project_root);
    if location.status != ProjectStorageStatus::Stale {
        return Ok(location);
    }
    let Some(store) = global_db
        .try_resolve_project_store_record_by_alias(project_root)
        .await?
    else {
        return Ok(location);
    };
    Ok(classify_registry_storage(project_root, profile_root, &store).unwrap_or(location))
}

/// Classifies a registry-recorded store instance against a profile root.
pub fn classify_registry_storage(
    project_root: &Path,
    profile_root: &Path,
    store: &crate::global_db::StoreInstanceRecord,
) -> Option<ProjectStorageLocation> {
    classify_registry_storage_fields(
        project_root,
        profile_root,
        &store.storage_mode,
        &store.store_relpath,
        store.manifest_relpath.as_deref(),
    )
}

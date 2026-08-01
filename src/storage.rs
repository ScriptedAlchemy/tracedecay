//! Root shim for the kernel `storage` module.
//!
//! The layout resolver moved to `tracedecay_runtime_core::storage` in the
//! one-shot crate split. One thin adapter could not follow it: it takes the
//! `global_db::StoreInstanceRecord` type that still lives above the kernel, so
//! it stays here and calls the kernel's field-shaped classifier. See
//! `crates/tracedecay-runtime-core/SEAMS.md`.

use std::path::Path;

pub use tracedecay_runtime_core::storage::*;

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

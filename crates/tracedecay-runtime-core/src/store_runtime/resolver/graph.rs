use std::fs;
use std::path::{Path, PathBuf};

use tracedecay_store::{DURABLE_GRAPH_STORE_DIRECTORY, graph_store_locator_path};

use super::{
    LocalStoreLocatorResolutionV1, LocalStoreLocatorResult, LocalStoreLocatorUnavailableReasonV1,
    LocalStoreLocatorUnavailableV1, LocalStoreRuntimeResolverV1, StoreRuntimeKey,
    canonical_or_prospective_directory, local_filesystem_safety, verified_locator,
};

impl LocalStoreRuntimeResolverV1 {
    /// Resolves the Grafeo directory paired with one exact canonical runtime
    /// shard.
    ///
    /// The relational locator selects the store family and authority root.
    /// Deriving a child directory from the relational filename keeps every
    /// project, session, and code shard physically distinct without letting a
    /// Graph consumer derive or normalize a path independently. Grafeo owns
    /// every file below the resolved directory.
    pub fn resolve_graph_key(&self, key: &StoreRuntimeKey) -> LocalStoreLocatorResolutionV1 {
        let resolved = self
            .resolve_key_inner(key, &local_filesystem_safety)
            .and_then(|store| {
                let metadata = store.metadata().clone();
                let durable_root = durable_graph_store_root(&metadata.canonical_store_root)?;
                let graph_path = graph_store_locator_path(
                    &metadata.canonical_store_root,
                    store.locator().path(),
                )
                .map_err(|_| LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)?;
                let graph_path = canonical_or_prospective_directory(&graph_path, &durable_root)?;
                verified_locator(
                    key,
                    metadata.kind,
                    metadata.canonical_profile_root,
                    metadata.canonical_store_root,
                    graph_path,
                    &local_filesystem_safety,
                )
            });
        match resolved {
            Ok(locator) => LocalStoreLocatorResolutionV1::Resolved(locator),
            Err(reason) => {
                LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                    shard_id: key.shard_id().clone(),
                    reason,
                })
            }
        }
    }
}

fn durable_graph_store_root(store_root: &Path) -> LocalStoreLocatorResult<PathBuf> {
    let durable_root = store_root.join(DURABLE_GRAPH_STORE_DIRECTORY);
    let metadata = fs::symlink_metadata(&durable_root)
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
    }
    tracedecay_private_fs::validate_private_directory(&durable_root)
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)?;
    let canonical = fs::canonicalize(&durable_root)
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable)?;
    canonical
        .starts_with(store_root)
        .then_some(canonical)
        .ok_or(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)
}

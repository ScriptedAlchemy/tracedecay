use std::fs;
use std::path::{Path, PathBuf};

use tracedecay_store::{GRAPH_STORE_PRIVATE_DIRECTORY, graph_store_locator_path};

use super::{
    LocalStoreLocatorResolutionV1, LocalStoreLocatorResult, LocalStoreLocatorUnavailableReasonV1,
    LocalStoreLocatorUnavailableV1, LocalStoreRuntimeResolverV1, StoreRuntimeKey,
    canonical_or_prospective_regular_file, local_filesystem_safety, verified_locator,
};

impl LocalStoreRuntimeResolverV1 {
    /// Resolves the Grafeo file paired with one exact canonical runtime shard.
    ///
    /// The relational locator selects the store family and authority root.
    /// Replacing its `.db` suffix with `.grafeo` keeps every project, session,
    /// and code shard physically distinct without letting a Graph consumer
    /// derive or normalize a path independently.
    pub fn resolve_graph_key(&self, key: &StoreRuntimeKey) -> LocalStoreLocatorResolutionV1 {
        let resolved = self
            .resolve_key_inner(key, &local_filesystem_safety)
            .and_then(|store| {
                let metadata = store.metadata().clone();
                let private_root = private_graph_root(&metadata.canonical_store_root)?;
                let graph_path = graph_store_locator_path(
                    &metadata.canonical_store_root,
                    store.locator().path(),
                )
                .map_err(|_| LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)?;
                let graph_path = canonical_or_prospective_regular_file(&graph_path, &private_root)?;
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

fn private_graph_root(store_root: &Path) -> LocalStoreLocatorResult<PathBuf> {
    let private_root = store_root.join(GRAPH_STORE_PRIVATE_DIRECTORY);
    let metadata = fs::symlink_metadata(&private_root)
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
        }
    }
    #[cfg(windows)]
    crate::windows_security::validate_private_directory(&private_root)
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)?;
    #[cfg(not(any(unix, windows)))]
    return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
    let canonical = fs::canonicalize(&private_root)
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable)?;
    canonical
        .starts_with(store_root)
        .then_some(canonical)
        .ok_or(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)
}

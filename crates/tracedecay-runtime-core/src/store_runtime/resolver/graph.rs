use super::{
    LocalStoreLocatorResolutionV1, LocalStoreLocatorUnavailableV1, LocalStoreRuntimeResolverV1,
    StoreRuntimeKey, canonical_or_prospective_regular_file, local_filesystem_safety,
    verified_locator,
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
                let graph_path = canonical_or_prospective_regular_file(
                    &store.locator().path().with_extension("grafeo"),
                    &metadata.canonical_store_root,
                )?;
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

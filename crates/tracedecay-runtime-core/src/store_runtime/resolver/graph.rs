use tracedecay_store::{StoreShardScopeV1, graph_store_locator_path};

use super::{
    LocalStoreLocatorResolutionV1, LocalStoreLocatorUnavailableReasonV1,
    LocalStoreLocatorUnavailableV1, LocalStoreRuntimeResolverV1, StoreRuntimeKey,
    canonical_or_prospective_regular_file, local_filesystem_safety, verified_locator,
};

impl LocalStoreRuntimeResolverV1 {
    /// Resolves the Grafeo database file paired with one exact project/profile
    /// relational authority.
    ///
    /// Code scopes are namespaces inside their owning project graph runtime,
    /// not physical stores. Callers must retain the project graph key through
    /// `StoreRuntimeRegistry::retain_code_graph_store`; resolving a code shard
    /// here fails closed instead of recreating per-worktree graph sharding.
    pub fn resolve_graph_key(&self, key: &StoreRuntimeKey) -> LocalStoreLocatorResolutionV1 {
        if matches!(key.shard_id().scope, StoreShardScopeV1::Code { .. }) {
            return LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                shard_id: key.shard_id().clone(),
                reason: LocalStoreLocatorUnavailableReasonV1::UnsupportedShardScope,
            });
        }
        let resolved = self
            .resolve_key_inner(key, &local_filesystem_safety)
            .and_then(|store| {
                let metadata = store.metadata().clone();
                let graph_path = graph_store_locator_path(
                    &metadata.canonical_store_root,
                    store.locator().path(),
                )
                .map_err(|_| LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)?;
                let graph_path = canonical_or_prospective_regular_file(
                    &graph_path,
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

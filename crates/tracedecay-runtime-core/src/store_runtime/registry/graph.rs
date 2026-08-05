use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_store::{
    RetainedGraphStoreLeaseV1, StoreRuntimeBindingV1, StoreShardScopeV1, VerifiedStoreLocatorV1,
};

use super::{
    RegistryEntry, RetainedGraphPublication, StoreRuntimeKey, StoreRuntimeRegistry,
    StoreRuntimeRegistryFailure,
};

pub struct CanonicalGraphStoreLeaseV1 {
    registry: StoreRuntimeRegistry,
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
}

impl RetainedGraphStoreLeaseV1 for CanonicalGraphStoreLeaseV1 {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

impl fmt::Debug for CanonicalGraphStoreLeaseV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalGraphStoreLeaseV1")
            .field("binding", &self.binding)
            .field("verified_locator", &self.verified_locator)
            .field("canonical_path", &self.canonical_path)
            .finish_non_exhaustive()
    }
}

impl Drop for CanonicalGraphStoreLeaseV1 {
    fn drop(&mut self) {
        let _released = self.registry.release_graph_store(
            &self.binding,
            &self.verified_locator,
            &self.canonical_path,
        );
    }
}

impl StoreRuntimeRegistry {
    pub async fn retain_graph_store(
        &self,
        key: StoreRuntimeKey,
    ) -> Result<Arc<CanonicalGraphStoreLeaseV1>, StoreRuntimeRegistryFailure> {
        validate_graph_scope(&key)?;
        let resolved = self.inner.resolver.resolve_graph(&key).await?;
        if !resolved.matches(&key) {
            return Err(StoreRuntimeRegistryFailure::LocatorIdentityMismatch {
                key: Box::new(key),
                locator: Box::new(resolved.verified().clone()),
            });
        }

        let mut state = self.lock_state();
        if let Some(retained) = state
            .graph_publications
            .iter()
            .find_map(|(retained_key, retained)| {
                (retained_key.shard_id() == key.shard_id() && retained_key != &key)
                    .then_some(&retained.binding)
            })
            .or_else(|| {
                state.entries.iter().find_map(|(retained_key, entry)| {
                    (retained_key.shard_id() == key.shard_id() && retained_key != &key)
                        .then(|| entry_binding(entry))
                })
            })
        {
            return Err(StoreRuntimeRegistryFailure::GraphIncarnationConflict {
                requested: Box::new(key),
                retained: Box::new(retained.clone()),
            });
        }
        if let Some((retained_key, retained)) =
            state
                .graph_publications
                .iter()
                .find(|(retained_key, retained)| {
                    *retained_key != &key && retained.canonical_path == resolved.path()
                })
        {
            return Err(StoreRuntimeRegistryFailure::GraphLocatorConflict {
                key: Box::new(retained_key.clone()),
                retained_path: retained.canonical_path.clone(),
                resolved_path: resolved.path().to_path_buf(),
            });
        }
        let runtime_binding = state.entries.get(&key).map(entry_binding).cloned();
        let binding = if let Some(retained) = state.graph_publications.get_mut(&key) {
            if let Some(runtime) = runtime_binding.as_ref()
                && runtime != &retained.binding
            {
                return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                    expected: Box::new(retained.binding.clone()),
                    actual: Box::new(runtime.clone()),
                });
            }
            if retained.verified_locator != *resolved.verified()
                || retained.canonical_path != resolved.path()
            {
                return Err(StoreRuntimeRegistryFailure::GraphLocatorConflict {
                    key: Box::new(key),
                    retained_path: retained.canonical_path.clone(),
                    resolved_path: resolved.path().to_path_buf(),
                });
            }
            retained.leases = retained.leases.checked_add(1).ok_or_else(|| {
                StoreRuntimeRegistryFailure::GraphLeaseCountExhausted {
                    binding: Box::new(retained.binding.clone()),
                }
            })?;
            retained.binding.clone()
        } else {
            let binding = match runtime_binding {
                Some(binding) => binding,
                None => StoreRuntimeBindingV1::new(
                    key.shard_id().clone(),
                    key.incarnation(),
                    super::open::allocate_authority_epoch()?,
                ),
            };
            state.graph_publications.insert(
                key,
                RetainedGraphPublication {
                    binding: binding.clone(),
                    verified_locator: resolved.verified().clone(),
                    canonical_path: resolved.path().to_path_buf(),
                    leases: 1,
                },
            );
            binding
        };

        Ok(Arc::new(CanonicalGraphStoreLeaseV1 {
            registry: self.clone(),
            binding,
            verified_locator: resolved.verified().clone(),
            canonical_path: resolved.path().to_path_buf(),
        }))
    }

    pub(super) fn release_graph_store(
        &self,
        binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
        canonical_path: &Path,
    ) -> bool {
        let key = StoreRuntimeKey::from_binding(binding);
        let mut state = self.lock_state();
        let Some(retained) = state.graph_publications.get_mut(&key) else {
            return false;
        };
        if retained.binding.authority_epoch != binding.authority_epoch
            || verified_locator != &retained.verified_locator
            || canonical_path != retained.canonical_path
        {
            return false;
        }
        if retained.leases == 1 {
            state.graph_publications.remove(&key);
            return true;
        }
        if retained.leases > 1 {
            retained.leases -= 1;
            return true;
        }
        false
    }

    #[cfg(test)]
    pub(super) fn retained_graph_publications_for_test(&self) -> usize {
        self.lock_state().graph_publications.len()
    }
}

fn entry_binding(entry: &RegistryEntry) -> &StoreRuntimeBindingV1 {
    match entry {
        RegistryEntry::Opening(opening) => &opening.binding,
        RegistryEntry::Ready(ready) => ready.handle.binding(),
        RegistryEntry::Evicting(evicting) => evicting.handle.binding(),
    }
}

fn validate_graph_scope(key: &StoreRuntimeKey) -> Result<(), StoreRuntimeRegistryFailure> {
    if matches!(
        key.shard_id().scope,
        StoreShardScopeV1::Project { .. }
            | StoreShardScopeV1::ProjectSessions { .. }
            | StoreShardScopeV1::ProfileSessions
            | StoreShardScopeV1::Code { .. }
    ) {
        Ok(())
    } else {
        Err(StoreRuntimeRegistryFailure::UnsupportedShardScope)
    }
}

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_domain::CodeGenerationId;
use tracedecay_graph_db::GraphNamespace;
use tracedecay_store::{
    RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1,
    RetainedGraphStoreOwnerOperationLeaseErrorV1, StoreRuntimeBindingV1, StoreShardIdV1,
    StoreShardScopeV1, VerifiedStoreLocatorV1,
};

use super::{
    GraphStoreOwnerAttachmentIdV1, GraphStoreOwnerAttachmentReservationIdV1,
    GraphStoreOwnerAttachmentState, GraphStoreOwnerIdentityV1, RegistryEntry,
    RetainedGraphPublication, StoreRuntimeKey, StoreRuntimeRegistry, StoreRuntimeRegistryFailure,
};

pub struct CanonicalGraphStoreLeaseV1 {
    registry: StoreRuntimeRegistry,
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
    lease_token: u64,
}

/// Non-client Store authority transferred to the registry that owns a graph
/// map entry. It cannot be cloned or dereferenced into a runtime handle.
pub struct CanonicalGraphStoreOwnerAttachmentV1 {
    registry: StoreRuntimeRegistry,
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
    owner_id: GraphStoreOwnerIdentityV1,
    attachment_id: GraphStoreOwnerAttachmentIdV1,
}

/// Exact Store-side counterpart of one graph-map owner attachment.
///
/// The daemon keeps this while the concrete attachment is moved into the graph
/// registry. It can reserve, restore, and terminalize only that exact map
/// owner during Store retirement; ordinary graph lease tokens are never
/// exempted.
pub struct CanonicalGraphStoreOwnerRetirementTargetV1 {
    registry: StoreRuntimeRegistry,
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
    owner_id: GraphStoreOwnerIdentityV1,
    attachment_id: GraphStoreOwnerAttachmentIdV1,
    reservation_id: Option<GraphStoreOwnerAttachmentReservationIdV1>,
    terminalized: bool,
}

/// One exact code scope routed through its owning project graph runtime.
///
/// The retained store lease deliberately stays project-scoped so all linked
/// worktrees share one physical Grafeo file and one `GraphDbRegistry` entry.
/// The namespace remains exact to the requested repository/worktree/ref or
/// snapshot scope and immutable generation so independently published
/// generations never overwrite one another.
pub struct CanonicalCodeGraphStoreLeaseV1 {
    store: Arc<CanonicalGraphStoreLeaseV1>,
    code_shard_id: StoreShardIdV1,
    generation_id: CodeGenerationId,
    namespace: GraphNamespace,
}

impl CanonicalCodeGraphStoreLeaseV1 {
    pub fn code_shard_id(&self) -> &StoreShardIdV1 {
        &self.code_shard_id
    }

    pub fn generation_id(&self) -> &CodeGenerationId {
        &self.generation_id
    }

    pub fn namespace(&self) -> &GraphNamespace {
        &self.namespace
    }
}

impl RetainedGraphStoreLeaseV1 for CanonicalCodeGraphStoreLeaseV1 {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        self.store.binding()
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        self.store.verified_locator()
    }

    fn canonical_path(&self) -> &Path {
        self.store.canonical_path()
    }
}

impl fmt::Debug for CanonicalCodeGraphStoreLeaseV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalCodeGraphStoreLeaseV1")
            .field("store", &self.store)
            .field("code_shard_id", &self.code_shard_id)
            .field("generation_id", &self.generation_id)
            .field("namespace", &self.namespace)
            .finish()
    }
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

impl RetainedGraphStoreOwnerAttachmentV1 for CanonicalGraphStoreOwnerAttachmentV1 {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    fn issue_operation_lease(
        &self,
    ) -> Result<Arc<dyn RetainedGraphStoreLeaseV1>, RetainedGraphStoreOwnerOperationLeaseErrorV1>
    {
        self.registry
            .issue_graph_store_owner_operation_lease(
                &self.binding,
                &self.verified_locator,
                &self.canonical_path,
                self.owner_id,
                self.attachment_id,
            )
            .map(|lease| {
                let lease: Arc<dyn RetainedGraphStoreLeaseV1> = lease;
                lease
            })
            .map_err(map_owner_operation_lease_error)
    }
}

impl fmt::Debug for CanonicalGraphStoreOwnerAttachmentV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalGraphStoreOwnerAttachmentV1")
            .field("binding", &self.binding)
            .field("verified_locator", &self.verified_locator)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for CanonicalGraphStoreOwnerRetirementTargetV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalGraphStoreOwnerRetirementTargetV1")
            .field("binding", &self.binding)
            .field("verified_locator", &self.verified_locator)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for CanonicalGraphStoreLeaseV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalGraphStoreLeaseV1")
            .field("binding", &self.binding)
            .field("verified_locator", &self.verified_locator)
            .field("canonical_path", &self.canonical_path)
            .field("lease_token", &self.lease_token)
            .finish_non_exhaustive()
    }
}

impl Drop for CanonicalGraphStoreLeaseV1 {
    fn drop(&mut self) {
        let _released = self.registry.release_graph_store(
            &self.binding,
            &self.verified_locator,
            &self.canonical_path,
            self.lease_token,
        );
    }
}

impl Drop for CanonicalGraphStoreOwnerAttachmentV1 {
    fn drop(&mut self) {
        let _released = self.registry.release_graph_store_owner_attachment(
            &self.binding,
            &self.verified_locator,
            &self.canonical_path,
            self.owner_id,
            self.attachment_id,
        );
    }
}

impl Drop for CanonicalGraphStoreOwnerRetirementTargetV1 {
    fn drop(&mut self) {
        if self.reservation_id.is_some() && !self.terminalized {
            let registry = self.registry.clone();
            let _restored = registry.restore_graph_store_owner_attachment(self);
        }
    }
}

impl StoreRuntimeRegistry {
    /// Retains one physical project graph store together with the exact code
    /// namespace selected by a linked worktree, ref, or immutable snapshot and
    /// code generation.
    pub async fn retain_code_graph_store(
        &self,
        project_key: StoreRuntimeKey,
        code_shard_id: StoreShardIdV1,
        generation_id: CodeGenerationId,
    ) -> Result<Arc<CanonicalCodeGraphStoreLeaseV1>, StoreRuntimeRegistryFailure> {
        validate_project_code_scope(&project_key, &code_shard_id)?;
        let namespace = code_graph_namespace(&code_shard_id, &generation_id)?;
        let store = self.retain_graph_store(project_key).await?;
        Ok(Arc::new(CanonicalCodeGraphStoreLeaseV1 {
            store,
            code_shard_id,
            generation_id,
            namespace,
        }))
    }

    pub async fn retain_graph_store(
        &self,
        key: StoreRuntimeKey,
    ) -> Result<Arc<CanonicalGraphStoreLeaseV1>, StoreRuntimeRegistryFailure> {
        validate_graph_scope(&key)?;
        self.reject_retiring_graph_admission(&key)?;
        let resolved = self.inner.resolver.resolve_graph(&key).await?;
        if !resolved.matches(&key) {
            return Err(StoreRuntimeRegistryFailure::LocatorIdentityMismatch {
                key: Box::new(key),
                locator: Box::new(resolved.verified().clone()),
            });
        }

        let mut state = self.lock_state();
        self.reject_retiring_graph_admission_locked(&state, &key)?;
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
        let lease_token = state.next_graph_lease_token.checked_add(1).ok_or(
            StoreRuntimeRegistryFailure::GraphLeaseTokenExhausted {
                key: Box::new(key.clone()),
            },
        )?;
        state.next_graph_lease_token = lease_token;
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
            if matches!(
                retained.owner_attachment,
                Some(
                    GraphStoreOwnerAttachmentState::OwnerReserved { .. }
                        | GraphStoreOwnerAttachmentState::Committed { .. }
                )
            ) {
                return Err(StoreRuntimeRegistryFailure::GraphOwnerAttachmentRetiring {
                    binding: Box::new(retained.binding.clone()),
                });
            }
            retained.lease_tokens.insert(lease_token);
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
                    owner_attachment: None,
                    lease_tokens: [lease_token].into_iter().collect(),
                },
            );
            binding
        };

        Ok(Arc::new(CanonicalGraphStoreLeaseV1 {
            registry: self.clone(),
            binding,
            verified_locator: resolved.verified().clone(),
            canonical_path: resolved.path().to_path_buf(),
            lease_token,
        }))
    }

    /// Creates the one non-client Store attachment for a graph-runtime map.
    ///
    /// The attachment is moved into the graph registry; the paired target stays
    /// with the daemon so Store retirement can reclassify only this exact map
    /// owner. Ordinary graph work must still obtain its own
    /// [`CanonicalGraphStoreLeaseV1`] through [`Self::retain_graph_store`].
    pub async fn attach_graph_store_owner(
        &self,
        key: StoreRuntimeKey,
    ) -> Result<
        (
            CanonicalGraphStoreOwnerAttachmentV1,
            CanonicalGraphStoreOwnerRetirementTargetV1,
        ),
        StoreRuntimeRegistryFailure,
    > {
        validate_graph_scope(&key)?;
        self.reject_retiring_graph_admission(&key)?;
        let resolved = self.inner.resolver.resolve_graph(&key).await?;
        if !resolved.matches(&key) {
            return Err(StoreRuntimeRegistryFailure::LocatorIdentityMismatch {
                key: Box::new(key),
                locator: Box::new(resolved.verified().clone()),
            });
        }

        let mut state = self.lock_state();
        self.reject_retiring_graph_admission_locked(&state, &key)?;
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
        let binding = if let Some(retained) = state.graph_publications.get(&key) {
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
            if retained.owner_attachment.is_some() {
                return Err(
                    StoreRuntimeRegistryFailure::GraphOwnerAttachmentAlreadyRegistered {
                        binding: Box::new(retained.binding.clone()),
                    },
                );
            }
            retained.binding.clone()
        } else {
            match runtime_binding {
                Some(binding) => binding,
                None => StoreRuntimeBindingV1::new(
                    key.shard_id().clone(),
                    key.incarnation(),
                    super::open::allocate_authority_epoch()?,
                ),
            }
        };
        let owner_id = GraphStoreOwnerIdentityV1(allocate_graph_owner_counter(
            &mut state.next_graph_owner_id,
        )?);
        let attachment_id = GraphStoreOwnerAttachmentIdV1(allocate_graph_owner_counter(
            &mut state.next_graph_owner_attachment_id,
        )?);
        if let Some(retained) = state.graph_publications.get_mut(&key) {
            retained.owner_attachment = Some(GraphStoreOwnerAttachmentState::MapOwned {
                owner_id,
                attachment_id,
            });
        } else {
            state.graph_publications.insert(
                key,
                RetainedGraphPublication {
                    binding: binding.clone(),
                    verified_locator: resolved.verified().clone(),
                    canonical_path: resolved.path().to_path_buf(),
                    owner_attachment: Some(GraphStoreOwnerAttachmentState::MapOwned {
                        owner_id,
                        attachment_id,
                    }),
                    lease_tokens: BTreeSet::new(),
                },
            );
        }

        let attachment = CanonicalGraphStoreOwnerAttachmentV1 {
            registry: self.clone(),
            binding: binding.clone(),
            verified_locator: resolved.verified().clone(),
            canonical_path: resolved.path().to_path_buf(),
            owner_id,
            attachment_id,
        };
        let target = CanonicalGraphStoreOwnerRetirementTargetV1 {
            registry: self.clone(),
            binding,
            verified_locator: resolved.verified().clone(),
            canonical_path: resolved.path().to_path_buf(),
            owner_id,
            attachment_id,
            reservation_id: None,
            terminalized: false,
        };
        Ok((attachment, target))
    }

    /// Issues one ordinary graph operation lease from an exact map-owner
    /// attachment. This only reads and updates retained registry state: it
    /// never resolves a path, opens a runtime, or invokes a foreign callback.
    pub(super) fn issue_graph_store_owner_operation_lease(
        &self,
        binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
        canonical_path: &Path,
        owner_id: GraphStoreOwnerIdentityV1,
        attachment_id: GraphStoreOwnerAttachmentIdV1,
    ) -> Result<Arc<CanonicalGraphStoreLeaseV1>, StoreRuntimeRegistryFailure> {
        let key = StoreRuntimeKey::from_binding(binding);
        let mut state = self.lock_state();
        self.reject_retiring_graph_admission_locked(&state, &key)?;
        if matches!(state.entries.get(&key), Some(RegistryEntry::Evicting(_))) {
            return Err(StoreRuntimeRegistryFailure::RuntimeEvictionInProgress {
                key: Box::new(key),
            });
        }

        let retained = state.graph_publications.get(&key).ok_or_else(|| {
            StoreRuntimeRegistryFailure::GraphOwnerAttachmentMissing {
                binding: Box::new(binding.clone()),
            }
        })?;
        if retained.binding != *binding
            || retained.verified_locator != *verified_locator
            || retained.canonical_path != canonical_path
        {
            return Err(
                StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                    binding: Box::new(binding.clone()),
                },
            );
        }
        match retained.owner_attachment {
            Some(GraphStoreOwnerAttachmentState::MapOwned {
                owner_id: retained_owner_id,
                attachment_id: retained_attachment_id,
            }) if retained_owner_id == owner_id && retained_attachment_id == attachment_id => {}
            Some(
                GraphStoreOwnerAttachmentState::OwnerReserved {
                    owner_id: retained_owner_id,
                    attachment_id: retained_attachment_id,
                    ..
                }
                | GraphStoreOwnerAttachmentState::Committed {
                    owner_id: retained_owner_id,
                    attachment_id: retained_attachment_id,
                    ..
                },
            ) if retained_owner_id == owner_id && retained_attachment_id == attachment_id => {
                return Err(StoreRuntimeRegistryFailure::GraphOwnerAttachmentRetiring {
                    binding: Box::new(binding.clone()),
                });
            }
            _ => {
                return Err(
                    StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                        binding: Box::new(binding.clone()),
                    },
                );
            }
        }

        let lease_token = state.next_graph_lease_token.checked_add(1).ok_or(
            StoreRuntimeRegistryFailure::GraphLeaseTokenExhausted {
                key: Box::new(key.clone()),
            },
        )?;
        state.next_graph_lease_token = lease_token;
        let retained = state.graph_publications.get_mut(&key).ok_or_else(|| {
            StoreRuntimeRegistryFailure::GraphOwnerAttachmentMissing {
                binding: Box::new(binding.clone()),
            }
        })?;
        retained.lease_tokens.insert(lease_token);

        Ok(Arc::new(CanonicalGraphStoreLeaseV1 {
            registry: self.clone(),
            binding: binding.clone(),
            verified_locator: verified_locator.clone(),
            canonical_path: canonical_path.to_path_buf(),
            lease_token,
        }))
    }

    pub(super) fn release_graph_store(
        &self,
        binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
        canonical_path: &Path,
        lease_token: u64,
    ) -> bool {
        let key = StoreRuntimeKey::from_binding(binding);
        let mut state = self.lock_state();
        let Some(retained) = state.graph_publications.get_mut(&key) else {
            return false;
        };
        if retained.binding != *binding
            || verified_locator != &retained.verified_locator
            || canonical_path != retained.canonical_path
        {
            return false;
        }
        if !retained.lease_tokens.remove(&lease_token) {
            return false;
        }
        if retained.lease_tokens.is_empty() && retained.owner_attachment.is_none() {
            state.graph_publications.remove(&key);
            return true;
        }
        true
    }

    fn release_graph_store_owner_attachment(
        &self,
        binding: &StoreRuntimeBindingV1,
        verified_locator: &VerifiedStoreLocatorV1,
        canonical_path: &Path,
        owner_id: GraphStoreOwnerIdentityV1,
        attachment_id: GraphStoreOwnerAttachmentIdV1,
    ) -> bool {
        let key = StoreRuntimeKey::from_binding(binding);
        let mut state = self.lock_state();
        let Some(retained) = state.graph_publications.get_mut(&key) else {
            return false;
        };
        if retained.binding != *binding
            || retained.verified_locator != *verified_locator
            || retained.canonical_path != canonical_path
        {
            return false;
        }
        if !matches!(
            retained.owner_attachment,
            Some(GraphStoreOwnerAttachmentState::MapOwned {
                owner_id: retained_owner_id,
                attachment_id: retained_attachment_id,
            }) if retained_owner_id == owner_id && retained_attachment_id == attachment_id
        ) {
            return false;
        }
        retained.owner_attachment = None;
        if retained.lease_tokens.is_empty() {
            state.graph_publications.remove(&key);
        }
        true
    }

    fn restore_graph_store_owner_attachment(
        &self,
        target: &mut CanonicalGraphStoreOwnerRetirementTargetV1,
    ) -> bool {
        let mut state = self.lock_state();
        target.restore_locked(self, &mut state)
    }

    fn reject_retiring_graph_admission(
        &self,
        key: &StoreRuntimeKey,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        let state = self.lock_state();
        self.reject_retiring_graph_admission_locked(&state, key)
    }

    fn reject_retiring_graph_admission_locked(
        &self,
        state: &super::RegistryState,
        key: &StoreRuntimeKey,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        let Some(entry) = state.entries.get(key) else {
            return Ok(());
        };
        match entry {
            RegistryEntry::Retiring(_) => {
                Err(StoreRuntimeRegistryFailure::RuntimeRetirementInProgress {
                    key: Box::new(key.clone()),
                })
            }
            RegistryEntry::Committing(_) => {
                Err(StoreRuntimeRegistryFailure::RuntimeRetirementCommitting {
                    key: Box::new(key.clone()),
                })
            }
            RegistryEntry::Faulted(_) => {
                Err(StoreRuntimeRegistryFailure::RuntimeRetirementFaulted {
                    key: Box::new(key.clone()),
                })
            }
            RegistryEntry::DurabilityUncertain(_) => Err(
                StoreRuntimeRegistryFailure::RuntimeRetirementDurabilityUncertain {
                    key: Box::new(key.clone()),
                },
            ),
            RegistryEntry::Opening(_) | RegistryEntry::Ready(_) | RegistryEntry::Evicting(_) => {
                Ok(())
            }
        }
    }

    #[cfg(test)]
    pub(super) fn retained_graph_publications_for_test(&self) -> usize {
        self.lock_state().graph_publications.len()
    }
}

fn map_owner_operation_lease_error(
    error: StoreRuntimeRegistryFailure,
) -> RetainedGraphStoreOwnerOperationLeaseErrorV1 {
    match error {
        StoreRuntimeRegistryFailure::GraphOwnerAttachmentRetiring { .. }
        | StoreRuntimeRegistryFailure::RuntimeRetirementInProgress { .. }
        | StoreRuntimeRegistryFailure::RuntimeRetirementCommitting { .. } => {
            RetainedGraphStoreOwnerOperationLeaseErrorV1::Retiring
        }
        StoreRuntimeRegistryFailure::GraphLeaseTokenExhausted { .. } => {
            RetainedGraphStoreOwnerOperationLeaseErrorV1::TokenExhausted
        }
        _ => RetainedGraphStoreOwnerOperationLeaseErrorV1::Unavailable,
    }
}

impl CanonicalGraphStoreOwnerRetirementTargetV1 {
    pub(super) fn belongs_to(&self, registry: &StoreRuntimeRegistry) -> bool {
        Arc::ptr_eq(&self.registry.inner, &registry.inner)
    }

    fn matches_publication(&self, publication: &RetainedGraphPublication) -> bool {
        publication.binding == self.binding
            && publication.verified_locator == self.verified_locator
            && publication.canonical_path == self.canonical_path
    }

    pub(super) fn matches_binding(&self, binding: &StoreRuntimeBindingV1) -> bool {
        &self.binding == binding
    }

    pub(super) fn validate_map_owned_locked(
        &self,
        registry: &StoreRuntimeRegistry,
        state: &super::RegistryState,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        if !self.belongs_to(registry) || self.reservation_id.is_some() || self.terminalized {
            return Err(
                StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                    binding: Box::new(self.binding.clone()),
                },
            );
        }
        let key = StoreRuntimeKey::from_binding(&self.binding);
        let Some(retained) = state.graph_publications.get(&key) else {
            return Err(StoreRuntimeRegistryFailure::GraphOwnerAttachmentMissing {
                binding: Box::new(self.binding.clone()),
            });
        };
        if self.matches_publication(retained)
            && matches!(
                retained.owner_attachment,
                Some(GraphStoreOwnerAttachmentState::MapOwned {
                    owner_id,
                    attachment_id,
                }) if owner_id == self.owner_id && attachment_id == self.attachment_id
            )
        {
            Ok(())
        } else {
            Err(
                StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                    binding: Box::new(self.binding.clone()),
                },
            )
        }
    }

    pub(super) fn reserve_locked(
        &mut self,
        registry: &StoreRuntimeRegistry,
        state: &mut super::RegistryState,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        self.validate_map_owned_locked(registry, state)?;
        let key = StoreRuntimeKey::from_binding(&self.binding);
        let reservation_id = GraphStoreOwnerAttachmentReservationIdV1(
            allocate_graph_owner_counter(&mut state.next_graph_owner_attachment_reservation_id)?,
        );
        let Some(retained) = state.graph_publications.get_mut(&key) else {
            return Err(StoreRuntimeRegistryFailure::GraphOwnerAttachmentMissing {
                binding: Box::new(self.binding.clone()),
            });
        };
        retained.owner_attachment = Some(GraphStoreOwnerAttachmentState::OwnerReserved {
            owner_id: self.owner_id,
            attachment_id: self.attachment_id,
            reservation_id,
        });
        self.reservation_id = Some(reservation_id);
        Ok(())
    }

    pub(super) fn restore_locked(
        &mut self,
        registry: &StoreRuntimeRegistry,
        state: &mut super::RegistryState,
    ) -> bool {
        let Some(reservation_id) = self.reservation_id else {
            return true;
        };
        if !self.belongs_to(registry) || self.terminalized {
            return false;
        }
        let key = StoreRuntimeKey::from_binding(&self.binding);
        let Some(retained) = state.graph_publications.get_mut(&key) else {
            return false;
        };
        if !self.matches_publication(retained)
            || !matches!(
                retained.owner_attachment,
                Some(GraphStoreOwnerAttachmentState::OwnerReserved {
                    owner_id,
                    attachment_id,
                    reservation_id: retained_reservation_id,
                }) if owner_id == self.owner_id
                    && attachment_id == self.attachment_id
                    && retained_reservation_id == reservation_id
            )
        {
            return false;
        }
        retained.owner_attachment = Some(GraphStoreOwnerAttachmentState::MapOwned {
            owner_id: self.owner_id,
            attachment_id: self.attachment_id,
        });
        self.reservation_id = None;
        true
    }

    pub(super) fn validate_reserved_locked(
        &self,
        registry: &StoreRuntimeRegistry,
        state: &super::RegistryState,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        let Some(reservation_id) = self.reservation_id else {
            return Err(
                StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                    binding: Box::new(self.binding.clone()),
                },
            );
        };
        if !self.belongs_to(registry) || self.terminalized {
            return Err(
                StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                    binding: Box::new(self.binding.clone()),
                },
            );
        }
        let key = StoreRuntimeKey::from_binding(&self.binding);
        let Some(retained) = state.graph_publications.get(&key) else {
            return Err(StoreRuntimeRegistryFailure::GraphOwnerAttachmentMissing {
                binding: Box::new(self.binding.clone()),
            });
        };
        if self.matches_publication(retained)
            && matches!(
                retained.owner_attachment,
                Some(GraphStoreOwnerAttachmentState::OwnerReserved {
                    owner_id,
                    attachment_id,
                    reservation_id: retained_reservation_id,
                }) if owner_id == self.owner_id
                    && attachment_id == self.attachment_id
                    && retained_reservation_id == reservation_id
            )
        {
            Ok(())
        } else {
            Err(
                StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                    binding: Box::new(self.binding.clone()),
                },
            )
        }
    }

    pub(super) fn terminalize_locked(
        &mut self,
        registry: &StoreRuntimeRegistry,
        state: &mut super::RegistryState,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        self.validate_reserved_locked(registry, state)?;
        let Some(reservation_id) = self.reservation_id else {
            return Err(
                StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                    binding: Box::new(self.binding.clone()),
                },
            );
        };
        let key = StoreRuntimeKey::from_binding(&self.binding);
        let Some(retained) = state.graph_publications.get_mut(&key) else {
            return Err(StoreRuntimeRegistryFailure::GraphOwnerAttachmentMissing {
                binding: Box::new(self.binding.clone()),
            });
        };
        retained.owner_attachment = Some(GraphStoreOwnerAttachmentState::Committed {
            owner_id: self.owner_id,
            attachment_id: self.attachment_id,
            reservation_id,
        });
        self.terminalized = true;
        Ok(())
    }

    pub(super) fn terminalize_after_commit_failure_locked(
        &mut self,
        registry: &StoreRuntimeRegistry,
        state: &mut super::RegistryState,
    ) {
        if self.terminalized || !self.belongs_to(registry) {
            self.terminalized = true;
            return;
        }
        let Some(reservation_id) = self.reservation_id else {
            self.terminalized = true;
            return;
        };
        let key = StoreRuntimeKey::from_binding(&self.binding);
        if let Some(retained) = state.graph_publications.get_mut(&key)
            && self.matches_publication(retained)
            && matches!(
                retained.owner_attachment,
                Some(GraphStoreOwnerAttachmentState::OwnerReserved {
                    owner_id,
                    attachment_id,
                    reservation_id: retained_reservation_id,
                }) if owner_id == self.owner_id
                    && attachment_id == self.attachment_id
                    && retained_reservation_id == reservation_id
            )
        {
            retained.owner_attachment = Some(GraphStoreOwnerAttachmentState::Committed {
                owner_id: self.owner_id,
                attachment_id: self.attachment_id,
                reservation_id,
            });
        }
        self.terminalized = true;
    }

    pub(super) fn remove_after_store_close_locked(
        &self,
        registry: &StoreRuntimeRegistry,
        state: &mut super::RegistryState,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        if !self.belongs_to(registry) || !self.terminalized {
            return Err(
                StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                    binding: Box::new(self.binding.clone()),
                },
            );
        }
        let Some(reservation_id) = self.reservation_id else {
            return Err(
                StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                    binding: Box::new(self.binding.clone()),
                },
            );
        };
        let key = StoreRuntimeKey::from_binding(&self.binding);
        let Some(retained) = state.graph_publications.get(&key) else {
            return Err(StoreRuntimeRegistryFailure::GraphOwnerAttachmentMissing {
                binding: Box::new(self.binding.clone()),
            });
        };
        if !self.matches_publication(retained)
            || !retained.lease_tokens.is_empty()
            || !matches!(
                retained.owner_attachment,
                Some(GraphStoreOwnerAttachmentState::Committed {
                    owner_id,
                    attachment_id,
                    reservation_id: retained_reservation_id,
                }) if owner_id == self.owner_id
                    && attachment_id == self.attachment_id
                    && retained_reservation_id == reservation_id
            )
        {
            return Err(
                StoreRuntimeRegistryFailure::GraphOwnerAttachmentReservationLost {
                    binding: Box::new(self.binding.clone()),
                },
            );
        }
        state.graph_publications.remove(&key);
        Ok(())
    }
}

fn allocate_graph_owner_counter(counter: &mut u64) -> Result<u64, StoreRuntimeRegistryFailure> {
    *counter = counter
        .checked_add(1)
        .ok_or(StoreRuntimeRegistryFailure::GraphOwnerAttachmentIdentityExhausted)?;
    Ok(*counter)
}

fn entry_binding(entry: &RegistryEntry) -> &StoreRuntimeBindingV1 {
    match entry {
        RegistryEntry::Opening(opening) => &opening.binding,
        RegistryEntry::Ready(ready) => ready.owner.binding(),
        RegistryEntry::Retiring(retiring) => retiring.owner.binding(),
        RegistryEntry::Committing(committing) => committing.owner.binding(),
        RegistryEntry::Faulted(faulted) | RegistryEntry::DurabilityUncertain(faulted) => {
            faulted.owner.binding()
        }
        RegistryEntry::Evicting(evicting) => evicting.owner.binding(),
    }
}

fn validate_graph_scope(key: &StoreRuntimeKey) -> Result<(), StoreRuntimeRegistryFailure> {
    if matches!(
        key.shard_id().scope,
        StoreShardScopeV1::Project { .. }
            | StoreShardScopeV1::ProjectSessions { .. }
            | StoreShardScopeV1::ProfileMemory
            | StoreShardScopeV1::ProfileSessions
    ) {
        Ok(())
    } else {
        Err(StoreRuntimeRegistryFailure::UnsupportedShardScope)
    }
}

fn validate_project_code_scope(
    project_key: &StoreRuntimeKey,
    code_shard_id: &StoreShardIdV1,
) -> Result<(), StoreRuntimeRegistryFailure> {
    let StoreShardScopeV1::Project { project_id } = &project_key.shard_id().scope else {
        return Err(StoreRuntimeRegistryFailure::UnsupportedShardScope);
    };
    let StoreShardScopeV1::Code {
        project_id: code_project_id,
        ..
    } = &code_shard_id.scope
    else {
        return Err(StoreRuntimeRegistryFailure::UnsupportedShardScope);
    };
    if project_key.shard_id().brain_id != code_shard_id.brain_id
        || project_key.shard_id().profile_id != code_shard_id.profile_id
        || project_id != code_project_id
    {
        return Err(StoreRuntimeRegistryFailure::ResolverFailed {
            message: "code graph scope does not belong to the retained project authority"
                .to_owned(),
        });
    }
    Ok(())
}

fn code_graph_namespace(
    code_shard_id: &StoreShardIdV1,
    generation_id: &CodeGenerationId,
) -> Result<GraphNamespace, StoreRuntimeRegistryFailure> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.code-graph.scope.v1",
        code_shard_id,
        generation_id,
    ))
    .map_err(|error| StoreRuntimeRegistryFailure::ResolverFailed {
        message: format!("derive exact code graph namespace: {error}"),
    })?;
    GraphNamespace::new(format!("code-scope:{}", digest.as_str())).map_err(|error| {
        StoreRuntimeRegistryFailure::ResolverFailed {
            message: format!("construct exact code graph namespace: {error}"),
        }
    })
}

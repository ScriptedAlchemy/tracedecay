use std::sync::Arc;

use tracedecay_store::{
    RuntimeLeaseIdV1, RuntimeLeaseV1, RuntimeMaintenanceStateV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, StoreShardScopeV1,
};

use super::{
    RegistryEntry, RegistryState, StoreRuntimeKey, StoreRuntimeRegistry,
    StoreRuntimeRegistryFailure, utc_now,
};

#[derive(Clone)]
pub struct ProfileAuthorityPin {
    inner: Arc<ProfileAuthorityPinToken>,
}

struct ProfileAuthorityPinToken {
    registry: StoreRuntimeRegistry,
    binding: StoreRuntimeBindingV1,
    token: u64,
}

impl ProfileAuthorityPin {
    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.inner.binding
    }

    fn belongs_to(&self, registry: &StoreRuntimeRegistry) -> bool {
        Arc::ptr_eq(&self.inner.registry.inner, &registry.inner)
    }

    fn is_live_in(&self, state: &RegistryState) -> bool {
        state
            .profile_pin_tokens
            .get(&StoreRuntimeKey::from_binding(self.binding()))
            .is_some_and(|tokens| tokens.contains(&self.inner.token))
    }
}

impl std::fmt::Debug for ProfileAuthorityPin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProfileAuthorityPin")
            .field("binding", self.binding())
            .finish_non_exhaustive()
    }
}

impl PartialEq for ProfileAuthorityPin {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner.registry.inner, &other.inner.registry.inner)
            && self.binding() == other.binding()
            && self.inner.token == other.inner.token
    }
}

impl Eq for ProfileAuthorityPin {}

impl Drop for ProfileAuthorityPinToken {
    fn drop(&mut self) {
        self.registry
            .release_profile_authority_pin(&self.binding, self.token);
    }
}

#[derive(Clone, Debug)]
pub struct StoreRuntimeOpenRequest {
    pub(super) key: StoreRuntimeKey,
    pub(super) profile_authority: Option<ProfileAuthorityPin>,
    pub(super) database_authority: Option<crate::db::DatabaseAuthority>,
    pub(super) expected_opened_file_identity: Option<u64>,
    pub(super) mode: StoreRuntimeOpenMode,
    pub(super) access: StoreRuntimeAccessMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreRuntimeOpenMode {
    Existing,
    Initialize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreRuntimeAccessMode {
    ReadOnly,
    ReadWrite,
}

impl StoreRuntimeOpenRequest {
    pub fn new_authorized(
        shard_id: StoreShardIdV1,
        incarnation: StoreIncarnationV1,
        profile_authority: Option<ProfileAuthorityPin>,
        database_authority: crate::db::DatabaseAuthority,
    ) -> Self {
        Self {
            key: StoreRuntimeKey::new(shard_id, incarnation),
            profile_authority,
            database_authority: Some(database_authority),
            expected_opened_file_identity: None,
            mode: StoreRuntimeOpenMode::Existing,
            access: StoreRuntimeAccessMode::ReadWrite,
        }
    }

    pub fn new_initialize_authorized(
        shard_id: StoreShardIdV1,
        incarnation: StoreIncarnationV1,
        profile_authority: Option<ProfileAuthorityPin>,
        database_authority: crate::db::DatabaseAuthority,
    ) -> Self {
        Self {
            key: StoreRuntimeKey::new(shard_id, incarnation),
            profile_authority,
            database_authority: Some(database_authority),
            expected_opened_file_identity: None,
            mode: StoreRuntimeOpenMode::Initialize,
            access: StoreRuntimeAccessMode::ReadWrite,
        }
    }

    pub fn new_read_only(
        shard_id: StoreShardIdV1,
        incarnation: StoreIncarnationV1,
        profile_authority: Option<ProfileAuthorityPin>,
    ) -> Self {
        Self {
            key: StoreRuntimeKey::new(shard_id, incarnation),
            profile_authority,
            database_authority: None,
            expected_opened_file_identity: None,
            mode: StoreRuntimeOpenMode::Existing,
            access: StoreRuntimeAccessMode::ReadOnly,
        }
    }

    #[cfg(test)]
    pub fn new(
        shard_id: StoreShardIdV1,
        incarnation: StoreIncarnationV1,
        profile_authority: Option<ProfileAuthorityPin>,
    ) -> Self {
        Self {
            key: StoreRuntimeKey::new(shard_id, incarnation),
            profile_authority,
            database_authority: None,
            expected_opened_file_identity: None,
            mode: StoreRuntimeOpenMode::Existing,
            access: StoreRuntimeAccessMode::ReadWrite,
        }
    }

    pub fn key(&self) -> &StoreRuntimeKey {
        &self.key
    }

    #[must_use]
    pub fn require_opened_file_identity(mut self, expected: u64) -> Self {
        self.expected_opened_file_identity = Some(expected);
        self
    }
}

#[derive(Clone, Debug)]
pub enum StoreRuntimeLeaseAcquireResult {
    Acquired(Box<RuntimeLeaseV1>),
    Opening {
        key: Box<StoreRuntimeKey>,
    },
    Evicting {
        key: Box<StoreRuntimeKey>,
    },
    Missing {
        key: Box<StoreRuntimeKey>,
    },
    Fenced {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    Rejected(StoreRuntimeRegistryFailure),
}

#[derive(Clone, Debug)]
pub enum ProfileAuthorityPinResult {
    Pinned(ProfileAuthorityPin),
    Opening { key: Box<StoreRuntimeKey> },
    Missing { profile_shard: Box<StoreShardIdV1> },
    Rejected(StoreRuntimeRegistryFailure),
}

impl StoreRuntimeRegistry {
    pub fn acquire_lease(&self, lease: RuntimeLeaseV1) -> StoreRuntimeLeaseAcquireResult {
        if let Err(error) = lease.validate() {
            return StoreRuntimeLeaseAcquireResult::Rejected(
                StoreRuntimeRegistryFailure::InvalidLease {
                    message: error.to_string(),
                },
            );
        }
        let expected = lease.binding.clone();
        let key = StoreRuntimeKey::from_binding(&expected);
        let runtime = {
            let state = self.lock_state();
            match state.entries.get(&key) {
                Some(RegistryEntry::Ready(ready)) => {
                    let actual = ready.owner.binding();
                    if actual.authority_epoch != expected.authority_epoch {
                        return StoreRuntimeLeaseAcquireResult::Fenced {
                            expected: Box::new(expected),
                            actual: Box::new(actual.clone()),
                        };
                    }
                    Arc::clone(ready.owner.runtime())
                }
                Some(RegistryEntry::Opening(_)) => {
                    return StoreRuntimeLeaseAcquireResult::Opening { key: Box::new(key) };
                }
                Some(RegistryEntry::Evicting(_)) => {
                    return StoreRuntimeLeaseAcquireResult::Evicting { key: Box::new(key) };
                }
                Some(RegistryEntry::Retiring(_)) => {
                    return StoreRuntimeLeaseAcquireResult::Rejected(
                        StoreRuntimeRegistryFailure::RuntimeRetirementInProgress {
                            key: Box::new(key),
                        },
                    );
                }
                Some(RegistryEntry::Committing(_)) => {
                    return StoreRuntimeLeaseAcquireResult::Rejected(
                        StoreRuntimeRegistryFailure::RuntimeRetirementCommitting {
                            key: Box::new(key),
                        },
                    );
                }
                Some(RegistryEntry::Faulted(_)) => {
                    return StoreRuntimeLeaseAcquireResult::Rejected(
                        StoreRuntimeRegistryFailure::RuntimeRetirementFaulted {
                            key: Box::new(key),
                        },
                    );
                }
                Some(RegistryEntry::DurabilityUncertain(_)) => {
                    return StoreRuntimeLeaseAcquireResult::Rejected(
                        StoreRuntimeRegistryFailure::RuntimeRetirementDurabilityUncertain {
                            key: Box::new(key),
                        },
                    );
                }
                None => {
                    return StoreRuntimeLeaseAcquireResult::Missing { key: Box::new(key) };
                }
            }
        };
        match runtime.acquire_runtime_lease(lease.clone(), utc_now()) {
            Ok(acquired) if acquired.binding == lease.binding => {
                StoreRuntimeLeaseAcquireResult::Acquired(Box::new(acquired))
            }
            Ok(acquired) => StoreRuntimeLeaseAcquireResult::Rejected(
                StoreRuntimeRegistryFailure::LeaseBindingMismatch {
                    expected: Box::new(lease.binding),
                    actual: Box::new(acquired.binding),
                },
            ),
            Err(error) => StoreRuntimeLeaseAcquireResult::Rejected(
                StoreRuntimeRegistryFailure::LeaseRejected {
                    message: error.to_string(),
                },
            ),
        }
    }

    #[cfg(test)]
    pub(crate) fn release_lease(
        &self,
        binding: &StoreRuntimeBindingV1,
        lease_id: &RuntimeLeaseIdV1,
    ) -> bool {
        let key = StoreRuntimeKey::from_binding(binding);
        let runtime = {
            let state = self.lock_state();
            let Some(RegistryEntry::Ready(ready)) = state.entries.get(&key) else {
                return false;
            };
            if ready.owner.binding() != binding {
                return false;
            }
            Arc::clone(ready.owner.runtime())
        };
        runtime.release_runtime_lease(lease_id)
    }

    pub fn profile_authority_pin(
        &self,
        profile_shard: &StoreShardIdV1,
    ) -> ProfileAuthorityPinResult {
        if !matches!(profile_shard.scope, StoreShardScopeV1::Profile) {
            return ProfileAuthorityPinResult::Rejected(
                StoreRuntimeRegistryFailure::ProfileAuthorityShardIsNotProfile {
                    shard_id: Box::new(profile_shard.clone()),
                },
            );
        }
        let mut state = self.lock_state();
        if let Some(binding) = state.profile_authorities.get(profile_shard).cloned() {
            if let Err(failure) = require_ready_profile_runtime(&state, &binding) {
                return ProfileAuthorityPinResult::Rejected(failure);
            }
            let key = StoreRuntimeKey::from_binding(&binding);
            let token = state.next_profile_pin_token.checked_add(1).ok_or_else(|| {
                StoreRuntimeRegistryFailure::ProfilePinTokenExhausted {
                    key: Box::new(key.clone()),
                }
            });
            let token = match token {
                Ok(token) => token,
                Err(failure) => return ProfileAuthorityPinResult::Rejected(failure),
            };
            state.next_profile_pin_token = token;
            state
                .profile_pin_tokens
                .entry(key)
                .or_default()
                .insert(token);
            return ProfileAuthorityPinResult::Pinned(ProfileAuthorityPin {
                inner: Arc::new(ProfileAuthorityPinToken {
                    registry: self.clone(),
                    binding,
                    token,
                }),
            });
        }
        state
            .entries
            .iter()
            .find_map(|(key, entry)| {
                (key.shard_id == *profile_shard && matches!(entry, RegistryEntry::Opening(_))).then(
                    || ProfileAuthorityPinResult::Opening {
                        key: Box::new(key.clone()),
                    },
                )
            })
            .unwrap_or_else(|| ProfileAuthorityPinResult::Missing {
                profile_shard: Box::new(profile_shard.clone()),
            })
    }

    fn release_profile_authority_pin(&self, binding: &StoreRuntimeBindingV1, token: u64) {
        let key = StoreRuntimeKey::from_binding(binding);
        let mut state = self.lock_state();
        let Some(tokens) = state.profile_pin_tokens.get_mut(&key) else {
            return;
        };
        if !tokens.remove(&token) {
            return;
        }
        if tokens.is_empty() {
            state.profile_pin_tokens.remove(&key);
        }
    }
}

pub(super) fn validate_profile_authority(
    registry: &StoreRuntimeRegistry,
    state: &RegistryState,
    request: &StoreRuntimeOpenRequest,
) -> Result<(), StoreRuntimeRegistryFailure> {
    if request.key.is_profile() {
        return request
            .profile_authority
            .is_none()
            .then_some(())
            .ok_or_else(
                || StoreRuntimeRegistryFailure::ProfileAuthorityMustNotBeSupplied {
                    key: Box::new(request.key.clone()),
                },
            );
    }
    let pin = request.profile_authority.as_ref().ok_or_else(|| {
        StoreRuntimeRegistryFailure::ProfileAuthorityRequired {
            key: Box::new(request.key.clone()),
        }
    })?;
    let expected_profile = StoreShardIdV1::profile(
        request.key.shard_id.brain_id.clone(),
        request.key.shard_id.profile_id.clone(),
    );
    if pin.binding().shard_id != expected_profile {
        return Err(StoreRuntimeRegistryFailure::ProfileAuthorityShardMismatch {
            key: Box::new(request.key.clone()),
            pin: Box::new(pin.binding().clone()),
        });
    }
    if !pin.belongs_to(registry) || !pin.is_live_in(state) {
        return Err(StoreRuntimeRegistryFailure::ProfileAuthorityNotPinned {
            profile_shard: Box::new(expected_profile),
        });
    }
    let actual = state.profile_authorities.get(&expected_profile).ok_or(
        StoreRuntimeRegistryFailure::ProfileAuthorityNotPinned {
            profile_shard: Box::new(expected_profile),
        },
    )?;
    if actual != pin.binding() {
        return Err(StoreRuntimeRegistryFailure::ProfileAuthorityFenced {
            expected: Box::new(pin.binding().clone()),
            actual: Box::new(actual.clone()),
        });
    }
    require_ready_profile_runtime(state, actual)
}

fn require_ready_profile_runtime(
    state: &RegistryState,
    binding: &StoreRuntimeBindingV1,
) -> Result<(), StoreRuntimeRegistryFailure> {
    let key = StoreRuntimeKey::from_binding(binding);
    let ready = match state.entries.get(&key) {
        Some(RegistryEntry::Ready(ready)) => ready,
        Some(RegistryEntry::Retiring(_)) => {
            return Err(StoreRuntimeRegistryFailure::RuntimeRetirementInProgress {
                key: Box::new(key),
            });
        }
        Some(RegistryEntry::Committing(_)) => {
            return Err(StoreRuntimeRegistryFailure::RuntimeRetirementCommitting {
                key: Box::new(key),
            });
        }
        Some(RegistryEntry::Faulted(_)) => {
            return Err(StoreRuntimeRegistryFailure::RuntimeRetirementFaulted {
                key: Box::new(key),
            });
        }
        Some(RegistryEntry::DurabilityUncertain(_)) => {
            return Err(
                StoreRuntimeRegistryFailure::RuntimeRetirementDurabilityUncertain {
                    key: Box::new(key),
                },
            );
        }
        Some(RegistryEntry::Opening(_) | RegistryEntry::Evicting(_)) | None => {
            return Err(StoreRuntimeRegistryFailure::ProfileAuthorityNotPinned {
                profile_shard: Box::new(binding.shard_id.clone()),
            });
        }
    };
    let runtime_state = ready.owner.runtime().maintenance_state();
    if runtime_state != RuntimeMaintenanceStateV1::Ready {
        return Err(StoreRuntimeRegistryFailure::ProfileAuthorityUnavailable {
            binding: Box::new(binding.clone()),
            state: runtime_state,
        });
    }
    Ok(())
}

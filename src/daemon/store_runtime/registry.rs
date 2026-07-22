//! Canonical daemon registry for store runtimes.
//!
//! Entries are keyed only by typed shard identity and incarnation. Locator
//! resolution starts after an opening entry wins singleflight, and publication
//! retains exactly one concrete [`ShardRuntime`] for that binding.
//!
//! Dead-code allowance lives on the parent `store_runtime` module until every
//! live open routes through this registry.

#![allow(unused_imports)] // Re-exports remain the registry's crate-visible API surface.

mod attachment;
mod capacity;
mod leases;
mod open;
mod ports;

#[cfg(test)]
mod rusqlite_graph;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::UtcMicros;
use tracedecay_store::{
    AdmissionConfigV1, RuntimeMaintenanceStateV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreRuntimeRegistryPublicationV1, StoreShardIdV1, StoreShardScopeV1,
    VerifiedStoreLocatorV1,
};

use super::shard::ShardRuntime;
use super::telemetry::{RuntimeRegistryInventory, RuntimeRegistryInventoryEntry};

#[cfg(test)]
pub(crate) use attachment::EmptyPhysicalRuntimeAttachment;
pub(crate) use attachment::{
    PhysicalRuntimeAttachment, PhysicalRuntimeSnapshot, PublishedShardRuntime,
};
pub(crate) use capacity::{
    DEFAULT_PROJECT_CODE_OPEN_RUNTIMES, MAX_PROJECT_CODE_OPEN_RUNTIMES, StoreRuntimeRegistryConfig,
};
pub(crate) use leases::{
    ProfileAuthorityPin, ProfileAuthorityPinResult, StoreRuntimeLeaseAcquireResult,
    StoreRuntimeOpenRequest,
};
pub(crate) use open::{StoreRuntimeOpenBegin, StoreRuntimeOpenJoin, StoreRuntimeOpenResult};
pub(crate) use ports::{
    LifecycleShardRuntimePublisher, ResolvedStoreLocator, RuntimeLocatorRecord,
    ShardRuntimeBuildRequest, ShardRuntimePublisher, StoreRuntimeRegistryFuture,
    StoreRuntimeResolver,
};
#[cfg(test)]
pub(crate) use rusqlite_graph::ExplicitPrecutoverRusqliteGraphPublisher;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct StoreRuntimeKey {
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
}

impl StoreRuntimeKey {
    pub(crate) fn new(shard_id: StoreShardIdV1, incarnation: StoreIncarnationV1) -> Self {
        Self {
            shard_id,
            incarnation,
        }
    }

    pub(crate) fn shard_id(&self) -> &StoreShardIdV1 {
        &self.shard_id
    }

    pub(crate) const fn incarnation(&self) -> StoreIncarnationV1 {
        self.incarnation
    }

    fn from_binding(binding: &StoreRuntimeBindingV1) -> Self {
        Self::new(binding.shard_id.clone(), binding.incarnation)
    }

    fn is_profile(&self) -> bool {
        matches!(self.shard_id.scope, StoreShardScopeV1::Profile)
    }
}

#[derive(Clone)]
pub(crate) struct StoreRuntimeHandle {
    inner: Arc<StoreRuntimeHandleInner>,
}

struct StoreRuntimeHandleInner {
    publication: StoreRuntimeRegistryPublicationV1,
    runtime: Arc<ShardRuntime>,
    attachment: Arc<dyn PhysicalRuntimeAttachment>,
    locator: RuntimeLocatorRecord,
}

impl StoreRuntimeHandle {
    pub(crate) fn publication(&self) -> &StoreRuntimeRegistryPublicationV1 {
        &self.inner.publication
    }

    pub(crate) fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.inner.publication.binding
    }

    pub(crate) fn runtime(&self) -> &Arc<ShardRuntime> {
        &self.inner.runtime
    }

    pub(crate) fn locator(&self) -> &RuntimeLocatorRecord {
        &self.inner.locator
    }

    pub(crate) fn physical_snapshot(&self) -> PhysicalRuntimeSnapshot {
        self.inner.attachment.snapshot()
    }

    pub(crate) async fn dispatch_submit(
        &self,
        request: tracedecay_store::RuntimeSubmitRequestV1,
        probe: Arc<dyn tracedecay_store::RuntimeRequestProbeV1>,
    ) -> Result<tracedecay_store::RuntimeSubmitOutcomeV1, StoreRuntimeRegistryFailure> {
        if request.binding() != self.binding() {
            return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                expected: Box::new(self.binding().clone()),
                actual: Box::new(request.binding().clone()),
            });
        }
        self.inner.attachment.dispatch_submit(request, probe).await
    }

    pub(crate) fn dispatch_read(
        &self,
        request: tracedecay_store::RuntimeReadRequestV1,
        probe: &dyn tracedecay_store::RuntimeRequestProbeV1,
    ) -> Result<tracedecay_store::RuntimeReadOutcomeV1, StoreRuntimeRegistryFailure> {
        if request.binding() != self.binding() {
            return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                expected: Box::new(self.binding().clone()),
                actual: Box::new(request.binding().clone()),
            });
        }
        self.inner.attachment.dispatch_read(request, probe)
    }

    fn is_exclusively_held_by_registry(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }
}

impl fmt::Debug for StoreRuntimeHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreRuntimeHandle")
            .field("publication", &self.inner.publication)
            .field("locator_key", self.inner.locator.key())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StoreRuntimeRegistryFailure {
    InvalidProjectCodeBudget {
        requested: usize,
        maximum: usize,
    },
    ProjectCodeBudgetExhausted {
        limit: usize,
    },
    RuntimeEvictionInProgress {
        key: Box<StoreRuntimeKey>,
    },
    AuthorityEpochExhausted,
    OpenAttemptExhausted,
    EvictionAttemptExhausted,
    EvictionReservationLost {
        key: Box<StoreRuntimeKey>,
    },
    PublicationIdExhausted,
    ResolverFailed {
        message: String,
    },
    UnsupportedShardScope,
    NetworkFilesystemUnavailable {
        filesystem_type: String,
    },
    FilesystemLocalityUnavailable {
        filesystem_type: String,
    },
    LocatorIdentityMismatch {
        key: Box<StoreRuntimeKey>,
        locator: Box<VerifiedStoreLocatorV1>,
    },
    RuntimeBindingMismatch {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    RuntimeLifecycleFailed {
        message: String,
    },
    PhysicalRuntimeFailed {
        operation: &'static str,
        message: String,
    },
    PhysicalRuntimeNotDrained {
        snapshot: PhysicalRuntimeSnapshot,
    },
    ProfileAuthorityRequired {
        key: Box<StoreRuntimeKey>,
    },
    ProfileAuthorityMustNotBeSupplied {
        key: Box<StoreRuntimeKey>,
    },
    ProfileAuthorityShardMismatch {
        key: Box<StoreRuntimeKey>,
        pin: Box<StoreRuntimeBindingV1>,
    },
    ProfileAuthorityNotPinned {
        profile_shard: Box<StoreShardIdV1>,
    },
    ProfileAuthorityFenced {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    ProfileAuthorityUnavailable {
        binding: Box<StoreRuntimeBindingV1>,
        state: RuntimeMaintenanceStateV1,
    },
    ProfileAuthorityShardIsNotProfile {
        shard_id: Box<StoreShardIdV1>,
    },
    InvalidLease {
        message: String,
    },
    LeaseBindingMismatch {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    LeaseRejected {
        message: String,
    },
    OpenTaskAbandoned {
        key: Box<StoreRuntimeKey>,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum StoreRuntimeLookup {
    Ready(StoreRuntimeHandle),
    Opening {
        key: Box<StoreRuntimeKey>,
    },
    Evicting {
        key: Box<StoreRuntimeKey>,
    },
    Missing {
        key: Box<StoreRuntimeKey>,
    },
    WrongIncarnation {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
    Fenced {
        expected: Box<StoreRuntimeBindingV1>,
        actual: Box<StoreRuntimeBindingV1>,
    },
}

struct ReadyRuntime {
    handle: StoreRuntimeHandle,
}

struct EvictingRuntime {
    attempt: u64,
    handle: StoreRuntimeHandle,
}

enum RegistryEntry {
    Opening(open::OpeningRuntime),
    Ready(ReadyRuntime),
    Evicting(EvictingRuntime),
}

#[derive(Default)]
struct RegistryState {
    entries: BTreeMap<StoreRuntimeKey, RegistryEntry>,
    profile_authorities: BTreeMap<StoreShardIdV1, StoreRuntimeBindingV1>,
    next_open_attempt: u64,
    next_eviction_attempt: u64,
    next_publication: u64,
}

struct StoreRuntimeRegistryInner {
    resolver: Arc<dyn StoreRuntimeResolver>,
    publisher: Arc<dyn ShardRuntimePublisher>,
    config: StoreRuntimeRegistryConfig,
    state: Mutex<RegistryState>,
}

#[derive(Clone)]
pub(crate) struct StoreRuntimeRegistry {
    inner: Arc<StoreRuntimeRegistryInner>,
}

impl StoreRuntimeRegistry {
    pub(crate) fn new(
        resolver: Arc<dyn StoreRuntimeResolver>,
        publisher: Arc<dyn ShardRuntimePublisher>,
    ) -> Self {
        Self {
            inner: Arc::new(StoreRuntimeRegistryInner {
                resolver,
                publisher,
                config: StoreRuntimeRegistryConfig::default(),
                state: Mutex::new(RegistryState::default()),
            }),
        }
    }

    pub(crate) fn with_config(
        resolver: Arc<dyn StoreRuntimeResolver>,
        publisher: Arc<dyn ShardRuntimePublisher>,
        config: StoreRuntimeRegistryConfig,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        Self::with_config_and_authority_epoch_floor(resolver, publisher, config, None)
    }

    pub(crate) fn with_config_and_authority_epoch_floor(
        resolver: Arc<dyn StoreRuntimeResolver>,
        publisher: Arc<dyn ShardRuntimePublisher>,
        config: StoreRuntimeRegistryConfig,
        authority_epoch_floor: Option<StoreAuthorityEpochV1>,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        config.validate()?;
        if let Some(floor) = authority_epoch_floor {
            open::retain_authority_epoch_floor(floor);
        }
        Ok(Self {
            inner: Arc::new(StoreRuntimeRegistryInner {
                resolver,
                publisher,
                config,
                state: Mutex::new(RegistryState::default()),
            }),
        })
    }

    pub(crate) fn lookup(&self, expected: &StoreRuntimeBindingV1) -> StoreRuntimeLookup {
        let key = StoreRuntimeKey::from_binding(expected);
        let state = self.lock_state();
        match state.entries.get(&key) {
            Some(RegistryEntry::Ready(ready)) => {
                let actual = ready.handle.binding();
                if actual.authority_epoch == expected.authority_epoch {
                    StoreRuntimeLookup::Ready(ready.handle.clone())
                } else {
                    StoreRuntimeLookup::Fenced {
                        expected: Box::new(expected.clone()),
                        actual: Box::new(actual.clone()),
                    }
                }
            }
            Some(RegistryEntry::Opening(_)) => StoreRuntimeLookup::Opening { key: Box::new(key) },
            Some(RegistryEntry::Evicting(_)) => StoreRuntimeLookup::Evicting { key: Box::new(key) },
            None => state
                .entries
                .iter()
                .find_map(|(candidate, entry)| {
                    (candidate.shard_id == expected.shard_id
                        && candidate.incarnation != expected.incarnation)
                        .then(|| match entry {
                            RegistryEntry::Ready(ready) => {
                                Some(StoreRuntimeLookup::WrongIncarnation {
                                    expected: Box::new(expected.clone()),
                                    actual: Box::new(ready.handle.binding().clone()),
                                })
                            }
                            RegistryEntry::Opening(_) => None,
                            RegistryEntry::Evicting(evicting) => {
                                Some(StoreRuntimeLookup::WrongIncarnation {
                                    expected: Box::new(expected.clone()),
                                    actual: Box::new(evicting.handle.binding().clone()),
                                })
                            }
                        })
                        .flatten()
                })
                .unwrap_or(StoreRuntimeLookup::Missing { key: Box::new(key) }),
        }
    }

    pub(crate) fn inventory(
        &self,
        admission: AdmissionConfigV1,
        global_queued_bytes: u64,
    ) -> RuntimeRegistryInventory {
        let handles = {
            let state = self.lock_state();
            state
                .entries
                .values()
                .filter_map(|entry| match entry {
                    RegistryEntry::Ready(ready) => Some(ready.handle.clone()),
                    RegistryEntry::Opening(_) => None,
                    RegistryEntry::Evicting(evicting) => Some(evicting.handle.clone()),
                })
                .collect::<Vec<_>>()
        };
        let entries = handles
            .into_iter()
            .map(|handle| {
                let mut observation = handle.runtime().observe(self.inner.config.eviction_idle());
                let physical = handle.physical_snapshot();
                observation.health.writer_present |= physical.writer_present;
                observation.health.queued_operations = observation
                    .health
                    .queued_operations
                    .saturating_add(physical.queued_operations);
                observation.health.queued_bytes = observation
                    .health
                    .queued_bytes
                    .saturating_add(physical.queued_bytes);
                observation.health.wal_bytes = physical.wal_bytes;
                observation.health.memory_estimate_bytes = physical.memory_estimate_bytes;
                if !physical.healthy
                    && observation.health.health != super::shard::ShardRuntimeHealth::Faulted
                {
                    observation.health.health = super::shard::ShardRuntimeHealth::Degraded;
                }
                let mut entry = RuntimeRegistryInventoryEntry::from(observation);
                entry.physical = physical;
                entry
            })
            .collect();
        RuntimeRegistryInventory {
            admission,
            global_queued_bytes,
            entries,
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, RegistryState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn utc_now() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

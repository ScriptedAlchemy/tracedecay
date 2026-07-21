//! Canonical daemon registry for store runtimes.
//!
//! Entries are keyed only by typed shard identity and incarnation. Locator
//! resolution starts after an opening entry wins singleflight, and publication
//! retains exactly one concrete [`ShardRuntime`] for that binding.

#![allow(dead_code)] // S3 lands before all daemon call sites route through this registry.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::watch;
use tracedecay_domain::UtcMicros;
use tracedecay_store::{
    AdmissionConfigV1, RuntimeLeaseIdV1, RuntimeLeaseV1, RuntimeMaintenanceStateV1,
    RuntimePublicationIdV1, StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1,
    StoreRuntimeRegistryPublicationV1, StoreShardIdV1, StoreShardScopeV1, VerifiedStoreLocatorV1,
};

use super::shard::{ShardRuntime, ShardRuntimeError};
use super::telemetry::{RuntimeRegistryInventory, RuntimeRegistryInventoryEntry};

pub(crate) const MAX_PROJECT_CODE_OPEN_RUNTIMES: usize = 8;
pub(crate) const DEFAULT_PROJECT_CODE_OPEN_RUNTIMES: usize = 4;

/// Process-wide fencing source. A daemon may rebuild registries, but it must
/// never reuse an epoch while the process remains alive.
static PROCESS_AUTHORITY_EPOCH: AtomicU64 = AtomicU64::new(0);

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedStoreLocator {
    verified: VerifiedStoreLocatorV1,
    path: PathBuf,
}

impl ResolvedStoreLocator {
    pub(super) fn new(verified: VerifiedStoreLocatorV1, path: PathBuf) -> Self {
        Self { verified, path }
    }

    pub(crate) fn verified(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    fn matches(&self, key: &StoreRuntimeKey) -> bool {
        self.verified.shard_id == key.shard_id && self.verified.incarnation == key.incarnation
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeLocatorRecord {
    key: StoreRuntimeKey,
    locator: ResolvedStoreLocator,
}

impl RuntimeLocatorRecord {
    fn new(key: StoreRuntimeKey, locator: ResolvedStoreLocator) -> Self {
        Self { key, locator }
    }

    pub(crate) fn key(&self) -> &StoreRuntimeKey {
        &self.key
    }

    pub(crate) fn verified(&self) -> &VerifiedStoreLocatorV1 {
        self.locator.verified()
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        self.locator.path()
    }
}

pub(crate) type StoreRuntimeRegistryFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Identity-first locator capability. It never receives a client alias.
pub(crate) trait StoreRuntimeResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>;
}

/// Narrow S4 extension point. Publishers may attach driver resources, but must
/// return the concrete runtime object that the registry makes canonical.
pub(crate) trait ShardRuntimePublisher: Send + Sync {
    fn publish(
        &self,
        request: ShardRuntimeBuildRequest,
    ) -> StoreRuntimeRegistryFuture<'_, Result<Arc<ShardRuntime>, StoreRuntimeRegistryFailure>>;
}

/// S3 publisher: creates lifecycle/accounting authority only and opens no DB.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LifecycleShardRuntimePublisher;

impl ShardRuntimePublisher for LifecycleShardRuntimePublisher {
    fn publish(
        &self,
        request: ShardRuntimeBuildRequest,
    ) -> StoreRuntimeRegistryFuture<'_, Result<Arc<ShardRuntime>, StoreRuntimeRegistryFailure>>
    {
        Box::pin(async move {
            let pinned_profile =
                matches!(request.binding.shard_id.scope, StoreShardScopeV1::Profile);
            let runtime = Arc::new(ShardRuntime::new(request.binding, pinned_profile));
            runtime
                .transition(RuntimeMaintenanceStateV1::Opening)
                .and_then(|()| runtime.transition(RuntimeMaintenanceStateV1::Ready))
                .map_err(runtime_lifecycle_failure)?;
            Ok(runtime)
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ShardRuntimeBuildRequest {
    binding: StoreRuntimeBindingV1,
    locator: RuntimeLocatorRecord,
}

impl ShardRuntimeBuildRequest {
    fn new(binding: StoreRuntimeBindingV1, locator: RuntimeLocatorRecord) -> Self {
        Self { binding, locator }
    }

    pub(crate) fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub(crate) fn locator(&self) -> &RuntimeLocatorRecord {
        &self.locator
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StoreRuntimeRegistryConfig {
    project_code_open_runtime_budget: usize,
    eviction_idle: Duration,
}

impl StoreRuntimeRegistryConfig {
    pub(crate) fn new(
        project_code_open_runtime_budget: usize,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        Self::with_eviction_idle(project_code_open_runtime_budget, Duration::ZERO)
    }

    pub(crate) fn with_eviction_idle(
        project_code_open_runtime_budget: usize,
        eviction_idle: Duration,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        if !(1..=MAX_PROJECT_CODE_OPEN_RUNTIMES).contains(&project_code_open_runtime_budget) {
            return Err(StoreRuntimeRegistryFailure::InvalidProjectCodeBudget {
                requested: project_code_open_runtime_budget,
                maximum: MAX_PROJECT_CODE_OPEN_RUNTIMES,
            });
        }
        Ok(Self {
            project_code_open_runtime_budget,
            eviction_idle,
        })
    }

    pub(crate) const fn project_code_open_runtime_budget(self) -> usize {
        self.project_code_open_runtime_budget
    }
}

impl Default for StoreRuntimeRegistryConfig {
    fn default() -> Self {
        Self {
            project_code_open_runtime_budget: DEFAULT_PROJECT_CODE_OPEN_RUNTIMES,
            eviction_idle: Duration::ZERO,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProfileAuthorityPin {
    binding: Arc<StoreRuntimeBindingV1>,
}

impl ProfileAuthorityPin {
    pub(crate) fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }
}

#[derive(Clone, Debug)]
pub(crate) struct StoreRuntimeOpenRequest {
    key: StoreRuntimeKey,
    profile_authority: Option<ProfileAuthorityPin>,
}

impl StoreRuntimeOpenRequest {
    pub(crate) fn new(
        shard_id: StoreShardIdV1,
        incarnation: StoreIncarnationV1,
        profile_authority: Option<ProfileAuthorityPin>,
    ) -> Self {
        Self {
            key: StoreRuntimeKey::new(shard_id, incarnation),
            profile_authority,
        }
    }

    pub(crate) fn key(&self) -> &StoreRuntimeKey {
        &self.key
    }
}

#[derive(Clone)]
pub(crate) struct StoreRuntimeHandle {
    inner: Arc<StoreRuntimeHandleInner>,
}

struct StoreRuntimeHandleInner {
    publication: StoreRuntimeRegistryPublicationV1,
    runtime: Arc<ShardRuntime>,
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
    AuthorityEpochExhausted,
    OpenAttemptExhausted,
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

pub(crate) enum StoreRuntimeOpenBegin {
    Ready(StoreRuntimeHandle),
    Started(StoreRuntimeOpenJoin),
    Joined(StoreRuntimeOpenJoin),
    Rejected(StoreRuntimeRegistryFailure),
}

impl StoreRuntimeOpenBegin {
    pub(crate) async fn wait(self) -> StoreRuntimeOpenResult {
        match self {
            Self::Ready(handle) => StoreRuntimeOpenResult::Published(handle),
            Self::Started(join) | Self::Joined(join) => join.wait().await,
            Self::Rejected(failure) => StoreRuntimeOpenResult::Failed(failure),
        }
    }
}

impl fmt::Debug for StoreRuntimeOpenBegin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(handle) => formatter.debug_tuple("Ready").field(handle).finish(),
            Self::Started(join) => formatter.debug_tuple("Started").field(join).finish(),
            Self::Joined(join) => formatter.debug_tuple("Joined").field(join).finish(),
            Self::Rejected(failure) => formatter.debug_tuple("Rejected").field(failure).finish(),
        }
    }
}

pub(crate) struct StoreRuntimeOpenJoin {
    key: Box<StoreRuntimeKey>,
    updates: watch::Receiver<OpenState>,
}

impl StoreRuntimeOpenJoin {
    async fn wait(mut self) -> StoreRuntimeOpenResult {
        loop {
            let current = self.updates.borrow().clone();
            match current {
                OpenState::Opening => {
                    if self.updates.changed().await.is_err() {
                        return StoreRuntimeOpenResult::Failed(
                            StoreRuntimeRegistryFailure::OpenTaskAbandoned {
                                key: self.key.clone(),
                            },
                        );
                    }
                }
                OpenState::Published(handle) => return StoreRuntimeOpenResult::Published(handle),
                OpenState::Failed(failure) => return StoreRuntimeOpenResult::Failed(failure),
            }
        }
    }
}

impl fmt::Debug for StoreRuntimeOpenJoin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreRuntimeOpenJoin")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum StoreRuntimeOpenResult {
    Published(StoreRuntimeHandle),
    Failed(StoreRuntimeRegistryFailure),
}

#[derive(Clone, Debug)]
pub(crate) enum StoreRuntimeLookup {
    Ready(StoreRuntimeHandle),
    Opening {
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

#[derive(Clone, Debug)]
pub(crate) enum StoreRuntimeLeaseAcquireResult {
    Acquired(Box<RuntimeLeaseV1>),
    Opening {
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
pub(crate) enum ProfileAuthorityPinResult {
    Pinned(ProfileAuthorityPin),
    Opening { key: Box<StoreRuntimeKey> },
    Missing { profile_shard: Box<StoreShardIdV1> },
    Rejected(StoreRuntimeRegistryFailure),
}

#[derive(Clone)]
enum OpenState {
    Opening,
    Published(StoreRuntimeHandle),
    Failed(StoreRuntimeRegistryFailure),
}

struct OpeningRuntime {
    attempt: u64,
    updates: watch::Sender<OpenState>,
}

struct ReadyRuntime {
    handle: StoreRuntimeHandle,
}

enum RegistryEntry {
    Opening(OpeningRuntime),
    Ready(ReadyRuntime),
}

#[derive(Default)]
struct RegistryState {
    entries: BTreeMap<StoreRuntimeKey, RegistryEntry>,
    profile_authorities: BTreeMap<StoreShardIdV1, StoreRuntimeBindingV1>,
    next_open_attempt: u64,
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
        StoreRuntimeRegistryConfig::with_eviction_idle(
            config.project_code_open_runtime_budget,
            config.eviction_idle,
        )?;
        if let Some(floor) = authority_epoch_floor {
            PROCESS_AUTHORITY_EPOCH.fetch_max(floor.get(), Ordering::AcqRel);
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

    pub(crate) fn begin_or_join_open(
        &self,
        request: &StoreRuntimeOpenRequest,
    ) -> StoreRuntimeOpenBegin {
        let key = request.key.clone();
        let (binding, attempt, updates, join) = {
            let mut state = self.lock_state();
            if let Err(failure) = validate_profile_authority(&state, request) {
                return StoreRuntimeOpenBegin::Rejected(failure);
            }
            if let Some(entry) = state.entries.get(&key) {
                return match entry {
                    RegistryEntry::Ready(ready) => {
                        StoreRuntimeOpenBegin::Ready(ready.handle.clone())
                    }
                    RegistryEntry::Opening(opening) => {
                        StoreRuntimeOpenBegin::Joined(StoreRuntimeOpenJoin {
                            key: Box::new(key),
                            updates: opening.updates.subscribe(),
                        })
                    }
                };
            }
            if !key.is_profile() && !self.reserve_project_code_capacity(&mut state) {
                return StoreRuntimeOpenBegin::Rejected(
                    StoreRuntimeRegistryFailure::ProjectCodeBudgetExhausted {
                        limit: self.inner.config.project_code_open_runtime_budget,
                    },
                );
            }

            let authority_epoch = match allocate_authority_epoch() {
                Ok(epoch) => epoch,
                Err(failure) => return StoreRuntimeOpenBegin::Rejected(failure),
            };
            let Some(attempt) = allocate_counter(&mut state.next_open_attempt) else {
                return StoreRuntimeOpenBegin::Rejected(
                    StoreRuntimeRegistryFailure::OpenAttemptExhausted,
                );
            };
            let binding =
                StoreRuntimeBindingV1::new(key.shard_id.clone(), key.incarnation, authority_epoch);
            let (updates, receiver) = watch::channel(OpenState::Opening);
            state.entries.insert(
                key.clone(),
                RegistryEntry::Opening(OpeningRuntime {
                    attempt,
                    updates: updates.clone(),
                }),
            );
            let join = StoreRuntimeOpenJoin {
                key: Box::new(key.clone()),
                updates: receiver,
            };
            (binding, attempt, updates, join)
        };

        let registry = self.clone();
        tokio::spawn(async move {
            let guard = OpenAttemptGuard::new(registry.clone(), key.clone(), attempt, updates);
            let outcome = registry.build_runtime(&key, binding).await;
            guard.complete(outcome);
        });
        StoreRuntimeOpenBegin::Started(join)
    }

    pub(crate) async fn open(&self, request: StoreRuntimeOpenRequest) -> StoreRuntimeOpenResult {
        self.begin_or_join_open(&request).wait().await
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
                        })
                        .flatten()
                })
                .unwrap_or(StoreRuntimeLookup::Missing { key: Box::new(key) }),
        }
    }

    pub(crate) fn acquire_lease(&self, lease: RuntimeLeaseV1) -> StoreRuntimeLeaseAcquireResult {
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
                    let actual = ready.handle.binding();
                    if actual.authority_epoch != expected.authority_epoch {
                        return StoreRuntimeLeaseAcquireResult::Fenced {
                            expected: Box::new(expected),
                            actual: Box::new(actual.clone()),
                        };
                    }
                    Arc::clone(ready.handle.runtime())
                }
                Some(RegistryEntry::Opening(_)) => {
                    return StoreRuntimeLeaseAcquireResult::Opening { key: Box::new(key) };
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
            if ready.handle.binding() != binding {
                return false;
            }
            Arc::clone(ready.handle.runtime())
        };
        runtime.release_runtime_lease(lease_id)
    }

    pub(crate) fn profile_authority_pin(
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
        let state = self.lock_state();
        if let Some(binding) = state.profile_authorities.get(profile_shard) {
            if let Err(failure) = require_ready_profile_runtime(&state, binding) {
                return ProfileAuthorityPinResult::Rejected(failure);
            }
            return ProfileAuthorityPinResult::Pinned(ProfileAuthorityPin {
                binding: Arc::new(binding.clone()),
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

    /// Captures each concrete runtime once; telemetry projection performs the
    /// deterministic detail bound and full aggregation.
    pub(crate) fn inventory(
        &self,
        admission: AdmissionConfigV1,
        global_queued_bytes: u64,
    ) -> RuntimeRegistryInventory {
        let runtimes = {
            let state = self.lock_state();
            state
                .entries
                .values()
                .filter_map(|entry| match entry {
                    RegistryEntry::Ready(ready) => Some(Arc::clone(ready.handle.runtime())),
                    RegistryEntry::Opening(_) => None,
                })
                .collect::<Vec<_>>()
        };
        let entries = runtimes
            .into_iter()
            .map(|runtime| {
                RuntimeRegistryInventoryEntry::from(
                    runtime.observe(self.inner.config.eviction_idle),
                )
            })
            .collect();
        RuntimeRegistryInventory {
            admission,
            global_queued_bytes,
            entries,
        }
    }

    fn reserve_project_code_capacity(&self, state: &mut RegistryState) -> bool {
        let occupied = state.entries.keys().filter(|key| !key.is_profile()).count();
        if occupied < self.inner.config.project_code_open_runtime_budget {
            return true;
        }
        let candidate = state.entries.iter().find_map(|(key, entry)| {
            let RegistryEntry::Ready(ready) = entry else {
                return None;
            };
            (!key.is_profile()
                && ready.handle.is_exclusively_held_by_registry()
                && Arc::strong_count(ready.handle.runtime()) == 1
                && ready
                    .handle
                    .runtime()
                    .eviction_eligibility(self.inner.config.eviction_idle)
                    .is_eligible())
            .then(|| key.clone())
        });
        let Some(candidate) = candidate else {
            return false;
        };
        let closed = match state.entries.get(&candidate) {
            Some(RegistryEntry::Ready(ready)) => ready
                .handle
                .runtime()
                .transition(RuntimeMaintenanceStateV1::Draining)
                .and_then(|()| {
                    ready
                        .handle
                        .runtime()
                        .transition(RuntimeMaintenanceStateV1::Closed)
                })
                .is_ok(),
            _ => false,
        };
        closed && state.entries.remove(&candidate).is_some()
    }

    fn lock_state(&self) -> MutexGuard<'_, RegistryState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn build_runtime<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        binding: StoreRuntimeBindingV1,
    ) -> StoreRuntimeRegistryFuture<
        'a,
        Result<(Arc<ShardRuntime>, RuntimeLocatorRecord), StoreRuntimeRegistryFailure>,
    > {
        Box::pin(async move {
            let resolved = self.inner.resolver.resolve(key).await?;
            if !resolved.matches(key) {
                return Err(StoreRuntimeRegistryFailure::LocatorIdentityMismatch {
                    key: Box::new(key.clone()),
                    locator: Box::new(resolved.verified().clone()),
                });
            }
            let locator = RuntimeLocatorRecord::new(key.clone(), resolved);
            let runtime = self
                .inner
                .publisher
                .publish(ShardRuntimeBuildRequest::new(
                    binding.clone(),
                    locator.clone(),
                ))
                .await?;
            if runtime.binding() != &binding {
                return Err(StoreRuntimeRegistryFailure::RuntimeBindingMismatch {
                    expected: Box::new(binding),
                    actual: Box::new(runtime.binding().clone()),
                });
            }
            Ok((runtime, locator))
        })
    }
}

struct OpenAttemptGuard {
    registry: StoreRuntimeRegistry,
    key: StoreRuntimeKey,
    attempt: u64,
    updates: watch::Sender<OpenState>,
    armed: bool,
}

impl OpenAttemptGuard {
    fn new(
        registry: StoreRuntimeRegistry,
        key: StoreRuntimeKey,
        attempt: u64,
        updates: watch::Sender<OpenState>,
    ) -> Self {
        Self {
            registry,
            key,
            attempt,
            updates,
            armed: true,
        }
    }

    fn complete(
        mut self,
        outcome: Result<(Arc<ShardRuntime>, RuntimeLocatorRecord), StoreRuntimeRegistryFailure>,
    ) {
        let mut state = self.registry.lock_state();
        let still_opening = matches!(
            state.entries.get(&self.key),
            Some(RegistryEntry::Opening(opening)) if opening.attempt == self.attempt
        );
        if !still_opening {
            self.armed = false;
            return;
        }

        match outcome {
            Ok((runtime, locator)) => {
                match allocate_publication(&mut state, runtime.binding().clone()) {
                    Ok(publication) => {
                        let handle = StoreRuntimeHandle {
                            inner: Arc::new(StoreRuntimeHandleInner {
                                publication,
                                runtime,
                                locator,
                            }),
                        };
                        if self.key.is_profile() {
                            state
                                .profile_authorities
                                .insert(self.key.shard_id.clone(), handle.binding().clone());
                        }
                        state.entries.insert(
                            self.key.clone(),
                            RegistryEntry::Ready(ReadyRuntime {
                                handle: handle.clone(),
                            }),
                        );
                        self.updates.send_replace(OpenState::Published(handle));
                    }
                    Err(failure) => {
                        state.entries.remove(&self.key);
                        self.updates.send_replace(OpenState::Failed(failure));
                    }
                }
            }
            Err(failure) => {
                state.entries.remove(&self.key);
                self.updates.send_replace(OpenState::Failed(failure));
            }
        }
        self.armed = false;
    }
}

impl Drop for OpenAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let failure = StoreRuntimeRegistryFailure::OpenTaskAbandoned {
            key: Box::new(self.key.clone()),
        };
        let mut state = self.registry.lock_state();
        let still_opening = matches!(
            state.entries.get(&self.key),
            Some(RegistryEntry::Opening(opening)) if opening.attempt == self.attempt
        );
        if still_opening {
            state.entries.remove(&self.key);
            self.updates.send_replace(OpenState::Failed(failure));
        }
    }
}

fn validate_profile_authority(
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
    if pin.binding.shard_id != expected_profile {
        return Err(StoreRuntimeRegistryFailure::ProfileAuthorityShardMismatch {
            key: Box::new(request.key.clone()),
            pin: Box::new(pin.binding.as_ref().clone()),
        });
    }
    let actual = state.profile_authorities.get(&expected_profile).ok_or(
        StoreRuntimeRegistryFailure::ProfileAuthorityNotPinned {
            profile_shard: Box::new(expected_profile),
        },
    )?;
    if actual != pin.binding.as_ref() {
        return Err(StoreRuntimeRegistryFailure::ProfileAuthorityFenced {
            expected: Box::new(pin.binding.as_ref().clone()),
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
    let Some(RegistryEntry::Ready(ready)) = state.entries.get(&key) else {
        return Err(StoreRuntimeRegistryFailure::ProfileAuthorityNotPinned {
            profile_shard: Box::new(binding.shard_id.clone()),
        });
    };
    let runtime_state = ready.handle.runtime().maintenance_state();
    if runtime_state != RuntimeMaintenanceStateV1::Ready {
        return Err(StoreRuntimeRegistryFailure::ProfileAuthorityUnavailable {
            binding: Box::new(binding.clone()),
            state: runtime_state,
        });
    }
    Ok(())
}

fn allocate_authority_epoch() -> Result<StoreAuthorityEpochV1, StoreRuntimeRegistryFailure> {
    let previous = PROCESS_AUTHORITY_EPOCH
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        })
        .map_err(|_| StoreRuntimeRegistryFailure::AuthorityEpochExhausted)?;
    StoreAuthorityEpochV1::new(previous + 1)
        .map_err(|_| StoreRuntimeRegistryFailure::AuthorityEpochExhausted)
}

fn allocate_publication(
    state: &mut RegistryState,
    binding: StoreRuntimeBindingV1,
) -> Result<StoreRuntimeRegistryPublicationV1, StoreRuntimeRegistryFailure> {
    let sequence = allocate_counter(&mut state.next_publication)
        .ok_or(StoreRuntimeRegistryFailure::PublicationIdExhausted)?;
    let publication_id = RuntimePublicationIdV1::new(format!("runtime-publication-{sequence}"))
        .map_err(|_| StoreRuntimeRegistryFailure::PublicationIdExhausted)?;
    Ok(StoreRuntimeRegistryPublicationV1 {
        publication_id,
        binding,
        published_at: utc_now(),
    })
}

fn allocate_counter(counter: &mut u64) -> Option<u64> {
    *counter = counter.checked_add(1)?;
    Some(*counter)
}

fn runtime_lifecycle_failure(error: ShardRuntimeError) -> StoreRuntimeRegistryFailure {
    StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
        message: error.to_string(),
    }
}

fn utc_now() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::sync::Weak;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize};

    use tracedecay_domain::{BrainId, LocatorDigest, ProjectId, UserProfileId};
    use tracedecay_store::{RuntimeLeaseIdV1, StoreClientIdV1};

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn incarnation() -> StoreIncarnationV1 {
        StoreIncarnationV1::new(1).unwrap()
    }

    fn profile_shard() -> StoreShardIdV1 {
        StoreShardIdV1::profile(
            id::<BrainId>("brain.registry"),
            id::<UserProfileId>("profile.registry"),
        )
    }

    fn project_shard(project: &str) -> StoreShardIdV1 {
        StoreShardIdV1::project(
            id::<BrainId>("brain.registry"),
            id::<UserProfileId>("profile.registry"),
            id::<ProjectId>(project),
        )
    }

    #[derive(Default)]
    struct TestResolver {
        calls: AtomicUsize,
    }

    impl StoreRuntimeResolver for TestResolver {
        fn resolve<'a>(
            &'a self,
            key: &'a StoreRuntimeKey,
        ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
        {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let locator = VerifiedStoreLocatorV1::new(
                key.shard_id.clone(),
                key.incarnation,
                LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            );
            Box::pin(async move {
                Ok(ResolvedStoreLocator::new(
                    locator,
                    PathBuf::from(format!("/verified/{call}")),
                ))
            })
        }
    }

    #[derive(Default)]
    struct TestPublisher {
        calls: AtomicUsize,
        block: AtomicBool,
        mode: AtomicU8, // 0 success, 1 failure
        release: tokio::sync::Notify,
        runtimes: Mutex<Vec<Weak<ShardRuntime>>>,
        bindings: Mutex<Vec<StoreRuntimeBindingV1>>,
    }

    impl TestPublisher {
        fn runtime(&self, index: usize) -> Arc<ShardRuntime> {
            self.runtimes
                .lock()
                .unwrap()
                .get(index)
                .unwrap()
                .upgrade()
                .unwrap()
        }
    }

    impl ShardRuntimePublisher for TestPublisher {
        fn publish<'a>(
            &'a self,
            request: ShardRuntimeBuildRequest,
        ) -> StoreRuntimeRegistryFuture<'a, Result<Arc<ShardRuntime>, StoreRuntimeRegistryFailure>>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.bindings.lock().unwrap().push(request.binding.clone());
            Box::pin(async move {
                if self.block.load(Ordering::SeqCst) {
                    self.release.notified().await;
                }
                match self.mode.load(Ordering::SeqCst) {
                    1 => {
                        return Err(StoreRuntimeRegistryFailure::ResolverFailed {
                            message: "publisher failed".to_owned(),
                        });
                    }
                    _ => {}
                }
                let runtime = Arc::new(ShardRuntime::new(
                    request.binding.clone(),
                    matches!(request.binding.shard_id.scope, StoreShardScopeV1::Profile),
                ));
                runtime
                    .transition(RuntimeMaintenanceStateV1::Opening)
                    .unwrap();
                runtime
                    .transition(RuntimeMaintenanceStateV1::Ready)
                    .unwrap();
                self.runtimes.lock().unwrap().push(Arc::downgrade(&runtime));
                Ok(runtime)
            })
        }
    }

    fn registry(
        config: StoreRuntimeRegistryConfig,
    ) -> (StoreRuntimeRegistry, Arc<TestResolver>, Arc<TestPublisher>) {
        let resolver = Arc::new(TestResolver::default());
        let publisher = Arc::new(TestPublisher::default());
        let registry =
            StoreRuntimeRegistry::with_config(resolver.clone(), publisher.clone(), config).unwrap();
        (registry, resolver, publisher)
    }

    async fn profile_pin(registry: &StoreRuntimeRegistry) -> ProfileAuthorityPin {
        let shard = profile_shard();
        assert!(matches!(
            registry
                .open(StoreRuntimeOpenRequest::new(
                    shard.clone(),
                    incarnation(),
                    None
                ))
                .await,
            StoreRuntimeOpenResult::Published(_)
        ));
        match registry.profile_authority_pin(&shard) {
            ProfileAuthorityPinResult::Pinned(pin) => pin,
            other => panic!("profile was not pinned: {other:?}"),
        }
    }

    fn project_request(project: &str, pin: &ProfileAuthorityPin) -> StoreRuntimeOpenRequest {
        StoreRuntimeOpenRequest::new(project_shard(project), incarnation(), Some(pin.clone()))
    }

    async fn wait_for_calls(calls: &AtomicUsize, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("publisher made progress");
    }

    fn active_lease(binding: &StoreRuntimeBindingV1, lease_id: &str) -> RuntimeLeaseV1 {
        let now = utc_now();
        RuntimeLeaseV1 {
            lease_id: RuntimeLeaseIdV1::new(lease_id).unwrap(),
            binding: binding.clone(),
            holder: StoreClientIdV1::new("client.registry").unwrap(),
            acquired_at: UtcMicros(now.0.saturating_sub(1_000_000)),
            expires_at: UtcMicros(now.0.saturating_add(60_000_000)),
        }
    }

    #[test]
    fn budget_defaults_to_four_caps_at_eight_and_rejects_zero() {
        assert_eq!(
            StoreRuntimeRegistryConfig::default().project_code_open_runtime_budget(),
            DEFAULT_PROJECT_CODE_OPEN_RUNTIMES
        );
        assert!(StoreRuntimeRegistryConfig::new(MAX_PROJECT_CODE_OPEN_RUNTIMES).is_ok());
        for invalid in [0, MAX_PROJECT_CODE_OPEN_RUNTIMES + 1] {
            assert!(matches!(
                StoreRuntimeRegistryConfig::new(invalid),
                Err(StoreRuntimeRegistryFailure::InvalidProjectCodeBudget { requested, .. })
                    if requested == invalid
            ));
        }
    }

    #[tokio::test]
    async fn concurrent_openers_publish_one_concrete_runtime_and_one_locator() {
        for round in 0..8 {
            let (registry, resolver, publisher) = registry(StoreRuntimeRegistryConfig::default());
            let pin = profile_pin(&registry).await;
            publisher.block.store(true, Ordering::SeqCst);
            let request = project_request(&format!("project.singleflight-{round}"), &pin);

            let mut joins = Vec::new();
            for index in 0..64 {
                match registry.begin_or_join_open(&request) {
                    StoreRuntimeOpenBegin::Started(join) if index == 0 => joins.push(join),
                    StoreRuntimeOpenBegin::Joined(join) => joins.push(join),
                    other => panic!("unexpected open result: {other:?}"),
                }
            }
            wait_for_calls(&publisher.calls, 2).await;
            publisher.release.notify_one();
            let mut handles = Vec::new();
            for join in joins {
                match join.wait().await {
                    StoreRuntimeOpenResult::Published(handle) => handles.push(handle),
                    other => panic!("publication failed: {other:?}"),
                }
            }
            assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
            assert_eq!(publisher.calls.load(Ordering::SeqCst), 2);
            assert!(
                handles[1..]
                    .iter()
                    .all(|handle| Arc::ptr_eq(handles[0].runtime(), handle.runtime()))
            );
            assert_eq!(
                handles[0].runtime().maintenance_state(),
                RuntimeMaintenanceStateV1::Ready
            );
        }
    }

    #[tokio::test]
    async fn failed_open_wakes_joiners_and_retry_uses_a_higher_fence() {
        let (registry, _, publisher) = registry(StoreRuntimeRegistryConfig::default());
        let pin = profile_pin(&registry).await;
        publisher.block.store(true, Ordering::SeqCst);
        publisher.mode.store(1, Ordering::SeqCst);
        let request = project_request("project.failure", &pin);
        let first = registry.begin_or_join_open(&request);
        wait_for_calls(&publisher.calls, 2).await;
        let second = registry.begin_or_join_open(&request);
        publisher.release.notify_one();
        let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(first.wait(), second.wait())
        })
        .await
        .expect("joiners cannot be stranded");
        for result in [first, second] {
            assert!(matches!(
                result,
                StoreRuntimeOpenResult::Failed(StoreRuntimeRegistryFailure::ResolverFailed { .. })
            ));
        }

        let failed_epoch = publisher.bindings.lock().unwrap()[1].authority_epoch;
        publisher.block.store(false, Ordering::SeqCst);
        publisher.mode.store(0, Ordering::SeqCst);
        let retry = match registry.open(request).await {
            StoreRuntimeOpenResult::Published(handle) => handle,
            other => panic!("retry failed: {other:?}"),
        };
        assert!(retry.binding().authority_epoch > failed_epoch);
    }

    #[test]
    fn cancelled_open_task_wakes_every_joiner_and_allows_retry() {
        for round in 0..8 {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (registry, _, publisher) = registry(StoreRuntimeRegistryConfig::default());
            let (first, second, request) = runtime.block_on(async {
                let pin = profile_pin(&registry).await;
                publisher.block.store(true, Ordering::SeqCst);
                let request = project_request(&format!("project.cancelled-{round}"), &pin);
                let first = registry.begin_or_join_open(&request);
                wait_for_calls(&publisher.calls, 2).await;
                let second = registry.begin_or_join_open(&request);
                (first, second, request)
            });

            // Runtime shutdown aborts the detached opener. OpenAttemptGuard must
            // remove Opening and publish one terminal failure to every joiner.
            drop(runtime);
            let waiter = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            waiter.block_on(async {
                let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
                    tokio::join!(first.wait(), second.wait())
                })
                .await
                .expect("cancelled opener cannot strand joiners");
                for result in [first, second] {
                    assert!(matches!(
                        result,
                        StoreRuntimeOpenResult::Failed(
                            StoreRuntimeRegistryFailure::OpenTaskAbandoned { .. }
                        )
                    ));
                }

                publisher.block.store(false, Ordering::SeqCst);
                assert!(matches!(
                    registry.open(request).await,
                    StoreRuntimeOpenResult::Published(_)
                ));
            });
        }
    }

    #[tokio::test]
    async fn profile_pin_budget_and_all_runtime_blockers_are_authoritative() {
        let config = StoreRuntimeRegistryConfig::new(2).unwrap();
        let (registry, _, publisher) = registry(config);
        let pin = profile_pin(&registry).await;
        let held = match registry.open(project_request("project.held", &pin)).await {
            StoreRuntimeOpenResult::Published(handle) => handle,
            other => panic!("open failed: {other:?}"),
        };
        let leased = match registry.open(project_request("project.leased", &pin)).await {
            StoreRuntimeOpenResult::Published(handle) => handle,
            other => panic!("open failed: {other:?}"),
        };
        let leased_binding = leased.binding().clone();
        let lease = active_lease(&leased_binding, "lease.registry.blocker");
        assert!(matches!(
            registry.acquire_lease(lease.clone()),
            StoreRuntimeLeaseAcquireResult::Acquired(_)
        ));
        drop(leased);

        assert!(matches!(
            registry.begin_or_join_open(&project_request("project.overflow", &pin)),
            StoreRuntimeOpenBegin::Rejected(
                StoreRuntimeRegistryFailure::ProjectCodeBudgetExhausted { limit: 2 }
            )
        ));
        assert!(matches!(
            registry.lookup(pin.binding()),
            StoreRuntimeLookup::Ready(_)
        ));
        assert_eq!(publisher.runtime(2).health_snapshot().client_leases, 1);

        let held_runtime = Arc::downgrade(held.runtime());
        drop(held);
        assert!(matches!(
            registry
                .open(project_request("project.overflow", &pin))
                .await,
            StoreRuntimeOpenResult::Published(_)
        ));
        assert!(
            held_runtime.upgrade().is_none(),
            "eviction must release the canonical runtime after closing it"
        );
        assert!(registry.release_lease(&leased_binding, &lease.lease_id));

        publisher
            .runtime(0)
            .transition(RuntimeMaintenanceStateV1::Faulted)
            .unwrap();
        assert!(matches!(
            registry.profile_authority_pin(&profile_shard()),
            ProfileAuthorityPinResult::Rejected(
                StoreRuntimeRegistryFailure::ProfileAuthorityUnavailable {
                    state: RuntimeMaintenanceStateV1::Faulted,
                    ..
                }
            )
        ));
        assert!(matches!(
            registry.begin_or_join_open(&project_request("project.after-profile-fault", &pin)),
            StoreRuntimeOpenBegin::Rejected(
                StoreRuntimeRegistryFailure::ProfileAuthorityUnavailable {
                    state: RuntimeMaintenanceStateV1::Faulted,
                    ..
                }
            )
        ));
    }

    #[tokio::test]
    async fn epochs_are_monotonic_across_registries_and_respect_a_retained_floor() {
        let resolver: Arc<dyn StoreRuntimeResolver> = Arc::new(TestResolver::default());
        let publisher: Arc<dyn ShardRuntimePublisher> = Arc::new(LifecycleShardRuntimePublisher);
        let floor = StoreAuthorityEpochV1::new(1_000_000).unwrap();
        let first = StoreRuntimeRegistry::with_config_and_authority_epoch_floor(
            resolver.clone(),
            publisher.clone(),
            StoreRuntimeRegistryConfig::default(),
            Some(floor),
        )
        .unwrap();
        let first = match first
            .open(StoreRuntimeOpenRequest::new(
                profile_shard(),
                incarnation(),
                None,
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            other => panic!("first open failed: {other:?}"),
        };
        let second = StoreRuntimeRegistry::new(resolver, publisher);
        let second = match second
            .open(StoreRuntimeOpenRequest::new(
                profile_shard(),
                incarnation(),
                None,
            ))
            .await
        {
            StoreRuntimeOpenResult::Published(handle) => handle,
            other => panic!("second open failed: {other:?}"),
        };
        assert!(first.binding().authority_epoch > floor);
        assert!(second.binding().authority_epoch > first.binding().authority_epoch);
        assert!(!Arc::ptr_eq(first.runtime(), second.runtime()));
    }
}

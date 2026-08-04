//! In-memory lifecycle and resource accounting for one canonical shard runtime.
//!
//! This is the runtime object published by [`super::registry::StoreRuntimeRegistry`].
//! Physical attachments are owned by the registry; this value tracks their
//! lifecycle and writer-presence guard.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracedecay_domain::UtcMicros;
use tracedecay_store::{
    RuntimeLeaseIdV1, RuntimeLeaseV1, RuntimeMaintenanceStateV1, RuntimeMaintenanceTransitionV1,
    StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ShardRuntimeHealth {
    #[default]
    Unknown,
    Healthy,
    Degraded,
    Faulted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShardRuntimeResource {
    Writer,
    Queue,
    GeneralReader,
    HealthReader,
    Snapshot,
    Watcher,
    Scheduler,
    Client,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShardRuntimeLeaseKind {
    GeneralReader,
    HealthReader,
    Snapshot,
    Watcher,
    Scheduler,
    Client,
}

impl ShardRuntimeLeaseKind {
    const fn resource(self) -> ShardRuntimeResource {
        match self {
            Self::GeneralReader => ShardRuntimeResource::GeneralReader,
            Self::HealthReader => ShardRuntimeResource::HealthReader,
            Self::Snapshot => ShardRuntimeResource::Snapshot,
            Self::Watcher => ShardRuntimeResource::Watcher,
            Self::Scheduler => ShardRuntimeResource::Scheduler,
            Self::Client => ShardRuntimeResource::Client,
        }
    }

    const fn counter_name(self) -> &'static str {
        match self {
            Self::GeneralReader => "general reader leases",
            Self::HealthReader => "health reader leases",
            Self::Snapshot => "snapshot leases",
            Self::Watcher => "watcher leases",
            Self::Scheduler => "scheduler leases",
            Self::Client => "client leases",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ShardRuntimeError {
    #[error("illegal shard runtime transition from {from:?} to {to:?}")]
    IllegalTransition {
        from: RuntimeMaintenanceStateV1,
        to: RuntimeMaintenanceStateV1,
    },
    #[error("shard runtime transition from {from:?} to {to:?} requires a drained runtime")]
    TransitionRequiresDrainedRuntime {
        from: RuntimeMaintenanceStateV1,
        to: RuntimeMaintenanceStateV1,
    },
    #[error("shard runtime cannot acquire {resource:?} while in {state:?}")]
    ResourceUnavailable {
        resource: ShardRuntimeResource,
        state: RuntimeMaintenanceStateV1,
    },
    #[error("the shard runtime already has a writer")]
    WriterAlreadyPresent,
    #[error("queued work must contain at least one operation")]
    ZeroQueuedOperations,
    #[error("shard runtime counter overflowed: {counter}")]
    CounterOverflow { counter: &'static str },
    #[error("a faulted health result requires an explicit fault transition")]
    FaultedHealthRequiresTransition,
    #[error("the shard runtime is terminally faulted")]
    RuntimeFaulted,
    #[error("runtime lease is invalid")]
    InvalidRuntimeLease,
    #[error("runtime lease binding does not match this runtime")]
    RuntimeLeaseBindingMismatch,
    #[error("runtime lease is not active at the acquisition time")]
    RuntimeLeaseNotActive,
    #[error("runtime lease id is already bound to different lease data")]
    RuntimeLeaseConflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardRuntimeHealthSnapshot {
    pub binding: StoreRuntimeBindingV1,
    pub state: RuntimeMaintenanceStateV1,
    pub pinned_profile: bool,
    pub writer_present: bool,
    pub queued_operations: u32,
    pub queued_bytes: u64,
    pub general_reader_leases: u32,
    pub health_reader_leases: u32,
    pub snapshot_leases: u32,
    pub watcher_leases: u32,
    pub scheduler_leases: u32,
    pub client_leases: u32,
    pub wal_bytes: u64,
    pub memory_estimate_bytes: u64,
    pub idle_for: Duration,
    pub health: ShardRuntimeHealth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShardRuntimeEvictionBlocker {
    NotReady {
        state: RuntimeMaintenanceStateV1,
    },
    PinnedProfile,
    WriterPresent,
    QueuedWork {
        operations: u32,
        bytes: u64,
    },
    GeneralReaderLeases {
        count: u32,
    },
    HealthReaderLeases {
        count: u32,
    },
    SnapshotLeases {
        count: u32,
    },
    WatcherLeases {
        count: u32,
    },
    SchedulerLeases {
        count: u32,
    },
    ClientLeases {
        count: u32,
    },
    NotIdle {
        idle_for: Duration,
        required_idle: Duration,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardRuntimeEvictionEligibility {
    pub idle_for: Duration,
    pub blockers: Vec<ShardRuntimeEvictionBlocker>,
}

impl ShardRuntimeEvictionEligibility {
    pub(crate) fn is_eligible(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// One internally consistent observation used by registry telemetry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardRuntimeObservation {
    pub health: ShardRuntimeHealthSnapshot,
    pub eviction: ShardRuntimeEvictionEligibility,
}

/// The one concrete runtime object retained for a published binding.
#[derive(Debug)]
pub struct ShardRuntime {
    binding: StoreRuntimeBindingV1,
    state: Mutex<ShardRuntimeState>,
}

#[derive(Debug)]
struct ShardRuntimeState {
    maintenance_state: RuntimeMaintenanceStateV1,
    pinned_profile: bool,
    writer_present: bool,
    queued_operations: u32,
    queued_bytes: u64,
    general_reader_leases: u32,
    health_reader_leases: u32,
    snapshot_leases: u32,
    watcher_leases: u32,
    scheduler_leases: u32,
    client_leases: u32,
    runtime_leases: BTreeMap<RuntimeLeaseIdV1, RuntimeLeaseV1>,
    wal_bytes: u64,
    memory_estimate_bytes: u64,
    last_activity: Instant,
    health: ShardRuntimeHealth,
}

impl ShardRuntime {
    pub fn new(binding: StoreRuntimeBindingV1, pinned_profile: bool) -> Self {
        Self {
            binding,
            state: Mutex::new(ShardRuntimeState {
                maintenance_state: RuntimeMaintenanceStateV1::Closed,
                pinned_profile,
                writer_present: false,
                queued_operations: 0,
                queued_bytes: 0,
                general_reader_leases: 0,
                health_reader_leases: 0,
                snapshot_leases: 0,
                watcher_leases: 0,
                scheduler_leases: 0,
                client_leases: 0,
                runtime_leases: BTreeMap::new(),
                wal_bytes: 0,
                memory_estimate_bytes: 0,
                last_activity: Instant::now(),
                health: ShardRuntimeHealth::Unknown,
            }),
        }
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn shard_id(&self) -> &StoreShardIdV1 {
        &self.binding.shard_id
    }

    pub fn incarnation(&self) -> StoreIncarnationV1 {
        self.binding.incarnation
    }

    pub fn authority_epoch(&self) -> StoreAuthorityEpochV1 {
        self.binding.authority_epoch
    }

    pub(crate) fn maintenance_state(&self) -> RuntimeMaintenanceStateV1 {
        self.lock_state().maintenance_state
    }

    pub fn transition(&self, to: RuntimeMaintenanceStateV1) -> Result<(), ShardRuntimeError> {
        let mut state = self.lock_state();
        state.prune_expired_runtime_leases(utc_now());
        let from = state.maintenance_state;
        if !RuntimeMaintenanceTransitionV1::is_allowed(from, to) {
            return Err(ShardRuntimeError::IllegalTransition { from, to });
        }
        if matches!(
            to,
            RuntimeMaintenanceStateV1::Closed | RuntimeMaintenanceStateV1::ExclusiveMaintenance
        ) && !state.is_drained()
        {
            return Err(ShardRuntimeError::TransitionRequiresDrainedRuntime { from, to });
        }
        state.maintenance_state = to;
        if to == RuntimeMaintenanceStateV1::Faulted {
            state.health = ShardRuntimeHealth::Faulted;
        }
        state.touch();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_pinned_profile(&self, pinned_profile: bool) {
        self.lock_state().pinned_profile = pinned_profile;
    }

    #[cfg(test)]
    pub(crate) fn record_storage_usage(&self, wal_bytes: u64, memory_estimate_bytes: u64) {
        let mut state = self.lock_state();
        state.wal_bytes = wal_bytes;
        state.memory_estimate_bytes = memory_estimate_bytes;
    }

    #[cfg(test)]
    pub(crate) fn set_health(&self, health: ShardRuntimeHealth) -> Result<(), ShardRuntimeError> {
        let mut state = self.lock_state();
        if state.maintenance_state == RuntimeMaintenanceStateV1::Faulted {
            return Err(ShardRuntimeError::RuntimeFaulted);
        }
        if health == ShardRuntimeHealth::Faulted {
            return Err(ShardRuntimeError::FaultedHealthRequiresTransition);
        }
        state.health = health;
        Ok(())
    }

    pub fn last_activity(&self) -> Instant {
        self.lock_state().last_activity
    }

    /// Retains a contract runtime lease until explicit release or expiry.
    pub(crate) fn acquire_runtime_lease(
        &self,
        lease: RuntimeLeaseV1,
        now: UtcMicros,
    ) -> Result<RuntimeLeaseV1, ShardRuntimeError> {
        lease
            .validate()
            .map_err(|_| ShardRuntimeError::InvalidRuntimeLease)?;
        if lease.binding != self.binding {
            return Err(ShardRuntimeError::RuntimeLeaseBindingMismatch);
        }
        if now < lease.acquired_at || lease.is_expired_at(now) {
            return Err(ShardRuntimeError::RuntimeLeaseNotActive);
        }

        let mut state = self.lock_state();
        state.require_ready(ShardRuntimeResource::Client)?;
        state.prune_expired_runtime_leases(now);
        if let Some(existing) = state.runtime_leases.get(&lease.lease_id) {
            return (existing == &lease)
                .then(|| existing.clone())
                .ok_or(ShardRuntimeError::RuntimeLeaseConflict);
        }
        state
            .runtime_leases
            .insert(lease.lease_id.clone(), lease.clone());
        state.touch();
        Ok(lease)
    }

    #[cfg(test)]
    pub(crate) fn release_runtime_lease(&self, lease_id: &RuntimeLeaseIdV1) -> bool {
        let mut state = self.lock_state();
        let released = state.runtime_leases.remove(lease_id).is_some();
        if released {
            state.touch();
        }
        released
    }

    #[cfg(test)]
    pub(crate) fn prune_expired_runtime_leases(&self, now: UtcMicros) -> usize {
        self.lock_state().prune_expired_runtime_leases(now)
    }

    pub fn health_snapshot(&self) -> ShardRuntimeHealthSnapshot {
        self.health_snapshot_at(Instant::now())
    }

    pub(crate) fn health_snapshot_at(&self, now: Instant) -> ShardRuntimeHealthSnapshot {
        self.lock_state().health_snapshot(&self.binding, now)
    }

    pub(crate) fn eviction_eligibility(
        &self,
        required_idle: Duration,
    ) -> ShardRuntimeEvictionEligibility {
        self.observe_at(Instant::now(), required_idle, utc_now())
            .eviction
    }

    #[cfg(test)]
    pub(crate) fn eviction_eligibility_at(
        &self,
        now: Instant,
        required_idle: Duration,
    ) -> ShardRuntimeEvictionEligibility {
        self.lock_state().eviction_eligibility(now, required_idle)
    }

    pub fn observe(&self, required_idle: Duration) -> ShardRuntimeObservation {
        self.observe_at(Instant::now(), required_idle, utc_now())
    }

    pub(crate) fn observe_at(
        &self,
        monotonic_now: Instant,
        required_idle: Duration,
        lease_now: UtcMicros,
    ) -> ShardRuntimeObservation {
        let mut state = self.lock_state();
        state.prune_expired_runtime_leases(lease_now);
        ShardRuntimeObservation {
            health: state.health_snapshot(&self.binding, monotonic_now),
            eviction: state.eviction_eligibility(monotonic_now, required_idle),
        }
    }

    #[cfg(test)]
    pub(crate) fn acquire_writer_presence(
        &self,
    ) -> Result<ShardRuntimeWriterGuard<'_>, ShardRuntimeError> {
        let mut state = self.lock_state();
        if !matches!(
            state.maintenance_state,
            RuntimeMaintenanceStateV1::Opening
                | RuntimeMaintenanceStateV1::Ready
                | RuntimeMaintenanceStateV1::Reopening
        ) {
            return Err(ShardRuntimeError::ResourceUnavailable {
                resource: ShardRuntimeResource::Writer,
                state: state.maintenance_state,
            });
        }
        if state.writer_present {
            return Err(ShardRuntimeError::WriterAlreadyPresent);
        }
        state.writer_present = true;
        state.touch();
        Ok(ShardRuntimeWriterGuard {
            runtime: self,
            active: true,
        })
    }

    pub fn acquire_lease(
        &self,
        kind: ShardRuntimeLeaseKind,
    ) -> Result<ShardRuntimeLease<'_>, ShardRuntimeError> {
        let mut state = self.lock_state();
        state.require_ready(kind.resource())?;
        state.increment_lease(kind)?;
        state.touch();
        Ok(ShardRuntimeLease {
            runtime: self,
            kind: Some(kind),
        })
    }

    #[cfg(test)]
    pub(crate) fn queue_work(
        &self,
        operations: u32,
        bytes: u64,
    ) -> Result<ShardRuntimeQueuedWork<'_>, ShardRuntimeError> {
        if operations == 0 {
            return Err(ShardRuntimeError::ZeroQueuedOperations);
        }
        let mut state = self.lock_state();
        state.require_ready(ShardRuntimeResource::Queue)?;
        state.add_queued_work(operations, bytes)?;
        state.touch();
        Ok(ShardRuntimeQueuedWork {
            runtime: self,
            operations,
            bytes,
            active: true,
        })
    }

    #[cfg(test)]
    fn release_writer_presence(&self) {
        let mut state = self.lock_state();
        debug_assert!(
            state.writer_present,
            "attempted to release an absent writer"
        );
        state.writer_present = false;
        state.touch();
    }

    fn release_lease(&self, kind: ShardRuntimeLeaseKind) {
        let mut state = self.lock_state();
        state.release_lease(kind);
        state.touch();
    }

    #[cfg(test)]
    fn release_queued_work(&self, operations: u32, bytes: u64) {
        let mut state = self.lock_state();
        state.release_queued_work(operations, bytes);
        state.touch();
    }

    fn lock_state(&self) -> MutexGuard<'_, ShardRuntimeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ShardRuntimeState {
    fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    fn active_client_leases(&self) -> u32 {
        self.client_leases
            .saturating_add(u32::try_from(self.runtime_leases.len()).unwrap_or(u32::MAX))
    }

    fn is_drained(&self) -> bool {
        !self.writer_present
            && self.queued_operations == 0
            && self.queued_bytes == 0
            && self.general_reader_leases == 0
            && self.health_reader_leases == 0
            && self.snapshot_leases == 0
            && self.watcher_leases == 0
            && self.scheduler_leases == 0
            && self.active_client_leases() == 0
    }

    fn require_ready(&self, resource: ShardRuntimeResource) -> Result<(), ShardRuntimeError> {
        (self.maintenance_state == RuntimeMaintenanceStateV1::Ready)
            .then_some(())
            .ok_or(ShardRuntimeError::ResourceUnavailable {
                resource,
                state: self.maintenance_state,
            })
    }

    fn prune_expired_runtime_leases(&mut self, now: UtcMicros) -> usize {
        let before = self.runtime_leases.len();
        self.runtime_leases
            .retain(|_, lease| !lease.is_expired_at(now));
        before - self.runtime_leases.len()
    }

    fn health_snapshot(
        &self,
        binding: &StoreRuntimeBindingV1,
        now: Instant,
    ) -> ShardRuntimeHealthSnapshot {
        ShardRuntimeHealthSnapshot {
            binding: binding.clone(),
            state: self.maintenance_state,
            pinned_profile: self.pinned_profile,
            writer_present: self.writer_present,
            queued_operations: self.queued_operations,
            queued_bytes: self.queued_bytes,
            general_reader_leases: self.general_reader_leases,
            health_reader_leases: self.health_reader_leases,
            snapshot_leases: self.snapshot_leases,
            watcher_leases: self.watcher_leases,
            scheduler_leases: self.scheduler_leases,
            client_leases: self.active_client_leases(),
            wal_bytes: self.wal_bytes,
            memory_estimate_bytes: self.memory_estimate_bytes,
            idle_for: now.saturating_duration_since(self.last_activity),
            health: self.health,
        }
    }

    fn eviction_eligibility(
        &self,
        now: Instant,
        required_idle: Duration,
    ) -> ShardRuntimeEvictionEligibility {
        let idle_for = now.saturating_duration_since(self.last_activity);
        let mut blockers = Vec::new();
        if self.maintenance_state != RuntimeMaintenanceStateV1::Ready {
            blockers.push(ShardRuntimeEvictionBlocker::NotReady {
                state: self.maintenance_state,
            });
        }
        if self.pinned_profile {
            blockers.push(ShardRuntimeEvictionBlocker::PinnedProfile);
        }
        if self.writer_present {
            blockers.push(ShardRuntimeEvictionBlocker::WriterPresent);
        }
        if self.queued_operations != 0 || self.queued_bytes != 0 {
            blockers.push(ShardRuntimeEvictionBlocker::QueuedWork {
                operations: self.queued_operations,
                bytes: self.queued_bytes,
            });
        }
        for (count, blocker) in [
            (
                self.general_reader_leases,
                ShardRuntimeEvictionBlocker::GeneralReaderLeases {
                    count: self.general_reader_leases,
                },
            ),
            (
                self.health_reader_leases,
                ShardRuntimeEvictionBlocker::HealthReaderLeases {
                    count: self.health_reader_leases,
                },
            ),
            (
                self.snapshot_leases,
                ShardRuntimeEvictionBlocker::SnapshotLeases {
                    count: self.snapshot_leases,
                },
            ),
            (
                self.watcher_leases,
                ShardRuntimeEvictionBlocker::WatcherLeases {
                    count: self.watcher_leases,
                },
            ),
            (
                self.scheduler_leases,
                ShardRuntimeEvictionBlocker::SchedulerLeases {
                    count: self.scheduler_leases,
                },
            ),
            (
                self.active_client_leases(),
                ShardRuntimeEvictionBlocker::ClientLeases {
                    count: self.active_client_leases(),
                },
            ),
        ] {
            if count != 0 {
                blockers.push(blocker);
            }
        }
        if idle_for < required_idle {
            blockers.push(ShardRuntimeEvictionBlocker::NotIdle {
                idle_for,
                required_idle,
            });
        }
        ShardRuntimeEvictionEligibility { idle_for, blockers }
    }

    fn increment_lease(&mut self, kind: ShardRuntimeLeaseKind) -> Result<(), ShardRuntimeError> {
        let counter_name = kind.counter_name();
        let counter = self.lease_counter_mut(kind);
        *counter = counter
            .checked_add(1)
            .ok_or(ShardRuntimeError::CounterOverflow {
                counter: counter_name,
            })?;
        Ok(())
    }

    fn release_lease(&mut self, kind: ShardRuntimeLeaseKind) {
        let counter_name = kind.counter_name();
        let counter = self.lease_counter_mut(kind);
        if let Some(next) = counter.checked_sub(1) {
            *counter = next;
        } else {
            debug_assert!(false, "attempted to release absent {counter_name}");
        }
    }

    fn lease_counter_mut(&mut self, kind: ShardRuntimeLeaseKind) -> &mut u32 {
        match kind {
            ShardRuntimeLeaseKind::GeneralReader => &mut self.general_reader_leases,
            ShardRuntimeLeaseKind::HealthReader => &mut self.health_reader_leases,
            ShardRuntimeLeaseKind::Snapshot => &mut self.snapshot_leases,
            ShardRuntimeLeaseKind::Watcher => &mut self.watcher_leases,
            ShardRuntimeLeaseKind::Scheduler => &mut self.scheduler_leases,
            ShardRuntimeLeaseKind::Client => &mut self.client_leases,
        }
    }

    #[cfg(test)]
    fn add_queued_work(&mut self, operations: u32, bytes: u64) -> Result<(), ShardRuntimeError> {
        let next_operations = self.queued_operations.checked_add(operations).ok_or(
            ShardRuntimeError::CounterOverflow {
                counter: "queued operations",
            },
        )?;
        let next_bytes =
            self.queued_bytes
                .checked_add(bytes)
                .ok_or(ShardRuntimeError::CounterOverflow {
                    counter: "queued bytes",
                })?;
        self.queued_operations = next_operations;
        self.queued_bytes = next_bytes;
        Ok(())
    }

    #[cfg(test)]
    fn release_queued_work(&mut self, operations: u32, bytes: u64) {
        match (
            self.queued_operations.checked_sub(operations),
            self.queued_bytes.checked_sub(bytes),
        ) {
            (Some(operations), Some(bytes)) => {
                self.queued_operations = operations;
                self.queued_bytes = bytes;
            }
            _ => debug_assert!(false, "queued-work accounting underflow"),
        }
    }
}

#[must_use = "dropping the guard releases writer-presence accounting"]
#[cfg(test)]
pub(crate) struct ShardRuntimeWriterGuard<'a> {
    runtime: &'a ShardRuntime,
    active: bool,
}

#[cfg(test)]
impl Drop for ShardRuntimeWriterGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.runtime.release_writer_presence();
        }
    }
}

#[must_use = "dropping the guard releases the runtime lease"]
pub struct ShardRuntimeLease<'a> {
    runtime: &'a ShardRuntime,
    kind: Option<ShardRuntimeLeaseKind>,
}

impl ShardRuntimeLease<'_> {
    pub fn release(mut self) {
        if let Some(kind) = self.kind.take() {
            self.runtime.release_lease(kind);
        }
    }
}

impl Drop for ShardRuntimeLease<'_> {
    fn drop(&mut self) {
        if let Some(kind) = self.kind.take() {
            self.runtime.release_lease(kind);
        }
    }
}

#[must_use = "dropping the guard releases queued-work accounting"]
#[cfg(test)]
pub(crate) struct ShardRuntimeQueuedWork<'a> {
    runtime: &'a ShardRuntime,
    operations: u32,
    bytes: u64,
    active: bool,
}

#[cfg(test)]
impl ShardRuntimeQueuedWork<'_> {
    pub fn release(mut self) {
        if self.active {
            self.runtime
                .release_queued_work(self.operations, self.bytes);
            self.active = false;
        }
    }
}

#[cfg(test)]
impl Drop for ShardRuntimeQueuedWork<'_> {
    fn drop(&mut self) {
        if self.active {
            self.runtime
                .release_queued_work(self.operations, self.bytes);
        }
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
    use std::sync::Barrier;

    use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
    use tracedecay_store::{RuntimeLeaseIdV1, StoreClientIdV1};

    use super::*;

    use RuntimeMaintenanceStateV1::{
        Closed, Draining, ExclusiveMaintenance, Faulted, Opening, Ready, Reopening,
    };

    const STATES: [RuntimeMaintenanceStateV1; 7] = [
        Closed,
        Opening,
        Ready,
        Draining,
        ExclusiveMaintenance,
        Reopening,
        Faulted,
    ];
    const LEGAL: [(RuntimeMaintenanceStateV1, RuntimeMaintenanceStateV1); 12] = [
        (Closed, Opening),
        (Opening, Ready),
        (Opening, Faulted),
        (Ready, Draining),
        (Ready, Faulted),
        (Draining, ExclusiveMaintenance),
        (Draining, Closed),
        (Draining, Faulted),
        (ExclusiveMaintenance, Reopening),
        (ExclusiveMaintenance, Faulted),
        (Reopening, Ready),
        (Reopening, Faulted),
    ];
    const LEASE_KINDS: [ShardRuntimeLeaseKind; 6] = [
        ShardRuntimeLeaseKind::GeneralReader,
        ShardRuntimeLeaseKind::HealthReader,
        ShardRuntimeLeaseKind::Snapshot,
        ShardRuntimeLeaseKind::Watcher,
        ShardRuntimeLeaseKind::Scheduler,
        ShardRuntimeLeaseKind::Client,
    ];

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: Debug,
    {
        T::try_from(value.to_owned()).expect("canonical fixture identity")
    }

    fn binding() -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                id::<BrainId>("brain.shard-runtime"),
                id::<UserProfileId>("profile.shard-runtime"),
                id::<ProjectId>("project.shard-runtime"),
            ),
            StoreIncarnationV1::new(3).unwrap(),
            StoreAuthorityEpochV1::new(7).unwrap(),
        )
    }

    fn drive_to(target: RuntimeMaintenanceStateV1) -> ShardRuntime {
        let path: &[RuntimeMaintenanceStateV1] = match target {
            Closed => &[],
            Opening => &[Opening],
            Ready => &[Opening, Ready],
            Draining => &[Opening, Ready, Draining],
            ExclusiveMaintenance => &[Opening, Ready, Draining, ExclusiveMaintenance],
            Reopening => &[Opening, Ready, Draining, ExclusiveMaintenance, Reopening],
            Faulted => &[Opening, Faulted],
        };
        let runtime = ShardRuntime::new(binding(), false);
        for state in path {
            runtime.transition(*state).unwrap();
        }
        runtime
    }

    fn runtime_lease(id: &str, acquired: i64, expires: i64) -> RuntimeLeaseV1 {
        RuntimeLeaseV1 {
            lease_id: RuntimeLeaseIdV1::new(id).unwrap(),
            binding: binding(),
            holder: StoreClientIdV1::new("client.shard-runtime").unwrap(),
            acquired_at: UtcMicros(acquired),
            expires_at: UtcMicros(expires),
        }
    }

    #[test]
    fn transition_matrix_is_exact_and_faulted_is_terminal() {
        for from in STATES {
            for to in STATES {
                let runtime = drive_to(from);
                let result = runtime.transition(to);
                assert_eq!(
                    result.is_ok(),
                    LEGAL.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
                if to == Faulted && result.is_ok() {
                    assert_eq!(
                        runtime.health_snapshot().health,
                        ShardRuntimeHealth::Faulted
                    );
                }
            }
        }

        let faulted = drive_to(Faulted);
        for state in STATES {
            assert!(matches!(
                faulted.transition(state),
                Err(ShardRuntimeError::IllegalTransition { from: Faulted, .. })
            ));
        }
        assert!(matches!(
            faulted.queue_work(1, 1),
            Err(ShardRuntimeError::ResourceUnavailable { state: Faulted, .. })
        ));
    }

    #[test]
    fn every_writer_queue_and_lease_blocker_participates_in_drain_and_eviction() {
        let runtime = drive_to(Ready);
        runtime.set_pinned_profile(true);
        let writer = runtime.acquire_writer_presence().unwrap();
        let queue = runtime.queue_work(2, 128).unwrap();
        let leases = LEASE_KINDS.map(|kind| runtime.acquire_lease(kind).unwrap());
        runtime.transition(Draining).unwrap();

        let blockers = runtime
            .eviction_eligibility_at(
                runtime.last_activity() + Duration::from_secs(30),
                Duration::ZERO,
            )
            .blockers;
        assert_eq!(blockers.len(), 10);
        assert!(blockers.contains(&ShardRuntimeEvictionBlocker::WriterPresent));
        assert!(blockers.contains(&ShardRuntimeEvictionBlocker::QueuedWork {
            operations: 2,
            bytes: 128,
        }));
        assert!(blockers.contains(&ShardRuntimeEvictionBlocker::HealthReaderLeases { count: 1 }));
        assert!(blockers.contains(&ShardRuntimeEvictionBlocker::SnapshotLeases { count: 1 }));
        assert!(blockers.contains(&ShardRuntimeEvictionBlocker::WatcherLeases { count: 1 }));
        assert!(blockers.contains(&ShardRuntimeEvictionBlocker::SchedulerLeases { count: 1 }));
        assert!(blockers.contains(&ShardRuntimeEvictionBlocker::ClientLeases { count: 1 }));
        assert!(matches!(
            runtime.transition(Closed),
            Err(ShardRuntimeError::TransitionRequiresDrainedRuntime { .. })
        ));

        drop(leases);
        drop(queue);
        drop(writer);
        runtime.set_pinned_profile(false);
        runtime.transition(Closed).unwrap();
    }

    #[test]
    fn contract_runtime_lease_is_idempotent_fenced_and_expires() {
        let runtime = drive_to(Ready);
        let now = utc_now();
        let acquired = now.0.saturating_sub(1_000);
        let expires = now.0.saturating_add(60_000_000);
        let lease = runtime_lease("lease.one", acquired, expires);
        assert_eq!(
            runtime.acquire_runtime_lease(lease.clone(), now),
            Ok(lease.clone())
        );
        assert_eq!(
            runtime.acquire_runtime_lease(lease.clone(), now),
            Ok(lease.clone())
        );
        assert_eq!(runtime.health_snapshot().client_leases, 1);

        let mut conflict = lease.clone();
        conflict.expires_at = UtcMicros(expires.saturating_add(1));
        assert_eq!(
            runtime.acquire_runtime_lease(conflict, now),
            Err(ShardRuntimeError::RuntimeLeaseConflict)
        );
        assert!(matches!(
            runtime
                .transition(Draining)
                .and_then(|()| runtime.transition(Closed)),
            Err(ShardRuntimeError::TransitionRequiresDrainedRuntime { .. })
        ));
        assert_eq!(runtime.prune_expired_runtime_leases(UtcMicros(expires)), 1);
        assert_eq!(runtime.health_snapshot().client_leases, 0);
        runtime.transition(Closed).unwrap();

        let wrong_time = drive_to(Ready);
        assert_eq!(
            wrong_time.acquire_runtime_lease(lease, UtcMicros(expires)),
            Err(ShardRuntimeError::RuntimeLeaseNotActive)
        );
    }

    #[test]
    fn queue_accounting_is_atomic_on_overflow_and_balances_under_concurrency() {
        let runtime = drive_to(Ready);
        let max = runtime.queue_work(u32::MAX, u64::MAX).unwrap();
        assert!(matches!(
            runtime.queue_work(1, 1),
            Err(ShardRuntimeError::CounterOverflow { .. })
        ));
        assert_eq!(runtime.health_snapshot().queued_operations, u32::MAX);
        drop(max);

        const THREADS: usize = 8;
        let barrier = Barrier::new(THREADS + 1);
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    let guard = runtime.queue_work(3, 192).unwrap();
                    barrier.wait();
                    barrier.wait();
                    drop(guard);
                });
            }
            barrier.wait();
            let held = runtime.health_snapshot();
            assert_eq!(held.queued_operations, 24);
            assert_eq!(held.queued_bytes, 1_536);
            barrier.wait();
        });
        assert_eq!(runtime.health_snapshot().queued_operations, 0);
    }

    #[test]
    fn observation_is_consistent_and_idle_threshold_is_inclusive() {
        let runtime = drive_to(Ready);
        runtime.record_storage_usage(4_096, 8_192);
        runtime.set_health(ShardRuntimeHealth::Degraded).unwrap();
        let last = runtime.last_activity();
        let observation = runtime.observe_at(
            last + Duration::from_secs(30),
            Duration::from_secs(30),
            UtcMicros(1),
        );
        assert_eq!(observation.health.idle_for, Duration::from_secs(30));
        assert_eq!(observation.health.wal_bytes, 4_096);
        assert_eq!(observation.health.health, ShardRuntimeHealth::Degraded);
        assert!(observation.eviction.is_eligible());
        assert_eq!(observation.eviction.idle_for, observation.health.idle_for);
    }
}

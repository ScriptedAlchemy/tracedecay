//! In-memory lifecycle and resource accounting for one canonical shard runtime.
//!
//! This is the runtime object published by [`super::registry::StoreRuntimeRegistry`].
//! Physical attachments are owned by the registry; this value tracks their
//! lifecycle and writer-presence guard.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracedecay_store::{
    RuntimeMaintenanceStateV1, RuntimeMaintenanceTransitionV1, StoreAuthorityEpochV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
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
    Client,
    Operation,
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardRuntimeHealthSnapshot {
    pub binding: StoreRuntimeBindingV1,
    pub state: RuntimeMaintenanceStateV1,
    pub pinned_profile: bool,
    pub writer_present: bool,
    pub queued_operations: u32,
    pub queued_bytes: u64,
    pub client_leases: u32,
    pub operation_leases: u32,
    pub wal_bytes: u64,
    pub memory_estimate_bytes: u64,
    pub idle_for: Duration,
    pub health: ShardRuntimeHealth,
}

/// Exact lifetime-token census used only by registry retirement preflight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShardRuntimeRetirementLeaseCounts {
    pub(crate) client_tokens: BTreeSet<u64>,
    pub(crate) operations: usize,
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
    ClientLeases {
        count: u32,
    },
    OperationLeases {
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
pub struct ShardRuntime {
    binding: StoreRuntimeBindingV1,
    /// Monotonic per-process instance number. Distinguishes a runtime that was
    /// closed and rebuilt from its predecessor even when the allocator reuses
    /// the predecessor's address, pointer identity is ABA-prone the moment
    /// the old `Arc` is dropped.
    instance_id: u64,
    /// Every lease acquire and release, every queue-depth update, and every
    /// telemetry observation for this shard takes this one lock, so it is the
    /// per-shard serialization point under concurrent operation load.
    state: std::sync::Mutex<ShardRuntimeState>,
}

/// Hand-written so the state behind the lock is never printed: formatting it
/// would have to acquire the lock being described.
impl std::fmt::Debug for ShardRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardRuntime")
            .field("binding", &self.binding)
            .field("instance_id", &self.instance_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct ShardRuntimeState {
    maintenance_state: RuntimeMaintenanceStateV1,
    pinned_profile: bool,
    writer_present: bool,
    queued_operations: u32,
    queued_bytes: u64,
    client_lifetime_leases: BTreeSet<u64>,
    operation_lifetime_leases: BTreeSet<u64>,
    next_lifetime_lease_token: u64,
    wal_bytes: u64,
    memory_estimate_bytes: u64,
    last_activity: Instant,
    health: ShardRuntimeHealth,
}

impl ShardRuntime {
    pub fn new(binding: StoreRuntimeBindingV1, pinned_profile: bool) -> Self {
        static NEXT_INSTANCE_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1);
        Self {
            binding,
            instance_id: NEXT_INSTANCE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            state: Mutex::new(ShardRuntimeState {
                maintenance_state: RuntimeMaintenanceStateV1::Closed,
                pinned_profile,
                writer_present: false,
                queued_operations: 0,
                queued_bytes: 0,
                client_lifetime_leases: BTreeSet::new(),
                operation_lifetime_leases: BTreeSet::new(),
                next_lifetime_lease_token: 0,
                wal_bytes: 0,
                memory_estimate_bytes: 0,
                last_activity: Instant::now(),
                health: ShardRuntimeHealth::Unknown,
            }),
        }
    }

    /// Monotonic per-process instance number; see the field doc.
    pub fn instance_id(&self) -> u64 {
        self.instance_id
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

    pub fn health_snapshot(&self) -> ShardRuntimeHealthSnapshot {
        self.health_snapshot_at(Instant::now())
    }

    pub(crate) fn retirement_lease_counts(&self) -> ShardRuntimeRetirementLeaseCounts {
        let state = self.lock_state();
        ShardRuntimeRetirementLeaseCounts {
            client_tokens: state.client_lifetime_leases.clone(),
            operations: state.operation_lifetime_leases.len(),
        }
    }

    pub(crate) fn health_snapshot_at(&self, now: Instant) -> ShardRuntimeHealthSnapshot {
        self.lock_state().health_snapshot(&self.binding, now)
    }

    pub(crate) fn eviction_eligibility(
        &self,
        required_idle: Duration,
    ) -> ShardRuntimeEvictionEligibility {
        self.observe_at(Instant::now(), required_idle).eviction
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
        self.observe_at(Instant::now(), required_idle)
    }

    pub(crate) fn observe_at(
        &self,
        monotonic_now: Instant,
        required_idle: Duration,
    ) -> ShardRuntimeObservation {
        let state = self.lock_state();
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

    /// Registers one client lifetime token. Clones share this token and only
    /// its final drop releases the canonical counter.
    pub(crate) fn issue_client_lifetime_lease(
        runtime: std::sync::Arc<Self>,
    ) -> Result<ShardRuntimeClientLifetimeLease, ShardRuntimeError> {
        let token = runtime.register_lifetime_lease(ShardRuntimeResource::Client)?;
        Ok(ShardRuntimeClientLifetimeLease {
            inner: std::sync::Arc::new(ShardRuntimeLifetimeLeaseToken {
                runtime,
                token,
                resource: ShardRuntimeResource::Client,
                active: AtomicBool::new(true),
            }),
        })
    }

    fn issue_operation_lifetime_lease(
        runtime: std::sync::Arc<Self>,
    ) -> Result<ShardRuntimeOperationLifetimeLease, ShardRuntimeError> {
        let token = runtime.register_lifetime_lease(ShardRuntimeResource::Operation)?;
        Ok(ShardRuntimeOperationLifetimeLease {
            inner: std::sync::Arc::new(ShardRuntimeLifetimeLeaseToken {
                runtime,
                token,
                resource: ShardRuntimeResource::Operation,
                active: AtomicBool::new(true),
            }),
        })
    }

    fn register_lifetime_lease(
        &self,
        resource: ShardRuntimeResource,
    ) -> Result<u64, ShardRuntimeError> {
        let mut state = self.lock_state();
        state.require_ready(resource)?;
        let token = state.next_lifetime_lease_token.checked_add(1).ok_or(
            ShardRuntimeError::CounterOverflow {
                counter: "runtime lifetime lease tokens",
            },
        )?;
        state.next_lifetime_lease_token = token;
        let tokens = match resource {
            ShardRuntimeResource::Client => &mut state.client_lifetime_leases,
            ShardRuntimeResource::Operation => &mut state.operation_lifetime_leases,
            _ => {
                return Err(ShardRuntimeError::ResourceUnavailable {
                    resource,
                    state: state.maintenance_state,
                });
            }
        };
        tokens.insert(token);
        state.touch();
        Ok(token)
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

    fn release_lifetime_lease(&self, resource: ShardRuntimeResource, token: u64) {
        let mut state = self.lock_state();
        let released = match resource {
            ShardRuntimeResource::Client => state.client_lifetime_leases.remove(&token),
            ShardRuntimeResource::Operation => state.operation_lifetime_leases.remove(&token),
            _ => false,
        };
        if released {
            state.touch();
        } else {
            debug_assert!(false, "lifetime token may only release once");
        }
    }

    #[cfg(test)]
    fn release_queued_work(&self, operations: u32, bytes: u64) {
        let mut state = self.lock_state();
        state.release_queued_work(operations, bytes);
        state.touch();
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ShardRuntimeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ShardRuntimeState {
    fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    fn client_leases(&self) -> u32 {
        u32::try_from(self.client_lifetime_leases.len()).unwrap_or(u32::MAX)
    }

    fn operation_leases(&self) -> u32 {
        u32::try_from(self.operation_lifetime_leases.len()).unwrap_or(u32::MAX)
    }

    fn is_drained(&self) -> bool {
        !self.writer_present
            && self.queued_operations == 0
            && self.queued_bytes == 0
            && self.client_lifetime_leases.is_empty()
            && self.operation_lifetime_leases.is_empty()
    }

    fn require_ready(&self, resource: ShardRuntimeResource) -> Result<(), ShardRuntimeError> {
        (self.maintenance_state == RuntimeMaintenanceStateV1::Ready)
            .then_some(())
            .ok_or(ShardRuntimeError::ResourceUnavailable {
                resource,
                state: self.maintenance_state,
            })
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
            client_leases: self.client_leases(),
            operation_leases: self.operation_leases(),
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
        let client_leases = self.client_leases();
        if client_leases != 0 {
            blockers.push(ShardRuntimeEvictionBlocker::ClientLeases {
                count: client_leases,
            });
        }
        let operation_leases = self.operation_leases();
        if operation_leases != 0 {
            blockers.push(ShardRuntimeEvictionBlocker::OperationLeases {
                count: operation_leases,
            });
        }
        if idle_for < required_idle {
            blockers.push(ShardRuntimeEvictionBlocker::NotIdle {
                idle_for,
                required_idle,
            });
        }
        ShardRuntimeEvictionEligibility { idle_for, blockers }
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

struct ShardRuntimeLifetimeLeaseToken {
    runtime: std::sync::Arc<ShardRuntime>,
    token: u64,
    resource: ShardRuntimeResource,
    active: AtomicBool,
}

impl ShardRuntimeLifetimeLeaseToken {
    fn release(&self) -> bool {
        if !self.active.swap(false, Ordering::AcqRel) {
            return false;
        }
        self.runtime
            .release_lifetime_lease(self.resource, self.token);
        true
    }
}

impl Drop for ShardRuntimeLifetimeLeaseToken {
    fn drop(&mut self) {
        self.release();
    }
}

/// Cloneable client capability with a single shared registered token.
#[derive(Clone)]
pub(crate) struct ShardRuntimeClientLifetimeLease {
    inner: std::sync::Arc<ShardRuntimeLifetimeLeaseToken>,
}

impl ShardRuntimeClientLifetimeLease {
    pub(crate) fn token(&self) -> u64 {
        self.inner.token
    }

    pub(crate) fn begin_operation(
        &self,
    ) -> Result<ShardRuntimeOperationLifetimeLease, ShardRuntimeError> {
        ShardRuntime::issue_operation_lifetime_lease(std::sync::Arc::clone(&self.inner.runtime))
    }

    pub(super) fn downgrade(&self) -> ShardRuntimeClientLifetimeWeakLease {
        ShardRuntimeClientLifetimeWeakLease {
            inner: std::sync::Arc::downgrade(&self.inner),
        }
    }
}

/// Non-retaining authority to release one exact client token when its
/// database-owner attachment crosses the irreversible retirement fence.
#[derive(Clone)]
pub(super) struct ShardRuntimeClientLifetimeWeakLease {
    inner: std::sync::Weak<ShardRuntimeLifetimeLeaseToken>,
}

impl ShardRuntimeClientLifetimeWeakLease {
    pub(super) fn release(&self) -> bool {
        self.inner.upgrade().is_some_and(|inner| inner.release())
    }
}

impl std::fmt::Debug for ShardRuntimeClientLifetimeLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShardRuntimeClientLifetimeLease")
            .field("token", &self.inner.token)
            .finish_non_exhaustive()
    }
}

/// Cloneable operation capability with a single shared registered token.
#[derive(Clone)]
pub(crate) struct ShardRuntimeOperationLifetimeLease {
    inner: std::sync::Arc<ShardRuntimeLifetimeLeaseToken>,
}

impl std::fmt::Debug for ShardRuntimeOperationLifetimeLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShardRuntimeOperationLifetimeLease")
            .field("token", &self.inner.token)
            .finish_non_exhaustive()
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use tracedecay_domain::{BrainId, ProjectId, UserProfileId};

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
    use tracedecay_domain::test_fixtures::id;

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
    fn every_writer_queue_and_lifetime_blocker_participates_in_drain_and_eviction() {
        let runtime = Arc::new(drive_to(Ready));
        runtime.set_pinned_profile(true);
        let writer = runtime.acquire_writer_presence().unwrap();
        let queue = runtime.queue_work(2, 128).unwrap();
        let client = ShardRuntime::issue_client_lifetime_lease(Arc::clone(&runtime)).unwrap();
        let client_clone = client.clone();
        let operation = client.begin_operation().unwrap();
        runtime.transition(Draining).unwrap();
        assert!(matches!(
            ShardRuntime::issue_client_lifetime_lease(Arc::clone(&runtime)),
            Err(ShardRuntimeError::ResourceUnavailable {
                resource: ShardRuntimeResource::Client,
                state: Draining,
            })
        ));

        let blockers = runtime
            .eviction_eligibility_at(
                runtime.last_activity() + Duration::from_secs(30),
                Duration::ZERO,
            )
            .blockers;
        assert_eq!(
            blockers,
            vec![
                ShardRuntimeEvictionBlocker::NotReady { state: Draining },
                ShardRuntimeEvictionBlocker::PinnedProfile,
                ShardRuntimeEvictionBlocker::WriterPresent,
                ShardRuntimeEvictionBlocker::QueuedWork {
                    operations: 2,
                    bytes: 128,
                },
                ShardRuntimeEvictionBlocker::ClientLeases { count: 1 },
                ShardRuntimeEvictionBlocker::OperationLeases { count: 1 },
            ]
        );
        assert!(matches!(
            runtime.transition(Closed),
            Err(ShardRuntimeError::TransitionRequiresDrainedRuntime { .. })
        ));

        drop(client);
        assert_eq!(runtime.health_snapshot().client_leases, 1);
        drop(client_clone);
        drop(operation);
        drop(queue);
        drop(writer);
        let released = runtime.health_snapshot();
        assert_eq!((released.client_leases, released.operation_leases), (0, 0));
        runtime.set_pinned_profile(false);
        runtime.transition(Closed).unwrap();
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
        let observation =
            runtime.observe_at(last + Duration::from_secs(30), Duration::from_secs(30));
        assert_eq!(observation.health.idle_for, Duration::from_secs(30));
        assert_eq!(observation.health.wal_bytes, 4_096);
        assert_eq!(observation.health.health, ShardRuntimeHealth::Degraded);
        assert!(observation.eviction.is_eligible());
        assert_eq!(observation.eviction.idle_for, observation.health.idle_for);
    }
}

//! In-memory lifecycle and resource accounting for one canonical shard runtime.
//!
//! This is the runtime object published by [`super::registry::StoreRuntimeRegistry`].
//! Physical attachments are owned by the registry; this value tracks their
//! lifecycle and client/operation lifetime leases.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tracedecay_store::{
    RuntimeMaintenanceStateV1, RuntimeMaintenanceTransitionV1, StoreAuthorityEpochV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShardRuntimeResource {
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
    #[error("shard runtime counter overflowed: {counter}")]
    CounterOverflow { counter: &'static str },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardRuntimeHealthSnapshot {
    pub binding: StoreRuntimeBindingV1,
    pub state: RuntimeMaintenanceStateV1,
    pub pinned_profile: bool,
    pub client_leases: u32,
    pub operation_leases: u32,
    pub idle_for: Duration,
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
    client_lifetime_leases: BTreeSet<u64>,
    operation_lifetime_leases: BTreeSet<u64>,
    next_lifetime_lease_token: u64,
    last_activity: Instant,
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
                client_lifetime_leases: BTreeSet::new(),
                operation_lifetime_leases: BTreeSet::new(),
                next_lifetime_lease_token: 0,
                last_activity: Instant::now(),
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
        state.touch();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_pinned_profile(&self, pinned_profile: bool) {
        self.lock_state().pinned_profile = pinned_profile;
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
        };
        tokens.insert(token);
        state.touch();
        Ok(token)
    }

    fn release_lifetime_lease(&self, resource: ShardRuntimeResource, token: u64) {
        let mut state = self.lock_state();
        let released = match resource {
            ShardRuntimeResource::Client => state.client_lifetime_leases.remove(&token),
            ShardRuntimeResource::Operation => state.operation_lifetime_leases.remove(&token),
        };
        if released {
            state.touch();
        } else {
            debug_assert!(false, "lifetime token may only release once");
        }
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
        self.client_lifetime_leases.is_empty() && self.operation_lifetime_leases.is_empty()
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
            client_leases: self.client_leases(),
            operation_leases: self.operation_leases(),
            idle_for: now.saturating_duration_since(self.last_activity),
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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
            ShardRuntime::issue_client_lifetime_lease(Arc::new(faulted)),
            Err(ShardRuntimeError::ResourceUnavailable { state: Faulted, .. })
        ));
    }

    #[test]
    fn every_lifetime_blocker_participates_in_drain_and_eviction() {
        let runtime = Arc::new(drive_to(Ready));
        runtime.set_pinned_profile(true);
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
        let released = runtime.health_snapshot();
        assert_eq!((released.client_leases, released.operation_leases), (0, 0));
        runtime.set_pinned_profile(false);
        runtime.transition(Closed).unwrap();
    }

    #[test]
    fn observation_is_consistent_and_idle_threshold_is_inclusive() {
        let runtime = drive_to(Ready);
        let last = runtime.last_activity();
        let observation =
            runtime.observe_at(last + Duration::from_secs(30), Duration::from_secs(30));
        assert_eq!(observation.health.idle_for, Duration::from_secs(30));
        assert!(observation.eviction.is_eligible());
        assert_eq!(observation.eviction.idle_for, observation.health.idle_for);
    }
}

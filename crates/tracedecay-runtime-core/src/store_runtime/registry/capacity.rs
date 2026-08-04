use std::sync::Arc;
use std::time::Duration;

use tracedecay_store::RuntimeMaintenanceStateV1;

use super::attachment::attachment_failure;
use super::{
    EvictingRuntime, RegistryEntry, RegistryState, StoreRuntimeHandle, StoreRuntimeKey,
    StoreRuntimeRegistry, StoreRuntimeRegistryFailure,
};

pub(crate) const MAX_PROJECT_CODE_OPEN_RUNTIMES: usize = 8;
pub(crate) const DEFAULT_PROJECT_CODE_OPEN_RUNTIMES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreRuntimeRegistryConfig {
    project_code_open_runtime_budget: usize,
    eviction_idle: Duration,
    exclusive_maintenance: bool,
}

impl StoreRuntimeRegistryConfig {
    pub fn new(
        project_code_open_runtime_budget: usize,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        Self::with_eviction_idle(project_code_open_runtime_budget, Duration::ZERO)
    }

    pub fn for_exclusive_maintenance(
        project_code_open_runtime_budget: usize,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        if project_code_open_runtime_budget == 0 {
            return Err(StoreRuntimeRegistryFailure::InvalidProjectCodeBudget {
                requested: project_code_open_runtime_budget,
                maximum: usize::MAX,
            });
        }
        Ok(Self {
            project_code_open_runtime_budget,
            eviction_idle: Duration::ZERO,
            exclusive_maintenance: true,
        })
    }

    pub(crate) fn with_eviction_idle(
        project_code_open_runtime_budget: usize,
        eviction_idle: Duration,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        let config = Self {
            project_code_open_runtime_budget,
            eviction_idle,
            exclusive_maintenance: false,
        };
        config.validate()?;
        Ok(config)
    }

    #[cfg(test)]
    pub(crate) const fn project_code_open_runtime_budget(self) -> usize {
        self.project_code_open_runtime_budget
    }

    pub(super) fn validate(self) -> Result<(), StoreRuntimeRegistryFailure> {
        let valid = self.project_code_open_runtime_budget > 0
            && (self.exclusive_maintenance
                || self.project_code_open_runtime_budget <= MAX_PROJECT_CODE_OPEN_RUNTIMES);
        if !valid {
            return Err(StoreRuntimeRegistryFailure::InvalidProjectCodeBudget {
                requested: self.project_code_open_runtime_budget,
                maximum: if self.exclusive_maintenance {
                    usize::MAX
                } else {
                    MAX_PROJECT_CODE_OPEN_RUNTIMES
                },
            });
        }
        Ok(())
    }

    pub(super) const fn project_budget(self) -> usize {
        self.project_code_open_runtime_budget
    }

    pub(super) const fn eviction_idle(self) -> Duration {
        self.eviction_idle
    }
}

impl Default for StoreRuntimeRegistryConfig {
    fn default() -> Self {
        Self {
            project_code_open_runtime_budget: DEFAULT_PROJECT_CODE_OPEN_RUNTIMES,
            eviction_idle: Duration::ZERO,
            exclusive_maintenance: false,
        }
    }
}

pub(super) enum CapacityReservation {
    Available,
    Exhausted,
    Eviction(EvictionReservation),
}

pub(super) struct EvictionReservation {
    key: StoreRuntimeKey,
    attempt: u64,
    handle: StoreRuntimeHandle,
}

impl StoreRuntimeRegistry {
    pub(super) fn reserve_project_code_capacity(
        &self,
        state: &mut RegistryState,
    ) -> Result<CapacityReservation, StoreRuntimeRegistryFailure> {
        let occupied = state
            .entries
            .keys()
            .filter(|key| !key.is_project_code_capacity_exempt())
            .count();
        if occupied < self.inner.config.project_budget() {
            return Ok(CapacityReservation::Available);
        }
        let candidate = state.entries.iter().find_map(|(key, entry)| {
            let RegistryEntry::Ready(ready) = entry else {
                return None;
            };
            (!key.is_project_code_capacity_exempt()
                && ready.handle.is_exclusively_held_by_registry()
                && Arc::strong_count(ready.handle.runtime()) == 1
                && ready
                    .handle
                    .runtime()
                    .eviction_eligibility(self.inner.config.eviction_idle())
                    .is_eligible())
            .then(|| key.clone())
        });
        let Some(candidate) = candidate else {
            return Ok(CapacityReservation::Exhausted);
        };
        let Some(attempt) = state.next_eviction_attempt.checked_add(1) else {
            return Err(StoreRuntimeRegistryFailure::EvictionAttemptExhausted);
        };
        state.next_eviction_attempt = attempt;
        let Some(RegistryEntry::Ready(ready)) = state.entries.remove(&candidate) else {
            return Ok(CapacityReservation::Exhausted);
        };
        if let Err(error) = ready
            .handle
            .runtime()
            .transition(RuntimeMaintenanceStateV1::Draining)
        {
            state.entries.insert(candidate, RegistryEntry::Ready(ready));
            return Err(StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
                message: error.to_string(),
            });
        }
        let handle = ready.handle;
        state.entries.insert(
            candidate.clone(),
            RegistryEntry::Evicting(EvictingRuntime {
                attempt,
                handle: handle.clone(),
            }),
        );
        Ok(CapacityReservation::Eviction(EvictionReservation {
            key: candidate,
            attempt,
            handle,
        }))
    }

    pub(super) fn complete_project_code_eviction(
        &self,
        reservation: EvictionReservation,
    ) -> Result<(), StoreRuntimeRegistryFailure> {
        let outcome = drain_and_close(&reservation.handle);
        if outcome.is_err() {
            let _ = reservation
                .handle
                .runtime()
                .transition(RuntimeMaintenanceStateV1::Faulted);
        }

        let mut state = self.lock_state();
        let entry = state.entries.remove(&reservation.key);
        let evicting = match entry {
            Some(RegistryEntry::Evicting(evicting)) if evicting.attempt == reservation.attempt => {
                evicting
            }
            Some(entry) => {
                state.entries.insert(reservation.key.clone(), entry);
                return Err(StoreRuntimeRegistryFailure::EvictionReservationLost {
                    key: Box::new(reservation.key),
                });
            }
            None => {
                return Err(StoreRuntimeRegistryFailure::EvictionReservationLost {
                    key: Box::new(reservation.key),
                });
            }
        };
        // Eviction is terminal once physical admission has been fenced. A
        // failed drain cannot be restored as Ready, but it must retain the
        // faulted attachment: dropping it could publish a second writer while
        // timed-out native work still owns the first one.
        if outcome.is_err() {
            state.entries.insert(
                reservation.key,
                RegistryEntry::Evicting(EvictingRuntime {
                    attempt: evicting.attempt,
                    handle: evicting.handle,
                }),
            );
            return outcome;
        }
        drop(state);
        drop(evicting);
        outcome
    }
}

pub(super) fn drain_and_close_physical(
    handle: &StoreRuntimeHandle,
) -> Result<(), StoreRuntimeRegistryFailure> {
    if let Err(message) = handle.inner.attachment.drain() {
        return Err(attachment_failure("drain", message));
    }
    let physical = handle.physical_snapshot();
    if !physical.is_drained() {
        return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeNotDrained { snapshot: physical });
    }
    if let Err(message) = handle.inner.attachment.close_and_join() {
        return Err(attachment_failure("close_and_join", message));
    }
    Ok(())
}

fn drain_and_close(handle: &StoreRuntimeHandle) -> Result<(), StoreRuntimeRegistryFailure> {
    drain_and_close_physical(handle)?;
    handle
        .runtime()
        .transition(RuntimeMaintenanceStateV1::Closed)
        .map_err(
            |error| StoreRuntimeRegistryFailure::RuntimeLifecycleFailed {
                message: error.to_string(),
            },
        )
}

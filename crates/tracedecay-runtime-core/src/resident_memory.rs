//! Process-wide admission for structurally measured resident allocations.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, Weak};

use tracedecay_domain::{CodeGenerationId, ProjectId, WorktreeId};

pub const DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1: NonZeroU64 =
    NonZeroU64::MIN.saturating_add(6 * 1024 * 1024 * 1024 - 1);

/// Stable component label inside one exact generation identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResidentMemoryComponentIdV1(&'static str);

impl ResidentMemoryComponentIdV1 {
    pub fn new(value: &'static str) -> Result<Self, ResidentMemoryComponentIdErrorV1> {
        if value.is_empty()
            || value.len() > 128
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(ResidentMemoryComponentIdErrorV1);
        }
        Ok(Self(value))
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("resident-memory component id must be canonical and at most 128 bytes")]
pub struct ResidentMemoryComponentIdErrorV1;

/// Exact owner of retained process memory.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResidentMemoryKeyV1 {
    pub project_id: ProjectId,
    pub worktree_id: WorktreeId,
    pub generation_id: CodeGenerationId,
    pub component: ResidentMemoryComponentIdV1,
}

/// Typed refusal after one bounded reclaim pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "resident-memory admission denied: used={used_bytes} requested={requested_bytes} limit={limit_bytes}"
)]
pub struct ResidentMemoryAdmissionFailureV1 {
    pub used_bytes: u64,
    pub requested_bytes: u64,
    pub limit_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "resident-memory reservation cannot grow after allocation: reserved={reserved_bytes} measured={measured_bytes}"
)]
pub struct ResidentMemoryAdjustmentFailureV1 {
    pub reserved_bytes: u64,
    pub measured_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("resident-memory reclaimer registration sequence exhausted")]
pub struct ResidentMemoryReclaimerRegistrationFailureV1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentMemoryChargeV1 {
    pub key: ResidentMemoryKeyV1,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentMemorySnapshotV1 {
    pub used_bytes: u64,
    pub limit_bytes: u64,
    pub charges: Vec<ResidentMemoryChargeV1>,
}

impl ResidentMemorySnapshotV1 {
    pub fn charge_for(&self, key: &ResidentMemoryKeyV1) -> u64 {
        self.charges
            .iter()
            .find(|charge| charge.key == *key)
            .map_or(0, |charge| charge.bytes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResidentMemoryReclaimRequestV1 {
    pub key: ResidentMemoryKeyV1,
    pub used_bytes: u64,
    pub requested_bytes: u64,
    pub limit_bytes: u64,
    pub shortfall_bytes: u64,
}

pub type ResidentMemoryReclaimerV1 = dyn Fn(ResidentMemoryReclaimRequestV1) + Send + Sync + 'static;

struct ReclaimerEntryV1 {
    callback: Arc<ResidentMemoryReclaimerV1>,
}

#[derive(Default)]
struct ResidentMemoryStateV1 {
    used_bytes: u64,
    charges: BTreeMap<ResidentMemoryKeyV1, u64>,
    reclaimers: BTreeMap<(u32, u64), Arc<ResidentMemoryReclaimerV1>>,
    next_reclaimer_sequence: u64,
}

/// The single process ceiling. Callers share one pointer-identical `Arc`.
pub struct ProcessResidentMemoryV1 {
    limit_bytes: NonZeroU64,
    state: Mutex<ResidentMemoryStateV1>,
}

impl fmt::Debug for ProcessResidentMemoryV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let snapshot = self.snapshot();
        formatter
            .debug_struct("ProcessResidentMemoryV1")
            .field("used_bytes", &snapshot.used_bytes)
            .field("limit_bytes", &snapshot.limit_bytes)
            .finish_non_exhaustive()
    }
}

impl ProcessResidentMemoryV1 {
    pub fn new(limit_bytes: NonZeroU64) -> Self {
        Self {
            limit_bytes,
            state: Mutex::new(ResidentMemoryStateV1::default()),
        }
    }

    pub fn reserve(
        self: &Arc<Self>,
        key: ResidentMemoryKeyV1,
        requested_bytes: NonZeroU64,
    ) -> Result<ResidentMemoryReservationV1, ResidentMemoryAdmissionFailureV1> {
        if let Some(reservation) = self.try_reserve(&key, requested_bytes) {
            return Ok(reservation);
        }

        if requested_bytes.get() <= self.limit_bytes.get() {
            let reclaimers = self.reclaimers();
            for reclaimer in reclaimers {
                (reclaimer.callback)(self.reclaim_request(key.clone(), requested_bytes));
                if let Some(reservation) = self.try_reserve(&key, requested_bytes) {
                    return Ok(reservation);
                }
            }
        }

        Err(self.admission_failure(requested_bytes))
    }

    pub fn register_reclaimer(
        self: &Arc<Self>,
        priority: u32,
        callback: Arc<ResidentMemoryReclaimerV1>,
    ) -> Result<ResidentMemoryReclaimerRegistrationV1, ResidentMemoryReclaimerRegistrationFailureV1>
    {
        let mut state = self.lock_state();
        let sequence = state.next_reclaimer_sequence;
        state.next_reclaimer_sequence = sequence
            .checked_add(1)
            .ok_or(ResidentMemoryReclaimerRegistrationFailureV1)?;
        state.reclaimers.insert((priority, sequence), callback);
        Ok(ResidentMemoryReclaimerRegistrationV1 {
            authority: Arc::downgrade(self),
            priority,
            sequence,
        })
    }

    pub fn snapshot(&self) -> ResidentMemorySnapshotV1 {
        let state = self.lock_state();
        ResidentMemorySnapshotV1 {
            used_bytes: state.used_bytes,
            limit_bytes: self.limit_bytes.get(),
            charges: state
                .charges
                .iter()
                .map(|(key, bytes)| ResidentMemoryChargeV1 {
                    key: key.clone(),
                    bytes: *bytes,
                })
                .collect(),
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ResidentMemoryStateV1> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn try_reserve(
        self: &Arc<Self>,
        key: &ResidentMemoryKeyV1,
        requested_bytes: NonZeroU64,
    ) -> Option<ResidentMemoryReservationV1> {
        let mut state = self.lock_state();
        let next_used = state.used_bytes.checked_add(requested_bytes.get())?;
        if next_used > self.limit_bytes.get() {
            return None;
        }
        state.used_bytes = next_used;
        *state.charges.entry(key.clone()).or_default() += requested_bytes.get();
        Some(ResidentMemoryReservationV1 {
            authority: Arc::clone(self),
            key: key.clone(),
            reserved_bytes: requested_bytes.get(),
        })
    }

    fn reclaimers(&self) -> Vec<ReclaimerEntryV1> {
        let state = self.lock_state();
        state
            .reclaimers
            .values()
            .map(|callback| ReclaimerEntryV1 {
                callback: Arc::clone(callback),
            })
            .collect()
    }

    fn reclaim_request(
        &self,
        key: ResidentMemoryKeyV1,
        requested_bytes: NonZeroU64,
    ) -> ResidentMemoryReclaimRequestV1 {
        let state = self.lock_state();
        let available_bytes = self.limit_bytes.get() - state.used_bytes;
        let shortfall_bytes = requested_bytes.get().saturating_sub(available_bytes);
        ResidentMemoryReclaimRequestV1 {
            key,
            used_bytes: state.used_bytes,
            requested_bytes: requested_bytes.get(),
            limit_bytes: self.limit_bytes.get(),
            shortfall_bytes,
        }
    }

    fn admission_failure(&self, requested_bytes: NonZeroU64) -> ResidentMemoryAdmissionFailureV1 {
        ResidentMemoryAdmissionFailureV1 {
            used_bytes: self.lock_state().used_bytes,
            requested_bytes: requested_bytes.get(),
            limit_bytes: self.limit_bytes.get(),
        }
    }

    fn shrink(
        &self,
        key: &ResidentMemoryKeyV1,
        reserved_bytes: u64,
        measured_bytes: u64,
    ) -> Result<(), ResidentMemoryAdjustmentFailureV1> {
        if measured_bytes > reserved_bytes {
            return Err(ResidentMemoryAdjustmentFailureV1 {
                reserved_bytes,
                measured_bytes,
            });
        }
        let released_bytes = reserved_bytes - measured_bytes;
        if released_bytes == 0 {
            return Ok(());
        }
        let mut state = self.lock_state();
        state.used_bytes -= released_bytes;
        if let Some(charge) = state.charges.get_mut(key) {
            *charge -= released_bytes;
            if *charge == 0 {
                state.charges.remove(key);
            }
        }
        Ok(())
    }

    fn release(&self, key: &ResidentMemoryKeyV1, reserved_bytes: u64) {
        if reserved_bytes == 0 {
            return;
        }
        let mut state = self.lock_state();
        state.used_bytes -= reserved_bytes;
        if let Some(charge) = state.charges.get_mut(key) {
            *charge -= reserved_bytes;
            if *charge == 0 {
                state.charges.remove(key);
            }
        }
    }
}

pub struct ResidentMemoryReservationV1 {
    authority: Arc<ProcessResidentMemoryV1>,
    key: ResidentMemoryKeyV1,
    reserved_bytes: u64,
}

impl fmt::Debug for ResidentMemoryReservationV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResidentMemoryReservationV1")
            .field("key", &self.key)
            .field("reserved_bytes", &self.reserved_bytes)
            .finish_non_exhaustive()
    }
}

impl ResidentMemoryReservationV1 {
    pub fn key(&self) -> &ResidentMemoryKeyV1 {
        &self.key
    }

    pub const fn reserved_bytes(&self) -> u64 {
        self.reserved_bytes
    }

    pub fn shrink_to(
        &mut self,
        measured_bytes: u64,
    ) -> Result<(), ResidentMemoryAdjustmentFailureV1> {
        self.authority
            .shrink(&self.key, self.reserved_bytes, measured_bytes)?;
        self.reserved_bytes = measured_bytes;
        Ok(())
    }
}

impl Drop for ResidentMemoryReservationV1 {
    fn drop(&mut self) {
        self.authority.release(&self.key, self.reserved_bytes);
    }
}

pub struct ResidentMemoryReclaimerRegistrationV1 {
    authority: Weak<ProcessResidentMemoryV1>,
    priority: u32,
    sequence: u64,
}

impl Drop for ResidentMemoryReclaimerRegistrationV1 {
    fn drop(&mut self) {
        let Some(authority) = self.authority.upgrade() else {
            return;
        };
        authority
            .lock_state()
            .reclaimers
            .remove(&(self.priority, self.sequence));
    }
}

#[cfg(test)]
mod tests;

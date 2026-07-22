//! Thread-safe admission accounting for one shard.

mod queue;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use tracedecay_store::{OperationPriorityV1, SaturationScopeV1, StoreOperationMetadataV1};

#[cfg(test)]
pub(crate) use queue::Selection;
pub(crate) use queue::{FairQueue, QueueItem};

pub(crate) const DEFAULT_RESERVED_HEALTH_OPERATIONS: u32 = 1;
pub(crate) const DEFAULT_RESERVED_HEALTH_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lane {
    General,
    Health,
}

impl Lane {
    fn for_priority(priority: OperationPriorityV1) -> Self {
        match priority {
            OperationPriorityV1::Health => Self::Health,
            OperationPriorityV1::Foreground | OperationPriorityV1::Background => Self::General,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Capacity {
    pub(crate) operations: u32,
    pub(crate) bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Limits {
    pub(crate) general: Capacity,
    pub(crate) health: Capacity,
    pub(crate) foreground_request_bytes: u64,
    pub(crate) background_request_bytes: u64,
}

impl Limits {
    pub(crate) fn new(
        general: Capacity,
        health: Capacity,
        foreground_request_bytes: u64,
        background_request_bytes: u64,
    ) -> Option<Self> {
        (general.operations > 0
            && general.bytes > 0
            && health.operations > 0
            && health.bytes > 0
            && foreground_request_bytes > 0
            && background_request_bytes > 0)
            .then_some(Self {
                general,
                health,
                foreground_request_bytes,
                background_request_bytes,
            })
    }

    fn for_lane(self, lane: Lane) -> Capacity {
        match lane {
            Lane::General => self.general,
            Lane::Health => self.health,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Usage {
    pub(crate) operations: u32,
    pub(crate) bytes: u64,
}

#[derive(Debug)]
struct State {
    limits: Limits,
    general: Usage,
    health: Usage,
}

impl State {
    fn usage(&self, lane: Lane) -> Usage {
        match lane {
            Lane::General => self.general,
            Lane::Health => self.health,
        }
    }

    fn usage_mut(&mut self, lane: Lane) -> &mut Usage {
        match lane {
            Lane::General => &mut self.general,
            Lane::Health => &mut self.health,
        }
    }
}

/// The sole admission authority. A successful reservation remains charged
/// from `submit` until the accepted request sends its terminal reply.
#[derive(Clone, Debug)]
pub(crate) struct Admission {
    state: Arc<Mutex<State>>,
}

impl Admission {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                limits,
                general: Usage::default(),
                health: Usage::default(),
            })),
        }
    }

    pub(crate) fn reserve(
        &self,
        metadata: &StoreOperationMetadataV1,
    ) -> Result<Permit, SaturationScopeV1> {
        let lane = Lane::for_priority(metadata.priority);
        let bytes = metadata.admission_bytes;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let capacity = state.limits.for_lane(lane);
        let request_limit = match metadata.priority {
            OperationPriorityV1::Background => state.limits.background_request_bytes,
            OperationPriorityV1::Health | OperationPriorityV1::Foreground => {
                state.limits.foreground_request_bytes
            }
        };
        let usage = state.usage(lane);
        if usage.operations >= capacity.operations {
            return Err(SaturationScopeV1::ShardOperations);
        }
        if bytes > request_limit
            || bytes > capacity.bytes
            || usage
                .bytes
                .checked_add(bytes)
                .is_none_or(|total| total > capacity.bytes)
        {
            return Err(SaturationScopeV1::ShardBytes);
        }
        let usage = state.usage_mut(lane);
        usage.operations += 1;
        usage.bytes += bytes;
        Ok(Permit {
            state: Arc::clone(&self.state),
            lane,
            bytes,
        })
    }

    #[cfg(test)]
    fn usage(&self, lane: Lane) -> Usage {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .usage(lane)
    }
}

#[must_use = "the permit must be retained through the request's terminal reply"]
#[derive(Debug)]
pub(crate) struct Permit {
    state: Arc<Mutex<State>>,
    lane: Lane,
    bytes: u64,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let usage = state.usage_mut(self.lane);
        usage.operations = usage.operations.saturating_sub(1);
        usage.bytes = usage.bytes.saturating_sub(self.bytes);
    }
}

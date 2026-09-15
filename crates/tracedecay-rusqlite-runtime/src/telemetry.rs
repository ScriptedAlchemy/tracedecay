//! Canonical DB timing and queue instrumentation for this crate.
//!
//! Callers must use these snapshots — they must not invent parallel counters.

mod lock_work;
mod reader;
mod recorder;
mod sqlite_vm;
mod store_size;
#[cfg(test)]
mod tests;

use std::time::Duration;

use tracedecay_store::{CommitSequenceV1, DurabilityClassV1, OperationPriorityV1, StoreClientIdV1};

pub(crate) use lock_work::{LockWorkScope, record_decoded_bytes, record_encoded_bytes};
pub(crate) use reader::ReaderAdmissionRecorder;
pub(crate) use recorder::WriterTelemetry;
pub(crate) use sqlite_vm::{observe_statement, take_observed_vm};
pub use store_size::SqliteStoreSizeTelemetryPort;

pub(crate) const MAX_TRACKED_WRITER_CLIENTS: usize = 64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WriterOperationCounters {
    pub offered_operations: u64,
    pub admitted_operations: u64,
    pub completed_operations: u64,
    pub shed_operations: u64,
    pub retried_operations: u64,
    pub cancelled_operations: u64,
    pub deadline_exceeded_operations: u64,
    pub conflicted_operations: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WriterQueueSnapshot {
    pub queued_operations: u32,
    pub queued_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WriterServiceCounts {
    pub health_services: u64,
    pub foreground_services: u64,
    pub background_services: u64,
}

impl WriterServiceCounts {
    pub(crate) fn record(&mut self, priority: OperationPriorityV1, operations: u64) {
        let counter = match priority {
            OperationPriorityV1::Health => &mut self.health_services,
            OperationPriorityV1::Foreground => &mut self.foreground_services,
            OperationPriorityV1::Background => &mut self.background_services,
        };
        *counter = counter.saturating_add(operations);
    }

    pub(crate) fn total(self) -> u64 {
        self.health_services
            .saturating_add(self.foreground_services)
            .saturating_add(self.background_services)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriterClientServiceSnapshot {
    pub client_id: StoreClientIdV1,
    pub services: WriterServiceCounts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriterBatchMetrics {
    pub priority: OperationPriorityV1,
    pub durability: DurabilityClassV1,
    pub batch_operations: u32,
    pub batch_bytes: u64,
    pub queue_wait_micros: u64,
    pub transaction_micros: u64,
    pub lock_held_micros: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WriterBatchTotals {
    pub committed_batches: u64,
    pub batch_operations: u64,
    pub batch_bytes: u64,
    pub queue_wait_micros: u64,
    pub transaction_micros: u64,
    pub lock_held_micros: u64,
    pub total_latency_micros: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WriterTransactionTotals {
    pub committed_transactions: u64,
    pub rolled_back_transactions: u64,
    pub commands: u64,
    pub rows: u64,
    pub lock_held_micros: u64,
    pub transaction_micros: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriterTransactionOutcome {
    Committed,
    RolledBack,
    Busy,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriterTransactionMetrics {
    pub outcome: WriterTransactionOutcome,
    pub commands: u64,
    pub rows: u64,
    pub lock_held_micros: u64,
    pub transaction_micros: u64,
    pub sqlite_vm: SqliteVmSnapshot,
    pub lock_work: WriterLockWorkSnapshot,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SqliteVmSnapshot {
    pub fullscan_steps: u64,
    pub sort_steps: u64,
    pub vm_steps: u64,
}

impl SqliteVmSnapshot {
    pub(crate) fn saturating_add(self, other: Self) -> Self {
        Self {
            fullscan_steps: self.fullscan_steps.saturating_add(other.fullscan_steps),
            sort_steps: self.sort_steps.saturating_add(other.sort_steps),
            vm_steps: self.vm_steps.saturating_add(other.vm_steps),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalCheckpointSnapshot {
    pub wal_frames: u64,
    pub wal_bytes: u64,
    pub checkpointed_frames: u64,
    pub reclaimed_frames: u64,
    pub busy_events: u64,
    pub blocker_count: u64,
    pub hard_pressure_events: u64,
    pub hard_retry_wakes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalCheckpointSample {
    pub wal_frames: u64,
    pub wal_bytes: u64,
    pub checkpointed_frames: u64,
    pub reclaimed_frames: u64,
    pub busy: bool,
    pub blocker_count: u64,
    pub hard_pressure: bool,
    pub completed: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WriterLockWorkSnapshot {
    pub bytes_encoded: u64,
    pub bytes_decoded: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReaderAdmissionSnapshot {
    pub acquire_events: u64,
    pub wait_events: u64,
    pub saturated_events: u64,
    pub interrupted_events: u64,
    pub release_events: u64,
    pub wait_micros: u64,
    pub execution_micros: u64,
    pub sqlite_vm: SqliteVmSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriterCommitSnapshot {
    pub commit_sequence: CommitSequenceV1,
    pub batch: WriterBatchMetrics,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WriterTelemetrySnapshot {
    pub operations: WriterOperationCounters,
    pub queue: WriterQueueSnapshot,
    pub priority_services: WriterServiceCounts,
    pub client_services: Vec<WriterClientServiceSnapshot>,
    pub omitted_client_service_operations: u64,
    pub batches: WriterBatchTotals,
    pub commit_sequence: CommitSequenceV1,
    pub busy_events: u64,
    pub error_events: u64,
    pub health_lane_services: u64,
    pub latest_commit: Option<WriterCommitSnapshot>,
    pub transactions: WriterTransactionTotals,
    pub sqlite_vm: SqliteVmSnapshot,
    pub wal: WalCheckpointSnapshot,
    pub lock_work: WriterLockWorkSnapshot,
}

pub(crate) fn duration_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

//! Bounded snapshots recorded by one shared writer telemetry authority.

mod recorder;
mod store_size;
#[cfg(test)]
mod tests;

use tracedecay_store::{CommitSequenceV1, DurabilityClassV1, OperationPriorityV1, StoreClientIdV1};

pub(crate) use recorder::WriterTelemetry;
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
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WriterBatchTotals {
    pub committed_batches: u64,
    pub batch_operations: u64,
    pub batch_bytes: u64,
    pub queue_wait_micros: u64,
    pub transaction_micros: u64,
    pub total_latency_micros: u64,
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
}

//! Driver-free read-consistency coordination.
//!
//! This module consumes writer publication and retained-snapshot authorities
//! through narrow ports. It does not inspect the commit ledger, open a store,
//! or imply that a frozen multi-shard vector is a distributed transaction.

mod coordinator;
mod ports;

pub use crate::watermark::CommitWatermarkSubscription;
pub(crate) use crate::watermark::{CommitWatermarkPublicationError, CommittedWatermarkPublisher};
pub use coordinator::{ReadConsistencyConfig, ReadConsistencyCoordinator, SystemConsistencyClock};
pub use ports::{
    CommitWatermarkSource, ConsistencyClock, RetainedSnapshotRegistry, RetainedSnapshotState,
    WatermarkSourceState,
};

#[cfg(test)]
mod tests;

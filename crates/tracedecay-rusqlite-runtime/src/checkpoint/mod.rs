//! Writer-owned WAL checkpoint policy.
//!
//! The persistent shard writer is the only intended owner of this controller.
//! It disables SQLite's connection-local automatic checkpointing and makes WAL
//! pressure and reader blockers explicit.
//! This module never deletes a database sidecar and does not create a writer,
//! scheduler, thread, or connection.

mod controller;
mod driver;
mod foreign;
mod types;

pub(crate) use controller::WriterCheckpointController;
pub(crate) use driver::{RusqliteCheckpointDriver, RusqliteCheckpointError};
pub(crate) use foreign::foreign_wal_holders;
pub(crate) use types::FENCE_READ_DEADLINE;
pub use types::{
    CheckpointBlocker, CheckpointBlockers, CheckpointFrameReport, CheckpointInterruption,
    CheckpointKind, CheckpointOutcome, CheckpointPressure, CheckpointStatus, CheckpointWal,
};
pub(crate) use types::{
    CheckpointBlockerSource, CheckpointConfig, CheckpointDecision, CheckpointError,
    CheckpointResult,
};
#[cfg(test)]
pub(crate) use types::{
    CheckpointConfigError, CheckpointMode, CheckpointReport, WalPressure, WalSample,
};

#[cfg(test)]
mod tests;

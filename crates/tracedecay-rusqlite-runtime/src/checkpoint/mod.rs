//! Writer-owned WAL checkpoint policy.
//!
//! The persistent shard writer is the only intended owner of this controller.
//! It disables SQLite's connection-local automatic checkpointing and makes WAL
//! pressure, reader blockers, and maintenance-only checkpoint modes explicit.
//! This module never deletes a database sidecar and does not create a writer,
//! scheduler, thread, or connection.

mod controller;
mod driver;
mod types;

pub(crate) use controller::WriterCheckpointController;
#[cfg(test)]
pub(crate) use driver::CheckpointDriver;
pub(crate) use driver::{RusqliteCheckpointDriver, RusqliteCheckpointError};
pub use types::{
    CheckpointBlocker, CheckpointBlockers, CheckpointFrameReport, CheckpointInterruption,
    CheckpointKind, CheckpointOutcome, CheckpointPressure, CheckpointStatus, CheckpointWal,
    MaintenanceCheckpointMode,
};
pub(crate) use types::{CheckpointConfig, CheckpointDecision, CheckpointError, CheckpointResult};
#[cfg(test)]
pub(crate) use types::{
    CheckpointConfigError, CheckpointMode, CheckpointReport, WalPressure, WalSample,
};

#[cfg(test)]
mod tests;

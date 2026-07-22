//! Policy boundary for diagnosing and repairing SQLite corruption.
//!
//! This module deliberately exposes capabilities rather than SQL. Adapters may
//! rebuild a derived FTS projection, but they cannot use this coordinator to
//! delete a database or its WAL/SHM companions. In particular, a diagnosis or
//! open failure is returned unchanged and never starts maintenance.

mod coordinator;
mod model;
mod sqlite;

pub use coordinator::{CorruptionProbe, MaintenanceAuthorization, RepairCoordinator, RepairDriver};
pub use model::{
    CorruptionClass, CorruptionDiagnosis, CorruptionEvidence, CorruptionObservation, FaultStage,
    QuarantineReceipt, RejectionReason, RepairFault, RepairOutcome, RepairReceipt,
};
pub use sqlite::{
    FilesystemQuarantineStore, QuarantineStore, SqliteCorruptionProbe, SqliteRepairDriver,
};

#[cfg(test)]
mod tests;

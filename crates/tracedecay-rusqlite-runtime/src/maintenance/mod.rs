//! Exclusive, fenced maintenance orchestration for one published shard.
//!
//! The coordinator owns lifecycle ordering only. Concrete SQLite work stays
//! behind [`coordinator::MaintenanceDriver`], whose closed methods cannot accept SQL or
//! filesystem paths. Locator resolution and registry publication remain daemon
//! responsibilities.

mod coordinator;
mod sqlite;
mod types;

pub use coordinator::{
    CanonicalRegistryAuthority, MaintenanceCoordinator, MaintenanceDriver, MaintenanceLifecycle,
};
pub use sqlite::{
    MaintenanceArtifactInstaller, SqliteFtsIndex, SqliteMaintenanceCatalog,
    SqliteMaintenanceDriver, SqliteMigration,
};
pub use types::*;

#[cfg(test)]
mod tests;

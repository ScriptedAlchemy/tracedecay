//! Compatibility façade for runtime database primitives.

pub mod migrations {
    pub use tracedecay_runtime_core::db::migrations::*;
}

#[cfg(test)]
pub(crate) use tracedecay_runtime_core::db::database_path_is_tombstoned;
#[cfg(windows)]
pub(crate) use tracedecay_runtime_core::db::windows_hard_link_count;
pub use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, DatabaseAuthorityRole, DependencyImportUse, RedundancyPairRow,
    RedundancyPairWrite, SQLITE_UNSAFE_FAST_ENV, StoredFingerprint,
    enter_maintenance_database_scope,
};
pub(crate) use tracedecay_runtime_core::db::{
    DatabaseDeletionFence, DatabaseDeletionStates, WriterOwnership, enter_daemon_database_scope,
    is_lock_contended, platform_safe_journal_mode, platform_safe_synchronous_mode,
    probe_writer_owner,
};
